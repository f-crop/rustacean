import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Readable } from 'node:stream';
import { runBridge } from '../src/bridge.js';
import type { PostFn } from '../src/bridge.js';
import { SESSION_NOT_FOUND, UNAUTHORIZED_MCP } from '../src/transport.js';

function makeInput(...lines: string[]): Readable {
  return Readable.from(lines.join('\n') + '\n');
}

function captureOutput(): { stream: NodeJS.WritableStream; lines: () => string[] } {
  const chunks: string[] = [];
  const stream = {
    write(chunk: string | Buffer) {
      const s = typeof chunk === 'string' ? chunk : chunk.toString();
      chunks.push(...s.split('\n').filter((l) => l.trim()));
      return true;
    },
  } as unknown as NodeJS.WritableStream;
  return { stream, lines: () => chunks };
}

const OK_RESP = (id: number | string) =>
  JSON.stringify({ jsonrpc: '2.0', id, result: {} });

const ERR_RESP = (id: number | string, code: number) =>
  JSON.stringify({ jsonrpc: '2.0', id, error: { code, message: 'err' } });

test('forwards response to output', async () => {
  const post: PostFn = async () => ({ body: OK_RESP(1), sessionId: undefined });
  const { stream, lines } = captureOutput();

  await runBridge(
    post,
    makeInput('{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}'),
    stream,
  );

  assert.equal(lines().length, 1);
  assert.deepEqual(JSON.parse(lines()[0]).id, 1);
});

test('initialize is sent without session id', async () => {
  const calls: Array<string | undefined> = [];
  const post: PostFn = async (_body, sid) => {
    calls.push(sid);
    return { body: OK_RESP(1), sessionId: 'sess-1' };
  };
  const { stream } = captureOutput();

  await runBridge(
    post,
    makeInput('{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'),
    stream,
  );

  assert.equal(calls[0], undefined);
});

test('stores session id from initialize response', async () => {
  const calls: Array<string | undefined> = [];
  const post: PostFn = async (_body, sid) => {
    calls.push(sid);
    // initialize → returns session; ping → uses it
    return calls.length === 1
      ? { body: OK_RESP(1), sessionId: 'sess-x' }
      : { body: OK_RESP(2), sessionId: undefined };
  };
  const { stream } = captureOutput();

  await runBridge(
    post,
    makeInput(
      '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}',
      '{"jsonrpc":"2.0","id":2,"method":"ping","params":{}}',
    ),
    stream,
  );

  assert.equal(calls[1], 'sess-x');
});

test('retries with fresh session on SESSION_NOT_FOUND', async () => {
  let callCount = 0;
  const results: PostFn = async (body, sid) => {
    callCount++;
    const m = JSON.parse(body).method as string;

    if (m === 'initialize' && callCount === 1) {
      return { body: OK_RESP(1), sessionId: 'old-sess' };
    }
    if (m === 'tools/list' && callCount === 2) {
      // Fail with SESSION_NOT_FOUND
      return { body: ERR_RESP(2, SESSION_NOT_FOUND), sessionId: undefined };
    }
    if (m === 'initialize' && callCount === 3) {
      // Re-init during retry
      return { body: OK_RESP('__reinit__'), sessionId: 'new-sess' };
    }
    // Retry of tools/list with new session
    assert.equal(sid, 'new-sess');
    return { body: JSON.stringify({ jsonrpc: '2.0', id: 2, result: { tools: [] } }), sessionId: undefined };
  };
  const { stream, lines } = captureOutput();

  await runBridge(
    results,
    makeInput(
      '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}',
      '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}',
    ),
    stream,
  );

  const last = JSON.parse(lines()[lines().length - 1]);
  assert.deepEqual(last.result, { tools: [] });
});

test('does not retry when no session established', async () => {
  let callCount = 0;
  const post: PostFn = async () => {
    callCount++;
    return { body: ERR_RESP(1, SESSION_NOT_FOUND), sessionId: undefined };
  };
  const { stream } = captureOutput();

  await runBridge(
    post,
    makeInput('{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'),
    stream,
  );

  assert.equal(callCount, 1);
});

test('surfaces UNAUTHORIZED_MCP error in output', async () => {
  const post: PostFn = async () => ({
    body: ERR_RESP(1, UNAUTHORIZED_MCP),
    sessionId: undefined,
  });
  const { stream, lines } = captureOutput();

  await runBridge(
    post,
    makeInput('{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'),
    stream,
  );

  const resp = JSON.parse(lines()[0]);
  assert.equal(resp.error.code, UNAUTHORIZED_MCP);
});

test('skips non-JSON lines without crashing', async () => {
  let callCount = 0;
  const post: PostFn = async () => {
    callCount++;
    return { body: OK_RESP(1), sessionId: undefined };
  };
  const { stream } = captureOutput();

  await runBridge(
    post,
    makeInput('not-json', '{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}'),
    stream,
  );

  assert.equal(callCount, 1);
});
