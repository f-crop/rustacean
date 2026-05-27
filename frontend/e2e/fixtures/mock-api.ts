import { type Page } from "@playwright/test";

export const ME_RESPONSE = {
  user: {
    id: "user-1",
    email: "test@example.com",
    email_verified: true,
    created_at: "2024-01-01T00:00:00Z",
    status: "active",
  },
  current_tenant: {
    id: "tenant-1",
    name: "Test Workspace",
    role: "owner",
    slug: "test-workspace",
  },
  available_tenants: [
    {
      id: "tenant-1",
      name: "Test Workspace",
      role: "owner",
      slug: "test-workspace",
    },
  ],
};

export const REPO_ITEM = {
  repo_id: "repo-1",
  full_name: "acme/web-app",
  installation_id: "install-uuid-1",
  default_branch: "main",
  status: "connected",
  connected_at: "2024-01-01T00:00:00Z",
  connected_by: "user-1",
};

export const REPOS_RESPONSE = { repos: [REPO_ITEM] };
export const REPOS_EMPTY_RESPONSE = { repos: [] };

export const INGEST_RESPONSE = { run_id: "run-id-1" };

export const STAGE_TIMELINE_RESPONSE = {
  ingestion_run_id: "run-uuid-axe-1",
  trace_id: "abc12345def67890abc12345def67890",
  stages: [
    { stage: "clone", status: "succeeded", started_at: "2024-01-01T00:00:00Z", finished_at: "2024-01-01T00:00:02Z", error_message: null },
    { stage: "expand", status: "succeeded", started_at: "2024-01-01T00:00:02Z", finished_at: "2024-01-01T00:00:04Z", error_message: null },
    { stage: "parse", status: "succeeded", started_at: "2024-01-01T00:00:04Z", finished_at: "2024-01-01T00:00:06Z", error_message: null },
    { stage: "typecheck", status: "succeeded", started_at: "2024-01-01T00:00:06Z", finished_at: "2024-01-01T00:00:08Z", error_message: null },
    { stage: "extract", status: "succeeded", started_at: "2024-01-01T00:00:08Z", finished_at: "2024-01-01T00:00:10Z", error_message: null },
    { stage: "embed", status: "succeeded", started_at: "2024-01-01T00:00:10Z", finished_at: "2024-01-01T00:00:12Z", error_message: null },
    { stage: "project_pg", status: "succeeded", started_at: "2024-01-01T00:00:12Z", finished_at: "2024-01-01T00:00:14Z", error_message: null },
    { stage: "project_neo4j", status: "succeeded", started_at: "2024-01-01T00:00:14Z", finished_at: "2024-01-01T00:00:16Z", error_message: null },
    { stage: "project_qdrant", status: "succeeded", started_at: "2024-01-01T00:00:16Z", finished_at: "2024-01-01T00:00:18Z", error_message: null },
  ],
};

export const CONNECT_REPO_RESPONSE = {
  repo_id: "repo-2",
  full_name: "acme/api",
  installation_id: "install-uuid-1",
  default_branch: "main",
  status: "connected",
  connected_at: "2024-01-01T00:00:00Z",
  connected_by: "user-1",
};

export const MEMBERS_RESPONSE = {
  members: [
    {
      user_id: "user-1",
      email: "test@example.com",
      role: "owner",
      invited_at: "2024-01-01T00:00:00Z",
    },
  ],
};

export const API_KEYS_RESPONSE = { keys: [] };

export async function mockAuthenticatedSession(page: Page): Promise<void> {
  await page.route("**/v1/me", (route) =>
    route.fulfill({ json: ME_RESPONSE }),
  );
}

export async function mockReposList(
  page: Page,
  response: { repos: typeof REPO_ITEM[] } = REPOS_RESPONSE,
): Promise<void> {
  await page.route("**/v1/repos", (route) => {
    if (route.request().method() === "GET") {
      return route.fulfill({ json: response });
    }
    return route.continue();
  });
}

export async function mockIngestTrigger(page: Page): Promise<void> {
  await page.route("**/v1/repos/*/ingest", (route) =>
    route.fulfill({ json: INGEST_RESPONSE }),
  );
}

export async function mockMembers(page: Page): Promise<void> {
  await page.route("**/v1/tenants/*/members", (route) =>
    route.fulfill({ json: MEMBERS_RESPONSE }),
  );
}

export const MODULE_TREE_RESPONSE = {
  repo_id: "repo-1",
  tree: {
    fqn: "my_crate",
    kind: "MOD",
    name: "my_crate",
    children: [
      {
        fqn: "my_crate::my_fn",
        kind: "FN",
        name: "my_fn",
        children: [],
        source: { path: "src/lib.rs", line_start: 1, line_end: 5 },
      },
    ],
    source: null,
  },
};

export const SEARCH_RESPONSE = {
  results: [
    {
      fqn: "my_crate::my_fn",
      crate_name: "my_crate",
      repo_id: "repo-1",
      score: 0.92,
    },
    {
      fqn: "my_crate::other_fn",
      crate_name: "my_crate",
      repo_id: "repo-1",
      score: 0.81,
    },
  ],
};

export const TEST_FQN = "my_crate::my_fn";
export const TEST_FQN_B64 = btoa(TEST_FQN)
  .replace(/\+/g, "-")
  .replace(/\//g, "_")
  .replace(/=/g, "");

export const CALLERS_RESPONSE = {
  root: { fqn: TEST_FQN, name: "my_fn", kind: "FN", file_path: "src/lib.rs", line: 1 },
  nodes: [
    { fqn: TEST_FQN, name: "my_fn", kind: "FN", file_path: "src/lib.rs", line: 1 },
    { fqn: "my_crate::caller_a", name: "caller_a", kind: "FN", file_path: "src/main.rs", line: 10 },
    { fqn: "my_crate::caller_b", name: "caller_b", kind: "FN", file_path: "src/main.rs", line: 20 },
  ],
  edges: [
    { from_fqn: "my_crate::caller_a", to_fqn: TEST_FQN, depth: 1, provenance: "direct" },
    { from_fqn: "my_crate::caller_b", to_fqn: TEST_FQN, depth: 1, provenance: "direct" },
  ],
  cycles_detected: false,
  next_cursor: null,
};

export const CALLEES_RESPONSE = {
  root: { fqn: TEST_FQN, name: "my_fn", kind: "FN", file_path: "src/lib.rs", line: 1 },
  nodes: [
    { fqn: TEST_FQN, name: "my_fn", kind: "FN", file_path: "src/lib.rs", line: 1 },
    { fqn: "my_crate::callee_x", name: "callee_x", kind: "FN", file_path: "src/utils.rs", line: 5 },
  ],
  edges: [
    { from_fqn: TEST_FQN, to_fqn: "my_crate::callee_x", depth: 1, provenance: "direct" },
  ],
  cycles_detected: false,
  next_cursor: null,
};

export const ITEM_RESPONSE = {
  fqn: TEST_FQN,
  kind: "FN",
  source_path: "src/lib.rs",
  source_preview: "fn my_fn() {\n    println!(\"hello\");\n}",
  line_start: 1,
  line_end: 3,
  blob_ref: null,
};

export async function mockModuleTree(page: Page): Promise<void> {
  await page.route("**/v1/repos/*/modules", (route) =>
    route.fulfill({ json: MODULE_TREE_RESPONSE }),
  );
}

export async function mockSearch(page: Page): Promise<void> {
  await page.route("**/v1/search", (route) => {
    if (route.request().method() === "POST") {
      return route.fulfill({ json: SEARCH_RESPONSE });
    }
    return route.continue();
  });
}

export async function mockCallers(page: Page): Promise<void> {
  await page.route("**/v1/repos/*/items/*/callers", (route) =>
    route.fulfill({ json: CALLERS_RESPONSE }),
  );
}

export async function mockCallees(page: Page): Promise<void> {
  await page.route("**/v1/repos/*/items/*/callees", (route) =>
    route.fulfill({ json: CALLEES_RESPONSE }),
  );
}

export async function mockItem(page: Page): Promise<void> {
  await page.route("**/v1/repos/*/items/*", (route) => {
    const url = route.request().url();
    if (!url.includes("/callers") && !url.includes("/callees")) {
      return route.fulfill({ json: ITEM_RESPONSE });
    }
    return route.continue();
  });
}
