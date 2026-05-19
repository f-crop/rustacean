import { createInterface } from 'node:readline';
import { Readable } from 'node:stream';
import { SESSION_NOT_FOUND, UNAUTHORIZED_MCP } from './transport.js';
import type { PostResult } from './transport.js';

export type PostFn = (body: string, sessionId?: string) => Promise<PostResult>;

interface RpcMessage {
  jsonrpc?: string;
  id?: unknown;
  method?: string;
  error?: { code: number; message: string };
}

// Sent to re-establish a session on SESSION_NOT_FOUND
const REINIT_BODY = JSON.stringify({
  jsonrpc: '2.0',
  id: '__reinit__',
  method: 'initialize',
  params: {
    protocolVersion: '2024-11-05',
    clientInfo: { name: 'rustbrain-mcp', version: '0.1.0' },
  },
});

export async function runBridge(
  post: PostFn,
  input: Readable = process.stdin,
  output: NodeJS.WritableStream = process.stdout,
  tenantId?: string,
): Promise<void> {
  let sessionId: string | undefined;

  if (tenantId) {
    process.stderr.write(`[rustbrain-mcp] tenant=${tenantId}\n`);
  }

  const rl = createInterface({ input, terminal: false });

  for await (const line of rl) {
    const trimmed = line.trim();
    if (!trimmed) continue;

    let parsed: RpcMessage;
    try {
      parsed = JSON.parse(trimmed);
    } catch {
      process.stderr.write('[rustbrain-mcp] skipping non-JSON line\n');
      continue;
    }

    // initialize must not carry a stale session id
    const sendSessionId = parsed.method === 'initialize' ? undefined : sessionId;
    const result = await sendWithRetry(post, trimmed, sendSessionId);

    if (result.sessionId !== undefined) {
      sessionId = result.sessionId;
    }

    // 202 with empty body (e.g. notifications/initialized) — nothing to forward
    if (!result.body.trim()) continue;

    const rpcResp: RpcMessage = JSON.parse(result.body);
    if (rpcResp.error?.code === UNAUTHORIZED_MCP) {
      process.stderr.write('[rustbrain-mcp] UNAUTHORIZED: check RB_AGENT_API_KEY\n');
    }

    output.write(result.body + '\n');
  }
}

async function sendWithRetry(
  post: PostFn,
  body: string,
  sessionId: string | undefined,
): Promise<PostResult> {
  const result = await post(body, sessionId);

  // Only retry when we had a session — avoids infinite loops on missing auth
  if (sessionId === undefined) {
    return result;
  }

  // Empty body (e.g. 202 for notifications) — no error code to inspect
  if (!result.body.trim()) {
    return result;
  }

  const rpc: RpcMessage = JSON.parse(result.body);
  if (rpc.error?.code !== SESSION_NOT_FOUND) {
    return result;
  }

  process.stderr.write('[rustbrain-mcp] session expired — re-initializing\n');

  const initResult = await post(REINIT_BODY, undefined);
  const newSessionId = initResult.sessionId;
  if (newSessionId === undefined) {
    // Re-init itself failed; surface the SESSION_NOT_FOUND error as-is
    return result;
  }

  return post(body, newSessionId);
}
