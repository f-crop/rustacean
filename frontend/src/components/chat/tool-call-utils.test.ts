import { describe, it, expect } from "vitest";
import { selectPrismStyle } from "./tool-call-utils";

// Dynamic imports so the test values are guaranteed-same object references as the implementation.
const { oneDark } = await import("react-syntax-highlighter/dist/esm/styles/prism");
const { oneLight } = await import("react-syntax-highlighter/dist/esm/styles/prism");

describe("selectPrismStyle", () => {
  it("returns oneDark for dark theme", () => {
    expect(selectPrismStyle("dark")).toBe(oneDark);
  });

  it("returns oneLight for light theme", () => {
    expect(selectPrismStyle("light")).toBe(oneLight);
  });
});
