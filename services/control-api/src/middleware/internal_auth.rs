use axum::{extract::Request, middleware::Next, response::Response};

use crate::error::AppError;

pub async fn require_internal_secret(request: Request, next: Next) -> Result<Response, AppError> {
    let expected = std::env::var("RB_INTERNAL_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or(AppError::Unauthorized)?;

    let provided = request
        .headers()
        .get("x-internal-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !constant_time_compare(expected.as_bytes(), provided.as_bytes()) {
        return Err(AppError::Unauthorized);
    }

    Ok(next.run(request).await)
}

fn constant_time_compare(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_compare_matches_equal_slices() {
        assert!(constant_time_compare(b"secret", b"secret"));
        assert!(!constant_time_compare(b"secret", b"other"));
        assert!(!constant_time_compare(b"a", b"ab"));
    }
}
