use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;
use uuid::Uuid;

use crate::client::{ApiClient, LoginRequest, LoginResponse, SignupRequest, SignupResponse};

/// A provisioned synthetic tenant with credentials for re-login.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantRecord {
    pub slot: usize,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl TenantRecord {
    pub fn password_for_slot(slot: usize) -> String {
        // Deterministic 20-char password, satisfies the ≥12 char requirement.
        format!("SynthLoad-{slot}-2026!Harness")
    }
}

/// Create a new synthetic tenant at `slot`, verify its email via Postgres,
/// and return a `TenantRecord`.
///
/// The client's session cookie is updated to this tenant after a successful login.
pub async fn provision(
    client: &mut ApiClient,
    slot: usize,
    database_url: &str,
) -> Result<TenantRecord> {
    let email = format!("synth-load-{slot}@synthetic.internal");
    let tenant_name = format!("synth-load-{slot}");
    let password = TenantRecord::password_for_slot(slot);

    // 1. Sign up
    let signup: SignupResponse = client
        .post_json(
            "/v1/auth/signup",
            &SignupRequest {
                email: email.clone(),
                password: password.clone(),
                tenant_name,
            },
        )
        .await
        .with_context(|| format!("signup for slot {slot} failed"))?;

    // 2. Verify email directly in Postgres (same pattern as board-smoke.sh)
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect(database_url)
        .await
        .context("failed to connect to Postgres for email verification")?;

    sqlx::query(
        "UPDATE control.users SET email_verified_at = now() \
         WHERE email = $1 AND email_verified_at IS NULL",
    )
    .bind(&email)
    .execute(&pool)
    .await
    .context("email verification UPDATE failed")?;

    let tenant_id: Uuid = sqlx::query_scalar(
        "SELECT tm.tenant_id FROM control.tenant_members tm \
         JOIN control.users u ON u.id = tm.user_id \
         WHERE u.email = $1 \
         LIMIT 1",
    )
    .bind(&email)
    .fetch_one(&pool)
    .await
    .context("could not look up tenant_id for new signup")?;

    pool.close().await;

    // 3. Login — sets the session cookie in the client
    login(client, &email, &password).await?;

    Ok(TenantRecord {
        slot,
        tenant_id,
        user_id: signup.user_id,
        email,
        created_at: chrono::Utc::now(),
    })
}

/// Log in and refresh the session cookie on the client.
pub async fn login(client: &mut ApiClient, email: &str, password: &str) -> Result<LoginResponse> {
    client
        .post_json(
            "/v1/auth/login",
            &LoginRequest {
                email: email.to_owned(),
                password: password.to_owned(),
            },
        )
        .await
        .with_context(|| format!("login for {email} failed"))
}

/// Two-phase force-delete for a synthetic tenant.
///
/// Phase 1: POST without `confirm_token` → get `confirm_token`.
/// Phase 2: POST with `confirm_token` → execute deletion.
///
/// Uses the admin bearer token (not the tenant's session cookie).
pub async fn force_delete(
    http: &reqwest::Client,
    base_url: &str,
    admin_token: &str,
    tenant_id: Uuid,
) -> Result<()> {
    #[derive(Deserialize)]
    struct Phase1Resp {
        confirm_token: String,
    }
    #[derive(Serialize)]
    struct Req {
        #[serde(skip_serializing_if = "Option::is_none")]
        confirm_token: Option<String>,
    }

    let path = format!(
        "{}/api/admin/v1/tenants/{tenant_id}/force-delete",
        base_url.trim_end_matches('/')
    );

    // Phase 1 — dry-run
    let p1: Phase1Resp = http
        .post(&path)
        .bearer_auth(admin_token)
        .header("X-Admin-Actor", "synthetic-load-harness")
        .json(&Req {
            confirm_token: None,
        })
        .send()
        .await
        .context("force-delete phase-1 network error")?
        .error_for_status()
        .context("force-delete phase-1 error")?
        .json()
        .await
        .context("force-delete phase-1 deserialize")?;

    // Phase 2 — confirm
    http.post(&path)
        .bearer_auth(admin_token)
        .header("X-Admin-Actor", "synthetic-load-harness")
        .json(&Req {
            confirm_token: Some(p1.confirm_token),
        })
        .send()
        .await
        .context("force-delete phase-2 network error")?
        .error_for_status()
        .context("force-delete phase-2 error")?;

    Ok(())
}
