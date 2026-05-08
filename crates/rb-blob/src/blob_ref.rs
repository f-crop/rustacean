use uuid::Uuid;

use crate::error::BlobError;

/// A handle identifying a single content-addressed blob.
///
/// URI form: `rb-blob://tenant_<tenant_id>/<sha256>`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef {
    pub tenant_id: Uuid,
    /// Hex-encoded SHA-256 of the blob content.
    pub sha256: String,
    pub content_type: String,
    pub size: u64,
}

impl BlobRef {
    pub fn new(
        tenant_id: Uuid,
        sha256: impl Into<String>,
        content_type: impl Into<String>,
        size: u64,
    ) -> Self {
        Self {
            tenant_id,
            sha256: sha256.into(),
            content_type: content_type.into(),
            size,
        }
    }

    /// Canonical URI: `rb-blob://tenant_<id>/<sha256>`
    #[must_use]
    pub fn to_uri(&self) -> String {
        format!("rb-blob://tenant_{}/{}", self.tenant_id, self.sha256)
    }

    /// Parse a URI into a minimal `BlobRef` that carries only `tenant_id` and
    /// `sha256`.  The `content_type` field is set to `""` and `size` is set to
    /// `0`.
    ///
    /// **This `BlobRef` cannot be passed to `put()` without first setting
    /// `content_type` and `size`.** Use this only for lookup-only operations
    /// (e.g. `get`, `exists`, `delete`) where those fields are not required.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidUri`] if the URI does not match the expected
    /// format `rb-blob://tenant_<uuid>/<sha256>`.
    pub fn from_uri_minimal(uri: &str) -> Result<Self, BlobError> {
        let rest = uri
            .strip_prefix("rb-blob://tenant_")
            .ok_or_else(|| BlobError::InvalidUri(uri.to_string()))?;
        let (tenant_str, sha256) = rest
            .split_once('/')
            .ok_or_else(|| BlobError::InvalidUri(uri.to_string()))?;
        let tenant_id =
            Uuid::parse_str(tenant_str).map_err(|_| BlobError::InvalidUri(uri.to_string()))?;
        Ok(Self {
            tenant_id,
            sha256: sha256.to_string(),
            content_type: String::new(),
            size: 0,
        })
    }

    /// Deprecated alias for [`from_uri_minimal`].
    ///
    /// Returns a partial `BlobRef` with `content_type = ""` and `size = 0`.
    /// Callers should migrate to [`from_uri_minimal`] to make the partial
    /// nature explicit and avoid accidental `SizeMismatch` errors on re-put.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::InvalidUri`] if the URI does not match the expected
    /// format `rb-blob://tenant_<uuid>/<sha256>`.
    #[deprecated(since = "0.1.0", note = "use `from_uri_minimal` instead")]
    pub fn from_uri(uri: &str) -> Result<Self, BlobError> {
        Self::from_uri_minimal(uri)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_roundtrip() {
        let tenant_id = Uuid::new_v4();
        let original = BlobRef::new(
            tenant_id,
            "deadbeef".repeat(8),
            "application/octet-stream",
            42,
        );
        let uri = original.to_uri();
        let parsed = BlobRef::from_uri_minimal(&uri).expect("valid uri");
        assert_eq!(parsed.tenant_id, original.tenant_id);
        assert_eq!(parsed.sha256, original.sha256);
        // Minimal ref has zeroed fields — not suitable for put().
        assert_eq!(parsed.content_type, "");
        assert_eq!(parsed.size, 0);
    }

    #[test]
    fn invalid_uri_rejected() {
        assert!(BlobRef::from_uri_minimal("https://example.com/blob").is_err());
        assert!(BlobRef::from_uri_minimal("rb-blob://tenant_notauuid/sha").is_err());
    }
}
