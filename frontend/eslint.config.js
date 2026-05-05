// REQ-FE-10: ban raw fetch() calls outside the typed openapi-fetch client.
import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";

export default tseslint.config(
  {
    ignores: ["dist", "node_modules", "src/api/generated/**"],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": [
        "warn",
        { allowConstantExport: true },
      ],
      "no-restricted-globals": [
        "error",
        {
          name: "fetch",
          message:
            "Use the typed apiClient from @/api instead of raw fetch — REQ-FE-10.",
        },
      ],
      "no-restricted-properties": [
        "error",
        {
          object: "window",
          property: "fetch",
          message:
            "Use the typed apiClient from @/api instead of window.fetch — REQ-FE-10.",
        },
      ],
    },
  },
  {
    files: ["src/api/client.ts", "src/api/tempo.ts"],
    rules: {
      // openapi-fetch wraps fetch internally (client.ts); Tempo is an external
      // observability backend with no OpenAPI spec (tempo.ts) — raw fetch allowed.
      "no-restricted-globals": "off",
      "no-restricted-properties": "off",
    },
  },
);
