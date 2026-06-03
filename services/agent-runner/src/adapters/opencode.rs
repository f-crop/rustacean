use std::collections::HashMap;

use anyhow::{Context, Result};
use async_trait::async_trait;
use rb_schemas::AgentRuntime;
use tokio::io::AsyncWriteExt;

use super::{
    AgentProcess, LineKind, ParsedLine, RuntimeAdapter, RuntimeCaps, RuntimeManifest, SessionCtx,
    build_base_command, write_mcp_config,
};

#[derive(Debug)]
enum LlmMode {
    LiteLlm {
        base_url: String,
        api_key: String,
        model: String,
    },
    OpenAiCompatible,
    DirectProvider,
}

impl LlmMode {
    fn from_env() -> Result<Self> {
        if let Some(base_url) = std::env::var("LITELLM_BASE_URL")
            .ok()
            .filter(|v| !v.is_empty())
        {
            let api_key = std::env::var("LITELLM_API_KEY")
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "error_kind=litellm_misconfigured: \
                         LITELLM_API_KEY is required when LITELLM_BASE_URL is set"
                    )
                })?;
            let model = std::env::var("CHAT_MODEL")
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "error_kind=litellm_misconfigured: \
                         CHAT_MODEL is required when LITELLM_BASE_URL is set"
                    )
                })?;
            Ok(Self::LiteLlm {
                base_url,
                api_key,
                model,
            })
        } else if std::env::var("OPENCODE_API_BASE")
            .ok()
            .as_ref()
            .is_some_and(|v| !v.is_empty())
        {
            Ok(Self::OpenAiCompatible)
        } else {
            Ok(Self::DirectProvider)
        }
    }
}

pub struct OpencodeAdapter {
    default_provider: String,
    default_model: String,
}

impl OpencodeAdapter {
    pub fn new() -> Self {
        Self {
            default_provider: std::env::var("OPENCODE_DEFAULT_PROVIDER")
                .unwrap_or_else(|_| "anthropic".to_string()),
            default_model: std::env::var("OPENCODE_DEFAULT_MODEL")
                .unwrap_or_else(|_| "claude-sonnet-4-20250514".to_string()),
        }
    }

    async fn write_opencode_config(
        &self,
        workspace: &std::path::Path,
        mode: &LlmMode,
    ) -> Result<()> {
        let config = match mode {
            LlmMode::LiteLlm {
                base_url,
                api_key,
                model,
            } => {
                // Dynamic model key requires manual Map construction — serde_json::json!
                // does not support variable interpolation in object keys.
                let mut models = serde_json::Map::new();
                models.insert(model.clone(), serde_json::json!({ "name": model }));
                serde_json::json!({
                    "$schema": "https://opencode.ai/config.json",
                    "provider": {
                        "litellm": {
                            "npm": "@ai-sdk/openai-compatible",
                            "options": {
                                "baseURL": format!("{}/v1", base_url),
                                "apiKey": api_key
                            },
                            "models": serde_json::Value::Object(models)
                        }
                    },
                    "model": format!("litellm/{}", model)
                })
            }
            LlmMode::OpenAiCompatible | LlmMode::DirectProvider => serde_json::json!({
                "provider": self.default_provider,
                "model": self.default_model,
            }),
        };
        let opencode_dir = workspace.join(".opencode");
        tokio::fs::create_dir_all(&opencode_dir).await?;
        let config_path = opencode_dir.join("config.json");
        tokio::fs::write(&config_path, serde_json::to_string_pretty(&config)?)
            .await
            .with_context(|| {
                format!(
                    "Failed to write opencode config to {}",
                    config_path.display()
                )
            })?;
        Ok(())
    }

    fn collect_provider_env() -> HashMap<String, String> {
        let mut env_vars = HashMap::new();
        // LiteLLM proxy vars (forwarded so the opencode child can also read them).
        for key in &["LITELLM_BASE_URL", "LITELLM_API_KEY", "CHAT_MODEL"] {
            if let Ok(val) = std::env::var(key) {
                env_vars.insert((*key).to_string(), val);
            }
        }
        // Direct provider API keys.
        for key in &[
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GOOGLE_API_KEY",
            "GROQ_API_KEY",
            "MISTRAL_API_KEY",
        ] {
            if let Ok(val) = std::env::var(key) {
                env_vars.insert((*key).to_string(), val);
            }
        }
        if let Ok(api_base) = std::env::var("OPENCODE_API_BASE") {
            env_vars.insert("OPENCODE_API_BASE".to_string(), api_base);
        }
        env_vars
    }
}

#[async_trait]
impl RuntimeAdapter for OpencodeAdapter {
    fn manifest(&self) -> RuntimeManifest {
        RuntimeManifest {
            kind: rb_schemas::AgentRuntime::Opencode,
            binary: "opencode",
            required_env: &[],
            capabilities: RuntimeCaps {
                multi_turn: false,
                streams_json: true,
            },
        }
    }

    async fn spawn(&self, ctx: &SessionCtx) -> Result<AgentProcess> {
        // Resolve and validate the LLM routing mode before touching the filesystem
        // or spawning any subprocess. A misconfigured LiteLLM setup fails here,
        // not silently mid-session.
        let mode = LlmMode::from_env()?;

        write_mcp_config(&ctx.workspace_path, &ctx.api_key, &ctx.tenant_id)
            .await
            .context("Failed to write MCP config")?;
        self.write_opencode_config(&ctx.workspace_path, &mode)
            .await
            .context("Failed to write opencode config")?;

        let mut cmd = build_base_command("opencode", &ctx.workspace_path);
        cmd.env("RB_AGENT_API_KEY", &ctx.api_key)
            .env("RB_AGENT_TENANT_ID", &ctx.tenant_id);

        for (key, val) in Self::collect_provider_env() {
            cmd.env(key, val);
        }

        if !ctx.initial_prompt.is_empty() {
            // `--` terminates flag parsing so a prompt starting with `-` cannot
            // inject CLI flags into the spawned process.
            cmd.args(["run", "--", &ctx.initial_prompt]);
        }

        let mut child = cmd.spawn().context("Failed to spawn opencode process")?;
        let pid = child.id().context("Failed to get process ID")?;
        let stdin = child.stdin.take();

        Ok(AgentProcess {
            child: Some(child),
            pid,
            runtime: AgentRuntime::Opencode,
            stdin,
        })
    }

    async fn send_input(&self, proc: &mut AgentProcess, input: &str) -> Result<()> {
        if let Some(stdin) = proc.stdin.as_mut() {
            stdin.write_all(input.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
            Ok(())
        } else {
            anyhow::bail!("Process stdin not available")
        }
    }

    async fn terminate(&self, proc: &mut AgentProcess, force: bool) -> Result<()> {
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;
            let signal = if force {
                Signal::SIGKILL
            } else {
                Signal::SIGTERM
            };
            // H3: Never fallback to i32::MAX — would send signal to wrong process.
            // Linux PIDs are constrained to fit in i32; overflow is impossible on valid systems.
            let pid_i32 = i32::try_from(proc.pid)
                .map_err(|_| anyhow::anyhow!("PID {} exceeds i32 range", proc.pid))?;
            kill(Pid::from_raw(pid_i32), signal).context("Failed to send signal")?;
        }
        #[cfg(not(unix))]
        if let Some(ref mut c) = proc.child {
            c.kill().await?;
        }
        Ok(())
    }

    fn parse_stdout_line(&self, line: &str) -> Option<ParsedLine> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.starts_with('{') {
            Some(ParsedLine {
                kind: LineKind::Json,
                payload: trimmed.to_string(),
            })
        } else {
            Some(ParsedLine {
                kind: LineKind::Text,
                payload: line.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    // Serialize tests that mutate process environment variables.
    static ENV_LOCK: Mutex<()> = Mutex::const_new(());

    fn adapter() -> OpencodeAdapter {
        OpencodeAdapter {
            default_provider: "anthropic".to_string(),
            default_model: "claude-sonnet-4-20250514".to_string(),
        }
    }

    fn read_config(dir: &TempDir) -> serde_json::Value {
        let raw = std::fs::read_to_string(dir.path().join(".opencode/config.json")).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    // ── Mode 1: LiteLLM ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn litellm_mode_writes_correct_config() {
        let _guard = ENV_LOCK.lock().await;
        let tmp = TempDir::new().unwrap();
        // SAFETY: ENV_LOCK serializes all env mutations across these tests.
        unsafe {
            std::env::set_var("LITELLM_BASE_URL", "http://litellm:4000");
            std::env::set_var("LITELLM_API_KEY", "test-virtual-key");
            std::env::set_var("CHAT_MODEL", "glm-latest");
            std::env::remove_var("OPENCODE_API_BASE");
        }

        let mode = LlmMode::from_env().unwrap();
        adapter()
            .write_opencode_config(tmp.path(), &mode)
            .await
            .unwrap();
        let cfg = read_config(&tmp);

        assert_eq!(cfg["$schema"], "https://opencode.ai/config.json");
        assert_eq!(cfg["model"], "litellm/glm-latest");
        assert_eq!(
            cfg["provider"]["litellm"]["npm"],
            "@ai-sdk/openai-compatible"
        );
        assert_eq!(
            cfg["provider"]["litellm"]["options"]["baseURL"],
            "http://litellm:4000/v1"
        );
        assert_eq!(
            cfg["provider"]["litellm"]["options"]["apiKey"],
            "test-virtual-key"
        );
        assert_eq!(
            cfg["provider"]["litellm"]["models"]["glm-latest"]["name"],
            "glm-latest"
        );

        // Cleanup
        unsafe {
            std::env::remove_var("LITELLM_BASE_URL");
            std::env::remove_var("LITELLM_API_KEY");
            std::env::remove_var("CHAT_MODEL");
        }
    }

    // ── Mode 2: OpenAI-compatible ─────────────────────────────────────────────

    #[tokio::test]
    async fn openai_compatible_mode_writes_simple_config() {
        let _guard = ENV_LOCK.lock().await;
        let tmp = TempDir::new().unwrap();
        // SAFETY: ENV_LOCK serializes all env mutations across these tests.
        unsafe {
            std::env::remove_var("LITELLM_BASE_URL");
            std::env::set_var("OPENCODE_API_BASE", "http://local-ollama:11434/v1");
            std::env::set_var("OPENCODE_DEFAULT_PROVIDER", "openai");
            std::env::set_var("OPENCODE_DEFAULT_MODEL", "llama3");
        }

        let mode = LlmMode::from_env().unwrap();
        // For OpenAI-compatible, adapter re-reads OPENCODE_DEFAULT_* from env.
        let a = OpencodeAdapter::new();
        a.write_opencode_config(tmp.path(), &mode).await.unwrap();
        let cfg = read_config(&tmp);

        // Config.json keeps the simple {provider, model} shape; no $schema or
        // nested litellm block. OPENCODE_API_BASE is forwarded via env, not config.
        assert_eq!(cfg["provider"], "openai");
        assert_eq!(cfg["model"], "llama3");
        assert!(cfg.get("$schema").is_none());

        // Cleanup
        unsafe {
            std::env::remove_var("OPENCODE_API_BASE");
            std::env::remove_var("OPENCODE_DEFAULT_PROVIDER");
            std::env::remove_var("OPENCODE_DEFAULT_MODEL");
        }
    }

    // ── Mode 3: LiteLLM misconfigured → fail-fast ─────────────────────────────

    #[tokio::test]
    async fn litellm_misconfigured_fails_fast_when_api_key_missing() {
        let _guard = ENV_LOCK.lock().await;
        // SAFETY: ENV_LOCK serializes all env mutations across these tests.
        unsafe {
            std::env::set_var("LITELLM_BASE_URL", "http://litellm:4000");
            std::env::remove_var("LITELLM_API_KEY");
            std::env::remove_var("CHAT_MODEL");
        }

        let err = LlmMode::from_env().unwrap_err();
        assert!(
            err.to_string().contains("litellm_misconfigured"),
            "expected litellm_misconfigured, got: {err}"
        );

        // Cleanup
        unsafe {
            std::env::remove_var("LITELLM_BASE_URL");
        }
    }

    // ── Empty-string regression ───────────────────────────────────────────────
    // compose/dev.yml uses `${LITELLM_BASE_URL:-}` (empty default). An empty
    // string must be treated as "not set" and resolve to DirectProvider, not
    // LiteLlm mode.

    #[tokio::test]
    async fn empty_litellm_base_url_resolves_to_direct_provider() {
        let _guard = ENV_LOCK.lock().await;
        // SAFETY: ENV_LOCK serializes all env mutations across these tests.
        unsafe {
            std::env::set_var("LITELLM_BASE_URL", "");
            std::env::remove_var("OPENCODE_API_BASE");
        }

        let mode = LlmMode::from_env().unwrap();
        assert!(
            matches!(mode, LlmMode::DirectProvider),
            "empty LITELLM_BASE_URL should resolve to DirectProvider, got: {mode:?}"
        );

        // Cleanup
        unsafe {
            std::env::remove_var("LITELLM_BASE_URL");
        }
    }
}
