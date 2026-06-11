import { describe, it, expect } from "vitest";
import { defaultSchema } from "rehype-sanitize";
import { sanitize } from "hast-util-sanitize";
import type { Root, Element } from "hast";

// Verify the rehype-sanitize defaultSchema provides XSS safety without the component needing DOM/JSDOM.
describe("markdown sanitization schema", () => {
  it("strips <script> elements", () => {
    expect(defaultSchema.strip).toContain("script");
  });

  it("does not allow javascript: in href protocols", () => {
    expect(defaultSchema.protocols?.href).not.toContain("javascript");
  });

  it("allows language-* classNames on <code> elements", () => {
    const codeAttrs = defaultSchema.attributes?.code ?? [];
    const hasLangPattern = codeAttrs.some(
      (attr) =>
        Array.isArray(attr) &&
        attr[0] === "className" &&
        attr[1] instanceof RegExp &&
        attr[1].test("language-python"),
    );
    expect(hasLangPattern).toBe(true);
  });
});

function sanitizeRoot(tree: Root): Root {
  return sanitize(tree, defaultSchema) as Root;
}

describe("markdown XSS sanitization — pipeline", () => {
  it("strips <script> element from HAST tree", () => {
    const tree: Root = {
      type: "root",
      children: [
        {
          type: "element",
          tagName: "p",
          properties: {},
          children: [{ type: "text", value: "hello" }],
        },
        {
          type: "element",
          tagName: "script",
          properties: {},
          children: [{ type: "text", value: "alert(1)" }],
        },
      ],
    };
    const sanitized = sanitizeRoot(tree);
    const tagNames = sanitized.children.map((n) =>
      n.type === "element" ? (n as Element).tagName : n.type,
    );
    expect(tagNames).not.toContain("script");
    expect(tagNames).toContain("p");
  });

  it("strips javascript: href from anchor elements", () => {
    const tree: Root = {
      type: "root",
      children: [
        {
          type: "element",
          tagName: "a",
          properties: { href: "javascript:alert(1)" },
          children: [{ type: "text", value: "click" }],
        },
      ],
    };
    const sanitized = sanitizeRoot(tree);
    const anchor = sanitized.children.find(
      (n) => n.type === "element" && (n as Element).tagName === "a",
    ) as Element | undefined;
    expect(anchor).toBeDefined();
    if (anchor) {
      expect(anchor.properties?.href).toBeUndefined();
    }
  });

  it("strips on* event handler attributes", () => {
    const tree: Root = {
      type: "root",
      children: [
        {
          type: "element",
          tagName: "img",
          properties: { src: "x", onerror: "alert(1)" },
          children: [],
        },
      ],
    };
    const sanitized = sanitizeRoot(tree);
    const img = sanitized.children.find(
      (n) => n.type === "element" && (n as Element).tagName === "img",
    ) as Element | undefined;
    if (img) {
      expect(img.properties?.onerror).toBeUndefined();
    }
  });
});
