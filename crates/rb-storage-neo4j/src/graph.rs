use std::sync::Arc;

use neo4rs::{
    BoltBoolean, BoltFloat, BoltInteger, BoltList, BoltMap, BoltNull, BoltString, BoltType, Graph,
};
use rb_schemas::TenantId;
use serde_json::Value as JsonValue;

use crate::{CypherError, injector::inject_tenant_label, label::tenant_label};

/// Tenant-scoped Neo4j connection.
///
/// All Cypher queries pass through [`inject_tenant_label`] before execution,
/// enforcing per-tenant node label isolation (ADR-007 §3.4).
///
/// No caller outside `rb-storage-neo4j` may hold a raw `neo4rs::Graph` reference;
/// use this type as the sole write path for Neo4j (CI lint enforces this).
pub struct TenantGraph {
    inner: Arc<Graph>,
}

impl TenantGraph {
    /// Connect to Neo4j at `uri` using `user`/`password`.
    ///
    /// # Errors
    ///
    /// Returns [`CypherError::Neo4j`] on connection failure.
    pub async fn connect(uri: &str, user: &str, password: &str) -> Result<Self, CypherError> {
        let graph = Graph::new(uri, user, password).await?;
        Ok(Self {
            inner: Arc::new(graph),
        })
    }

    /// Execute a fire-and-forget Cypher query, injecting the tenant label before execution.
    ///
    /// `params` is a list of `(key, value)` pairs bound as Cypher string parameters.
    ///
    /// # Errors
    ///
    /// - [`CypherError::MultiStatement`] — query contains a bare semicolon outside strings/comments.
    /// - [`CypherError::UnclosedNodePattern`] — unbalanced `(` in a path clause.
    /// - [`CypherError::Neo4j`] — driver or network failure.
    pub async fn run(
        &self,
        tenant_id: &TenantId,
        cypher: &str,
        params: &[(&str, &str)],
    ) -> Result<(), CypherError> {
        let label = tenant_label(tenant_id);
        let injected = inject_tenant_label(cypher, &label)?;
        let mut q = neo4rs::query(&injected);
        for (k, v) in params {
            q = q.param(k, *v);
        }
        self.inner.run(q).await?;
        Ok(())
    }

    /// Execute a Cypher query with mixed string and `i64` parameters.
    ///
    /// # Errors
    ///
    /// Same as [`Self::run`].
    pub async fn run_mixed(
        &self,
        tenant_id: &TenantId,
        cypher: &str,
        str_params: &[(&str, &str)],
        i64_params: &[(&str, i64)],
    ) -> Result<(), CypherError> {
        let label = tenant_label(tenant_id);
        let injected = inject_tenant_label(cypher, &label)?;
        let mut q = neo4rs::query(&injected);
        for (k, v) in str_params {
            q = q.param(k, *v);
        }
        for (k, v) in i64_params {
            q = q.param(k, *v);
        }
        self.inner.run(q).await?;
        Ok(())
    }

    /// Delete all nodes whose `repo_id` property matches `repo_id` for `tenant_id`.
    ///
    /// Uses `DETACH DELETE` so all attached relationships are removed too. Idempotent
    /// (no-op when no matching nodes exist).
    ///
    /// # Errors
    ///
    /// Returns [`CypherError`] on driver or injection failure.
    pub async fn delete_repo_nodes(
        &self,
        tenant_id: &TenantId,
        repo_id: &str,
    ) -> Result<(), CypherError> {
        self.run(
            tenant_id,
            "MATCH (n {repo_id: $repo_id}) DETACH DELETE n",
            &[("repo_id", repo_id)],
        )
        .await
    }

    /// Delete all nodes for `tenant_id`.
    ///
    /// The tenant label is injected automatically by [`Self::run`], so this
    /// removes every node belonging to this tenant and all their relationships.
    /// Idempotent (no-op when the tenant has no data).
    ///
    /// # Errors
    ///
    /// Returns [`CypherError`] on driver or injection failure.
    pub async fn delete_all_tenant_nodes(&self, tenant_id: &TenantId) -> Result<(), CypherError> {
        self.run(tenant_id, "MATCH (n) DETACH DELETE n", &[]).await
    }

    /// Execute a read Cypher query with string parameters, injecting the tenant label.
    ///
    /// Returns all matching rows collected in memory.  Use this for all read
    /// queries so that tenant-label isolation (ADR-007 §3.4) is enforced on the
    /// read path too — callers must not hold a raw `neo4rs::Graph` reference.
    ///
    /// # Errors
    ///
    /// - [`CypherError::MultiStatement`] — semicolon found outside a string/comment.
    /// - [`CypherError::UnclosedNodePattern`] — unbalanced `(` in a path clause.
    /// - [`CypherError::Neo4j`] — driver or network failure.
    pub async fn execute_read(
        &self,
        tenant_id: &TenantId,
        cypher: &str,
        params: &[(&str, &str)],
    ) -> Result<Vec<neo4rs::Row>, CypherError> {
        let label = tenant_label(tenant_id);
        let injected = inject_tenant_label(cypher, &label)?;
        let mut q = neo4rs::query(&injected);
        for (k, v) in params {
            q = q.param(k, *v);
        }
        let mut stream = self.inner.execute(q).await?;
        let mut rows = Vec::new();
        while let Some(row) = stream.next().await? {
            rows.push(row);
        }
        Ok(rows)
    }

    /// Count `TypeInstance` nodes scoped to `tenant_id`.
    ///
    /// Used by projector-neo4j to enforce the `RB_MONOMORPH_NODE_CAP` per ADR-007 §13.7.
    ///
    /// # Errors
    ///
    /// Returns [`CypherError::Neo4j`] on driver failure.
    pub async fn count_type_instances(&self, tenant_id: &TenantId) -> Result<i64, CypherError> {
        let label = tenant_label(tenant_id);
        // Label is derived from TenantId — safe to interpolate (hex chars + underscore only).
        let cypher = format!("MATCH (n:{label}:TypeInstance) RETURN count(n) AS cnt");
        let mut stream = self.inner.execute(neo4rs::query(&cypher)).await?;
        if let Some(row) = stream.next().await? {
            let cnt: i64 = row.get("cnt").unwrap_or(0);
            return Ok(cnt);
        }
        Ok(0)
    }

    /// Execute an arbitrary Cypher query and return each row deserialised as a
    /// `serde_json::Value` object.  The tenant label is injected automatically;
    /// multi-statement queries are rejected.
    ///
    /// `params` maps parameter names to JSON values which are converted to
    /// Bolt types before dispatch.
    ///
    /// # Errors
    ///
    /// - [`CypherError::MultiStatement`] — query contains a bare semicolon.
    /// - [`CypherError::UnclosedNodePattern`] — unbalanced `(` in path clause.
    /// - [`CypherError::ParamConversion`] — a JSON parameter value has no Bolt equivalent.
    /// - [`CypherError::Neo4j`] — driver or network failure.
    pub async fn execute_query(
        &self,
        tenant_id: &TenantId,
        cypher: &str,
        params: &serde_json::Map<String, JsonValue>,
    ) -> Result<Vec<JsonValue>, CypherError> {
        let label = tenant_label(tenant_id);
        let injected = inject_tenant_label(cypher, &label)?;
        let mut q = neo4rs::query(&injected);
        for (key, val) in params {
            q = q.param(key.as_str(), json_to_bolt(val)?);
        }
        let mut stream = self.inner.execute(q).await?;
        let mut rows = Vec::new();
        while let Some(row) = stream.next().await? {
            let json: JsonValue = row
                .to()
                .map_err(|e| CypherError::Neo4j(neo4rs::Error::DeserializationError(e)))?;
            rows.push(json);
        }
        Ok(rows)
    }
}

fn json_to_bolt(v: &JsonValue) -> Result<BoltType, CypherError> {
    match v {
        JsonValue::Null => Ok(BoltType::Null(BoltNull)),
        JsonValue::Bool(b) => Ok(BoltType::Boolean(BoltBoolean { value: *b })),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(BoltType::Integer(BoltInteger { value: i }))
            } else if let Some(f) = n.as_f64() {
                Ok(BoltType::Float(BoltFloat { value: f }))
            } else {
                Err(CypherError::ParamConversion(v.to_string()))
            }
        }
        JsonValue::String(s) => Ok(BoltType::String(BoltString { value: s.clone() })),
        JsonValue::Array(arr) => {
            let items: Result<Vec<BoltType>, CypherError> = arr.iter().map(json_to_bolt).collect();
            Ok(BoltType::List(BoltList { value: items? }))
        }
        JsonValue::Object(map) => {
            let mut bolt_map = BoltMap::new();
            for (k, val) in map {
                bolt_map.put(BoltString { value: k.clone() }, json_to_bolt(val)?);
            }
            Ok(BoltType::Map(bolt_map))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::TenantId;

    #[test]
    fn tenant_label_is_safe_for_cypher_interpolation() {
        let label = tenant_label(&TenantId::new());
        assert!(label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        assert!(label.starts_with("Tenant_"));
    }

    #[test]
    fn json_to_bolt_null() {
        assert!(matches!(json_to_bolt(&JsonValue::Null), Ok(BoltType::Null(_))));
    }

    #[test]
    fn json_to_bolt_bool_true() {
        let bt = json_to_bolt(&JsonValue::Bool(true)).unwrap();
        assert!(matches!(bt, BoltType::Boolean(BoltBoolean { value: true })));
    }

    #[test]
    fn json_to_bolt_integer() {
        let bt = json_to_bolt(&serde_json::json!(42i64)).unwrap();
        assert!(matches!(bt, BoltType::Integer(BoltInteger { value: 42 })));
    }

    #[test]
    fn json_to_bolt_float() {
        let bt = json_to_bolt(&serde_json::json!(3.14f64)).unwrap();
        assert!(matches!(bt, BoltType::Float(_)));
    }

    #[test]
    fn json_to_bolt_string() {
        let bt = json_to_bolt(&serde_json::json!("hello")).unwrap();
        assert!(matches!(bt, BoltType::String(BoltString { value }) if value == "hello"));
    }

    #[test]
    fn json_to_bolt_array() {
        let bt = json_to_bolt(&serde_json::json!([1, 2, 3])).unwrap();
        assert!(matches!(bt, BoltType::List(_)));
    }

    #[test]
    fn json_to_bolt_object() {
        let bt = json_to_bolt(&serde_json::json!({"k": "v"})).unwrap();
        assert!(matches!(bt, BoltType::Map(_)));
    }
}
