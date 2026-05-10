use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::error::GhError;
use crate::secret::Secret;

type HmacSha256 = Hmac<Sha256>;

/// Verifies the `X-Hub-Signature-256` header against the raw request body.
///
/// The signature header format is `sha256=<hex>`. Verification uses
/// constant-time comparison to prevent timing-oracle attacks.
///
/// # Errors
///
/// Returns [`GhError::BadSignatureFormat`] if the header is not in the
/// expected `sha256=<hex>` format, and [`GhError::SignatureMismatch`] if the
/// computed HMAC does not match the provided value.
///
/// # Panics
///
/// Does not panic in practice; `HmacSha256::new_from_slice` only panics for
/// invalid key lengths, but HMAC accepts keys of any length.
pub fn verify_signature(
    body: &[u8],
    sig_header: &str,
    secret: &Secret<Vec<u8>>,
) -> Result<(), GhError> {
    let hex_part = sig_header
        .strip_prefix("sha256=")
        .ok_or(GhError::BadSignatureFormat)?;
    let provided = hex::decode(hex_part).map_err(|_| GhError::BadSignatureFormat)?;

    let mut mac = HmacSha256::new_from_slice(secret.expose()).expect("HMAC accepts any key length");
    mac.update(body);
    let expected = mac.finalize().into_bytes();

    if expected.ct_eq(&provided[..]).into() {
        Ok(())
    } else {
        Err(GhError::SignatureMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sig(body: &[u8], secret: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn valid_signature_passes() {
        let body = b"hello world";
        let secret = Secret::new(b"my-webhook-secret".to_vec());
        let sig = make_sig(body, b"my-webhook-secret");
        assert!(verify_signature(body, &sig, &secret).is_ok());
    }

    #[test]
    fn tampered_body_fails() {
        let secret = Secret::new(b"my-webhook-secret".to_vec());
        let sig = make_sig(b"original", b"my-webhook-secret");
        assert!(matches!(
            verify_signature(b"tampered", &sig, &secret),
            Err(GhError::SignatureMismatch)
        ));
    }

    #[test]
    fn wrong_secret_fails() {
        let body = b"hello";
        let secret = Secret::new(b"wrong-secret".to_vec());
        let sig = make_sig(body, b"correct-secret");
        assert!(matches!(
            verify_signature(body, &sig, &secret),
            Err(GhError::SignatureMismatch)
        ));
    }

    #[test]
    fn bad_format_no_prefix_fails() {
        let secret = Secret::new(b"s".to_vec());
        assert!(matches!(
            verify_signature(b"body", "no-prefix", &secret),
            Err(GhError::BadSignatureFormat)
        ));
    }

    #[test]
    fn bad_format_invalid_hex_fails() {
        let secret = Secret::new(b"s".to_vec());
        assert!(matches!(
            verify_signature(b"body", "sha256=ZZZZ", &secret),
            Err(GhError::BadSignatureFormat)
        ));
    }
}
