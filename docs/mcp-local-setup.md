# Local MCP Setup for Claude Code

This guide walks you through connecting Claude Code to Rust Brain via the
`@rustbrain/mcp-server` MCP bridge so you can call Rust Brain tools
(`search_items`, `get_item`) directly from a local Claude Code session.

---

## Prerequisites

- Node.js 18+ (needed for `npx`)
- Claude Code CLI installed: `npm install -g @anthropic-ai/claude-code`
- A Rust Brain account with API access

---

## 1. Create a Read-scoped API key

1. Open the Rust Brain dashboard and navigate to **Settings → API Keys**.
2. Click **Create Key**, choose scope **Read**, and give it a memorable label
   (e.g., `claude-code-local`).
3. Copy the key — it starts with `rb_live_`.

> The MCP server only needs `Read` scope. Narrower scope limits blast radius if
> the key is ever exposed.

---

## 2. Export environment variables

```bash
export RB_AGENT_API_KEY=rb_live_<your-key>
export RB_AGENT_API_BASE=https://<your-host>   # e.g. https://app.rustbrain.io
```

Add these to your shell profile (`~/.bashrc`, `~/.zshrc`, etc.) to make them
permanent.

---

## 3. Register the MCP server with Claude Code

```bash
claude mcp add rust-brain -- npx -y @rustbrain/mcp-server
```

This writes a `rust-brain` entry into Claude Code's global MCP config
(`~/.claude/mcp_servers.json`). The `npx -y` prefix means the bridge is
resolved lazily on first use — no separate install step required.

---

## 4. Verify the connection

Start a new Claude Code session and run:

```
/mcp
```

You should see `rust-brain` listed with status **connected**.

To list available tools:

```
tools/list
```

Expected output includes at least:

```
search_items   Search Rust Brain items by keyword or semantic query
get_item       Fetch a single Rust Brain item by ID
```

To smoke-test a tool call:

```
tools/call search_items {"query": "hello world", "limit": 3}
```

A successful call returns a JSON array of item results.

---

## 5. Troubleshooting

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `rust-brain` server listed as **error** in `/mcp` | `RB_AGENT_API_KEY` not set or empty | Run `echo $RB_AGENT_API_KEY` and re-export |
| `401 Unauthorized` from tool call | Key expired or revoked | Regenerate the API key in the dashboard |
| `TENANT_DRIFT` error in tool response | Session tenant does not match the key's tenant | Ensure `RB_AGENT_API_BASE` points to the same host the key was created on |
| `npx: command not found` | Node.js not installed or not on PATH | Install Node.js 18+ and verify `which npx` |
| Tools list returns 0 tools | Outdated package cached by npx | Run `npx --yes @rustbrain/mcp-server --version` to force a cache refresh |

---

## See also

- Agent-runner MCP config: `services/agent-runner/src/adapters/mod.rs` (`write_mcp_config`)
- MCP server source: `packages/mcp-server-node/` (npm: `@rustbrain/mcp-server`)
- ADR-009: MCP server design invariants (`docs/decisions/ADR-009-*.md`)
