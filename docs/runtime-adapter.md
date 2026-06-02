# Runtime Adapter Contract

The runtime adapter is the interface between Rustacean's session supervisor (`agent-runner`) and an external coding runtime (Claude Code, OpenCode, or a future runtime). Each adapter wraps one runtime binary and teaches the supervisor how to spawn it, feed it user input, parse its output, check its health, and shut it down.

**ADR**: [ADR-013 &sect;4](decisions/ADR-013-chat-panel-architecture.md) (Wave 9).
**Code**: `services/agent-runner/src/adapters/mod.rs`.

---

## The `RuntimeAdapter` trait

```rust
#[async_trait]
pub trait RuntimeAdapter: Send + Sync {
    /// Static description of the runtime.
    fn manifest(&self) -> RuntimeManifest;

    /// Spawn one supervised OS process for a session in an isolated workspace.
    async fn spawn(&self, ctx: &SessionCtx) -> Result<AgentProcess>;

    /// Feed one user turn to a live process over stdin.
    async fn send_input(&self, proc: &mut AgentProcess, input: &str) -> Result<()>;

    /// Parse one stdout line into a typed event.
    fn parse_stdout_line(&self, line: &str) -> Option<ParsedLine>;

    /// Liveness probe used by the idle/health reaper.
    async fn health(&self, proc: &AgentProcess) -> RuntimeHealth;

    /// Graceful then forced termination.
    async fn terminate(&self, proc: &mut AgentProcess, force: bool) -> Result<()>;
}
```

### Supporting types

```rust
pub struct RuntimeManifest {
    pub kind: AgentRuntime,             // rb_schemas enum variant
    pub binary: &'static str,          // e.g. "claude", "opencode"
    pub required_env: &'static [&'static str],
    pub capabilities: RuntimeCaps,
}

pub struct RuntimeCaps {
    pub multi_turn: bool,              // supports stdin-based multi-turn
    pub streams_json: bool,            // stdout emits structured JSON lines
}

pub struct SessionCtx {
    pub session_id: String,
    pub tenant_id: String,
    pub workspace_path: PathBuf,
    pub api_key: String,               // MCP JWT for chat sessions
    pub initial_prompt: String,
}

pub struct AgentProcess {
    pub child: Child,                  // tokio::process::Child (kill_on_drop)
    pub pid: u32,
    pub runtime: AgentRuntime,
}

pub struct ParsedLine {
    pub kind: LineKind,                // Text or Json
    pub payload: String,
}
```

---

## Existing adapters

### `ClaudeCodeAdapter`

| Field | Value |
|-------|-------|
| Binary | `claude` |
| Auth | OAuth via shared `claude-credentials` volume |
| Capabilities | multi-turn, streams JSON |
| File | `services/agent-runner/src/adapters/claude_code.rs` |

Spawns `claude` in the isolated workspace with `--print` mode. Reads OAuth tokens from the mounted `claude-credentials` Docker volume. The `.mcp.json` written to the workspace carries the session's MCP credential (JWT for chat sessions, API key for agent sessions).

### `OpencodeAdapter`

| Field | Value |
|-------|-------|
| Binary | `opencode` |
| Auth | LiteLLM proxy (`LITELLM_BASE_URL`) |
| Capabilities | multi-turn, streams JSON |
| File | `services/agent-runner/src/adapters/opencode.rs` |

Supports three LLM routing modes via `OPENCODE_LLM_MODE`:

1. **LiteLLM** (default) -- routes through the LiteLLM gateway for multi-provider access.
2. **OpenAI-compatible** -- direct connection to any OpenAI-compatible endpoint.
3. **DirectProvider** -- direct API key auth to a specific provider.

### `PiAdapter` (stub)

| Field | Value |
|-------|-------|
| Binary | -- |
| Status | Deferred to Wave 10 |
| File | `services/agent-runner/src/adapters/pi.rs` |

Returns `not implemented: pi runtime evaluation pending (ADR-009 Phase 3)` on spawn.

---

## Writing a new adapter

A new runtime plugs in with exactly **one adapter implementation + one registry line**. No changes to the gateway, persistence, or token model.

### Step 1 -- Add an `AgentRuntime` variant

In `proto/rust_brain/v1/agent.proto`:

```protobuf
enum AgentRuntime {
  AGENT_RUNTIME_UNSPECIFIED = 0;
  AGENT_RUNTIME_CLAUDE_CODE = 1;
  AGENT_RUNTIME_OPENCODE    = 2;
  AGENT_RUNTIME_PI          = 3;
  AGENT_RUNTIME_MY_RUNTIME  = 4;   // <-- add here
}
```

Rebuild protobuf: `cargo build -p rb-schemas`.

### Step 2 -- Implement the trait

Create `services/agent-runner/src/adapters/my_runtime.rs`:

```rust
use anyhow::Result;
use async_trait::async_trait;
use rb_schemas::AgentRuntime;

use super::{
    AgentProcess, LineKind, ParsedLine, RuntimeAdapter, RuntimeCaps,
    RuntimeHealth, RuntimeManifest, SessionCtx, build_base_command,
    write_mcp_config,
};

pub struct MyRuntimeAdapter;

impl MyRuntimeAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeAdapter for MyRuntimeAdapter {
    fn manifest(&self) -> RuntimeManifest {
        RuntimeManifest {
            kind: AgentRuntime::MyRuntime,
            binary: "my-runtime",
            required_env: &[],
            capabilities: RuntimeCaps {
                multi_turn: true,
                streams_json: false,
            },
        }
    }

    async fn spawn(&self, ctx: &SessionCtx) -> Result<AgentProcess> {
        // Write MCP config so the runtime can call Rustacean tools
        write_mcp_config(&ctx.workspace_path, &ctx.api_key, &ctx.tenant_id)
            .await?;

        let mut cmd = build_base_command("my-runtime", &ctx.workspace_path);
        cmd.arg("--prompt").arg(&ctx.initial_prompt);
        // Add runtime-specific env vars here

        let child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0);

        Ok(AgentProcess {
            child,
            pid,
            runtime: AgentRuntime::MyRuntime,
        })
    }

    async fn send_input(&self, proc: &mut AgentProcess, input: &str) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let stdin = proc.child.stdin.as_mut()
            .ok_or_else(|| anyhow::anyhow!("stdin not available"))?;
        stdin.write_all(input.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    fn parse_stdout_line(&self, line: &str) -> Option<ParsedLine> {
        if line.trim().is_empty() {
            return None;
        }
        Some(ParsedLine {
            kind: LineKind::Text,
            payload: line.to_string(),
        })
    }

    async fn health(&self, proc: &AgentProcess) -> RuntimeHealth {
        // Check if the process is still running
        // Return RuntimeHealth::Healthy or RuntimeHealth::Unhealthy
        todo!()
    }

    async fn terminate(&self, proc: &mut AgentProcess, force: bool) -> Result<()> {
        if force {
            proc.child.kill().await?;
        } else {
            proc.child.kill().await?; // Replace with graceful signal
        }
        Ok(())
    }
}
```

### Step 3 -- Register in the adapter factory

In `services/agent-runner/src/adapters/mod.rs`, add the module and the match arm:

```rust
pub mod my_runtime;

pub fn adapter_for_runtime(runtime: AgentRuntime) -> anyhow::Result<Box<dyn RuntimeAdapter>> {
    match runtime {
        AgentRuntime::ClaudeCode => Ok(Box::new(claude_code::ClaudeCodeAdapter::new())),
        AgentRuntime::Opencode => Ok(Box::new(opencode::OpencodeAdapter::new())),
        AgentRuntime::Pi => Ok(Box::new(pi::PiAdapter::new())),
        AgentRuntime::MyRuntime => Ok(Box::new(my_runtime::MyRuntimeAdapter::new())),
        AgentRuntime::Unspecified => anyhow::bail!("Unspecified runtime received"),
    }
}
```

### Step 4 -- Add the runtime to the chat session CHECK constraint

In the migration (or a follow-up migration):

```sql
ALTER TABLE control.chat_sessions
  DROP CONSTRAINT chat_sessions_runtime_check,
  ADD CONSTRAINT chat_sessions_runtime_check
    CHECK (runtime IN ('claude_code','opencode','pi','my_runtime'));
```

### Step 5 -- Verify

```bash
cargo build -p rb-agent-runner   # adapter compiles
cargo test -p rb-agent-runner    # existing tests still pass
```

The chat gateway, persistence layer, MCP token model, and SSE event relay require **zero changes** -- they operate on the `RuntimeAdapter` trait, not on concrete types.

---

## Process lifecycle

```
Session created (first message)
  |
  +-- spawn() --> OS process starts in isolated workspace
                  .mcp.json written with MCP JWT (0600 perms)
                  kill_on_drop(true) set on Child
     |
     |-- send_input() (warm turn) --> stdin
     |     |
     |     +-- stdout --> parse_stdout_line() --> redaction --> agent_events + SSE
     |
     |-- health() --> liveness probe (idle reaper)
     |
     +-- terminate() --> graceful then forced kill
                         workspace cleaned by workspace_gc
```

- **One process per session.** The process stays warm between user messages. Each message is a `send_input()` call over stdin.
- **No automatic restart.** A crashed process emits `session_failed{error_kind="runtime_crashed"}` and surfaces a typed error to the UI. The user re-sends to start fresh.
- **Isolation.** Each process gets its own workspace directory, its own MCP credential (tenant-bound, read-scoped, short-lived JWT), and its own cgroup resource limits. One crash never affects other sessions.

---

## Resource limits per process

| Limit | Default | Mechanism |
|-------|---------|-----------|
| Memory | 1 GiB | cgroup v2 `memory.max` |
| CPU | 1 core-equiv | cgroup `cpu.weight` |
| Idle timeout | 15 min | reaper on `chat_sessions.last_activity_at` |
| Wall-clock | 60 min/session | hard cap |
| Concurrency | 20/tenant, 200/node | per-tenant counter + per-node semaphore |
