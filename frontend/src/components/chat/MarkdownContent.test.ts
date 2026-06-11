import { describe, it, expect } from "vitest";
import { defaultSchema } from "rehype-sanitize";

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
