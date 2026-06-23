import { test, expect } from "@playwright/test";
import {
  mockAuthenticatedSession,
  mockReposList,
  REPOS_RESPONSE,
  REPO_ITEM,
} from "./fixtures/mock-api";
import {
  mockChatSessionsListAndCreate,
  mockSendChatMessage,
  mockChatStream,
  mockListChatMessages,
  CHAT_SESSION_ID,
  LIST_SESSIONS_ONE,
  LIST_MESSAGES_EMPTY,
} from "./fixtures/chat-mock-api";

// CitationV1 payload matching the rb-schemas shape shipped by RUSAA-2089.
const CITATION_V1 = {
  version: "v1",
  repo_id: REPO_ITEM.repo_id,
  file_path: "src/query/hybrid.rs",
  line_range: { start: 42, end: 58 },
  commit_sha: "deadbeefcafe1234",
  score: 0.87,
  source_kind: "hybrid",
};

// The backend serialises citations as serde_json::to_string_pretty — a JSON string.
const CITATION_RESULT_JSON = JSON.stringify([CITATION_V1], null, 2);

const CITATION_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "find hybrid search code" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_use",
    sequence: 2,
    payload: {
      type: "tool_use",
      id: "tu-search-001",
      name: "mcp__rust_brain__search_items",
      input: { query: "hybrid search", limit: 5 },
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 3,
    payload: { type: "turn_complete", stop_reason: "tool_use" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_result",
    sequence: 4,
    payload: {
      type: "tool_result",
      tool_use_id: "tu-search-001",
      content: CITATION_RESULT_JSON,
      is_error: false,
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 5,
    payload: {
      type: "text",
      text: "I found the hybrid search implementation.",
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 6,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  "",
].join("\n");

// Regression guard for RUSAA-2091 (S4): CitationV1 chips rendered in chat tool results.
test.describe("Chat panel — CitationV1 citation chips [RUSAA-2091]", () => {
  test("AC2+AC3+AC4: search_items result renders clickable citation chips with badge and GitHub link", async ({
    page,
  }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, CITATION_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // ToolCallBlock for search_items is visible
    const toolBlock = page.getByTestId("tool-call-block");
    await expect(toolBlock).toBeVisible();
    await expect(toolBlock).toContainText("mcp__rust_brain__search_items");

    // Expand the tool block to reveal the citation chips
    await toolBlock.getByRole("button").click();

    // At least one citation chip is present
    const chip = page.getByTestId("citation-chip").first();
    await expect(chip).toBeVisible();

    // Chip contains file path and line range (AC2)
    await expect(chip).toContainText("src/query/hybrid.rs");
    await expect(chip).toContainText("42");

    // Chip has a valid GitHub link (AC3) — commit SHA in href, not "main"
    const href = await chip.getAttribute("href");
    expect(href).not.toBeNull();
    expect(href).toContain("github.com");
    expect(href).toContain("deadbeefcafe1234");
    expect(href).not.toContain("/main/");

    // source_kind badge is present (AC4) with "hybrid" label
    const badge = page.getByTestId("source-kind-badge-hybrid");
    await expect(badge).toBeVisible();
    await expect(badge).toContainText("hybrid");

    // Chip opens in new tab (target=_blank)
    const target = await chip.getAttribute("target");
    expect(target).toBe("_blank");
  });

  test("AC5: version mismatch renders soft warning, not a JS error", async ({ page }) => {
    const unknownVersionResult = JSON.stringify([
      { ...CITATION_V1, version: "v99" },
    ], null, 2);

    const unknownVersionSse = CITATION_SSE.replace(
      CITATION_RESULT_JSON,
      unknownVersionResult,
    );

    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, unknownVersionSse);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    const toolBlock = page.getByTestId("tool-call-block");
    await expect(toolBlock).toBeVisible();
    await toolBlock.getByRole("button").click();

    // No chips rendered (unknown version)
    await expect(page.getByTestId("citation-chip")).not.toBeVisible();

    // Soft warning is shown instead
    await expect(page.getByTestId("citation-version-warning")).toBeVisible();

    // No uncaught JS errors — if there were any, Playwright would have thrown above
  });

  test("AC2 empty: empty citation array renders graceful empty state", async ({ page }) => {
    const emptyCitationsSse = CITATION_SSE.replace(
      CITATION_RESULT_JSON,
      "[]",
    );

    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_RESPONSE);
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, emptyCitationsSse);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    const toolBlock = page.getByTestId("tool-call-block");
    await expect(toolBlock).toBeVisible();
    await toolBlock.getByRole("button").click();

    // No chips, no JS errors — graceful empty state
    await expect(page.getByTestId("citation-chip")).not.toBeVisible();
    await expect(page.getByTestId("citation-empty")).toBeVisible();
  });
});
