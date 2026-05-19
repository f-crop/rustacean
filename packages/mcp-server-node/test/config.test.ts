import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { loadConfig } from '../src/config.js';

beforeEach(() => {
  delete process.env.RB_AGENT_API_KEY;
  delete process.env.RB_AGENT_API_BASE;
  delete process.env.RB_AGENT_TENANT_ID;
});

test('throws when API key is absent', () => {
  assert.throws(() => loadConfig(), /RB_AGENT_API_KEY/);
});

test('returns api key from env', () => {
  process.env.RB_AGENT_API_KEY = 'sk-test';
  const cfg = loadConfig();
  assert.equal(cfg.apiKey, 'sk-test');
});

test('defaults apiBase to production URL', () => {
  process.env.RB_AGENT_API_KEY = 'sk-test';
  const cfg = loadConfig();
  assert.equal(cfg.apiBase, 'https://api.rustbrain.dev');
});

test('accepts custom apiBase', () => {
  process.env.RB_AGENT_API_KEY = 'sk-test';
  process.env.RB_AGENT_API_BASE = 'http://localhost:3000';
  const cfg = loadConfig();
  assert.equal(cfg.apiBase, 'http://localhost:3000');
});

test('strips trailing slash from apiBase', () => {
  process.env.RB_AGENT_API_KEY = 'sk-test';
  process.env.RB_AGENT_API_BASE = 'http://localhost:3000/';
  const cfg = loadConfig();
  assert.equal(cfg.apiBase, 'http://localhost:3000');
});

test('tenantId is undefined when env var absent', () => {
  process.env.RB_AGENT_API_KEY = 'sk-test';
  const cfg = loadConfig();
  assert.equal(cfg.tenantId, undefined);
});

test('captures tenant id', () => {
  process.env.RB_AGENT_API_KEY = 'sk-test';
  process.env.RB_AGENT_TENANT_ID = 'tenant-abc';
  const cfg = loadConfig();
  assert.equal(cfg.tenantId, 'tenant-abc');
});
