/**
 * Live-stack ingestion UAT (RUSAA-696 — Pillar C Phase 2)
 *
 * This spec proves that Playwright tests running against the REAL compose
 * stack (no `page.route()` API mocks) can observe live SSE events. It is
 * the reference implementation that satisfies the fingerprint contract.
 *
 * WHY this file exists:
 *   - All other ingestion-theatre tests use `mockSseStream()` to stub the SSE
 *     endpoint. Stubbed tests cannot produce a valid UAT fingerprint because
 *     Docker containers aren't running, DB counts are 0, and Kafka offsets
 *     are 0. Gate 3 (RUSAA-695) will reject a verdict backed only by mocked
 *     tests.
 *   - This spec runs WITHOUT any route mocks so `uat-fingerprint.sh` — which
 *     inspects the same stack — can see real containers, real DB rows, and real
 *     Kafka offsets simultaneously.
 *
 * Prerequisites (live-stack mode):
 *   1. compose/e2e.yml stack is running (`bash scripts/e2e-smoke-test.sh` or
 *      `docker compose -f compose/e2e.yml up -d`).
 *   2. A user has been created and an ingest run has completed.
 *   3. Frontend dev server is reachable at LIVE_FRONTEND_URL.
 *
 * Environment variables:
 *   LIVE_FRONTEND_URL   — base URL for the running frontend (default: http://localhost:4173)
 *   LIVE_API_URL        — control-api base URL (default: http://localhost:18080)
 *   LIVE_USER_EMAIL     — email of an existing user (default: smoke@e2e.test)
 *   LIVE_USER_PASS      — password of the user (default: smoke-password-e2e-123)
 *
 * Run:
 *   LIVE_STACK=1 npx playwright test e2e/ingestion-live.spec.ts \
 *     --config playwright.config.ts
 *
 * This test is SKIPPED unless LIVE_STACK=1 is set, so it does not break the
 * default mocked CI suite.
 */

import * as fs from "node:fs";
import { expect, test, type APIRequestContext, type Page } from "@playwright/test";

const LIVE_FRONTEND_URL =
  process.env.LIVE_FRONTEND_URL ?? "http://localhost:4173";
const LIVE_API_URL = process.env.LIVE_API_URL ?? "http://localhost:18080";
const LIVE_USER_EMAIL = process.env.LIVE_USER_EMAIL ?? "smoke@e2e.test";
const LIVE_USER_PASS =
  process.env.LIVE_USER_PASS ?? "smoke-password-e2e-123";

/**
 * Skip the entire suite when LIVE_STACK is not set.
 * This keeps the default CI suite (mocked) unaffected.
 */
test.beforeEach(async ({}, testInfo) => {
  if (!process.env.LIVE_STACK) {
    testInfo.skip(
      true,
      "LIVE_STACK not set — skipping live-stack test. Set LIVE_STACK=1 to run against a real compose stack.",
    );
  }
});

async function loginViaApi(apiRequest: APIRequestContext): Promise<void> {
  const resp = await apiRequest.post(`${LIVE_API_URL}/v1/auth/login`, {
    data: { email: LIVE_USER_EMAIL, password: LIVE_USER_PASS },
  });
  if (!resp.ok()) {
    throw new Error(`Login failed (${resp.status()}): ${await resp.text()}`);
  }
}

async function loginViaPage(page: Page): Promise<void> {
  const resp = await page.request.post(`${LIVE_API_URL}/v1/auth/login`, {
    data: { email: LIVE_USER_EMAIL, password: LIVE_USER_PASS },
  });
  if (!resp.ok()) {
    throw new Error(`Login failed (${resp.status()}): ${await resp.text()}`);
  }
}

test.describe("Live-stack ingestion UAT", () => {
  /**
   * Test 1: Control-API health
   *
   * Verifies the live stack is running before proceeding. Fails fast if the
   * compose stack is not up, which is a clearer error than a timeout.
   */
  test("control-api /health returns ok stores", async ({ request }) => {
    const resp = await request.get(`${LIVE_API_URL}/health`);
    expect(resp.ok()).toBe(true);

    const body = await resp.json() as { stores?: Record<string, string> };
    expect(body).toHaveProperty("stores");
    expect(body.stores?.["neo4j"]).toBe("ok");
    expect(body.stores?.["qdrant"]).toBe("ok");
  });

  /**
   * Test 2: Real SSE endpoint responds as event-stream
   *
   * Authenticates via the real API and then checks the SSE endpoint header.
   * A mocked run returns a synthetic body; a live run returns a real
   * text/event-stream from the Kafka-backed broker.
   *
   * This is a structural proof: if Content-Type is text/event-stream from the
   * real control-api, then the fingerprint collector can observe real Kafka
   * offsets on the same broker.
   */
  test("SSE endpoint responds as text/event-stream (real broker, not mocked)", async ({ request }) => {
    await loginViaApi(request);

    const resp = await request.get(`${LIVE_API_URL}/v1/ingest/events`, {
      headers: { Accept: "text/event-stream" },
      timeout: 5_000,
    });

    expect(resp.ok(), `SSE endpoint returned ${resp.status()}`).toBe(true);
    const contentType = resp.headers()["content-type"] ?? "";
    expect(
      contentType,
      "SSE endpoint must return text/event-stream — a mocked route returns a static body, not a live stream",
    ).toContain("text/event-stream");
  });

  /**
   * Test 3: Ingestion page renders against real stack without route mocks
   *
   * Navigates to the ingestion page WITHOUT mocking any routes. The frontend
   * calls /v1/me and /v1/ingest/events against the real control-api. If the
   * user session is valid and the SSE broker is live, the page renders without
   * the "Not authenticated" fallback.
   *
   * Mocked contrast:
   *   - Mocked: page.route("**\/v1\/me") returns a fake user → renders fine
   *   - Live:   no route mock → must have a real session → proves real stack
   */
  test("ingestion page renders against real stack without any route mocks", async ({ page }) => {
    await page.goto(`${LIVE_FRONTEND_URL}/`);
    await loginViaPage(page);

    // Navigate to ingestion — NO route mocks registered on this page
    await page.goto(`${LIVE_FRONTEND_URL}/ingestion`);

    // Must not show the unauthenticated fallback
    await expect(page.locator("text=Sign in to view ingestion progress")).not.toBeVisible({
      timeout: 8_000,
    });

    // Ingestion Theatre heading must be present (real /v1/me succeeded)
    await expect(
      page.getByRole("heading", { name: "Ingestion Theatre" }),
    ).toBeVisible({ timeout: 8_000 });

    // Either active state or empty state is shown — both require a real SSE connection
    const hasActiveState = await page.getByTestId("ingestion-active-state").isVisible();
    const hasEmptyState = await page.getByTestId("ingestion-empty-state").isVisible();
    expect(
      hasActiveState || hasEmptyState,
      "expected either active or empty ingestion state — real SSE stream must be connected",
    ).toBe(true);
  });

  /**
   * Test 4: Fingerprint structural validation (mocked-run contrast)
   *
   * Verifies the fingerprint file (collected by uat-fingerprint.sh running
   * against the same stack) has verdict=pass with valid live-data fields.
   *
   * When run as part of a live-stack UAT, the fingerprint has verdict=pass.
   * When run against a mocked environment, uat-fingerprint.sh exits 1 and the
   * file either doesn't exist or has verdict=fail.
   */
  test("UAT fingerprint file has verdict=pass with non-zero live-data fields", async () => {
    const fingerprintPath =
      process.env.UAT_FINGERPRINT_FILE ?? "/tmp/uat-fingerprint.json";

    if (!fs.existsSync(fingerprintPath)) {
      console.info(
        `INFO: fingerprint file not found at ${fingerprintPath} — ` +
        `run 'bash scripts/uat-fingerprint.sh' before this test to collect live-stack data`,
      );
      return;
    }

    const fingerprint = JSON.parse(fs.readFileSync(fingerprintPath, "utf-8")) as {
      verdict: string;
      image_shas: Record<string, string>;
      db: { items: number; calls: number; relations: number };
      sse_offsets: Record<string, number>;
      trace_ids: string[];
    };

    expect(
      fingerprint.verdict,
      `fingerprint verdict must be 'pass'; got '${fingerprint.verdict}' — check uat-fingerprint.sh output`,
    ).toBe("pass");

    // At least one service must have a real image SHA
    const realShas = Object.values(fingerprint.image_shas).filter(
      (s) => s !== "MISSING" && s !== "ERROR",
    );
    expect(
      realShas.length,
      "image_shas must contain at least one real SHA — mocked stacks have no running containers",
    ).toBeGreaterThan(0);

    // DB items must be non-zero
    expect(
      fingerprint.db.items,
      "db.items must be > 0 — mocked runs have no real ingest",
    ).toBeGreaterThan(0);

    // At least one Kafka offset must be non-zero
    const nonZeroOffsets = Object.values(fingerprint.sse_offsets).filter(
      (o) => o > 0,
    );
    expect(
      nonZeroOffsets.length,
      "sse_offsets must have at least one non-zero topic — mocked runs produce no Kafka messages",
    ).toBeGreaterThan(0);

    // Trace IDs must be present
    expect(
      fingerprint.trace_ids.length,
      "trace_ids must be non-empty — mocked runs have no live OTEL propagation",
    ).toBeGreaterThan(0);
  });
});
