use axum::{extract::Request, extract::State, middleware::Next, response::Response};

use crate::error::AppError;
use crate::state::AppState;

pub async fn require_internal_secret(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    if state.internal_secret.is_empty() {
        return Err(AppError::Unauthorized);
    }

    let provided = request
        .headers()
        .get("x-internal-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !constant_time_compare(state.internal_secret.as_bytes(), provided.as_bytes()) {
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
