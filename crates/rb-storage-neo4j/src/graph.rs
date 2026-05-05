use std::sync::Arc;

use neo4rs::Graph;
use rb_schemas::TenantId;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::TenantId;

    #[test]
    fn tenant_label_is_safe_for_cypher_interpolation() {
        let label = tenant_label(&TenantId::new());
        // Must only contain alphanumeric + underscore — safe for direct Cypher interpolation.
        assert!(label.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        assert!(label.starts_with("Tenant_"));
    }
}
