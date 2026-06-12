import { describe, it, expect } from "vitest";
import {
  deepDecodeJsonStrings,
  looksLikeMarkdown,
  needsTruncation,
} from "./tool-call-utils";

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

describe("deepDecodeJsonStrings", () => {
  it("(a) decodes board reproducer payload — text field expands to inner array", () => {
    const innerArray = [{ crate_name: "src_legacy_monad", version: "0.1.0" }];
    const input = [{ text: JSON.stringify(innerArray), type: "text" }];
    const result = deepDecodeJsonStrings(input);
    expect(result).toEqual([{ text: innerArray, type: "text" }]);
  });

  it("(b) leaves numeric-looking string fields intact", () => {
    const input = { count: "42", flag: "true", label: "hello" };
    expect(deepDecodeJsonStrings(input)).toEqual(input);
  });

  it("(c) decodes doubly-encoded JSON through both layers", () => {
    // layer1: inner object
    const inner = { deepest: 1 };
    // layer2: inner JSON string nested inside an array of objects
    const layer1Str = JSON.stringify(inner);              // '{"deepest":1}'  — starts with {
    const midArray = [{ nested: layer1Str }];             // [{ nested: '{"deepest":1}' }]
    const layer2Str = JSON.stringify(midArray);           // '[{"nested":"..."}]' — starts with [
    const input = { payload: layer2Str };
    // Expected: both layers decoded
    expect(deepDecodeJsonStrings(input)).toEqual({
      payload: [{ nested: inner }],
    });
  });

  it("(d) leaves malformed JSON strings intact", () => {
    const input = { bad: "{not json" };
    expect(deepDecodeJsonStrings(input)).toEqual(input);
  });

  it("decodes a top-level stringified empty array", () => {
    expect(deepDecodeJsonStrings("[]")).toEqual([]);
  });

  it("decodes a top-level stringified empty object", () => {
    expect(deepDecodeJsonStrings("{}")).toEqual({});
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
