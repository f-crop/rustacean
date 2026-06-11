import { describe, it, expect } from "vitest";
import { looksLikeMarkdown, needsTruncation } from "./tool-call-utils";

describe("looksLikeMarkdown", () => {
  it("returns false for a JSON object string", () => {
    expect(looksLikeMarkdown('{"key": "value"}')).toBe(false);
  });

  it("returns false for a JSON array string", () => {
    expect(looksLikeMarkdown("[1, 2, 3]")).toBe(false);
  });

  it("returns false for a JSON number string", () => {
    expect(looksLikeMarkdown("42")).toBe(false);
  });

  it("returns false for a JSON boolean string", () => {
    expect(looksLikeMarkdown("true")).toBe(false);
  });

  it("returns false for a JSON null string", () => {
    expect(looksLikeMarkdown("null")).toBe(false);
  });

  it("returns true for plain text", () => {
    expect(looksLikeMarkdown("Result: success")).toBe(true);
  });

  it("returns true for markdown with headings", () => {
    expect(looksLikeMarkdown("# Heading\n\nSome content here.")).toBe(true);
  });

  it("returns true for multi-line text output", () => {
    expect(looksLikeMarkdown("Line one\nLine two\nLine three")).toBe(true);
  });

  it("returns true for an empty string (not valid JSON)", () => {
    expect(looksLikeMarkdown("")).toBe(true);
  });
});

describe("needsTruncation", () => {
  it("returns false for short content", () => {
    expect(needsTruncation("a\nb\nc")).toBe(false);
  });

  it("returns false for exactly 200 lines", () => {
    const content = Array.from({ length: 200 }, (_, i) => `line ${i}`).join(
      "\n",
    );
    expect(needsTruncation(content)).toBe(false);
  });

  it("returns true for content over 200 lines", () => {
    const content = Array.from({ length: 201 }, (_, i) => `line ${i}`).join(
      "\n",
    );
    expect(needsTruncation(content)).toBe(true);
  });

  it("returns true for content over 8KB regardless of line count", () => {
    const content = "a".repeat(8193);
    expect(needsTruncation(content)).toBe(true);
  });

  it("returns false for content exactly at 8KB limit with few lines", () => {
    const content = "a".repeat(8192);
    expect(needsTruncation(content)).toBe(false);
  });
});
