import { useState } from "react";
import { ChevronDown, ChevronUp } from "lucide-react";
import PrismLight from "react-syntax-highlighter/dist/esm/prism-light";
import typescript from "react-syntax-highlighter/dist/esm/languages/prism/typescript";
import javascript from "react-syntax-highlighter/dist/esm/languages/prism/javascript";
import python from "react-syntax-highlighter/dist/esm/languages/prism/python";
import rust from "react-syntax-highlighter/dist/esm/languages/prism/rust";
import bash from "react-syntax-highlighter/dist/esm/languages/prism/bash";
import json from "react-syntax-highlighter/dist/esm/languages/prism/json";
import tsx from "react-syntax-highlighter/dist/esm/languages/prism/tsx";
import { needsTruncation, selectPrismStyle } from "../tool-call-utils";
import { useTheme } from "@/components/theme/theme-context";

PrismLight.registerLanguage("typescript", typescript);
PrismLight.registerLanguage("ts", typescript);
PrismLight.registerLanguage("tsx", tsx);
PrismLight.registerLanguage("javascript", javascript);
PrismLight.registerLanguage("js", javascript);
PrismLight.registerLanguage("python", python);
PrismLight.registerLanguage("py", python);
PrismLight.registerLanguage("rust", rust);
PrismLight.registerLanguage("rs", rust);
PrismLight.registerLanguage("bash", bash);
PrismLight.registerLanguage("sh", bash);
PrismLight.registerLanguage("json", json);

const TRUNCATE_LINES = 200;

const EXT_LANGUAGE_MAP: Record<string, string> = {
  ts: "typescript",
  tsx: "tsx",
  js: "javascript",
  jsx: "javascript",
  py: "python",
  rs: "rust",
  sh: "bash",
  bash: "bash",
  json: "json",
  toml: "toml",
  yaml: "yaml",
  yml: "yaml",
  md: "markdown",
  html: "html",
  css: "css",
};

function detectLanguage(filePath: unknown): string {
  if (typeof filePath !== "string") return "text";
  const ext = filePath.split(".").pop()?.toLowerCase() ?? "";
  return EXT_LANGUAGE_MAP[ext] ?? "text";
}

interface ReadResultRendererProps {
  readonly result: unknown;
  readonly input?: unknown;
}

export function ReadResultRenderer({ result, input }: ReadResultRendererProps): JSX.Element {
  const text = typeof result === "string" ? result : JSON.stringify(result, null, 2);
  const truncate = needsTruncation(text);
  const [showMore, setShowMore] = useState(false);
  const lines = text.split("\n");
  const displayText = truncate && !showMore ? lines.slice(0, TRUNCATE_LINES).join("\n") : text;
  const { resolvedTheme } = useTheme();
  const prismStyle = selectPrismStyle(resolvedTheme);

  const filePath = input !== null && typeof input === "object" && !Array.isArray(input)
    ? (input as Record<string, unknown>)["file_path"]
    : undefined;
  const language = detectLanguage(filePath);

  return (
    <div className="overflow-hidden rounded bg-muted">
      <PrismLight
        language={language}
        style={prismStyle}
        PreTag="div"
        showLineNumbers
        customStyle={{ margin: 0, borderRadius: 0, fontSize: "0.75rem", lineHeight: "1.5" }}
        lineNumberStyle={{ minWidth: "2.5em", paddingRight: "1em", color: "#6b7280", userSelect: "none" }}
      >
        {displayText}
      </PrismLight>
      {truncate && (
        <button
          type="button"
          onClick={() => setShowMore((s) => !s)}
          className="flex w-full items-center justify-center gap-1 bg-accent py-1.5 text-xs text-muted-foreground hover:text-foreground"
        >
          {showMore ? (
            <><ChevronUp className="h-3.5 w-3.5" />Show less</>
          ) : (
            <><ChevronDown className="h-3.5 w-3.5" />Show more ({lines.length - TRUNCATE_LINES} more lines)</>
          )}
        </button>
      )}
    </div>
  );
}
