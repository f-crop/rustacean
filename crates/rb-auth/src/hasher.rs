use argon2::{
    Argon2, Params, Version,
    password_hash::{
        PasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString, rand_core::OsRng,
    },
};

use crate::error::AuthError;

/// Argon2id password hasher with configurable parameters.
///
/// Use [`PasswordHasher::from_config`] to build from env-driven config.
/// The resulting PHC string is safe to store directly in the database.
#[derive(Clone)]
pub struct PasswordHasher {
    params: Params,
}

impl PasswordHasher {
    /// Build from explicit argon2id parameters.
    ///
    /// - `memory_kb`: memory cost in KiB (PRD default 19 456)
    /// - `time_cost`: iteration count (PRD default 2)
    /// - `parallelism`: lane count (PRD default 1)
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Argon2`] if the parameters are rejected by the argon2 crate.
    pub fn from_config(
        memory_kb: u32,
        time_cost: u32,
        parallelism: u32,
    ) -> Result<Self, AuthError> {
        let params = Params::new(memory_kb, time_cost, parallelism, None)
            .map_err(|e| AuthError::Argon2(e.to_string()))?;
        Ok(Self { params })
    }

    /// Hash a plaintext password, returning a PHC string.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Argon2`] on internal hash failure.
    pub fn hash(&self, password: &str) -> Result<String, AuthError> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::new(
            argon2::Algorithm::Argon2id,
            Version::V0x13,
            self.params.clone(),
        );
        argon2
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| AuthError::Argon2(e.to_string()))
    }

    /// Verify a plaintext password against a stored PHC hash.
    ///
    /// Returns `true` if the password matches, `false` if it does not.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Argon2`] if the stored hash string is malformed.
    pub fn verify(&self, password: &str, hash: &str) -> Result<bool, AuthError> {
        let parsed = PasswordHash::new(hash).map_err(|e| AuthError::Argon2(e.to_string()))?;
        let argon2 = Argon2::new(
            argon2::Algorithm::Argon2id,
            Version::V0x13,
            self.params.clone(),
        );
        match argon2.verify_password(password.as_bytes(), &parsed) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(e) => Err(AuthError::Argon2(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hasher() -> PasswordHasher {
        // Use minimal params so tests run fast.
        PasswordHasher::from_config(64, 1, 1).unwrap()
    }

    #[test]
    fn hash_produces_phc_string() {
        let h = hasher().hash("correct-horse-battery-staple").unwrap();
        assert!(
            h.starts_with("$argon2id$"),
            "must be an argon2id PHC string"
        );
    }

    #[test]
    fn verify_correct_password_returns_true() {
        let pw = "correct-horse-battery-staple";
        let h = hasher().hash(pw).unwrap();
        assert!(hasher().verify(pw, &h).unwrap());
    }

    #[test]
    fn verify_wrong_password_returns_false() {
        let h = hasher().hash("correct-horse-battery-staple").unwrap();
        assert!(!hasher().verify("wrong-password", &h).unwrap());
    }

    #[test]
    fn hash_is_non_deterministic() {
        let h1 = hasher().hash("same-password").unwrap();
        let h2 = hasher().hash("same-password").unwrap();
        assert_ne!(h1, h2, "salted hashes must differ");
    }

    #[test]
    fn verify_malformed_hash_returns_error() {
        let result = hasher().verify("password", "not-a-phc-string");
        assert!(result.is_err());
    }

    #[test]
    fn from_config_invalid_params_returns_error() {
        // memory_kb=0 is invalid
        let result = PasswordHasher::from_config(0, 1, 1);
        assert!(result.is_err());
    }
}
