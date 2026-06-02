/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_API_BASE_URL?: string;
  readonly VITE_TEMPO_URL?: string;
  readonly VITE_FEATURE_CHAT_PANEL?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

declare namespace JSX {
  type Element = import("react").ReactElement;
}
