import { test, expect, type Page, type Route } from "@playwright/test";
import { ME_RESPONSE, REPO_ITEM } from "./fixtures/mock-api";

// ─── Fixture data ────────────────────────────────────────────────────────────

const REPO_ID = REPO_ITEM.repo_id; // "repo-1"

const FIXTURE_FQN = "my_crate::config::load_config";
const FIXTURE_FQN_B64 = btoa(FIXTURE_FQN)
  .replace(/\+/g, "-")
  .replace(/\//g, "_")
  .replace(/=+$/, "");

const mockModuleTree = {
  repo_id: REPO_ID,
  tree: {
    fqn: "my_crate",
    name: "my_crate",
    kind: "MOD",
    children: [
      {
        fqn: "my_crate::config",
        name: "config",
        kind: "MOD",
        children: [
          {
            fqn: FIXTURE_FQN,
            name: "load_config",
            kind: "FN",
            children: [],
            source: {
              path: "src/config.rs",
              line_start: 23,
              line_end: 28,
            },
          },
          {
            fqn: "my_crate::config::Config",
            name: "Config",
            kind: "STRUCT",
            children: [],
            source: {
              path: "src/config.rs",
              line_start: 5,
              line_end: 10,
            },
          },
        ],
        source: null,
      },
      {
        fqn: "my_crate::main",
        name: "main",
        kind: "FN",
        children: [],
        source: {
          path: "src/main.rs",
          line_start: 1,
          line_end: 8,
        },
      },
    ],
    source: null,
  },
};

const mockItemResponse = {
  id: "aaaaaaaa-bbbb-4ccc-9ddd-eeeeeeeeeeee",
  fqn: FIXTURE_FQN,
  kind: "FN",
  repo_id: REPO_ID,
  source_path: "src/config.rs",
  line_start: 23,
  line_end: 28,
  source_preview: [
    "pub fn load_config(path: &Path) -> Result<Config, ConfigError> {",
    "    let contents = std::fs::read_to_string(path)?;",
    "    let config: Config = toml::from_str(&contents)?;",
    "    validate(&config)?;",
    "    Ok(config)",
    "}",
  ].join("\n"),
  blob_ref: null,
};

const mockCallersResponse = {
  root: {
    fqn: FIXTURE_FQN,
    name: "load_config",
    kind: "fn",
    file_path: "src/config.rs",
    line: 23,
  },
  nodes: [
    {
      fqn: "my_crate::main",
      name: "main",
      kind: "fn",
      file_path: "src/main.rs",
      line: 10,
    },
    {
      fqn: "my_crate::cli::run",
      name: "run",
      kind: "fn",
      file_path: "src/cli.rs",
      line: 55,
    },
  ],
  edges: [
    {
      from_fqn: "my_crate::main",
      to_fqn: FIXTURE_FQN,
      depth: 1,
      provenance: "direct",
    },
    {
      from_fqn: "my_crate::cli::run",
      to_fqn: FIXTURE_FQN,
      depth: 1,
      provenance: "direct",
    },
  ],
  cycles_detected: false,
  next_cursor: null,
};

const mockCalleesResponse = {
  root: {
    fqn: FIXTURE_FQN,
    name: "load_config",
    kind: "fn",
    file_path: "src/config.rs",
    line: 23,
  },
  nodes: [
    {
      fqn: "std::fs::read_to_string",
      name: "read_to_string",
      kind: "fn",
      file_path: null,
      line: null,
    },
    {
      fqn: "toml::from_str",
      name: "from_str",
      kind: "fn",
      file_path: null,
      line: null,
    },
  ],
  edges: [
    {
      from_fqn: FIXTURE_FQN,
      to_fqn: "std::fs::read_to_string",
      depth: 1,
      provenance: "direct",
    },
    {
      from_fqn: FIXTURE_FQN,
      to_fqn: "toml::from_str",
      depth: 1,
      provenance: "direct",
    },
  ],
  cycles_detected: false,
  next_cursor: null,
};

const mockSearchResults = {
  results: [
    {
      fqn: FIXTURE_FQN,
      name: "load_config",
      item_type: "function",
      visibility: "pub",
      signature:
        "pub fn load_config(path: &Path) -> Result<Config, ConfigError>",
      doc_comment: "Loads configuration from a TOML file.",
      file_path: "src/config.rs",
      start_line: 23,
      end_line: 28,
      score: 0.94,
    },
  ],
  query_time_ms: 18,
};

// ─── Route helpers ───────────────────────────────────────────────────────────

async function mockCodeWorkspaceApis(page: Page): Promise<void> {
  await page.route("**/v1/me", (route: Route) =>
    route.fulfill({ json: ME_RESPONSE }),
  );
  await page.route("**/v1/repos", (route: Route) => {
    if (route.request().method() === "GET") {
      return route.fulfill({ json: { repos: [REPO_ITEM] } });
    }
    return route.continue();
  });
  await page.route(`**/v1/repos/${REPO_ID}/modules`, (route: Route) =>
    route.fulfill({ json: mockModuleTree }),
  );
  await page.route(`**/v1/repos/${REPO_ID}/items/*`, (route: Route) => {
    const url = route.request().url();
    if (url.includes("/callers") || url.includes("/callees")) {
      return route.continue();
    }
    return route.fulfill({ json: mockItemResponse });
  });
}

async function mockTraversalApis(page: Page): Promise<void> {
  await page.route(`**/v1/repos/${REPO_ID}/items/*/callers*`, (route: Route) =>
    route.fulfill({ json: mockCallersResponse }),
  );
  await page.route(`**/v1/repos/${REPO_ID}/items/*/callees*`, (route: Route) =>
    route.fulfill({ json: mockCalleesResponse }),
  );
}

async function mockSearchApi(page: Page): Promise<void> {
  await page.route("**/v1/search", (route: Route) => {
    if (route.request().method() === "POST") {
      return route.fulfill({ json: mockSearchResults });
    }
    return route.continue();
  });
}

// ─── Tests ───────────────────────────────────────────────────────────────────

test.describe("Wave 6 exit-criteria: code intelligence workspace (RUSAA-477)", () => {
  test.beforeEach(async ({ page }) => {
    await mockCodeWorkspaceApis(page);
  });

  test("code workspace page loads with module tree", async ({ page }) => {
    await page.goto(`/repos/${REPO_ID}/code`);

    await expect(page.getByText("Code workspace")).toBeVisible();
    await expect(
      page.getByRole("navigation", { name: "Module tree" }),
    ).toBeVisible();
    await expect(page.getByText("my_crate")).toBeVisible();
  });

  test("module tree renders nested items with kind labels", async ({
    page,
  }) => {
    await page.goto(`/repos/${REPO_ID}/code`);

    const tree = page.getByRole("navigation", { name: "Module tree" });
    await expect(tree).toBeVisible();

    await expect(tree.getByText("my_crate")).toBeVisible();
    await expect(tree.getByText("config", { exact: true })).toBeVisible();
    await expect(tree.getByText("load_config")).toBeVisible();
    await expect(tree.getByText("Config", { exact: true })).toBeVisible();
    await expect(tree.getByText("main", { exact: true })).toBeVisible();
  });

  test("clicking module tree item loads source viewer with line numbers", async ({
    page,
  }) => {
    await page.goto(`/repos/${REPO_ID}/code`);

    const treeItem = page.locator(
      `[aria-label="load_config — function"]`,
    );
    await expect(treeItem).toBeVisible();
    await treeItem.click();

    const sourceViewer = page.locator('[aria-label="Source viewer"]');
    await expect(sourceViewer).toBeVisible();

    await expect(
      page.locator(`[aria-label="Source for ${FIXTURE_FQN}"]`),
    ).toBeVisible();
    await expect(page.getByText("pub fn load_config")).toBeVisible();
    await expect(page.getByText("src/config.rs")).toBeVisible();

    // Line numbers rendered (start_line = 23)
    await expect(page.getByText("23", { exact: true })).toBeVisible();
    await expect(page.getByText("28", { exact: true })).toBeVisible();
  });

  test("source viewer shows FQN and kind badge", async ({ page }) => {
    await page.goto(`/repos/${REPO_ID}/code?fqn=${FIXTURE_FQN_B64}`);

    await expect(page.getByText(FIXTURE_FQN)).toBeVisible();
    await expect(page.getByText("FN", { exact: true })).toBeVisible();
    await expect(page.getByText("src/config.rs")).toBeVisible();
    await expect(page.getByText(":23")).toBeVisible();
  });

  test("side panel has search and relations tabs", async ({ page }) => {
    await page.goto(`/repos/${REPO_ID}/code`);

    const tabList = page.locator('[role="tablist"][aria-label="Side panel tabs"]');
    await expect(tabList).toBeVisible();

    const searchTab = tabList.getByRole("tab", { name: "search" });
    const relationsTab = tabList.getByRole("tab", { name: "relations" });
    await expect(searchTab).toBeVisible();
    await expect(relationsTab).toBeVisible();

    // Search tab is selected by default
    await expect(searchTab).toHaveAttribute("aria-selected", "true");

    // Switch to relations tab
    await relationsTab.click();
    await expect(relationsTab).toHaveAttribute("aria-selected", "true");
    await expect(searchTab).toHaveAttribute("aria-selected", "false");
  });

  test("no cross-tenant leakage: module tree API scoped to repo_id", async ({
    page,
  }) => {
    const moduleTreeUrls: string[] = [];
    const itemUrls: string[] = [];

    await page.route(`**/v1/repos/*/modules`, (route: Route) => {
      moduleTreeUrls.push(route.request().url());
      return route.fulfill({ json: mockModuleTree });
    });
    await page.route(`**/v1/repos/*/items/*`, (route: Route) => {
      itemUrls.push(route.request().url());
      return route.fulfill({ json: mockItemResponse });
    });

    await page.goto(`/repos/${REPO_ID}/code`);
    await expect(page.getByText("my_crate")).toBeVisible();

    const treeItem = page.locator('[aria-label="load_config — function"]');
    await treeItem.click();
    await expect(page.getByText("pub fn load_config")).toBeVisible();

    for (const url of moduleTreeUrls) {
      expect(url).toContain(`/repos/${REPO_ID}/`);
    }
    for (const url of itemUrls) {
      expect(url).toContain(`/repos/${REPO_ID}/`);
    }
  });

  test("selecting different tree items updates source viewer", async ({
    page,
  }) => {
    const configItem = {
      ...mockItemResponse,
      fqn: "my_crate::config::Config",
      kind: "STRUCT",
      source_preview: "pub struct Config {\n    pub db_url: String,\n}",
      line_start: 5,
      line_end: 7,
    };

    await page.route(`**/v1/repos/${REPO_ID}/items/*`, (route: Route) => {
      const url = route.request().url();
      if (url.includes("/callers") || url.includes("/callees")) {
        return route.continue();
      }
      if (url.includes(btoa("my_crate::config::Config").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, ""))) {
        return route.fulfill({ json: configItem });
      }
      return route.fulfill({ json: mockItemResponse });
    });

    await page.goto(`/repos/${REPO_ID}/code`);

    // Select load_config
    await page.locator('[aria-label="load_config — function"]').click();
    await expect(page.getByText("pub fn load_config")).toBeVisible();

    // Select Config struct
    await page.locator('[aria-label="Config — struct"]').click();
    await expect(page.getByText("pub struct Config")).toBeVisible();
    await expect(page.getByText("STRUCT", { exact: true })).toBeVisible();
  });

  // ─── Search and relations panels (stubs — need REQ-DP-01/03 wiring) ──────

  test.fixme(
    "search panel: enter query and click result navigates to item",
    async ({ page }) => {
      await mockSearchApi(page);
      await page.goto(`/repos/${REPO_ID}/code`);

      const searchTab = page.getByRole("tab", { name: "search" });
      await searchTab.click();

      const searchInput = page.getByPlaceholder(/search/i);
      await expect(searchInput).toBeVisible();
      await searchInput.fill("load configuration");
      await searchInput.press("Enter");

      await expect(page.getByText("load_config")).toBeVisible();
      await page.getByText("load_config").click();

      await expect(
        page.locator(`[aria-label="Source for ${FIXTURE_FQN}"]`),
      ).toBeVisible();
    },
  );

  test.fixme(
    "relations panel: callers tab renders BFS graph with ≥1 edge",
    async ({ page }) => {
      await mockTraversalApis(page);
      await page.goto(`/repos/${REPO_ID}/code?fqn=${FIXTURE_FQN_B64}`);

      const relationsTab = page.getByRole("tab", { name: "relations" });
      await relationsTab.click();

      await expect(page.getByText("main")).toBeVisible();
      await expect(page.getByText("run")).toBeVisible();

      const edges = page.locator('[data-testid="graph-edge"]');
      expect(await edges.count()).toBeGreaterThanOrEqual(1);
    },
  );

  test.fixme(
    "relations panel: callees tab renders BFS graph with ≥1 edge",
    async ({ page }) => {
      await mockTraversalApis(page);
      await page.goto(`/repos/${REPO_ID}/code?fqn=${FIXTURE_FQN_B64}`);

      const relationsTab = page.getByRole("tab", { name: "relations" });
      await relationsTab.click();

      // Switch to callees sub-tab
      await page.getByRole("tab", { name: /callees/i }).click();

      await expect(page.getByText("read_to_string")).toBeVisible();
      await expect(page.getByText("from_str")).toBeVisible();

      const edges = page.locator('[data-testid="graph-edge"]');
      expect(await edges.count()).toBeGreaterThanOrEqual(1);
    },
  );

  test.fixme(
    "full exit-criteria flow: search → item → callers → callees → source",
    async ({ page }, testInfo) => {
      await mockSearchApi(page);
      await mockTraversalApis(page);
      await page.goto(`/repos/${REPO_ID}/code`);

      // Step 1: Semantic search
      const searchInput = page.getByPlaceholder(/search/i);
      await searchInput.fill("load configuration");
      await searchInput.press("Enter");
      await expect(page.getByText("load_config")).toBeVisible();

      // Step 2: Click search result → item detail
      await page.getByText("load_config").click();
      await expect(
        page.locator(`[aria-label="Source for ${FIXTURE_FQN}"]`),
      ).toBeVisible();
      await expect(page.getByText("pub fn load_config")).toBeVisible();

      // Step 3: Callers
      const relationsTab = page.getByRole("tab", { name: "relations" });
      await relationsTab.click();
      const callerEdges = page.locator('[data-testid="graph-edge"]');
      await expect(callerEdges.first()).toBeVisible({ timeout: 5_000 });
      expect(await callerEdges.count()).toBeGreaterThanOrEqual(1);

      // Step 4: Callees
      await page.getByRole("tab", { name: /callees/i }).click();
      const calleeEdges = page.locator('[data-testid="graph-edge"]');
      await expect(calleeEdges.first()).toBeVisible({ timeout: 5_000 });
      expect(await calleeEdges.count()).toBeGreaterThanOrEqual(1);

      // Step 5: Source viewer still visible
      await expect(page.getByText("pub fn load_config")).toBeVisible();
      await expect(page.getByText("23", { exact: true })).toBeVisible();

      await testInfo.attach("wave6-full-flow.png", {
        body: await page.screenshot({ fullPage: true }),
        contentType: "image/png",
      });
    },
  );
});
