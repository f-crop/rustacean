import https from 'node:https';
import http from 'node:http';
import { URL } from 'node:url';

// JSON-RPC error codes mirrored from rb-mcp (server-side constants)
export const SESSION_NOT_FOUND = -32_001;
export const UNAUTHORIZED_MCP = -32_003;

export interface TransportOptions {
  apiKey: string;
  mcpUrl: string;
}

export interface PostResult {
  body: string;
  sessionId: string | undefined;
}

export function postMcp(
  opts: TransportOptions,
  body: string,
  sessionId?: string,
): Promise<PostResult> {
  const url = new URL(opts.mcpUrl);
  const bodyBuf = Buffer.from(body, 'utf8');
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    'Authorization': `Bearer ${opts.apiKey}`,
    'Content-Length': bodyBuf.byteLength.toString(),
  };
  if (sessionId !== undefined) {
    headers['Mcp-Session-Id'] = sessionId;
  }

  const lib = url.protocol === 'https:' ? https : http;

  return new Promise((resolve, reject) => {
    const req = lib.request(
      {
        hostname: url.hostname,
        port: url.port || (url.protocol === 'https:' ? 443 : 80),
        path: url.pathname + url.search,
        method: 'POST',
        headers,
      },
      (res) => {
        const chunks: Buffer[] = [];
        res.on('data', (chunk: Buffer) => chunks.push(chunk));
        res.on('end', () => {
          const responseBody = Buffer.concat(chunks).toString('utf8');
          const newSessionId = res.headers['mcp-session-id'] as string | undefined;
          resolve({ body: responseBody, sessionId: newSessionId });
        });
        res.on('error', reject);
      },
    );
    req.on('error', reject);
    req.write(bodyBuf);
    req.end();
  });
}
