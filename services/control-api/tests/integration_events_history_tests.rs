//! Integration tests for `GET /v1/agents/sessions/{id}/events/history` — paged history (RUSAA-1317).
//!
//! Covers: 401 no-auth, 404 unknown session, 403 cross-tenant, empty result,
//! pagination correctness, boundary conditions (`after` at start / past end),
//! default-limit enforcement, and invalid-limit rejection.
//!
//! Requires a running Postgres instance via `RB_DATABASE_URL`.  Tests skip
//! gracefully when that variable is absent.

#[path = "integration_events_history_tests/helpers.rs"]
mod helpers;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::Value;
use tower::ServiceExt as _;

use control_api::build_public;
use helpers::{
    history_uri, history_uri_with_params, insert_fixtures, insert_n_events, real_db_state,
};

// ---------------------------------------------------------------------------
// AC1 — 401 when no auth header / cookie
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac1_no_auth_returns_401() {
    let Some((state, _pool)) = real_db_state().await else {
        return;
    };
    let session_id = uuid::Uuid::new_v4();
    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri(session_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "AC1: must be 401");
}

// ---------------------------------------------------------------------------
// AC2 — 404 for unknown session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac2_unknown_session_returns_404() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri(uuid::Uuid::new_v4()))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "AC2: must be 404");
}

// ---------------------------------------------------------------------------
// AC3 — 403 cross-tenant access
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac3_cross_tenant_returns_403() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx_a = insert_fixtures(&pool).await;
    let fx_b = insert_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri(fx_a.agent_session_id))
                .header("cookie", format!("rb_session={}", fx_b.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "AC3: must be 403");
}

// ---------------------------------------------------------------------------
// AC4 — empty result when session has no events
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac4_empty_session_returns_200_with_empty_events() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri(fx.agent_session_id))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC4: must be 200");

    let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).expect("AC4: must be valid JSON");

    assert!(body["events"].is_array(), "AC4: events must be an array");
    assert_eq!(
        body["events"].as_array().unwrap().len(),
        0,
        "AC4: events must be empty"
    );
    assert!(body["next_seq"].is_null(), "AC4: next_seq must be null");
}

// ---------------------------------------------------------------------------
// AC5 — pagination correctness: two pages from 150 events with limit=100
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac5_pagination_two_pages_from_150_events() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    insert_n_events(&pool, fx.agent_session_id, fx.tenant_id, 150).await;

    // Page 1: no `after`, limit=100
    let resp1 = build_public(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri_with_params(
                    fx.agent_session_id,
                    None,
                    Some(100),
                ))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp1.status(), StatusCode::OK, "AC5-p1: must be 200");
    let body1: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp1.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();

    let events1 = body1["events"].as_array().unwrap();
    assert_eq!(
        events1.len(),
        100,
        "AC5-p1: first page must have 100 events"
    );

    // next_seq must be 100 (sequence of the last event on page 1).
    let next_seq = body1["next_seq"]
        .as_i64()
        .expect("AC5-p1: next_seq must be present");
    assert_eq!(next_seq, 100, "AC5-p1: next_seq must be 100");

    // Sequences must be 1..=100 in order.
    for (i, ev) in events1.iter().enumerate() {
        let seq = ev["sequence"].as_i64().unwrap();
        assert_eq!(
            seq,
            i64::try_from(i + 1).unwrap(),
            "AC5-p1: sequence at position {i} must be {}",
            i + 1
        );
    }

    // Page 2: after=100, limit=100
    let resp2 = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri_with_params(
                    fx.agent_session_id,
                    Some(next_seq),
                    Some(100),
                ))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp2.status(), StatusCode::OK, "AC5-p2: must be 200");
    let body2: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp2.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();

    let events2 = body2["events"].as_array().unwrap();
    assert_eq!(events2.len(), 50, "AC5-p2: second page must have 50 events");
    assert!(
        body2["next_seq"].is_null(),
        "AC5-p2: next_seq must be null on last page"
    );

    // Sequences must be 101..=150 in order.
    for (i, ev) in events2.iter().enumerate() {
        let seq = ev["sequence"].as_i64().unwrap();
        assert_eq!(
            seq,
            i64::try_from(i + 101).unwrap(),
            "AC5-p2: sequence at position {i} must be {}",
            i + 101
        );
    }
}

// ---------------------------------------------------------------------------
// AC6 — `after` at the beginning: same as no `after`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac6_after_zero_equivalent_to_no_after() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    insert_n_events(&pool, fx.agent_session_id, fx.tenant_id, 10).await;

    // Sequences start at 1, so after=0 is effectively the same as no `after`.
    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri_with_params(fx.agent_session_id, Some(0), None))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC6: must be 200");
    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 4096).await.unwrap())
            .unwrap();

    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 10, "AC6: must return all 10 events");
    assert!(body["next_seq"].is_null(), "AC6: next_seq must be null");
}

// ---------------------------------------------------------------------------
// AC7 — `after` past last sequence returns empty page
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac7_after_past_last_seq_returns_empty() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    insert_n_events(&pool, fx.agent_session_id, fx.tenant_id, 5).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri_with_params(
                    fx.agent_session_id,
                    Some(9999),
                    None,
                ))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC7: must be 200");
    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 4096).await.unwrap())
            .unwrap();

    assert_eq!(
        body["events"].as_array().unwrap().len(),
        0,
        "AC7: must return empty events array"
    );
    assert!(body["next_seq"].is_null(), "AC7: next_seq must be null");
}

// ---------------------------------------------------------------------------
// AC8 — default limit is 100: insert 200 events, no explicit limit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac8_default_limit_is_100() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    insert_n_events(&pool, fx.agent_session_id, fx.tenant_id, 200).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri(fx.agent_session_id))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC8: must be 200");
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();

    let events = body["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        100,
        "AC8: default limit must return exactly 100 events"
    );
    assert!(
        body["next_seq"].as_i64().is_some(),
        "AC8: next_seq must be present when more events exist"
    );
}

// ---------------------------------------------------------------------------
// AC9 — invalid limit returns 400
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac9_limit_zero_returns_400() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let resp = build_public(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri_with_params(fx.agent_session_id, None, Some(0)))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "AC9: limit=0 must return 400"
    );
}

#[tokio::test]
async fn ac9_limit_above_max_returns_400() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri_with_params(
                    fx.agent_session_id,
                    None,
                    Some(501),
                ))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "AC9: limit=501 must return 400"
    );
}

// ---------------------------------------------------------------------------
// AC10 — response shape: events have expected fields
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac10_response_shape_has_expected_fields() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    insert_n_events(&pool, fx.agent_session_id, fx.tenant_id, 3).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri(fx.agent_session_id))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC10: must be 200");
    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 4096).await.unwrap())
            .unwrap();

    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 3, "AC10: must return 3 events");

    let ev = &events[0];
    assert!(ev.get("id").is_some(), "AC10: event must have id");
    assert!(
        ev.get("session_id").is_some(),
        "AC10: event must have session_id"
    );
    assert!(
        ev.get("tenant_id").is_some(),
        "AC10: event must have tenant_id"
    );
    assert!(
        ev.get("event_type").is_some(),
        "AC10: event must have event_type"
    );
    assert!(
        ev.get("sequence").is_some(),
        "AC10: event must have sequence"
    );
    assert!(ev.get("payload").is_some(), "AC10: event must have payload");
    assert!(
        ev.get("created_at").is_some(),
        "AC10: event must have created_at"
    );
}
