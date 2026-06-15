import { test, expect, type Page } from "@playwright/test";
import {
  mockAuthenticatedSession,
  mockReposList,
  REPOS_EMPTY_RESPONSE,
} from "./fixtures/mock-api";
import {
  mockChatSessionsListAndCreate,
  mockChatStream,
  mockListChatMessages,
  CHAT_SESSION_ID,
  LIST_SESSIONS_ONE,
  LIST_MESSAGES_EMPTY,
} from "./fixtures/chat-mock-api";

// ---------------------------------------------------------------------------
// RUSAA-2018 regression guard: AppShell footer must not appear on /chat
//
// Root cause: AppShell always rendered a <footer> ("Rustacean control plane")
// that consumed 49px at the bottom of the viewport, creating a visible band
// below the MessageComposer in both light and dark themes.
//
// Fix: footer is suppressed when location.pathname === "/chat".
// ---------------------------------------------------------------------------

const CHAT_URL = "/chat";
const BOARD_VIEWPORT = { width: 1728, height: 1044 };

async function setupChatPage(page: Page, sessions = LIST_SESSIONS_ONE): Promise<void> {
  await mockAuthenticatedSession(page);
  await mockReposList(page, REPOS_EMPTY_RESPONSE);
  await mockChatSessionsListAndCreate(page, sessions);
  await mockListChatMessages(page, CHAT_SESSION_ID, LIST_MESSAGES_EMPTY);
  await mockChatStream(page, CHAT_SESSION_ID, "");
}

test.describe("Chat — no footer band (RUSAA-2018)", () => {
  test.use({ viewport: BOARD_VIEWPORT });

  test("footer is not rendered on /chat (light mode)", async ({ page }) => {
    await setupChatPage(page);
    await page.goto(`${CHAT_URL}?sessionId=${CHAT_SESSION_ID}`);
    await page.waitForSelector("aside[aria-label='Chat sessions']");

    const footer = page.locator("footer");
    await expect(footer).toHaveCount(0);
  });

  test("footer is not rendered on /chat (dark mode)", async ({ page }) => {
    await page.emulateMedia({ colorScheme: "dark" });
    await setupChatPage(page);
    await page.goto(`${CHAT_URL}?sessionId=${CHAT_SESSION_ID}`);
    await page.waitForSelector("aside[aria-label='Chat sessions']");

    const footer = page.locator("footer");
    await expect(footer).toHaveCount(0);
  });

  test("main fills full height below header on /chat — no unused vertical gap", async ({ page }) => {
    await setupChatPage(page);
    await page.goto(`${CHAT_URL}?sessionId=${CHAT_SESSION_ID}`);
    await page.waitForSelector("aside[aria-label='Chat sessions']");

    const { headerBottom, mainBottom, viewportHeight } = await page.evaluate(() => {
      const header = document.querySelector("header");
      const main = document.querySelector("main");
      const hb = header?.getBoundingClientRect();
      const mb = main?.getBoundingClientRect();
      return {
        headerBottom: Math.round(hb?.bottom ?? 0),
        mainBottom: Math.round(mb?.bottom ?? 0),
        viewportHeight: window.innerHeight,
      };
    });

    // main must start immediately below the header
    expect(mainBottom).toBe(viewportHeight);
    // header sanity: it is above main
    expect(headerBottom).toBeLessThan(viewportHeight);
  });

  test("footer IS rendered on non-chat pages (regression guard)", async ({ page }) => {
    await mockAuthenticatedSession(page);
    await mockReposList(page, REPOS_EMPTY_RESPONSE);

    await page.goto("/repos");
    await page.waitForSelector("header");

    const footer = page.locator("footer");
    await expect(footer).toBeVisible();
    await expect(footer).toContainText("Rustacean control plane");
  });
});
