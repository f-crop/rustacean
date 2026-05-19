#!/usr/bin/env node
import { loadConfig } from './config.js';
import { postMcp } from './transport.js';
import { runBridge } from './bridge.js';

let config;
try {
  config = loadConfig();
} catch (err) {
  process.stderr.write(`[rustbrain-mcp] ${err}\n`);
  process.exit(1);
}

const mcpUrl = `${config.apiBase}/mcp`;
const post = (body: string, sessionId?: string) =>
  postMcp({ apiKey: config.apiKey, mcpUrl }, body, sessionId);

runBridge(post, process.stdin, process.stdout, config.tenantId).catch((err) => {
  process.stderr.write(`[rustbrain-mcp] fatal: ${err}\n`);
  process.exit(1);
});
