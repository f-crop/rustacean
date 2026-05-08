use rb_schemas::TenantId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TenantError {
    #[error("invalid schema name '{0}': must match ^tenant_[0-9a-f]{{24}}$")]
    InvalidSchemaName(String),
}

/// Validated tenant schema name. Guaranteed to match `^tenant_[0-9a-f]{24}$`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SchemaName(String);

impl SchemaName {
    /// Construct from a pre-existing string, validating the format.
    ///
    /// # Errors
    ///
    /// Returns [`TenantError::InvalidSchemaName`] if the string does not match
    /// `^tenant_[0-9a-f]{24}$`.
    pub fn new(s: impl Into<String>) -> Result<Self, TenantError> {
        let s = s.into();
        if !is_valid_schema_name(&s) {
            return Err(TenantError::InvalidSchemaName(s));
        }
        Ok(Self(s))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SchemaName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_valid_schema_name(s: &str) -> bool {
    let Some(suffix) = s.strip_prefix("tenant_") else {
        return false;
    };
    suffix.len() == 24
        && suffix
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Tenant execution context: tenant identity paired with its validated schema name.
///
/// Construct via [`TenantCtx::new`]. The schema name is derived deterministically from
/// the tenant ID — the lower 96 bits (last 12 bytes) of the UUID rendered as 24
/// lowercase hex chars.
#[derive(Debug, Clone)]
pub struct TenantCtx {
    tenant_id: TenantId,
    schema_name: SchemaName,
}

impl TenantCtx {
    /// Construct a `TenantCtx` from a `TenantId`.
    ///
    /// Schema name is derived from the lower 96 bits (last 12 bytes) of the UUID,
    /// rendered as 24 lowercase hex chars: `tenant_<24hex>`.
    #[must_use]
    pub fn new(tenant_id: TenantId) -> Self {
        let schema_name = derive_schema_name(&tenant_id);
        Self {
            tenant_id,
            schema_name,
        }
    }

    #[must_use]
    pub fn tenant_id(&self) -> &TenantId {
        &self.tenant_id
    }

    #[must_use]
    pub fn schema_name(&self) -> &str {
        self.schema_name.as_str()
    }

    /// Returns the fully qualified table reference `schema_name.table`.
    ///
    /// This is the single codepath for all tenant-scoped SQL table references.
    /// Never use `search_path`; always call `qualify`.
    #[must_use]
    pub fn qualify(&self, table: &str) -> String {
        format!("{}.{}", self.schema_name.as_str(), table)
    }
}

fn derive_schema_name(tenant_id: &TenantId) -> SchemaName {
    let uuid = tenant_id.as_uuid();
    let bytes = uuid.as_bytes();
    let mut hex = String::with_capacity(24);
    #[allow(clippy::use_debug)]
    for b in &bytes[4..] {
        use std::fmt::Write as _;
        write!(hex, "{b:02x}").expect("infallible write to String");
    }
    // Safety: derived hex is always 24 lowercase hex chars.
    SchemaName(format!("tenant_{hex}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_name_valid_format_accepted() {
        let name = SchemaName::new("tenant_deadbeef1234567890abcdef").unwrap();
        assert_eq!(name.as_str(), "tenant_deadbeef1234567890abcdef");
    }

    #[test]
    fn schema_name_wrong_prefix_rejected() {
        assert!(SchemaName::new("schema_deadbeef1234567890abcdef").is_err());
    }

    #[test]
    fn schema_name_too_short_rejected() {
        assert!(SchemaName::new("tenant_deadbeef").is_err());
    }

    #[test]
    fn schema_name_too_long_rejected() {
        assert!(SchemaName::new("tenant_deadbeef1234567890abcdefXX").is_err());
    }

    #[test]
    fn schema_name_uppercase_hex_rejected() {
        assert!(SchemaName::new("tenant_DEADBEEF1234567890ABCDEF").is_err());
    }

    #[test]
    fn schema_name_non_hex_chars_rejected() {
        assert!(SchemaName::new("tenant_gggggggg1234567890abcdef").is_err());
    }

    #[test]
    fn tenant_ctx_schema_name_has_correct_length() {
        let ctx = TenantCtx::new(TenantId::new());
        // "tenant_" (7) + 24 hex = 31 chars
        assert_eq!(ctx.schema_name().len(), 31);
    }

    #[test]
    fn tenant_ctx_schema_name_matches_pattern() {
        let ctx = TenantCtx::new(TenantId::new());
        let name = ctx.schema_name();
        assert!(name.starts_with("tenant_"), "must start with tenant_");
        let suffix = &name["tenant_".len()..];
        assert_eq!(suffix.len(), 24, "suffix must be 24 chars");
        assert!(
            suffix
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "suffix must be lowercase hex"
        );
    }

    #[test]
    fn schema_name_derivation_is_deterministic() {
        let tid = TenantId::new();
        let ctx1 = TenantCtx::new(tid);
        let ctx2 = TenantCtx::new(tid);
        assert_eq!(ctx1.schema_name(), ctx2.schema_name());
    }

    #[test]
    fn different_tenant_ids_produce_different_schema_names() {
        let ctx1 = TenantCtx::new(TenantId::new());
        let ctx2 = TenantCtx::new(TenantId::new());
        // UUIDs are random; collision probability is astronomically small.
        assert_ne!(ctx1.schema_name(), ctx2.schema_name());
    }

    #[test]
    fn qualify_produces_schema_dot_table() {
        let ctx = TenantCtx::new(TenantId::new());
        let qualified = ctx.qualify("users");
        let expected = format!("{}.users", ctx.schema_name());
        assert_eq!(qualified, expected);
    }
}
