export interface Config {
  apiKey: string;
  apiBase: string;
  tenantId: string | undefined;
}

export function loadConfig(): Config {
  const apiKey = process.env.RB_AGENT_API_KEY;
  if (!apiKey) {
    throw new Error('RB_AGENT_API_KEY is required');
  }
  const apiBase = (
    process.env.RB_AGENT_API_BASE ?? 'https://api.rustbrain.dev'
  ).replace(/\/$/, '');
  const tenantId = process.env.RB_AGENT_TENANT_ID;
  return { apiKey, apiBase, tenantId };
}
