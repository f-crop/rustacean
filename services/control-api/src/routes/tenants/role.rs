use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, SessionInfo, require_verified_session},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum TenantRole {
    Member = 0,
    Admin = 1,
    Owner = 2,
}

impl TenantRole {
    pub(super) fn from_str(s: &str) -> Option<Self> {
        match s {
            "member" => Some(TenantRole::Member),
            "admin" => Some(TenantRole::Admin),
            "owner" => Some(TenantRole::Owner),
            _ => None,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            TenantRole::Member => "member",
            TenantRole::Admin => "admin",
            TenantRole::Owner => "owner",
        }
    }
}

/// Extract a verified `SessionInfo` from `AuthContext` or return an error.
///
/// Returns `Unauthorized` for anonymous callers and `EmailNotVerified` when
/// the user has not yet confirmed their email address.
pub(super) fn require_session(auth: AuthContext) -> Result<SessionInfo, AppError> {
    require_verified_session(auth)
}

/// Look up the caller's role in a specific tenant and enforce a minimum role.
pub(super) async fn require_role(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    tenant_id: Uuid,
    minimum: TenantRole,
) -> Result<TenantRole, AppError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM control.tenant_members \
         WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    match row {
        None => Err(AppError::NotAMember),
        Some((role_str,)) => {
            let role = TenantRole::from_str(&role_str).unwrap_or(TenantRole::Member);
            if role >= minimum {
                Ok(role)
            } else {
                Err(AppError::InsufficientRole)
            }
        }
    }
}

/// Percent-encode a string using unreserved characters only (RFC 3986 §2.3).
pub(super) fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                use std::fmt::Write as _;
                write!(out, "%{b:02X}").expect("infallible");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_role_member_lt_admin_lt_owner() {
        assert!(TenantRole::Member < TenantRole::Admin);
        assert!(TenantRole::Admin < TenantRole::Owner);
        assert!(TenantRole::Member < TenantRole::Owner);
    }

    #[test]
    fn tenant_role_ordering_is_total() {
        use std::cmp::Ordering;
        assert_eq!(TenantRole::Member.cmp(&TenantRole::Member), Ordering::Equal);
        assert_eq!(TenantRole::Admin.cmp(&TenantRole::Admin), Ordering::Equal);
        assert_eq!(TenantRole::Owner.cmp(&TenantRole::Owner), Ordering::Equal);
    }

    #[test]
    fn tenant_role_from_str_member() {
        assert_eq!(TenantRole::from_str("member"), Some(TenantRole::Member));
    }

    #[test]
    fn tenant_role_from_str_admin() {
        assert_eq!(TenantRole::from_str("admin"), Some(TenantRole::Admin));
    }

    #[test]
    fn tenant_role_from_str_owner() {
        assert_eq!(TenantRole::from_str("owner"), Some(TenantRole::Owner));
    }

    #[test]
    fn tenant_role_from_str_unknown_returns_none() {
        assert!(TenantRole::from_str("superadmin").is_none());
        assert!(TenantRole::from_str("").is_none());
        assert!(TenantRole::from_str("ADMIN").is_none());
    }

    #[test]
    fn tenant_role_as_str_roundtrips() {
        for role in [TenantRole::Member, TenantRole::Admin, TenantRole::Owner] {
            assert_eq!(TenantRole::from_str(role.as_str()), Some(role));
        }
    }

    #[test]
    fn urlencoding_simple_encodes_at_sign() {
        let result = urlencoding_simple("user@example.com");
        assert!(result.contains("%40"), "@ must be percent-encoded");
        assert!(!result.contains('@'));
    }

    #[test]
    fn urlencoding_simple_preserves_unreserved_chars() {
        let input = "hello-world_123.test~";
        assert_eq!(urlencoding_simple(input), input);
    }

    #[test]
    fn urlencoding_simple_encodes_plus() {
        let result = urlencoding_simple("a+b");
        assert_eq!(result, "a%2Bb");
    }
}
