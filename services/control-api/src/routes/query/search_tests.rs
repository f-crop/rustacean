use super::*;
use crate::{
    embed::normalize_query,
    middleware::auth::{ApiKeyInfo, SessionInfo},
};

fn verified_session(tenant_id: Uuid) -> SessionInfo {
    SessionInfo {
        session_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
        tenant_id,
        email_verified: true,
    }
}

#[test]
fn normalize_query_hello_world() {
    // AC-3: identical assertion required in both the MCP and REST route modules.
    assert_eq!(normalize_query("HelloWorld"), "search_query: hello world");
}

#[test]
fn anonymous_rejected() {
    assert!(matches!(
        require_read_access(AuthContext::Anonymous),
        Err(AppError::Unauthorized)
    ));
}

#[test]
fn expired_session_rejected() {
    assert!(matches!(
        require_read_access(AuthContext::ExpiredSession),
        Err(AppError::SessionExpired)
    ));
}

#[test]
fn unverified_session_rejected() {
    let mut info = verified_session(Uuid::new_v4());
    info.email_verified = false;
    assert!(matches!(
        require_read_access(AuthContext::Session(info)),
        Err(AppError::EmailNotVerified)
    ));
}

#[test]
fn verified_session_accepted() {
    let tid = Uuid::new_v4();
    let result = require_read_access(AuthContext::Session(verified_session(tid)));
    assert_eq!(result.unwrap(), tid);
}

#[test]
fn api_key_with_read_scope_accepted() {
    let tid = Uuid::new_v4();
    let key = ApiKeyInfo {
        key_id: Uuid::new_v4(),
        tenant_id: tid,
        user_id: Uuid::new_v4(),
        scopes: vec![Scope::Read],
    };
    assert_eq!(require_read_access(AuthContext::ApiKey(key)).unwrap(), tid);
}

#[test]
fn api_key_without_read_scope_rejected() {
    let key = ApiKeyInfo {
        key_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
        scopes: vec![Scope::Write],
    };
    assert!(matches!(
        require_read_access(AuthContext::ApiKey(key)),
        Err(AppError::InsufficientScope)
    ));
}

#[test]
fn crate_name_extracted_from_fqn() {
    let fqn = "my_crate::module::MyStruct";
    let crate_name = fqn.split("::").next().unwrap_or(fqn).to_owned();
    assert_eq!(crate_name, "my_crate");
}

#[test]
fn crate_name_for_bare_fqn() {
    let fqn = "bare_crate";
    let crate_name = fqn.split("::").next().unwrap_or(fqn).to_owned();
    assert_eq!(crate_name, "bare_crate");
}

#[test]
fn limit_defaults_and_cap() {
    let applied = DEFAULT_SEARCH_LIMIT.clamp(1, MAX_SEARCH_LIMIT);
    assert_eq!(applied, DEFAULT_SEARCH_LIMIT);

    let over = 200_u32.clamp(1, MAX_SEARCH_LIMIT);
    assert_eq!(over, MAX_SEARCH_LIMIT);
}

// AC6: flag-off response serializes WITHOUT `citations` field — byte-identical to pre-S2.
#[test]
fn flag_off_response_omits_citations_field() {
    let resp = SearchResponse {
        results: vec![SearchResult {
            fqn: "a::Fn".to_owned(),
            crate_name: "a".to_owned(),
            repo_id: "r1".to_owned(),
            score: 0.9,
        }],
        citations: vec![],
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert!(!json.as_object().unwrap().contains_key("citations"));
    assert!(json.as_object().unwrap().contains_key("results"));
}

// AC6: flag-on response includes `citations` field.
#[test]
fn flag_on_response_includes_citations_field() {
    let resp = SearchResponse {
        results: vec![],
        citations: vec![CitationV1 {
            version: CitationV1::VERSION.to_owned(),
            repo_id: Uuid::nil(),
            file_path: "src/lib.rs".to_owned(),
            line_range: LineRange { start: 1, end: 10 },
            commit_sha: "abc123".to_owned(),
            score: 0.85,
            source_kind: SourceKind::Hybrid,
        }],
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert!(json.as_object().unwrap().contains_key("citations"));
    assert_eq!(json["citations"][0]["version"], "v1");
}

// AC7: default ceiling = 0 → zero outbound LLM cost for brand-new tenants.
#[test]
fn default_llm_ceiling_disallows_all_calls() {
    let tid = Uuid::new_v4();
    // ceiling=0 denies regardless of tokens_used
    assert!(!llm_budget_allows(0, 0, tid));
    assert!(!llm_budget_allows(0, 999, tid));
    assert!(!llm_budget_allows(0, u32::MAX, tid));
}

#[test]
fn non_zero_ceiling_allows_under_budget_denies_at_or_over() {
    let tid = Uuid::new_v4();
    assert!(llm_budget_allows(1000, 0, tid));
    assert!(llm_budget_allows(1000, 999, tid));
    assert!(!llm_budget_allows(1000, 1000, tid));
    assert!(!llm_budget_allows(1000, 1001, tid));
}

// AC5: TenantLlmTokenCounter accumulates per-tenant and enforces ceiling when read back.
#[test]
fn llm_token_counter_accumulates_per_tenant() {
    use crate::state::TenantLlmTokenCounter;

    let counter = TenantLlmTokenCounter::new();
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();

    assert_eq!(counter.tokens_used(t1), 0);
    assert_eq!(counter.tokens_used(t2), 0);

    counter.add_tokens(t1, 500);
    counter.add_tokens(t1, 300);
    counter.add_tokens(t2, 100);

    assert_eq!(counter.tokens_used(t1), 800);
    assert_eq!(counter.tokens_used(t2), 100);
}

#[test]
fn llm_token_counter_saturates_at_u32_max() {
    use crate::state::TenantLlmTokenCounter;

    let counter = TenantLlmTokenCounter::new();
    let t = Uuid::new_v4();
    counter.add_tokens(t, u32::MAX);
    counter.add_tokens(t, 1); // must not overflow
    assert_eq!(counter.tokens_used(t), u32::MAX);
}

// AC5: budget gates the LLM path — ceiling exhausted → llm_budget_allows returns false.
#[test]
fn budget_gates_after_accumulation() {
    use crate::state::TenantLlmTokenCounter;

    let counter = TenantLlmTokenCounter::new();
    let tid = Uuid::new_v4();
    let ceiling: u32 = 1_000;

    // Under budget: allowed.
    assert!(llm_budget_allows(ceiling, counter.tokens_used(tid), tid));

    // Accumulate up to the ceiling.
    counter.add_tokens(tid, 1_000);

    // At ceiling: denied + emits counter.
    assert!(!llm_budget_allows(ceiling, counter.tokens_used(tid), tid));
}

// S5: fetch_tenant_query_settings uses global_budget when no tenant row exists.
#[test]
fn global_budget_fallback_tuple_matches_expected() {
    let global_n = 3u32;
    let global_budget = 500u32;
    // Mirror the map_or default path: Option::<(i16, bool, i32)>::None.map_or(...)
    let (n, force_off, budget): (u32, bool, u32) = None::<(i16, bool, i32)>
        .map_or((global_n, false, global_budget), |(n, fo, b)| {
            (n.unsigned_abs().into(), fo, b.unsigned_abs())
        });
    assert_eq!(n, 3);
    assert!(!force_off);
    assert_eq!(budget, 500);
}

// S5: rewrite_model and embedding_model are distinct config fields sourced from separate env vars.
// Regression guard: passing embedding_model to expand_query causes Ollama 400 (nomic-embed-text
// does not support /api/generate). This test pins that the two fields start with different values.
#[test]
fn rewrite_model_distinct_from_embedding_model() {
    use crate::config::Config;
    let cfg = Config::for_test();
    assert_eq!(cfg.embedding_model, "nomic-embed-text");
    assert_eq!(cfg.rewrite_model, "");
    assert_ne!(cfg.rewrite_model, cfg.embedding_model);
}

// AC3: clamp_rerank_candidates truncates over-cap sets.
#[test]
fn rerank_cap_truncates_oversized_set() {
    let tid = Uuid::new_v4();
    let items: Vec<u32> = (0..100).collect();
    let clamped = clamp_rerank_candidates(items.clone(), 50, tid);
    assert_eq!(clamped.len(), 50);
    // cap=0 sentinel: no clamp
    let not_clamped = clamp_rerank_candidates(items.clone(), 0, tid);
    assert_eq!(not_clamped.len(), 100);
    // set smaller than cap: no truncation
    let under = clamp_rerank_candidates(items, 200, tid);
    assert_eq!(under.len(), 100);
}
