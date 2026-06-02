// Feature flag gate for the chat panel.
// Returns true when the tenant has the rb-feature-resolver flag RB_CHAT_PANEL_ENABLED.
//
// Current implementation: reads VITE_FEATURE_CHAT_PANEL env var (for local dev and
// QA flag-on testing). When S5 ships rb-feature-resolver, replace with a
// /v1/features API call that checks the tenant's flag state.
export function useChatFeatureEnabled(): boolean {
  return import.meta.env.VITE_FEATURE_CHAT_PANEL === "true";
}
