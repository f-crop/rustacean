import { describe, it, expect } from "vitest";
import { filterTree } from "./module-tree-utils";
import type { components } from "@/api/generated/schema";

type ModuleNodeItem = components["schemas"]["ModuleNodeItem"];

function node(
  fqn: string,
  children: ModuleNodeItem[] = [],
  kind = "MOD",
): ModuleNodeItem {
  return { fqn, name: fqn.split("::").at(-1) ?? fqn, kind, children, source: null };
}

// 4-level fixture:
// root
//   routes        (depth 1)
//     routes::chat  (depth 2)
//       routes::chat::handler  (depth 3 — fn)
//     routes::admin (depth 2)
//   utils         (depth 1)
//     utils::fmt  (depth 2)
const FIXTURE: ModuleNodeItem = node("root", [
  node("routes", [
    node("routes::chat", [
      node("routes::chat::handler", [], "FN"),
    ]),
    node("routes::admin", []),
  ]),
  node("utils", [
    node("utils::fmt", []),
  ]),
]);

describe("filterTree", () => {
  it("returns unfiltered tree with matchCount 0 when query is empty", () => {
    const { filteredNode, matchCount } = filterTree(FIXTURE, "");
    // empty query handled upstream; filterTree is still called with empty string
    expect(filteredNode).toBe(FIXTURE);
    expect(matchCount).toBe(0);
  });

  it("matches on substring of FQN (case-insensitive)", () => {
    const { filteredNode, matchCount } = filterTree(FIXTURE, "CHAT");
    expect(matchCount).toBe(2); // routes::chat + routes::chat::handler
    expect(filteredNode).not.toBeNull();
    // root is preserved as ancestor
    expect(filteredNode?.fqn).toBe("root");
    // routes is preserved as ancestor
    const routes = filteredNode?.children[0];
    expect(routes?.fqn).toBe("routes");
    // routes::chat preserved
    const chat = routes?.children[0];
    expect(chat?.fqn).toBe("routes::chat");
    // routes::admin NOT preserved (no match)
    expect(routes?.children).toHaveLength(1);
    // utils NOT preserved (no match)
    expect(filteredNode?.children).toHaveLength(1);
  });

  it("returns null when nothing matches", () => {
    const { filteredNode, matchCount } = filterTree(FIXTURE, "zzznomatch");
    expect(filteredNode).toBeNull();
    expect(matchCount).toBe(0);
  });

  it("self-matching leaf node is included without children", () => {
    const { filteredNode, matchCount } = filterTree(FIXTURE, "handler");
    expect(matchCount).toBe(1);
    // Leaf itself
    const handler = filteredNode?.children[0]?.children[0]?.children[0];
    expect(handler?.fqn).toBe("routes::chat::handler");
    expect(handler?.children).toHaveLength(0);
  });

  it("self-matching ancestor exposes its matching children", () => {
    const { filteredNode, matchCount } = filterTree(FIXTURE, "routes::chat");
    // routes::chat matches (1) AND routes::chat::handler matches (2)
    expect(matchCount).toBe(2);
    const chat = filteredNode?.children[0]?.children[0];
    expect(chat?.fqn).toBe("routes::chat");
    // handler is included because it also matches
    expect(chat?.children).toHaveLength(1);
  });

  it("non-ancestor siblings are excluded", () => {
    const { filteredNode } = filterTree(FIXTURE, "utils");
    // Only utils branch; routes branch must be absent
    expect(filteredNode?.children).toHaveLength(1);
    expect(filteredNode?.children[0]?.fqn).toBe("utils");
  });

  it("match count reflects every matched FQN", () => {
    const { matchCount } = filterTree(FIXTURE, "routes");
    // routes, routes::chat, routes::chat::handler, routes::admin all contain "routes"
    expect(matchCount).toBe(4);
  });
});
