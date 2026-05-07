# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: ingestion-live.spec.ts >> Live-stack ingestion UAT >> SSE endpoint responds as text/event-stream (real broker, not mocked)
- Location: e2e/ingestion-live.spec.ts:107:3

# Error details

```
TimeoutError: apiRequestContext.get: Timeout 5000ms exceeded.
Call log:
  - → GET http://localhost:8080/v1/ingest/events
    - user-agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.7727.15 Safari/537.36
    - accept: text/event-stream
    - accept-encoding: gzip,deflate,br
    - cookie: rb_session=2d1f941d2864a5e702d99e5b6af348b23eef45009603dc0e3b74d8af5a84659b
  - ← 200 OK
    - content-type: text/event-stream
    - cache-control: no-cache
    - vary: origin, access-control-request-method, access-control-request-headers
    - access-control-allow-origin: *
    - transfer-encoding: chunked
    - date: Wed, 06 May 2026 23:31:17 GMT

```

# Test source

```ts
  10  |  *     endpoint. Stubbed tests cannot produce a valid UAT fingerprint because
  11  |  *     Docker containers aren't running, DB counts are 0, and Kafka offsets
  12  |  *     are 0. Gate 3 (RUSAA-695) will reject a verdict backed only by mocked
  13  |  *     tests.
  14  |  *   - This spec runs WITHOUT any route mocks so `uat-fingerprint.sh` — which
  15  |  *     inspects the same stack — can see real containers, real DB rows, and real
  16  |  *     Kafka offsets simultaneously.
  17  |  *
  18  |  * Prerequisites (live-stack mode):
  19  |  *   1. compose/e2e.yml stack is running (`bash scripts/e2e-smoke-test.sh` or
  20  |  *      `docker compose -f compose/e2e.yml up -d`).
  21  |  *   2. A user has been created and an ingest run has completed.
  22  |  *   3. Frontend dev server is reachable at LIVE_FRONTEND_URL.
  23  |  *
  24  |  * Environment variables:
  25  |  *   LIVE_FRONTEND_URL   — base URL for the running frontend (default: http://localhost:4173)
  26  |  *   LIVE_API_URL        — control-api base URL (default: http://localhost:18080)
  27  |  *   LIVE_USER_EMAIL     — email of an existing user (default: smoke@e2e.test)
  28  |  *   LIVE_USER_PASS      — password of the user (default: smoke-password-e2e-123)
  29  |  *
  30  |  * Run:
  31  |  *   LIVE_STACK=1 npx playwright test e2e/ingestion-live.spec.ts \
  32  |  *     --config playwright.config.ts
  33  |  *
  34  |  * This test is SKIPPED unless LIVE_STACK=1 is set, so it does not break the
  35  |  * default mocked CI suite.
  36  |  */
  37  | 
  38  | import * as fs from "node:fs";
  39  | import { expect, test, type APIRequestContext, type Page } from "@playwright/test";
  40  | 
  41  | const LIVE_FRONTEND_URL =
  42  |   process.env.LIVE_FRONTEND_URL ?? "http://localhost:4173";
  43  | const LIVE_API_URL = process.env.LIVE_API_URL ?? "http://localhost:18080";
  44  | const LIVE_USER_EMAIL = process.env.LIVE_USER_EMAIL ?? "smoke@e2e.test";
  45  | const LIVE_USER_PASS =
  46  |   process.env.LIVE_USER_PASS ?? "smoke-password-e2e-123";
  47  | 
  48  | /**
  49  |  * Skip the entire suite when LIVE_STACK is not set.
  50  |  * This keeps the default CI suite (mocked) unaffected.
  51  |  */
  52  | test.beforeEach(async ({}, testInfo) => {
  53  |   if (!process.env.LIVE_STACK) {
  54  |     testInfo.skip(
  55  |       true,
  56  |       "LIVE_STACK not set — skipping live-stack test. Set LIVE_STACK=1 to run against a real compose stack.",
  57  |     );
  58  |   }
  59  | });
  60  | 
  61  | async function loginViaApi(apiRequest: APIRequestContext): Promise<void> {
  62  |   const resp = await apiRequest.post(`${LIVE_API_URL}/v1/auth/login`, {
  63  |     data: { email: LIVE_USER_EMAIL, password: LIVE_USER_PASS },
  64  |   });
  65  |   if (!resp.ok()) {
  66  |     throw new Error(`Login failed (${resp.status()}): ${await resp.text()}`);
  67  |   }
  68  | }
  69  | 
  70  | async function loginViaPage(page: Page): Promise<void> {
  71  |   const resp = await page.request.post(`${LIVE_API_URL}/v1/auth/login`, {
  72  |     data: { email: LIVE_USER_EMAIL, password: LIVE_USER_PASS },
  73  |   });
  74  |   if (!resp.ok()) {
  75  |     throw new Error(`Login failed (${resp.status()}): ${await resp.text()}`);
  76  |   }
  77  | }
  78  | 
  79  | test.describe("Live-stack ingestion UAT", () => {
  80  |   /**
  81  |    * Test 1: Control-API health
  82  |    *
  83  |    * Verifies the live stack is running before proceeding. Fails fast if the
  84  |    * compose stack is not up, which is a clearer error than a timeout.
  85  |    */
  86  |   test("control-api /health returns ok stores", async ({ request }) => {
  87  |     const resp = await request.get(`${LIVE_API_URL}/health`);
  88  |     expect(resp.ok()).toBe(true);
  89  | 
  90  |     const body = await resp.json() as { stores?: Record<string, string> };
  91  |     expect(body).toHaveProperty("stores");
  92  |     expect(body.stores?.["neo4j"]).toBe("ok");
  93  |     expect(body.stores?.["qdrant"]).toBe("ok");
  94  |   });
  95  | 
  96  |   /**
  97  |    * Test 2: Real SSE endpoint responds as event-stream
  98  |    *
  99  |    * Authenticates via the real API and then checks the SSE endpoint header.
  100 |    * A mocked run returns a synthetic body; a live run returns a real
  101 |    * text/event-stream from the Kafka-backed broker.
  102 |    *
  103 |    * This is a structural proof: if Content-Type is text/event-stream from the
  104 |    * real control-api, then the fingerprint collector can observe real Kafka
  105 |    * offsets on the same broker.
  106 |    */
  107 |   test("SSE endpoint responds as text/event-stream (real broker, not mocked)", async ({ request }) => {
  108 |     await loginViaApi(request);
  109 | 
> 110 |     const resp = await request.get(`${LIVE_API_URL}/v1/ingest/events`, {
      |                                ^ TimeoutError: apiRequestContext.get: Timeout 5000ms exceeded.
  111 |       headers: { Accept: "text/event-stream" },
  112 |       timeout: 5_000,
  113 |     });
  114 | 
  115 |     expect(resp.ok(), `SSE endpoint returned ${resp.status()}`).toBe(true);
  116 |     const contentType = resp.headers()["content-type"] ?? "";
  117 |     expect(
  118 |       contentType,
  119 |       "SSE endpoint must return text/event-stream — a mocked route returns a static body, not a live stream",
  120 |     ).toContain("text/event-stream");
  121 |   });
  122 | 
  123 |   /**
  124 |    * Test 3: Ingestion page renders against real stack without route mocks
  125 |    *
  126 |    * Navigates to the ingestion page WITHOUT mocking any routes. The frontend
  127 |    * calls /v1/me and /v1/ingest/events against the real control-api. If the
  128 |    * user session is valid and the SSE broker is live, the page renders without
  129 |    * the "Not authenticated" fallback.
  130 |    *
  131 |    * Mocked contrast:
  132 |    *   - Mocked: page.route("**\/v1\/me") returns a fake user → renders fine
  133 |    *   - Live:   no route mock → must have a real session → proves real stack
  134 |    */
  135 |   test("ingestion page renders against real stack without any route mocks", async ({ page }) => {
  136 |     await page.goto(`${LIVE_FRONTEND_URL}/`);
  137 |     await loginViaPage(page);
  138 | 
  139 |     // Navigate to ingestion — NO route mocks registered on this page
  140 |     await page.goto(`${LIVE_FRONTEND_URL}/ingestion`);
  141 | 
  142 |     // Must not show the unauthenticated fallback
  143 |     await expect(page.locator("text=Sign in to view ingestion progress")).not.toBeVisible({
  144 |       timeout: 8_000,
  145 |     });
  146 | 
  147 |     // Ingestion Theatre heading must be present (real /v1/me succeeded)
  148 |     await expect(
  149 |       page.getByRole("heading", { name: "Ingestion Theatre" }),
  150 |     ).toBeVisible({ timeout: 8_000 });
  151 | 
  152 |     // Either active state or empty state is shown — both require a real SSE connection
  153 |     const hasActiveState = await page.getByTestId("ingestion-active-state").isVisible();
  154 |     const hasEmptyState = await page.getByTestId("ingestion-empty-state").isVisible();
  155 |     expect(
  156 |       hasActiveState || hasEmptyState,
  157 |       "expected either active or empty ingestion state — real SSE stream must be connected",
  158 |     ).toBe(true);
  159 |   });
  160 | 
  161 |   /**
  162 |    * Test 4: Fingerprint structural validation (mocked-run contrast)
  163 |    *
  164 |    * Verifies the fingerprint file (collected by uat-fingerprint.sh running
  165 |    * against the same stack) has verdict=pass with valid live-data fields.
  166 |    *
  167 |    * When run as part of a live-stack UAT, the fingerprint has verdict=pass.
  168 |    * When run against a mocked environment, uat-fingerprint.sh exits 1 and the
  169 |    * file either doesn't exist or has verdict=fail.
  170 |    */
  171 |   test("UAT fingerprint file has verdict=pass with non-zero live-data fields", async () => {
  172 |     const fingerprintPath =
  173 |       process.env.UAT_FINGERPRINT_FILE ?? "/tmp/uat-fingerprint.json";
  174 | 
  175 |     if (!fs.existsSync(fingerprintPath)) {
  176 |       console.info(
  177 |         `INFO: fingerprint file not found at ${fingerprintPath} — ` +
  178 |         `run 'bash scripts/uat-fingerprint.sh' before this test to collect live-stack data`,
  179 |       );
  180 |       return;
  181 |     }
  182 | 
  183 |     const fingerprint = JSON.parse(fs.readFileSync(fingerprintPath, "utf-8")) as {
  184 |       verdict: string;
  185 |       image_shas: Record<string, string>;
  186 |       db: { items: number; calls: number; relations: number };
  187 |       sse_offsets: Record<string, number>;
  188 |       trace_ids: string[];
  189 |     };
  190 | 
  191 |     expect(
  192 |       fingerprint.verdict,
  193 |       `fingerprint verdict must be 'pass'; got '${fingerprint.verdict}' — check uat-fingerprint.sh output`,
  194 |     ).toBe("pass");
  195 | 
  196 |     // At least one service must have a real image SHA
  197 |     const realShas = Object.values(fingerprint.image_shas).filter(
  198 |       (s) => s !== "MISSING" && s !== "ERROR",
  199 |     );
  200 |     expect(
  201 |       realShas.length,
  202 |       "image_shas must contain at least one real SHA — mocked stacks have no running containers",
  203 |     ).toBeGreaterThan(0);
  204 | 
  205 |     // DB items must be non-zero
  206 |     expect(
  207 |       fingerprint.db.items,
  208 |       "db.items must be > 0 — mocked runs have no real ingest",
  209 |     ).toBeGreaterThan(0);
  210 | 
```