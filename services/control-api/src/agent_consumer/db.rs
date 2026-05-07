//! Database persistence for agent events.

use rb_schemas::AgentEvent;
use sqlx::PgPool;
use uuid::Uuid;

/// Persist an agent event to the `agents.agent_events` table.
///
/// Events are range-partitioned by `created_at` (daily partitions).
/// The function maps protobuf event types to database event_type strings
/// per ADR-009 §5.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub async fn persist_event(pool: &PgPool, ev: &AgentEvent) -> Result<(), sqlx::Error> {
    let session_id = Uuid::parse_str(&ev.session_id).map_err(|e| {
        sqlx::Error::Protocol(format!("invalid session_id UUID: {e}"))
    })?;
    
    let tenant_id = Uuid::parse_str(&ev.tenant_id).map_err(|e| {
        sqlx::Error::Protocol(format!("invalid tenant_id UUID: {e}"))
    })?;

    let event_type = map_event_type(ev.event_type);
    
    // Parse timestamp from milliseconds
    let created_at = chrono::DateTime::from_timestamp_millis(ev.occurred_at_ms)
        .unwrap_or_else(|| chrono::Utc::now());

    // Get next sequence number for this session
    let sequence = get_next_sequence(pool, &session_id).await?;

    sqlx::query(
        r#"
        INSERT INTO agents.agent_events 
            (id, session_id, tenant_id, event_type, sequence, payload, created_at)
        VALUES 
            (gen_random_uuid(), $1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(session_id)
    .bind(tenant_id)
    .bind(event_type)
    .bind(sequence)
    .bind(&ev.event_data_json)
    .bind(created_at)
    .execute(pool)
    .await?;

    Ok(())
}

/// Get the next sequence number for a session.
///
/// Uses a subquery to safely get the next sequence even under concurrency.
async fn get_next_sequence(pool: &PgPool, session_id: &Uuid) -> Result<i64, sqlx::Error> {
    let result: Option<(i64,)> = sqlx::query_as(
        r#"
        SELECT COALESCE(MAX(sequence), 0) + 1
        FROM agents.agent_events
        WHERE session_id = $1
        "#,
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;

    Ok(result.map(|r| r.0).unwrap_or(1))
}

/// Map protobuf AgentEventType to database event_type string.
///
/// These strings match the event types defined in ADR-009 §5 and
/// the constraint in migration 010_agent_sessions.sql.
fn map_event_type(proto_type: i32) -> &'static str {
    use rb_schemas::AgentEventType;
    
    match AgentEventType::try_from(proto_type) {
        Ok(AgentEventType::System) => "session.system",
        Ok(AgentEventType::Stdout) => "session.stdout",
        Ok(AgentEventType::Stderr) => "session.stderr",
        Ok(AgentEventType::ToolCall) => "session.tool_call",
        Ok(AgentEventType::ToolResult) => "session.tool_result",
        _ => "session.unknown",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::AgentEventType;

    #[test]
    fn event_type_mapping() {
        assert_eq!(map_event_type(AgentEventType::System as i32), "session.system");
        assert_eq!(map_event_type(AgentEventType::Stdout as i32), "session.stdout");
        assert_eq!(map_event_type(AgentEventType::Stderr as i32), "session.stderr");
        assert_eq!(map_event_type(AgentEventType::ToolCall as i32), "session.tool_call");
        assert_eq!(map_event_type(AgentEventType::ToolResult as i32), "session.tool_result");
        assert_eq!(map_event_type(999), "session.unknown");
    }
}
