import { test, expect } from "@playwright/test";
import {
  mockAuthenticatedSession,
  mockReposList,
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

// Citation fixture — repo_id matches REPO_ITEM.repo_id so chips resolve GitHub links
const CITATION_REPO_ID = REPO_ITEM.repo_id;

const SEARCH_RESULT_WITH_CITATIONS = JSON.stringify({
  results: [
    { fqn: "rb_query::hybrid_search", crate_name: "rb_query", repo_id: CITATION_REPO_ID, score: 0.87 },
  ],
  citations: [
    {
      version: "v1",
      repo_id: CITATION_REPO_ID,
      file_path: "crates/rb-query/src/lib.rs",
      line_range: { start: 42, end: 87 },
      commit_sha: "deadbeef1234567890",
      score: 0.87,
      source_kind: "hybrid",
    },
    {
      version: "v1",
      repo_id: CITATION_REPO_ID,
      file_path: "crates/rb-query/src/dense.rs",
      line_range: { start: 1, end: 15 },
      commit_sha: "deadbeef1234567890",
      score: 0.72,
      source_kind: "dense",
    },
    {
      version: "v1",
      repo_id: CITATION_REPO_ID,
      file_path: "crates/rb-query/src/sparse.rs",
      line_range: { start: 5, end: 30 },
      commit_sha: "deadbeef1234567890",
      score: 0.65,
      source_kind: "sparse",
    },
  ],
});

// SSE stream: user_input → tool_use(search_items) → tool_result(with citations) → text → turn_complete
const SEARCH_WITH_CITATIONS_SSE = [
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "user_input",
    sequence: 1,
    payload: { type: "user_input", text: "Search for hybrid search implementation" },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_use",
    sequence: 2,
    payload: {
      type: "tool_use",
      id: "search-tool-001",
      name: "search_items",
      input: { query: "hybrid search" },
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "tool_result",
    sequence: 3,
    payload: {
      type: "tool_result",
      tool_use_id: "search-tool-001",
      content: SEARCH_RESULT_WITH_CITATIONS,
      is_error: false,
    },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "text",
    sequence: 4,
    payload: { type: "text", text: "Here are the search results with citations." },
  })}`,
  "",
  "event: session.event",
  `data: ${JSON.stringify({
    session_id: CHAT_SESSION_ID,
    event_type: "turn_complete",
    sequence: 5,
    payload: { type: "turn_complete", stop_reason: "end_turn" },
  })}`,
  "",
  "",
].join("\n");

// AC2, AC4, AC7: citation chips render in score-desc order, all visible and clickable
test.describe("Citation chips — RUSAA-2091", () => {
  test("AC2+AC7: renders citation chips from search tool result", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, { repos: [REPO_ITEM] });
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, SEARCH_WITH_CITATIONS_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);

    // Wait for the text content to appear (turn is complete)
    await expect(page.getByText("Here are the search results with citations.")).toBeVisible();

    // Citation chips container must be present
    const container = page.getByTestId("citation-chips");
    await expect(container).toBeVisible();

    // All 3 citations must be rendered as chips
    const chips = container.getByTestId("citation-chip");
    await expect(chips).toHaveCount(3);

    // AC7: at least one chip is visible
    await expect(chips.first()).toBeVisible();
  });

  test("AC4: correct source_kind badges render for all kinds in fixture", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, { repos: [REPO_ITEM] });
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, SEARCH_WITH_CITATIONS_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);
    await expect(page.getByText("Here are the search results with citations.")).toBeVisible();

    const container = page.getByTestId("citation-chips");

    // Verify each source_kind is represented
    await expect(container.locator('[data-source-kind="hybrid"]')).toHaveCount(1);
    await expect(container.locator('[data-source-kind="dense"]')).toHaveCount(1);
    await expect(container.locator('[data-source-kind="sparse"]')).toHaveCount(1);
  });

  test("AC3: chip href points to GitHub blob URL via commit_sha", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, { repos: [REPO_ITEM] });
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, SEARCH_WITH_CITATIONS_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);
    await expect(page.getByText("Here are the search results with citations.")).toBeVisible();

    const container = page.getByTestId("citation-chips");
    const firstChip = container.getByTestId("citation-chip").first();

    // First chip (highest score = hybrid at 0.87) should be an anchor with GitHub URL
    const href = await firstChip.getAttribute("href");
    expect(href).not.toBeNull();
    expect(href).toContain("https://github.com/acme/web-app/blob/deadbeef1234567890");
    expect(href).not.toContain("/main/");
  });

  test("score-desc ordering: highest score chip appears first", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, { repos: [REPO_ITEM] });
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, SEARCH_WITH_CITATIONS_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);
    await expect(page.getByText("Here are the search results with citations.")).toBeVisible();

    const container = page.getByTestId("citation-chips");
    const chips = container.getByTestId("citation-chip");

    // First chip must be the hybrid one (score 0.87) — highest
    const firstKind = await chips.first().getAttribute("data-source-kind");
    expect(firstKind).toBe("hybrid");
  });

  test("AC5: no citation chips when tool result has no citations field", async ({ page }) => {
    const NO_CITATIONS_SSE = [
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "text",
        sequence: 1,
        payload: { type: "text", text: "No citations here." },
      })}`,
      "",
      "event: session.event",
      `data: ${JSON.stringify({
        session_id: CHAT_SESSION_ID,
        event_type: "turn_complete",
        sequence: 2,
        payload: { type: "turn_complete", stop_reason: "end_turn" },
      })}`,
      "",
      "",
    ].join("\n");

    await mockAuthenticatedSession(page);
    await mockReposList(page, { repos: [REPO_ITEM] });
    await mockChatSessionsListAndCreate(page, LIST_SESSIONS_ONE);
    await mockChatStream(page, CHAT_SESSION_ID, NO_CITATIONS_SSE);
    await mockSendChatMessage(page);
    await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);

    await page.goto(`/chat?sessionId=${CHAT_SESSION_ID}`);
    await expect(page.getByText("No citations here.")).toBeVisible();

    // Citation chips container must NOT exist when there are no citations
    await expect(page.getByTestId("citation-chips")).not.toBeVisible();
  });
});
