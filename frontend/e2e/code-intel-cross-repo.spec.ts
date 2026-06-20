import { test, expect } from "@playwright/test";
import {
  mockAuthenticatedSession,
  mockModuleTree,
  mockCallers,
  mockCallees,
  mockItem,
  mockReposList,
  REPOS_MULTI_RESPONSE,
  SEARCH_RESPONSE_CROSS_REPO,
  CROSS_REPO_FQN,
  CROSS_REPO_FQN_B64,
} from "./fixtures/mock-api";

const REPO_1_ID = "repo-1";
const WORKSPACE_URL = `/repos/${REPO_1_ID}/code`;

async function setupMultiRepoWorkspace(page: import("@playwright/test").Page): Promise<void> {
  await mockAuthenticatedSession(page);
  await mockReposList(page, REPOS_MULTI_RESPONSE);
  await mockModuleTree(page);
  await mockItem(page);
  await mockCallers(page);
  await mockCallees(page);

  await page.route("**/v1/search", (route) => {
    if (route.request().method() === "POST") {
      return route.fulfill({ json: SEARCH_RESPONSE_CROSS_REPO });
    }
    return route.continue();
  });
}

test.describe("Cross-repo search result navigation", () => {
  test("clicking a same-repo result keeps the current workspace repo in the URL", async ({ page }) => {
    await setupMultiRepoWorkspace(page);
    await page.goto(WORKSPACE_URL);

    await page.getByRole("tab", { name: "search" }).click();
    await page.getByLabel("Search query").fill("Builder");
    await page.getByRole("button", { name: "Go" }).click();

    const sameRepoResult = SEARCH_RESPONSE_CROSS_REPO.results[0];
    await page.getByRole("button", { name: `Open ${sameRepoResult.fqn}` }).click();

    await expect(page).toHaveURL(new RegExp(`/repos/${REPO_1_ID}/code`));
    await expect(page).toHaveURL(/fqn=/);
  });

  test("clicking a cross-repo result switches workspace to the result's repo", async ({ page }) => {
    await setupMultiRepoWorkspace(page);
    await page.goto(WORKSPACE_URL);

    await page.getByRole("tab", { name: "search" }).click();
    await page.getByLabel("Search query").fill("Builder");
    await page.getByRole("button", { name: "Go" }).click();

    await page.getByRole("button", { name: `Open ${CROSS_REPO_FQN}` }).click();

    await expect(page).toHaveURL(new RegExp(`/repos/repo-2/code`));
    await expect(page).toHaveURL(new RegExp(`fqn=${CROSS_REPO_FQN_B64}`));
  });

  test("repo pill is shown on cross-repo results when repos data is available", async ({ page }) => {
    await setupMultiRepoWorkspace(page);
    await page.goto(WORKSPACE_URL);

    await page.getByRole("tab", { name: "search" }).click();
    await page.getByLabel("Search query").fill("Builder");
    await page.getByRole("button", { name: "Go" }).click();

    const pills = page.getByTestId("repo-pill");
    await expect(pills.first()).toBeVisible();
    await expect(pills.first()).toContainText("acme/");
  });
});
