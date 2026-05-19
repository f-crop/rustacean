import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { postMcp } from '../src/transport.js';

type RequestListener = (req: http.IncomingMessage, res: http.ServerResponse) => void;

async function withServer(
  handler: RequestListener,
  fn: (baseUrl: string) => Promise<void>,
): Promise<void> {
  const server = http.createServer(handler);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as { port: number };
  try {
    await fn(`http://127.0.0.1:${port}`);
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}

test('sends Authorization: Bearer header', async () => {
  let capturedAuth = '';
  await withServer((req, res) => {
    capturedAuth = req.headers['authorization'] ?? '';
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end('{}');
  }, async (base) => {
    await postMcp({ apiKey: 'my-key', mcpUrl: `${base}/mcp` }, '{}');
    assert.equal(capturedAuth, 'Bearer my-key');
  });
});

test('forwards request body verbatim', async () => {
  let capturedBody = '';
  await withServer((req, res) => {
    req.setEncoding('utf8');
    req.on('data', (c: string) => { capturedBody += c; });
    req.on('end', () => {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end('{}');
    });
  }, async (base) => {
    const payload = '{"jsonrpc":"2.0","id":1,"method":"ping"}';
    await postMcp({ apiKey: 'k', mcpUrl: `${base}/mcp` }, payload);
    assert.equal(capturedBody, payload);
  });
});

test('captures Mcp-Session-Id response header', async () => {
  await withServer((_req, res) => {
    res.writeHead(200, {
      'Content-Type': 'application/json',
      'Mcp-Session-Id': 'sess-abc',
    });
    res.end('{"jsonrpc":"2.0","id":1,"result":{}}');
  }, async (base) => {
    const result = await postMcp({ apiKey: 'k', mcpUrl: `${base}/mcp` }, '{}');
    assert.equal(result.sessionId, 'sess-abc');
  });
});

test('sends Mcp-Session-Id request header when provided', async () => {
  let capturedSid = '';
  await withServer((req, res) => {
    capturedSid = req.headers['mcp-session-id'] as string ?? '';
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end('{}');
  }, async (base) => {
    await postMcp({ apiKey: 'k', mcpUrl: `${base}/mcp` }, '{}', 'my-session');
    assert.equal(capturedSid, 'my-session');
  });
});

test('returns undefined sessionId when header absent', async () => {
  await withServer((_req, res) => {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end('{}');
  }, async (base) => {
    const result = await postMcp({ apiKey: 'k', mcpUrl: `${base}/mcp` }, '{}');
    assert.equal(result.sessionId, undefined);
  });
});

test('omits Mcp-Session-Id header when not provided', async () => {
  let hadSid = false;
  await withServer((req, res) => {
    hadSid = 'mcp-session-id' in req.headers;
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end('{}');
  }, async (base) => {
    await postMcp({ apiKey: 'k', mcpUrl: `${base}/mcp` }, '{}');
    assert.equal(hadSid, false);
  });
});
