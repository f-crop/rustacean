import type { components } from "@/api/generated/schema";

type ModuleNodeItem = components["schemas"]["ModuleNodeItem"];

/** Returns a pruned copy of the tree containing only nodes whose FQN contains
 *  `query` (case-insensitive) and their ancestors. Also returns the match count.
 *  Returns the original node unchanged with matchCount 0 when query is empty. */
export function filterTree(
  node: ModuleNodeItem,
  query: string,
): { filteredNode: ModuleNodeItem | null; matchCount: number } {
  if (!query) return { filteredNode: node, matchCount: 0 };

  const lowerQuery = query.toLowerCase();
  let matchCount = 0;

  function process(n: ModuleNodeItem): ModuleNodeItem | null {
    const selfMatches = n.fqn.toLowerCase().includes(lowerQuery);
    if (selfMatches) matchCount++;

    const filteredChildren = n.children
      .map((child) => process(child))
      .filter((c): c is ModuleNodeItem => c !== null);

    if (!selfMatches && filteredChildren.length === 0) {
      return null;
    }

    return { ...n, children: filteredChildren };
  }

  return { filteredNode: process(node), matchCount };
}
