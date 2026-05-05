use std::env;
use std::path::PathBuf;

use anyhow::{Context as _, Result};

/// Service configuration loaded from environment variables.
#[derive(Debug, Clone)]
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

    // --- Neo4j (REQ-DP-04) ---
    /// `RB_NEO4J_URI` — bolt URI for the Neo4j instance (e.g. `bolt://neo4j:7687`).
    /// Optional; graph endpoints return 503 when absent.
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
            neo4j_uri: env::var("RB_NEO4J_URI").ok().filter(|s| !s.is_empty()),
            neo4j_user: env::var("RB_NEO4J_USER").unwrap_or_else(|_| "neo4j".to_owned()),
            neo4j_password: env::var("RB_NEO4J_PASSWORD").ok().filter(|s| !s.is_empty()),
            kafka_bootstrap_servers: env::var("KAFKA_BOOTSTRAP_SERVERS")
                .unwrap_or_else(|_| "kafka:9092".to_owned()),
            dev_test_routes: env::var("RB_DEV_TEST_ROUTES")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            migrations_root: env::var("RB_MIGRATIONS_ROOT").ok().map(PathBuf::from),
        })
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
            neo4j_uri: None,
            neo4j_user: "neo4j".to_owned(),
            neo4j_password: None,
            kafka_bootstrap_servers: "localhost:9092".to_owned(),
            dev_test_routes: false,
            migrations_root: None,
        }
    }
}
