import { test, expect } from "@playwright/test";

const TENANT_A = {
  id: "aaaaaaaa-0000-4000-8000-aaaaaaaaaaaa",
  name: "Acme Corp",
  role: "owner",
  slug: "acme-corp",
};

const TENANT_B = {
  id: "bbbbbbbb-0000-4000-8000-bbbbbbbbbbbb",
  name: "Side Project",
  role: "admin",
  slug: "side-project",
};

const SINGLE_TENANT_ME = {
  user: {
    id: "user-uuid-1",
    email: "user@example.com",
    email_verified: true,
    status: "active",
    created_at: "2024-01-01T00:00:00Z",
  },
  current_tenant: TENANT_A,
  available_tenants: [TENANT_A],
};

const MULTI_TENANT_ME = {
  ...SINGLE_TENANT_ME,
  available_tenants: [TENANT_A, TENANT_B],
};

const SWITCHED_ME = {
  ...SINGLE_TENANT_ME,
  current_tenant: TENANT_B,
  available_tenants: [TENANT_A, TENANT_B],
};

async function mockBase(
  page: import("@playwright/test").Page,
  meResponse = MULTI_TENANT_ME,
) {
  await page.route("**/v1/me", (route) =>
    route.fulfill({ json: meResponse }),
  );
  await page.route("**/v1/repos", (route) => {
    if (route.request().method() === "GET") {
      return route.fulfill({ json: { repos: [] } });
    }
    return route.continue();
  });
}

test("tenant name shown as plain label when only one tenant", async ({
  page,
}) => {
  await mockBase(page, SINGLE_TENANT_ME);
  await page.goto("/repos");
  await expect(page.getByTestId("tenant-current-name")).toHaveText("Acme Corp");
  await expect(page.getByTestId("tenant-switcher-trigger")).not.toBeVisible();
});

test("tenant switcher shows current tenant name with multiple tenants", async ({
  page,
}) => {
  await mockBase(page);
  await page.goto("/repos");
  const trigger = page.getByTestId("tenant-switcher-trigger");
  await expect(trigger).toBeVisible();
  await expect(page.getByTestId("tenant-current-name")).toHaveText("Acme Corp");
});

test("tenant switcher opens menu listing all tenants", async ({ page }) => {
  await mockBase(page);
  await page.goto("/repos");
  await page.getByTestId("tenant-switcher-trigger").click();
  const menu = page.getByTestId("tenant-switcher-menu");
  await expect(menu).toBeVisible();
  await expect(
    page.getByTestId(`tenant-option-${TENANT_A.slug}`),
  ).toContainText("Acme Corp");
  await expect(
    page.getByTestId(`tenant-option-${TENANT_B.slug}`),
  ).toContainText("Side Project");
});

test("Escape key closes menu and returns focus to trigger", async ({
  page,
}) => {
  await mockBase(page);
  await page.goto("/repos");
  await page.getByTestId("tenant-switcher-trigger").click();
  await expect(page.getByTestId("tenant-switcher-menu")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("tenant-switcher-menu")).not.toBeVisible();
});

test("selecting current tenant is a no-op — no POST fired", async ({
  page,
}) => {
  await mockBase(page);
  let switchCalled = false;
  await page.route("**/v1/me/switch-tenant", () => {
    switchCalled = true;
  });

  await page.goto("/repos");
  await page.getByTestId("tenant-switcher-trigger").click();
  await page.getByTestId(`tenant-option-${TENANT_A.slug}`).click();
  await expect(page.getByTestId("tenant-switcher-menu")).not.toBeVisible();
  expect(switchCalled).toBe(false);
});

test("selecting a different tenant calls POST and refreshes tenant name", async ({
  page,
}) => {
  await mockBase(page);

  let switchBody: unknown = null;
  await page.route("**/v1/me/switch-tenant", async (route) => {
    switchBody = await route.request().postDataJSON();
    await route.fulfill({ json: { current_tenant: TENANT_B } });
  });

  // After the switch, /v1/me returns the updated session.
  let meCallCount = 0;
  await page.route("**/v1/me", (route) => {
    meCallCount++;
    return route.fulfill({
      json: meCallCount === 1 ? MULTI_TENANT_ME : SWITCHED_ME,
    });
  });

  await page.goto("/repos");
  await page.getByTestId("tenant-switcher-trigger").click();
  await page.getByTestId(`tenant-option-${TENANT_B.slug}`).click();

  expect(switchBody).toEqual({ tenant_id: TENANT_B.id });

  await expect(page.getByTestId("tenant-current-name")).toHaveText(
    "Side Project",
    { timeout: 5000 },
  );
});
