use std::env;
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;

/// Service configuration loaded from environment variables.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    pub listen_addr: String,
    pub database_url: String,
    pub cors_origins: Vec<String>,
    pub base_url: String,
    pub session_ttl_days: i64,
    pub argon2_memory_kb: u32,
    pub argon2_time_cost: u32,
    pub argon2_parallelism: u32,
    pub email_transport: String,
    pub service_name: String,
    /// Whether to set the `Secure` flag on `rb_session` cookies.
    /// Set `RB_SECURE_COOKIES=false` when running behind an HTTP proxy in development.
    pub secure_cookies: bool,

    // --- GitHub App (REQ-GH-01) ---
    /// `RB_GH_APP_ID` — numeric GitHub App ID. Optional; GitHub routes
    /// return 503 when absent.
    pub gh_app_id: Option<i64>,
    /// `RB_GH_APP_PRIVATE_KEY` — base64-encoded RSA PEM private key.
    pub gh_app_private_key_b64: Option<String>,
    /// `RB_GH_APP_WEBHOOK_SECRET` — shared secret for HMAC-SHA256 webhook
    /// signature verification.
    pub gh_app_webhook_secret: Option<String>,
    /// `RB_GH_APP_ENC_KEY` — base64 of a 32-byte AES-256-GCM key used to
    /// encrypt App credentials in `control.github_app_config`. Optional in
    /// Phase 1 (no rows yet); becomes required in Phase 2 once the per-request
    /// loader queries the table.
    pub gh_app_enc_key_b64: Option<String>,
    /// `RB_GH_API_BASE` — GitHub REST API base URL. Defaults to
    /// `https://api.github.com`. Override in integration tests to point at
    /// a wiremock stub.
    pub gh_api_base: String,

    // --- Neo4j (REQ-DP-04, REQ-DP-07) ---
    /// `RB_NEO4J_URI` — bolt URI for the Neo4j instance (e.g. `bolt://neo4j:7687`).
    /// Optional; graph endpoints and Neo4j health probe return 503/unknown when absent.
    pub neo4j_uri: Option<String>,
    /// `RB_NEO4J_USER` — Neo4j username.  Defaults to `"neo4j"` when URI is set.
    pub neo4j_user: String,
    /// `RB_NEO4J_PASSWORD` — Neo4j password.
    pub neo4j_password: Option<String>,

    // --- Kafka / SSE (REQ-DP-08) ---
    /// `KAFKA_BOOTSTRAP_SERVERS` — broker list for the ingest consumer.
    /// Defaults to `kafka:9092` (dev compose alias).
    pub kafka_bootstrap_servers: String,
    /// `RB_DEV_TEST_ROUTES=1` — enable `POST /v1/ingest/test-publish` route.
    /// Must not be set in production.
    pub dev_test_routes: bool,

    /// `RB_MIGRATIONS_ROOT` — directory that contains `tenant/` and `control/`
    /// migration sub-directories. When set, tenant migrations are applied
    /// automatically after a new tenant schema is created during signup.
    /// Defaults to `/migrations` (the standard mount point in Docker).
    /// Set to `None` (env var absent) to disable automatic tenant migration.
    pub migrations_root: Option<PathBuf>,

    // --- Semantic search / Health probes (REQ-DP-01, REQ-DP-07) ---
    /// `RB_QDRANT_URL` — Qdrant REST base URL (e.g. `http://qdrant:6333`).
    /// Optional; `POST /v1/search` and Qdrant health probe return 503/unknown when absent.
    pub qdrant_url: Option<String>,
    /// `RB_OLLAMA_URL` — Ollama HTTP base URL (e.g. `http://ollama:11434`).
    /// Optional; `POST /v1/search` returns 503 when absent.
    pub ollama_url: Option<String>,
    /// `RB_EMBEDDING_MODEL` — Ollama model used to embed search queries.
    /// Must match the model used by `embed-worker`. Defaults to `nomic-embed-text`.
    pub embedding_model: String,

    pub internal_secret: Option<String>,
    pub internal_listen_addr: String,

    // --- Agent session rate limiting (REQ-MC-02) ---
    /// `RB_SESSION_CREATE_RATE_LIMIT` — max session creates per tenant per window.
    /// Defaults to 10.
    pub session_create_rate_limit: usize,
    /// `RB_SESSION_CREATE_WINDOW_SECS` — sliding window size in seconds.
    /// Defaults to 60.
    pub session_create_window_secs: u64,
    /// `RB_TENANT_SESSION_CAP` — max concurrent active sessions per tenant.
    /// Defaults to 100.
    pub tenant_session_cap: usize,

    // --- Chat panel (ADR-013 §2) ---
    /// `RB_CHAT_PANEL_ENABLED` — enables all `/v1/chat/*` routes. Default false.
    pub chat_panel_enabled: bool,

    // --- MCP JWT (ADR-013 §5.2) ---
    /// `RB_MCP_JWT_SECRET` — HS256 signing secret for short-lived MCP session
    /// tokens. Optional; chat routes requiring JWT mint return 503 when absent.
    pub mcp_jwt_secret: Option<String>,
    /// `RB_MCP_JWT_TTL_SECS` — token lifetime in seconds (default 900 = 15 min).
    pub mcp_jwt_ttl_secs: u64,

    // --- LLM key (ADR-013 §2, Q4 board decision) ---
    /// `RB_LLM_API_KEY` — company-level LLM provider API key.
    /// Loaded via `rb-secrets::from_env("RB")` at the call site.
    /// This field stores the resolved value; it is never logged (`SecretValue`).
    pub llm_api_key: Option<String>,

    // --- Hybrid search (ADR-014 §5, Wave 10 S2) ---
    /// `RB_HYBRID_SEARCH_ENABLED` — enables hybrid (FTS + RRF) retrieval at
    /// `POST /v1/search` and the `search_items` MCP tool. Default **off**.
    /// Set to `true` only after migration 007 has been applied and S6 eval clears.
    pub hybrid_search_enabled: bool,

    // --- Multi-query rewrite (ADR-014 §6, Wave 10 S5) ---
    /// `RB_MULTI_QUERY_N` — global default number of query variants (1 = off, max 3).
    /// Per-tenant `multi_query_n` in `tenant_query_settings` overrides this value;
    /// `multi_query_force_off` always wins over both. Default **1** (disabled in v1).
    pub multi_query_n: u32,

    // --- Retrieval cost ceilings (ADR-014 §9, Wave 10 S7) ---
    /// `RB_RERANK_ENABLED` — enables the local cross-encoder reranker after hybrid
    /// retrieval. Default **off**. Requires `RB_RERANK_MODEL_DIR` to point at a
    /// downloaded `bge-reranker-base` model directory.
    pub rerank_enabled: bool,
    /// `RB_RERANK_MODEL_DIR` — filesystem path to the ONNX cross-encoder model.
    /// Default `/models/rerank`.
    pub rerank_model_dir: std::path::PathBuf,
    /// `RB_RERANK_CANDIDATE_CAP` — maximum candidates passed to the cross-encoder
    /// reranker. Requests exceeding the cap are clamped (warning + counter). Default **50**.
    pub rerank_candidate_cap: u32,
    /// `RB_LLM_TOKEN_CEILING_PER_TENANT` — per-tenant token ceiling for LLM-rewrite
    /// and LLM-rerank operations. **0 means disabled (zero outbound LLM cost)**.
    /// Over-ceiling calls short-circuit and emit `llm_budget_exceeded_total{tenant}`.
    /// Default **0**.
    pub llm_token_ceiling_per_tenant: u32,

    // --- Admin bootstrap (REQ-AD-01, ADR-012 §S1) ---
    /// `RB_ADMIN_TOKEN` — shared bearer secret that gates all `/api/admin/v1/*`
    /// endpoints. Optional at boot so the service starts without it; admin
    /// routes return 503 when unset.
    pub admin_token: Option<String>,

    // --- Trace ID surfacing (REQ-TR-03) ---
    /// `RB_TEMPO_BASE_URL` — Grafana base URL used to construct deep-link redirects
    /// from `GET /v1/traces/:trace_id`. Defaults to `http://localhost:3000`.
    pub tempo_base_url: String,
}

impl Config {
    /// Loads configuration from environment variables.
    ///
    /// # Errors
    ///
    /// Returns an error if `RB_DATABASE_URL` is absent or if any numeric
    /// environment variable cannot be parsed.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            listen_addr: env::var("RB_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_owned()),
            database_url: env::var("RB_DATABASE_URL").context("RB_DATABASE_URL is required")?,
            cors_origins: env::var("RB_CORS_ORIGINS")
                .unwrap_or_else(|_| "http://localhost:15173".to_owned())
                .split(',')
                .map(|s| s.trim().to_owned())
                .collect(),
            base_url: env::var("RB_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:8080".to_owned()),
            session_ttl_days: env::var("RB_SESSION_TTL_DAYS")
                .unwrap_or_else(|_| "30".to_owned())
                .parse()
                .context("RB_SESSION_TTL_DAYS must be a positive integer")?,
            argon2_memory_kb: env::var("RB_ARGON2_MEMORY_KB")
                .unwrap_or_else(|_| "19456".to_owned())
                .parse()
                .context("RB_ARGON2_MEMORY_KB must be a positive integer")?,
            argon2_time_cost: env::var("RB_ARGON2_TIME_COST")
                .unwrap_or_else(|_| "2".to_owned())
                .parse()
                .context("RB_ARGON2_TIME_COST must be a positive integer")?,
            argon2_parallelism: env::var("RB_ARGON2_PARALLELISM")
                .unwrap_or_else(|_| "1".to_owned())
                .parse()
                .context("RB_ARGON2_PARALLELISM must be a positive integer")?,
            email_transport: env::var("RB_EMAIL_TRANSPORT")
                .unwrap_or_else(|_| "console".to_owned()),
            service_name: env::var("OTEL_SERVICE_NAME")
                .unwrap_or_else(|_| "control-api".to_owned()),
            secure_cookies: env::var("RB_SECURE_COOKIES")
                .map_or(true, |v| !v.eq_ignore_ascii_case("false")),
            gh_app_id: env::var("RB_GH_APP_ID")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| s.parse::<i64>())
                .transpose()
                .context("RB_GH_APP_ID must be a positive integer")?,
            gh_app_private_key_b64: env::var("RB_GH_APP_PRIVATE_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
            gh_app_webhook_secret: env::var("RB_GH_APP_WEBHOOK_SECRET")
                .ok()
                .filter(|s| !s.is_empty()),
            gh_app_enc_key_b64: env::var("RB_GH_APP_ENC_KEY").ok().filter(|s| !s.is_empty()),
            gh_api_base: env::var("RB_GH_API_BASE")
                .unwrap_or_else(|_| rb_github::DEFAULT_GITHUB_API_BASE.to_owned()),
            neo4j_uri: env::var("RB_NEO4J_URI").ok().filter(|s| !s.is_empty()),
            neo4j_user: env::var("RB_NEO4J_USER").unwrap_or_else(|_| "neo4j".to_owned()),
            neo4j_password: env::var("RB_NEO4J_PASSWORD").ok().filter(|s| !s.is_empty()),
            kafka_bootstrap_servers: env::var("KAFKA_BOOTSTRAP_SERVERS")
                .unwrap_or_else(|_| "kafka:9092".to_owned()),
            dev_test_routes: env::var("RB_DEV_TEST_ROUTES")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            migrations_root: env::var("RB_MIGRATIONS_ROOT").ok().map(PathBuf::from),
            qdrant_url: env::var("RB_QDRANT_URL").ok().filter(|s| !s.is_empty()),
            ollama_url: env::var("RB_OLLAMA_URL").ok().filter(|s| !s.is_empty()),
            embedding_model: env::var("RB_EMBEDDING_MODEL")
                .unwrap_or_else(|_| "nomic-embed-text".to_owned()),
            internal_secret: env::var("RB_INTERNAL_SECRET")
                .ok()
                .filter(|s| !s.is_empty()),
            internal_listen_addr: env::var("RB_INTERNAL_LISTEN_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:8081".to_owned()),
            session_create_rate_limit: env::var("RB_SESSION_CREATE_RATE_LIMIT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10),
            session_create_window_secs: env::var("RB_SESSION_CREATE_WINDOW_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(60),
            tenant_session_cap: env::var("RB_TENANT_SESSION_CAP")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100),
            admin_token: env::var("RB_ADMIN_TOKEN").ok().filter(|s| !s.is_empty()),
            tempo_base_url: env::var("RB_TEMPO_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:3000".to_owned()),
            chat_panel_enabled: env::var("RB_CHAT_PANEL_ENABLED")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            mcp_jwt_secret: env::var("RB_MCP_JWT_SECRET").ok().filter(|s| !s.is_empty()),
            mcp_jwt_ttl_secs: env::var("RB_MCP_JWT_TTL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(900),
            llm_api_key: env::var("RB_LLM_API_KEY").ok().filter(|s| !s.is_empty()),
            hybrid_search_enabled: env::var("RB_HYBRID_SEARCH_ENABLED")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            multi_query_n: env::var("RB_MULTI_QUERY_N")
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(1)
                .min(rb_query::MAX_MULTI_QUERY_N),
            rerank_enabled: env::var("RB_RERANK_ENABLED")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            rerank_model_dir: env::var("RB_RERANK_MODEL_DIR").map_or_else(
                |_| std::path::PathBuf::from("/models/rerank"),
                std::path::PathBuf::from,
            ),
            rerank_candidate_cap: env::var("RB_RERANK_CANDIDATE_CAP")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(50),
            llm_token_ceiling_per_tenant: env::var("RB_LLM_TOKEN_CEILING_PER_TENANT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        })
    }

    /// Validates critical config invariants that could produce silent runtime misbehaviour.
    ///
    /// Called immediately after `from_env()`. The service refuses to bind if the environment
    /// is misconfigured.
    ///
    /// # Errors
    ///
    /// Returns an error if `RB_BASE_URL` is not an HTTP/S URL or appears to point at the API
    /// rather than the frontend, or if `RB_GH_APP_PRIVATE_KEY` contains non-base64 characters.
    pub fn validate(&self) -> Result<()> {
        let mut errors: Vec<String> = Vec::new();

        // RB_BASE_URL must be an HTTP/S URL and must NOT be the same host:port as the
        // API listen address — it feeds email links and GH callback redirects to the
        // *frontend*, not the API.
        if !self.base_url.starts_with("http://") && !self.base_url.starts_with("https://") {
            errors.push(format!(
                "RB_BASE_URL={:?}: must start with http:// or https://",
                self.base_url
            ));
        } else if self.base_url.contains(":8080") {
            // :8080 is the API listen address — RB_BASE_URL must point at the frontend.
            // Allow the local dev default only when email is non-sending (console/noop).
            let is_local =
                self.base_url.contains("localhost") || self.base_url.contains("127.0.0.1");
            if !is_local || !matches!(self.email_transport.as_str(), "console" | "noop") {
                errors.push(format!(
                    "RB_BASE_URL={:?}: looks like the API address (:8080), not the frontend. \
                     This will break email links and the GitHub install callback redirect. \
                     Set RB_BASE_URL to the frontend origin (e.g. http://host:15173).",
                    self.base_url
                ));
            }
        }

        // RB_GH_APP_PRIVATE_KEY must be valid base64 when present.
        if let Some(key) = &self.gh_app_private_key_b64 {
            if key.contains("BEGIN RSA") || key.contains("-----") {
                errors.push(
                    "RB_GH_APP_PRIVATE_KEY: value looks like a raw PEM, not base64. \
                     Encode it first: base64 -w0 < app.pem"
                        .to_owned(),
                );
            } else {
                // Verify it's valid base64 by attempting a decode check on the first 128 chars
                let sample = &key[..key.len().min(128)];
                if !sample
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
                {
                    errors.push(format!(
                        "RB_GH_APP_PRIVATE_KEY={:?}...: contains non-base64 characters",
                        &key[..key.len().min(20)]
                    ));
                }
            }
        }

        // RB_GH_APP_ENC_KEY must decode to exactly 32 bytes when present. We
        // validate the shape here so a misconfigured deployment fails fast at
        // boot rather than at first manifest exchange.
        if let Some(key) = self
            .gh_app_enc_key_b64
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match base64::engine::general_purpose::STANDARD.decode(key) {
                Ok(bytes) if bytes.len() == 32 => {}
                Ok(bytes) => errors.push(format!(
                    "RB_GH_APP_ENC_KEY must decode to exactly 32 bytes, got {}",
                    bytes.len()
                )),
                Err(e) => errors.push(format!("RB_GH_APP_ENC_KEY is not valid base64: {e}")),
            }
        }

        // RB_MCP_JWT_SECRET must be at least 32 bytes when set.
        // HS256 requires a 256-bit key; shorter secrets weaken the HMAC guarantee.
        if let Some(secret) = &self.mcp_jwt_secret {
            if secret.len() < 32 {
                errors.push(format!(
                    "RB_MCP_JWT_SECRET is {} bytes; HS256 requires at least 32 bytes (256 bits). \
                     Use `openssl rand -hex 32` to generate a suitable value.",
                    secret.len()
                ));
            }
        }

        // RB_INTERNAL_SECRET must be non-empty when internal routes are used.
        // The internal routes are always compiled in, so require the secret.
        if self.internal_secret.as_ref().is_none_or(String::is_empty) {
            errors.push(
                "RB_INTERNAL_SECRET is required and must be non-empty. \
                 Set a strong shared secret for agent-runner callbacks."
                    .to_owned(),
            );
        }

        if !errors.is_empty() {
            bail!(
                "control-api boot validation failed ({} error(s)):\n{}",
                errors.len(),
                errors
                    .iter()
                    .map(|e| format!("  - {e}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
        Ok(())
    }

    /// Creates a minimal config for tests and integration-test harnesses.
    ///
    /// Uses fast argon2id params and noop email transport.
    #[doc(hidden)]
    #[must_use]
    pub fn for_test() -> Self {
        Self {
            listen_addr: "127.0.0.1:0".to_owned(),
            database_url: "postgres://localhost/test".to_owned(),
            cors_origins: vec!["http://localhost:15173".to_owned()],
            base_url: "http://localhost:8080".to_owned(),
            session_ttl_days: 30,
            argon2_memory_kb: 64,
            argon2_time_cost: 1,
            argon2_parallelism: 1,
            email_transport: "noop".to_owned(),
            service_name: "control-api-test".to_owned(),
            secure_cookies: true,
            gh_app_id: None,
            gh_app_private_key_b64: None,
            gh_app_webhook_secret: None,
            gh_app_enc_key_b64: None,
            gh_api_base: rb_github::DEFAULT_GITHUB_API_BASE.to_owned(),
            neo4j_uri: None,
            neo4j_user: "neo4j".to_owned(),
            neo4j_password: None,
            kafka_bootstrap_servers: "localhost:9092".to_owned(),
            dev_test_routes: false,
            migrations_root: None,
            qdrant_url: None,
            ollama_url: None,
            embedding_model: "nomic-embed-text".to_owned(),
            internal_secret: None,
            internal_listen_addr: "127.0.0.1:0".to_owned(),
            session_create_rate_limit: 10,
            session_create_window_secs: 60,
            tenant_session_cap: 100,
            admin_token: None,
            tempo_base_url: "http://localhost:3000".to_owned(),
            chat_panel_enabled: false,
            mcp_jwt_secret: Some("test-mcp-jwt-secret-for-unit-tests-only".to_owned()),
            mcp_jwt_ttl_secs: 900,
            llm_api_key: None,
            hybrid_search_enabled: false,
            multi_query_n: 1,
            rerank_enabled: false,
            rerank_model_dir: std::path::PathBuf::from("/models/rerank"),
            rerank_candidate_cap: 50,
            llm_token_ceiling_per_tenant: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        let mut cfg = Config::for_test();
        cfg.internal_secret = Some("test-internal-secret-for-validation".to_owned());
        cfg
    }

    #[test]
    fn validate_localhost_8080_noop_passes() {
        // Local dev default — non-sending transport makes this acceptable.
        let c = base(); // base_url = http://localhost:8080, email_transport = noop
        assert!(c.validate().is_ok());
    }

    #[test]
    fn validate_localhost_8080_smtp_fails() {
        let mut c = base();
        c.email_transport = "smtp".to_owned();
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_non_localhost_8080_fails() {
        // e.g. http://mars.tailnet:8080 — always wrong regardless of transport.
        let mut c = base();
        c.base_url = "http://mars.tailnet:8080".to_owned();
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_frontend_url_passes() {
        let mut c = base();
        c.base_url = "http://localhost:15173".to_owned();
        assert!(c.validate().is_ok());
    }

    #[test]
    fn validate_non_http_scheme_fails() {
        let mut c = base();
        c.base_url = "ftp://localhost:15173".to_owned();
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_gh_app_enc_key_absent_passes() {
        // RB_GH_APP_ENC_KEY is optional in Phase 1 — env-only deployments must
        // continue to validate.
        let mut c = base();
        c.gh_app_enc_key_b64 = None;
        assert!(c.validate().is_ok());
    }

    #[test]
    fn validate_gh_app_enc_key_correct_length_passes() {
        let mut c = base();
        c.gh_app_enc_key_b64 = Some(base64::engine::general_purpose::STANDARD.encode([0u8; 32]));
        assert!(c.validate().is_ok());
    }

    #[test]
    fn validate_gh_app_enc_key_wrong_length_fails() {
        let mut c = base();
        c.gh_app_enc_key_b64 = Some(base64::engine::general_purpose::STANDARD.encode([0u8; 16]));
        let err = c.validate().expect_err("must reject 16-byte key");
        assert!(err.to_string().contains("32 bytes"));
    }

    #[test]
    fn validate_gh_app_enc_key_garbage_fails() {
        let mut c = base();
        c.gh_app_enc_key_b64 = Some("not base64 !!!".to_owned());
        let err = c.validate().expect_err("must reject malformed base64");
        assert!(err.to_string().contains("base64"));
    }

    #[test]
    fn validate_mcp_jwt_secret_too_short_fails() {
        let mut c = base();
        c.mcp_jwt_secret = Some("tooshort".to_owned());
        let err = c.validate().expect_err("must reject <32-byte secret");
        assert!(err.to_string().contains("32 bytes"));
    }

    #[test]
    fn validate_mcp_jwt_secret_sufficient_length_passes() {
        let mut c = base();
        c.mcp_jwt_secret = Some("a".repeat(32));
        assert!(c.validate().is_ok());
    }

    #[test]
    fn validate_mcp_jwt_secret_absent_passes() {
        let mut c = base();
        c.mcp_jwt_secret = None;
        assert!(c.validate().is_ok());
    }
}
