# Codemap: agent-runner

Kafka consumer service that spawns and manages AI coding agent subprocesses. Consumes `AgentSessionCommand` messages from `rb.agent.commands`, spawns runtime-specific subprocesses (Claude Code, OpenCode), streams stdout events back to `control-api` via HTTP relay, and handles session lifecycle (start, input, terminate).

## Module tree

```
services/agent-runner/src/
├── main.rs                     # Binary entrypoint (rb-agent-runner): init tracing,
│                               #   Kafka consumer, graceful shutdown
├── lib.rs                      # Crate root: re-exports EventSender, RelayConfig,
│                               #   StreamJsonNormalizer, relay helpers
├── consumer.rs                 # Kafka message dispatcher → SessionManager
├── event_relay.rs              # Ring-buffer event sender + HTTP batch relay to
│                               #   control-api /internal/agent/sessions/{id}/events
├── normalizer.rs               # StreamJsonNormalizer: raw stdout → RuntimeEvent[]
├── workspace_gc.rs             # Background GC for expired agent workspaces
├── adapters/
│   ├── mod.rs                  # RuntimeAdapter trait + adapter_for_runtime() factory
│   ├── claude_code.rs          # ClaudeCodeAdapter: spawns `claude` CLI,
│   │                           #   OAuth credentials via shared volume
│   ├── opencode.rs             # OpencodeAdapter: spawns `opencode` CLI,
│   │                           #   LiteLLM proxy configuration
│   └── pi.rs                   # PiAdapter: stub (ADR-009 Phase 3)
└── session/
    ├── mod.rs                  # SessionManager: session lifecycle orchestration
    ├── natural_exit.rs         # Automatic exit detection + status update
    ├── seq.rs                  # Sequence counter + GC for completed sessions
    └── tests.rs                # Integration tests
```

## Runtime adapter shape

The `RuntimeAdapter` trait defines how agent-runner interacts with different AI runtimes:

```
trait RuntimeAdapter: Send + Sync {
    fn spawn(&self, ctx: &SessionCtx) -> Result<AgentProcess>
    fn send_input(&self, proc: &mut AgentProcess, input: &str) -> Result<()>
    fn terminate(&self, proc: &mut AgentProcess, force: bool) -> Result<()>
    fn parse_stdout_line(&self, line: &str) -> Option<ParsedLine>
}
```

### ClaudeCodeAdapter

- Spawns `claude` CLI binary with `--json` output
- Reads credentials from `CLAUDE_CONFIG_DIR` (mounted from `claude-credentials` volume)
- Writes `.mcp.json` to workspace with tenant-scoped MCP server config
- Copies Claude credentials to session-local `.claude-config`

### OpencodeAdapter

- Spawns `opencode` CLI binary
- Detects LLM mode: LiteLLM, OpenAI-compatible, or direct provider
- Generates `.opencode/config.json` with provider configuration
- Forwards `LITELLM_BASE_URL`, `LITELLM_API_KEY`, `ANTHROPIC_API_KEY` to child

### PiAdapter

- Stub implementation — returns `RuntimeNotConfigured` error
- Reserved for ADR-009 Phase 3

## Public API surface

| Type | Kind | Description |
|------|------|-------------|
| `EventSender` | struct | Cloneable ring-buffer sender — `send(RelayItem)` non-blocking, evicts oldest if full |
| `RelayConfig` | struct | Relay settings: `capacity`, `batch_size`, `flush_interval`, `control_api_base`, `http_client` |
| `RelayItem` | struct | Event envelope: `session_id`, `tenant_id`, `seq`, `event: RuntimeEvent`, `emitted_at_ms` |
| `StreamJsonNormalizer` | struct | Parses raw stdout lines into `Vec<RuntimeEvent>` |
| `SessionManager` | struct | Session lifecycle: `start_session()`, `send_input()`, `terminate_session()`, `terminate_all()` |
| `RuntimeAdapter` | trait | `spawn()`, `send_input()`, `terminate()`, `parse_stdout_line()` |
| `SessionCtx` | struct | Session context: `session_id`, `tenant_id`, `workspace_path`, `api_key`, `initial_prompt` |
| `AgentProcess` | struct | Process wrapper: `child: Child`, `pid: u32`, `runtime: AgentRuntime` |
| `ParsedLine` | struct | Parsed output: `kind: LineKind`, `payload: String` |

### Free functions

| Function | Description |
|----------|-------------|
| `adapter_for_runtime(AgentRuntime) -> Result<Box<dyn RuntimeAdapter>>` | Factory for runtime-specific adapters |
| `relay_stdout_events(...)` | Stream stdout from a subprocess to the event relay |
| `spawn(consumer, session_manager, event_sender)` | Start the Kafka consumer loop |

### Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `DEFAULT_CAPACITY` | 8,000 | Event relay ring-buffer size |
| `DEFAULT_BATCH_SIZE` | 100 | Events per HTTP relay batch |
| `DEFAULT_FLUSH_INTERVAL_MS` | 250 | Relay flush interval |
| `MAX_INITIAL_PROMPT_LEN` | 100,000 bytes | Prompt size cap |
| `MAX_TRACKED_SESSIONS` | 100,000 | Max sessions tracked in memory |
| `PROCESS_TERMINATE_TIMEOUT_SECS` | 30 | Grace period before SIGKILL |

## Volume mounts and environment contract

### Volumes (from compose/dev.yml)

| Mount path | Volume | Mode | Purpose |
|------------|--------|------|---------|
| `/data/agent-workspaces` | `agent-workspace-data` | rw | Session workspace directories |
| `/home/loginuser/.claude` | `claude-credentials` | ro | Claude OAuth credentials (shared with `claude-login` sidecar) |

### Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `KAFKA_BOOTSTRAP_SERVERS` | `kafka:9092` | Kafka broker list |
| `RB_AGENT_WORKSPACE_BASE` | `/data/agent-workspaces` | Root directory for session workspaces |
| `RB_AGENT_WORKSPACE_TTL_DAYS` | `7` | Workspace GC retention |
| `RB_CONTROL_API_BASE_URL` | `http://control-api:8081` | Internal API for event relay |
| `RB_INTERNAL_SECRET` | — | X-Internal-Secret header value |
| `CLAUDE_CONFIG_DIR` | `/home/loginuser/.claude` | Claude credentials location |
| `LITELLM_BASE_URL` | — | LiteLLM proxy (opencode mode) |
| `LITELLM_API_KEY` | — | LiteLLM virtual key |
| `ANTHROPIC_API_KEY` | — | Direct Anthropic key (optional) |
| `OTEL_SERVICE_NAME` | `agent-runner` | Trace service name |
| `RUST_LOG` | `info,agent_runner=debug` | Log filter |

## External dependencies (rb-* crates)

| Crate | Role |
|-------|------|
| `rb-build-info` | Compile-time build provenance |
| `rb-kafka` | Kafka consumer/producer |
| `rb-schemas` | Protobuf types (`AgentSessionCommand`, `RuntimeEvent`, `AgentRuntime`) |
| `rb-tracing` | OpenTelemetry integration |

## Related docs

- [ADR-009: Agent Execution Architecture](../decisions/ADR-009-agent-execution-architecture.md)
- [ADR-011: Dev-stack auto-rebuild watcher](../decisions/ADR-011-dev-stack-auto-rebuild.md)
- [Runbook: claude-login](../runbooks/claude-login.md)
- [Runbook: stack-rebuild-verify](../runbooks/stack-rebuild-verify.md)
- [API reference: Agent session endpoints](../api-reference.md#agent-session-endpoints-wave-7)
