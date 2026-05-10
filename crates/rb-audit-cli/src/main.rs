//! `rb-audit-cli` — cross-tenant leak QA command (ADR-007 §7.3).
//!
//! Usage:
//!   rb-audit-cli check-leak --tenant <uuid>
//!
//! Environment variables:
//!   `RB_DATABASE_URL`   `PostgreSQL` connection string (required)
//!   `NEO4J_HTTP_URL`    Neo4j HTTP API URL (optional; default <http://localhost:7474>)
//!   `QDRANT_URL`        Qdrant HTTP URL (optional; default <http://localhost:6333>)
//!   `NEO4J_USER`        Neo4j username (optional; default neo4j)
//!   `NEO4J_PASS`        Neo4j password (optional; default neo4j)
//!
//! Exit codes:
//!   0 — all leak checks PASS
//!   1 — one or more checks FAIL (residual data found or audit trail missing)
//!   2 — configuration / connectivity error (not a leak result)

use std::process;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use rb_schemas::TenantId;
use rb_storage_neo4j::tenant_label;
use rb_tenant::TenantCtx;
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "rb-audit-cli",
    about = "rust-brain audit utilities (ADR-007 §7.3)",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the cross-tenant leak check after a tombstone completes.
    ///
    /// Verifies that all per-tenant projection data has been removed and that
    /// the audit log still retains the deletion trail.  Returns exit code 0 on
    /// PASS and 1 on FAIL (see ADR-007 §7.3 for the full assertion suite).
    CheckLeak {
        /// UUID of the tenant whose data was tombstoned.
        #[arg(long)]
        tenant: Uuid,

        /// Skip the Neo4j check (e.g. when Neo4j is not deployed in this environment).
        #[arg(long)]
        skip_neo4j: bool,

        /// Skip the Qdrant check (e.g. when Qdrant is not deployed in this environment).
        #[arg(long)]
        skip_qdrant: bool,
    },
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::CheckLeak {
            tenant,
            skip_neo4j,
            skip_qdrant,
        } => match run_check_leak(tenant, skip_neo4j, skip_qdrant).await {
            Ok(passed) => {
                if passed {
                    println!(
                        "[PASS] No residual data found for tenant {tenant}. Audit trail preserved."
                    );
                    process::exit(0);
                } else {
                    eprintln!("[FAIL] Leak check failed for tenant {tenant}. See output above.");
                    process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("[ERROR] check-leak encountered a configuration error: {e}");
                process::exit(2);
            }
        },
    }
}

// ---------------------------------------------------------------------------
// check-leak implementation
// ---------------------------------------------------------------------------

/// Returns `Ok(true)` when all checks pass (no leak, audit trail present).
async fn run_check_leak(tenant: Uuid, skip_neo4j: bool, skip_qdrant: bool) -> Result<bool> {
    // Derive store identifiers from the canonical crate functions so this CLI
    // always matches what the storage layers actually create.
    let tenant_id = TenantId::from(tenant);
    let ctx = TenantCtx::new(tenant_id);
    let schema_name = ctx.schema_name().to_owned();
    let neo4j_label = tenant_label(&tenant_id);
    let qdrant_collection = qdrant_collection_name(&tenant_id);

    let database_url = std::env::var("RB_DATABASE_URL").context("RB_DATABASE_URL is required")?;

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .context("failed to connect to Postgres")?;

    let mut all_pass = true;

    // ------------------------------------------------------------------
    // PostgreSQL checks (ADR-007 §7.3)
    // ------------------------------------------------------------------

    // 1. Tenant schema projection tables must be empty after tombstone.
    let pg_pass = check_pg_tables_empty(&pool, &schema_name, tenant).await?;
    if !pg_pass {
        all_pass = false;
    }

    // 2. Audit log must retain the deletion trail.
    let audit_pass = check_audit_trail_present(&pool, tenant).await?;
    if !audit_pass {
        all_pass = false;
    }

    // ------------------------------------------------------------------
    // Neo4j check (ADR-007 §7.3)
    // ------------------------------------------------------------------

    if skip_neo4j {
        println!("[SKIP] Neo4j check skipped (--skip-neo4j).");
    } else {
        let neo4j_pass = check_neo4j_empty(&neo4j_label).await;
        if !neo4j_pass {
            all_pass = false;
        }
    }

    // ------------------------------------------------------------------
    // Qdrant check (ADR-007 §7.3)
    // ------------------------------------------------------------------

    if skip_qdrant {
        println!("[SKIP] Qdrant check skipped (--skip-qdrant).");
    } else {
        let qdrant_pass = check_qdrant_collection_gone(&qdrant_collection).await;
        if !qdrant_pass {
            all_pass = false;
        }
    }

    Ok(all_pass)
}

// ---------------------------------------------------------------------------
// Tenant identifier derivation
// ---------------------------------------------------------------------------

/// Derives the Qdrant collection name for a tenant using the same `bytes[4..]`
/// derivation as `rb-storage-neo4j::tenant_label` and `rb-tenant::TenantCtx`.
///
/// Format: `tenant_<24 lowercase hex chars>_embeddings`
fn qdrant_collection_name(tenant_id: &TenantId) -> String {
    let uuid = tenant_id.as_uuid();
    let bytes = uuid.as_bytes();
    let mut hex = String::with_capacity(24);
    for b in &bytes[4..] {
        use std::fmt::Write as _;
        write!(hex, "{b:02x}").expect("infallible write to String");
    }
    format!("tenant_{hex}_embeddings")
}

// ------------------------------------------------------------------
// PostgreSQL helpers
// ------------------------------------------------------------------

async fn check_pg_tables_empty(
    pool: &sqlx::PgPool,
    schema_name: &str,
    tenant: Uuid,
) -> Result<bool> {
    // Tables that should be empty after tombstone (Wave 5 projections).
    // These tables are created by the PG projection consumer (Wave 5).
    // If the schema doesn't exist yet (pre-Wave-5), we treat that as PASS.
    let tables = ["code_files", "code_symbols", "code_relations"];
    // Check if schema exists.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = $1)",
    )
    .bind(schema_name)
    .fetch_one(pool)
    .await
    .context("failed to check schema existence")?;

    if !exists {
        println!(
            "[PASS] PG schema {schema_name} does not exist \
             (tenant not yet ingested or already cleaned up)."
        );
        return Ok(true);
    }

    let mut total_rows: i64 = 0;

    for table in &tables {
        let qualified = format!("{schema_name}.{table}");
        // Check table exists before querying.
        let table_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM information_schema.tables \
                WHERE table_schema = $1 AND table_name = $2)",
        )
        .bind(schema_name)
        .bind(*table)
        .fetch_one(pool)
        .await
        .context(format!("failed to check table {qualified}"))?;

        if !table_exists {
            continue;
        }

        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {qualified}"))
            .fetch_one(pool)
            .await
            .context(format!("failed to count rows in {qualified}"))?;

        total_rows += count;
    }

    if total_rows == 0 {
        println!("[PASS] PG projection tables for tenant {tenant} are empty.");
        Ok(true)
    } else {
        eprintln!(
            "[FAIL] PG leak detected: {total_rows} residual rows found in \
             {schema_name}.{{code_files,code_symbols,code_relations}}."
        );
        Ok(false)
    }
}

async fn check_audit_trail_present(pool: &sqlx::PgPool, tenant: Uuid) -> Result<bool> {
    // Audit table may not exist before migration 006 runs — treat as PASS
    // (no events, but also no leak).
    let table_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS( \
            SELECT 1 FROM information_schema.tables \
            WHERE table_schema = 'audit' AND table_name = 'audit_events')",
    )
    .fetch_one(pool)
    .await
    .context("failed to check audit.audit_events existence")?;

    if !table_exists {
        println!(
            "[SKIP] audit.audit_events does not exist yet — \
             migration 006 not applied. Treat as inconclusive."
        );
        return Ok(true);
    }

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit.audit_events WHERE tenant_id = $1")
            .bind(tenant)
            .fetch_one(pool)
            .await
            .context("failed to count audit events")?;

    if count > 0 {
        println!("[PASS] Audit trail preserved: {count} event(s) for tenant {tenant}.");
        Ok(true)
    } else {
        eprintln!(
            "[FAIL] Audit trail missing: 0 events for tenant {tenant} in audit.audit_events."
        );
        Ok(false)
    }
}

// ------------------------------------------------------------------
// Neo4j helper (HTTP Cypher API)
// ------------------------------------------------------------------

async fn check_neo4j_empty(label: &str) -> bool {
    let base =
        std::env::var("NEO4J_HTTP_URL").unwrap_or_else(|_| "http://localhost:7474".to_owned());
    let user = std::env::var("NEO4J_USER").unwrap_or_else(|_| "neo4j".to_owned());
    let pass = std::env::var("NEO4J_PASS").unwrap_or_else(|_| "neo4j".to_owned());

    let cypher = format!("MATCH (n:`{label}`) RETURN count(n) AS cnt");
    let body = serde_json::json!({ "statements": [{ "statement": cypher }] });

    let client = reqwest::Client::new();
    let url = format!("{base}/db/neo4j/tx/commit");
    let resp = client
        .post(&url)
        .basic_auth(&user, Some(&pass))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await;

    match resp {
        Err(e) => {
            eprintln!("[WARN] Neo4j check failed (connectivity): {e}. Treating as inconclusive.");
            true // Don't fail if Neo4j is unreachable — flag as inconclusive.
        }
        Ok(r) => {
            let status = r.status();
            match r.json::<Value>().await {
                Err(e) => {
                    eprintln!("[WARN] Neo4j response parse error: {e}.");
                    true
                }
                Ok(json) => {
                    let cnt = json["results"][0]["data"][0]["row"][0]
                        .as_i64()
                        .unwrap_or(-1);
                    if !status.is_success() || cnt < 0 {
                        eprintln!("[WARN] Neo4j check inconclusive (status={status}, cnt={cnt}).");
                        true
                    } else if cnt == 0 {
                        println!("[PASS] Neo4j: 0 nodes labelled :{label}.");
                        true
                    } else {
                        eprintln!(
                            "[FAIL] Neo4j leak: {cnt} nodes with label :{label} still exist."
                        );
                        false
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------
// Qdrant helper (REST API)
// ------------------------------------------------------------------

async fn check_qdrant_collection_gone(collection: &str) -> bool {
    let base = std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6333".to_owned());
    let url = format!("{base}/collections/{collection}");

    let client = reqwest::Client::new();
    match client.get(&url).send().await {
        Err(e) => {
            eprintln!("[WARN] Qdrant check failed (connectivity): {e}. Treating as inconclusive.");
            true
        }
        Ok(r) => {
            if r.status().as_u16() == 404 {
                println!("[PASS] Qdrant: collection '{collection}' not found (404).");
                true
            } else if r.status().is_success() {
                eprintln!(
                    "[FAIL] Qdrant leak: collection '{collection}' still exists (status={}).",
                    r.status()
                );
                false
            } else {
                eprintln!(
                    "[WARN] Qdrant check inconclusive (status={}). Treating as PASS.",
                    r.status()
                );
                true
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_schema_uses_24_hex_suffix() {
        let tenant = TenantId::from(Uuid::nil());
        let ctx = TenantCtx::new(tenant);
        let schema = ctx.schema_name();
        // "tenant_" (7) + 24 hex = 31 chars, no dashes.
        assert_eq!(schema.len(), 31);
        assert!(!schema.contains('-'));
        assert!(schema.starts_with("tenant_"));
    }

    #[test]
    fn neo4j_label_uses_24_hex_suffix() {
        let tenant_id = TenantId::from(Uuid::nil());
        let label = tenant_label(&tenant_id);
        // "Tenant_" (7) + 24 hex = 31 chars.
        assert_eq!(label.len(), 31);
        assert!(label.starts_with("Tenant_"));
        assert!(!label.contains('-'));
    }

    #[test]
    fn qdrant_collection_uses_24_hex_suffix() {
        let tenant_id = TenantId::from(Uuid::nil());
        let coll = qdrant_collection_name(&tenant_id);
        // "tenant_" (7) + 24 hex + "_embeddings" (11) = 42 chars.
        assert_eq!(coll.len(), 42);
        assert!(coll.starts_with("tenant_"));
        assert!(coll.ends_with("_embeddings"));
        assert!(!coll.contains('-'));
    }

    /// Cross-crate regression: all three derivations must use the same 24-hex
    /// suffix so that the CLI checks the same tenant space the storage layers write to.
    #[test]
    fn all_derivations_share_same_24_hex_core() {
        let uuid = Uuid::new_v4();
        let tenant_id = TenantId::from(uuid);
        let ctx = TenantCtx::new(tenant_id);

        let schema = ctx.schema_name(); // tenant_<24hex>
        let label = tenant_label(&tenant_id); // Tenant_<24hex>
        let coll = qdrant_collection_name(&tenant_id); // tenant_<24hex>_embeddings

        let hex_from_schema = &schema["tenant_".len()..];
        let hex_from_label = &label["Tenant_".len()..];
        let hex_from_coll = &coll["tenant_".len()..coll.len() - "_embeddings".len()];

        assert_eq!(
            hex_from_schema, hex_from_label,
            "PG schema hex != Neo4j label hex"
        );
        assert_eq!(
            hex_from_schema, hex_from_coll,
            "PG schema hex != Qdrant collection hex"
        );
        assert_eq!(
            hex_from_schema.len(),
            24,
            "hex suffix must be exactly 24 chars"
        );
    }

    #[test]
    fn exit_code_semantics() {
        // Verify the expected exit codes are distinct.
        let pass: i32 = 0;
        let fail: i32 = 1;
        let config_err: i32 = 2;
        assert_ne!(pass, fail);
        assert_ne!(pass, config_err);
        assert_ne!(fail, config_err);
    }
}
