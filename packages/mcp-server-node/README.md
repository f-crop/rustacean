# @rustbrain/mcp-server

A thin Node.js stdio↔HTTP bridge for the [Rustbrain](https://rustbrain.dev) MCP server.
Connects Claude Code (or any MCP client) to a Rustbrain control-api instance over JSON-RPC 2.0.

## Quick start

```bash
npx -y @rustbrain/mcp-server
```

Or install globally:

```bash
npm install -g @rustbrain/mcp-server
rustbrain-mcp
```

## Configuration

| Env var | Required | Description |
|---------|----------|-------------|
| `RB_AGENT_API_KEY` | **yes** | Agent API key (Bearer token) |
| `RB_AGENT_API_BASE` | no | Base URL of control-api (default: `https://api.rustbrain.dev`) |
| `RB_AGENT_TENANT_ID` | no | Informational — logged on startup, not sent to the server |

## Claude Code integration

Add to your `.claude/mcp.json` (or the MCP section of Claude Code settings):

```json
{
  "mcpServers": {
    "rustbrain": {
      "command": "npx",
      "args": ["-y", "@rustbrain/mcp-server"],
      "env": {
        "RB_AGENT_API_KEY": "<your-api-key>"
      }
    }
  }
}
```

## Protocol

The bridge:

1. Reads newline-delimited JSON-RPC 2.0 messages from **stdin**.
2. On `initialize`: forwards the request to `POST /mcp` _without_ a session header; captures the `Mcp-Session-Id` from the response.
3. On all subsequent requests: attaches `Mcp-Session-Id` to each HTTP call.
4. On `SESSION_NOT_FOUND` (`-32001`): transparently re-initializes and retries once.
5. Writes server responses, newline-delimited, to **stdout**.
6. Diagnostic messages go to **stderr** only.

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `UNAUTHORIZED: check RB_AGENT_API_KEY` on stderr | Invalid or missing API key | Set `RB_AGENT_API_KEY` |
| `session expired — re-initializing` | Session TTL expired mid-session | Bridge retries automatically |
| `RB_AGENT_API_KEY is required` on startup | Key not set | Set the env var before launching |
| No tools returned by `tools/list` | Wrong API base or unauthenticated | Verify `RB_AGENT_API_BASE` and API key |

## Development

```bash
npm install
npm run build   # tsc → dist/
npm test        # build + node --test
bash test/integration.sh   # requires a local control-api
```

## License

MIT
