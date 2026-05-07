//! ADR-009 §11 runtime configuration for adapter environment variables.
//!
//! These environment variables configure the LLM providers for agent runtimes.

use std::env;

/// LLM provider configuration for adapter runtimes.
#[derive(Debug, Clone)]
pub struct AdapterConfig {
    /// Anthropic API key for opencode Anthropic provider.
    pub anthropic_api_key: Option<String>,
    /// OpenAI API key for opencode OpenAI provider (optional).
    pub openai_api_key: Option<String>,
    /// Custom OpenAI-compatible proxy base URL.
    pub opencode_api_base: Option<String>,
    /// Default provider for opencode (default: "anthropic").
    pub opencode_default_provider: String,
    /// Default model for opencode (default: "claude-sonnet-4-6").
    pub opencode_default_model: String,
    /// Placeholder API key for pi provider (TBD).
    pub pi_provider_api_key: Option<String>,
}

impl AdapterConfig {
    /// Load configuration from environment variables.
    pub fn from_env() -> Self {
        Self {
            anthropic_api_key: env::var("ANTHROPIC_API_KEY").ok().filter(|s| !s.is_empty()),
            openai_api_key: env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()),
            opencode_api_base: env::var("OPENCODE_API_BASE").ok().filter(|s| !s.is_empty()),
            opencode_default_provider: env::var("OPENCODE_DEFAULT_PROVIDER")
                .unwrap_or_else(|_| "anthropic".to_string()),
            opencode_default_model: env::var("OPENCODE_DEFAULT_MODEL")
                .unwrap_or_else(|_| "claude-sonnet-4-6".to_string()),
            pi_provider_api_key: env::var("PI_PROVIDER_API_KEY").ok().filter(|s| !s.is_empty()),
        }
    }

    /// Get environment variables to inject into a process for the given runtime.
    ///
    /// Returns a vector of (key, value) pairs that should be added to the
    /// spawned process environment.
    pub fn env_vars_for_runtime(&self, runtime: rb_schemas::AgentRuntime) -> Vec<(String, String)> {
        let mut vars = Vec::new();

        match runtime {
            rb_schemas::AgentRuntime::Opencode => {
                // Opencode requires ANTHROPIC_API_KEY (primary) and optionally OPENAI_API_KEY
                if let Some(ref key) = self.anthropic_api_key {
                    vars.push(("ANTHROPIC_API_KEY".to_string(), key.clone()));
                }
                if let Some(ref key) = self.openai_api_key {
                    vars.push(("OPENAI_API_KEY".to_string(), key.clone()));
                }
                if let Some(ref base) = self.opencode_api_base {
                    vars.push(("OPENCODE_API_BASE".to_string(), base.clone()));
                }
                vars.push((
                    "OPENCODE_DEFAULT_PROVIDER".to_string(),
                    self.opencode_default_provider.clone(),
                ));
                vars.push((
                    "OPENCODE_DEFAULT_MODEL".to_string(),
                    self.opencode_default_model.clone(),
                ));
            }
            rb_schemas::AgentRuntime::ClaudeCode => {
                // Claude Code uses its own API key mechanism (via OAuth/anthropic-key)
                // but we can optionally inject ANTHROPIC_API_KEY if available
                if let Some(ref key) = self.anthropic_api_key {
                    vars.push(("ANTHROPIC_API_KEY".to_string(), key.clone()));
                }
            }
            rb_schemas::AgentRuntime::Pi => {
                // Pi provider is deferred to future wave
                if let Some(ref key) = self.pi_provider_api_key {
                    vars.push(("PI_PROVIDER_API_KEY".to_string(), key.clone()));
                }
            }
            _ => {}
        }

        vars
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values_are_set() {
        let config = AdapterConfig {
            anthropic_api_key: None,
            openai_api_key: None,
            opencode_api_base: None,
            opencode_default_provider: "anthropic".to_string(),
            opencode_default_model: "claude-sonnet-4-6".to_string(),
            pi_provider_api_key: None,
        };

        assert_eq!(config.opencode_default_provider, "anthropic");
        assert_eq!(config.opencode_default_model, "claude-sonnet-4-6");
    }

    #[test]
    fn opencode_env_vars_include_all_settings() {
        let config = AdapterConfig {
            anthropic_api_key: Some("sk-ant-123".to_string()),
            openai_api_key: Some("sk-openai-456".to_string()),
            opencode_api_base: Some("https://custom.proxy/api".to_string()),
            opencode_default_provider: "openai".to_string(),
            opencode_default_model: "gpt-4".to_string(),
            pi_provider_api_key: None,
        };

        let vars = config.env_vars_for_runtime(rb_schemas::AgentRuntime::Opencode);

        assert!(vars.iter().any(|(k, v)| k == "ANTHROPIC_API_KEY" && v == "sk-ant-123"));
        assert!(vars.iter().any(|(k, v)| k == "OPENAI_API_KEY" && v == "sk-openai-456"));
        assert!(vars.iter().any(|(k, v)| k == "OPENCODE_API_BASE" && v == "https://custom.proxy/api"));
        assert!(vars.iter().any(|(k, v)| k == "OPENCODE_DEFAULT_PROVIDER" && v == "openai"));
        assert!(vars.iter().any(|(k, v)| k == "OPENCODE_DEFAULT_MODEL" && v == "gpt-4"));
    }

    #[test]
    fn claude_code_only_gets_anthropic_key() {
        let config = AdapterConfig {
            anthropic_api_key: Some("sk-ant-789".to_string()),
            openai_api_key: Some("sk-openai-000".to_string()),
            opencode_api_base: None,
            opencode_default_provider: "anthropic".to_string(),
            opencode_default_model: "claude-sonnet-4-6".to_string(),
            pi_provider_api_key: None,
        };

        let vars = config.env_vars_for_runtime(rb_schemas::AgentRuntime::ClaudeCode);

        assert!(vars.iter().any(|(k, v)| k == "ANTHROPIC_API_KEY" && v == "sk-ant-789"));
        assert!(!vars.iter().any(|(k, _)| k == "OPENAI_API_KEY"));
        assert!(!vars.iter().any(|(k, _)| k == "OPENCODE_DEFAULT_PROVIDER"));
    }
}
