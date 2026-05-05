import { test, expect } from "@playwright/test";
import {
  mockAuthenticatedSession,
  mockReposList,
  mockModuleTree,
  mockSearch,
  mockCallers,
  mockCallees,
  mockItem,
  TEST_FQN_B64,
  SEARCH_RESPONSE,
  CALLERS_RESPONSE,
  CALLEES_RESPONSE,
} from "./fixtures/mock-api";

const REPO_ID = "repo-1";
const WORKSPACE_URL = `/repos/${REPO_ID}/code`;
const WORKSPACE_WITH_ITEM = `${WORKSPACE_URL}?fqn=${TEST_FQN_B64}`;

async function setupWorkspace(page: import("@playwright/test").Page): Promise<void> {
  await mockAuthenticatedSession(page);
  await mockReposList(page);
  await mockModuleTree(page);
  await mockItem(page);
  await mockCallers(page);
  await mockCallees(page);
  await mockSearch(page);
}

test.describe("Wave 6 — SearchPanel API integration", () => {
  test("SearchPanel query submits to /v1/search and renders results", async ({ page }) => {
    await setupWorkspace(page);
    await page.goto(WORKSPACE_URL);

    await page.getByRole("tab", { name: "search" }).click();

    const input = page.getByLabel("Search query");
    await expect(input).toBeVisible();

    await input.fill("my function");
    await page.getByRole("button", { name: "Go" }).click();

    const results = page.getByTestId("search-results");
    await expect(results).toBeVisible();

    for (const result of SEARCH_RESPONSE.results) {
      await expect(results.getByRole("button", { name: `Open ${result.fqn}` })).toBeVisible();
    }
  });

  test("Clicking a search result updates the URL with the selected item fqn", async ({ page }) => {
    await setupWorkspace(page);
    await page.goto(WORKSPACE_URL);

    await page.getByRole("tab", { name: "search" }).click();

    const input = page.getByLabel("Search query");
    await input.fill("my function");
    await page.getByRole("button", { name: "Go" }).click();

    const firstResult = SEARCH_RESPONSE.results[0];
    await page.getByRole("button", { name: `Open ${firstResult.fqn}` }).click();

    await expect(page).toHaveURL(/fqn=/);
  });
});

test.describe("Wave 6 — RelationsPanel API integration", () => {
  test("RelationsPanel callers tab renders BFS nodes from /callers endpoint", async ({ page }) => {
    await setupWorkspace(page);
    await page.goto(WORKSPACE_WITH_ITEM);

    await page.getByRole("tab", { name: "relations" }).click();

    await page.getByRole("tab", { name: "callers" }).click();

    const callersList = page.getByTestId("callers-list");
    await expect(callersList).toBeVisible();

    const callerNodes = CALLERS_RESPONSE.nodes.filter(
      (n) => n.fqn !== CALLERS_RESPONSE.root.fqn,
    );
    for (const node of callerNodes) {
      await expect(callersList.getByRole("button", { name: `Navigate to ${node.fqn}` })).toBeVisible();
    }
  });

  test("RelationsPanel callees tab renders BFS nodes from /callees endpoint", async ({ page }) => {
    await setupWorkspace(page);
    await page.goto(WORKSPACE_WITH_ITEM);

    await page.getByRole("tab", { name: "relations" }).click();

    await page.getByRole("tab", { name: "callees" }).click();

    const calleesList = page.getByTestId("callees-list");
    await expect(calleesList).toBeVisible();

    const calleeNodes = CALLEES_RESPONSE.nodes.filter(
      (n) => n.fqn !== CALLEES_RESPONSE.root.fqn,
    );
    for (const node of calleeNodes) {
      await expect(calleesList.getByRole("button", { name: `Navigate to ${node.fqn}` })).toBeVisible();
    }
  });
});
