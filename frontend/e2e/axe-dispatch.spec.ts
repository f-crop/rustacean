import { test, expect } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mkdirSync, writeFileSync } from "fs";
import path from "path";
import {
  ME_RESPONSE,
  REPOS_RESPONSE,
  MEMBERS_RESPONSE,
  API_KEYS_RESPONSE,
  STAGE_TIMELINE_RESPONSE,
} from "./fixtures/mock-api";

const ALL_ROUTES = [
  "/login",
  "/signup",
  "/verify-email",
  "/forgot-password",
  "/reset-password",
  "/repos",
  "/members",
  "/api-keys",
  "/trace/abc12345def67890abc12345def67890",
] as const;

const scanRouteEnv = (process.env.SCAN_ROUTE ?? "").trim();
const routes: string[] = scanRouteEnv ? [scanRouteEnv] : [...ALL_ROUTES];

type MockFn = (page: import("@playwright/test").Page) => Promise<void>;

const ROUTE_MOCKS: Partial<Record<string, MockFn>> = {
  "/repos": async (page) => {
    await page.route("**/v1/me", (r) => r.fulfill({ json: ME_RESPONSE }));
    await page.route("**/v1/repos", (r) => r.fulfill({ json: REPOS_RESPONSE }));
  },
  "/members": async (page) => {
    await page.route("**/v1/me", (r) => r.fulfill({ json: ME_RESPONSE }));
    await page.route("**/v1/tenants/*/members", (r) =>
      r.fulfill({ json: MEMBERS_RESPONSE }),
    );
  },
  "/api-keys": async (page) => {
    await page.route("**/v1/me", (r) => r.fulfill({ json: ME_RESPONSE }));
    await page.route("**/v1/api-keys", (r) =>
      r.fulfill({ json: API_KEYS_RESPONSE }),
    );
  },
  "/trace/abc12345def67890abc12345def67890": async (page) => {
    await page.route("**/v1/me", (r) => r.fulfill({ json: ME_RESPONSE }));
    await page.route("**/v1/ingestions/*/stages", (r) =>
      r.fulfill({ json: STAGE_TIMELINE_RESPONSE }),
    );
  },
};

const OUT_DIR = path.resolve("axe-results");

function routeToSlug(route: string): string {
  return route.replace(/^\//, "").replace(/\//g, "-") || "root";
}

interface FocusEntry {
  tag: string;
  role: string | null;
  label: string | null;
}

interface RouteAxeResult {
  route: string;
  violations: { id: string; description: string; impact: string | null; nodes: number }[];
  passes: number;
  incomplete: number;
}

const allResults: RouteAxeResult[] = [];

test.describe("axe-dispatch", () => {
  test.describe.configure({ mode: "serial" });

  test.beforeAll(() => {
    mkdirSync(OUT_DIR, { recursive: true });
  });

  test.afterAll(() => {
    const report = {
      branch: process.env.SCAN_BRANCH ?? "",
      sha: process.env.SCAN_SHA ?? "",
      scannedAt: new Date().toISOString(),
      summary: {
        routes: allResults.length,
        totalViolations: allResults.reduce((s, r) => s + r.violations.length, 0),
      },
      results: allResults,
    };
    writeFileSync(path.join(OUT_DIR, "axe-report.json"), JSON.stringify(report, null, 2));
  });

  for (const route of routes) {
    const slug = routeToSlug(route);

    test(`axe scan: ${route}`, async ({ page }, testInfo) => {
      const mock = ROUTE_MOCKS[route];
      if (mock) await mock(page);

      await page.goto(route);
      await page.waitForLoadState("networkidle");

      const result = await new AxeBuilder({ page })
        .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
        .analyze();

      const routeResult: RouteAxeResult = {
        route,
        violations: result.violations.map((v) => ({
          id: v.id,
          description: v.description,
          impact: v.impact ?? null,
          nodes: v.nodes.length,
        })),
        passes: result.passes.length,
        incomplete: result.incomplete.length,
      };
      allResults.push(routeResult);

      const screenshot = await page.screenshot({ fullPage: true });
      await testInfo.attach(`screenshot-${slug}`, {
        body: screenshot,
        contentType: "image/png",
      });
      writeFileSync(path.join(OUT_DIR, `${slug}.png`), screenshot);
      writeFileSync(path.join(OUT_DIR, `${slug}-axe.json`), JSON.stringify(routeResult, null, 2));

      expect(
        result.violations,
        `axe violations on ${route}: ${result.violations.map((v) => v.id).join(", ")}`,
      ).toHaveLength(0);
    });

    test(`focus-trap probe: ${route}`, async ({ page }, testInfo) => {
      const mock = ROUTE_MOCKS[route];
      if (mock) await mock(page);

      await page.goto(route);

      const focusOrder: FocusEntry[] = [];
      for (let i = 0; i < 10; i++) {
        await page.keyboard.press("Tab");
        const entry = await page.evaluate((): FocusEntry | null => {
          const el = document.activeElement as HTMLElement | null;
          if (!el || el === document.body) return null;
          return {
            tag: el.tagName,
            role: el.getAttribute("role"),
            label:
              el.getAttribute("aria-label") ??
              el.textContent?.trim().slice(0, 40) ??
              null,
          };
        });
        if (entry) focusOrder.push(entry);
      }

      const focusReport = { route, focusOrder };
      await testInfo.attach(`focus-${slug}.json`, {
        body: Buffer.from(JSON.stringify(focusReport, null, 2)),
        contentType: "application/json",
      });
      writeFileSync(path.join(OUT_DIR, `${slug}-focus.json`), JSON.stringify(focusReport, null, 2));

      expect(focusOrder, `no focusable elements on ${route}`).not.toHaveLength(0);
    });
  }
});
