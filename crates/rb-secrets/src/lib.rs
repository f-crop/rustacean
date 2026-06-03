mod error;
mod redact;
mod source;
mod value;

pub use error::SecretError;
pub use redact::{redact, redact_with_token};
pub use source::{EnvSource, FileSource, SecretSource};
pub use value::SecretValue;

/// Creates an [`EnvSource`] with the given prefix.
///
/// Convenience wrapper for the common pattern of reading secrets from
/// environment variables namespaced under a service prefix (e.g. `"RB"`).
#[must_use]
pub fn from_env(prefix: &str) -> Box<dyn SecretSource> {
    Box::new(EnvSource::new(prefix))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use tempfile::tempdir;

    use super::*;

    // Each test uses a globally unique env var name to avoid cross-test
    // interference when the test runner parallelises across threads.

    #[test]
    fn env_source_gets_existing_var() {
        // SAFETY: unique var name; test-binary scope; no concurrent reader.
        unsafe {
            std::env::set_var("RB_RBSEC_TEST1_SECRET", "hello_world");
        }
        let src = EnvSource::new("RB_RBSEC_TEST1");
        let val = src.get("SECRET").expect("env var should be found");
        assert_eq!(val.expose(), "hello_world");
        // SAFETY: same thread, removes what we set.
        unsafe {
            std::env::remove_var("RB_RBSEC_TEST1_SECRET");
        }
    }

    #[test]
    fn env_source_missing_key_returns_not_found() {
        // SAFETY: removing a var that we guarantee does not exist elsewhere.
        unsafe {
            std::env::remove_var("RB_RBSEC_TEST2_DEFINITELY_ABSENT_XQ7");
        }
        let src = EnvSource::new("RB_RBSEC_TEST2");
        let err = src
            .get("DEFINITELY_ABSENT_XQ7")
            .expect_err("absent var should return error");
        assert!(matches!(err, SecretError::NotFound { .. }));
    }

    #[test]
    fn env_source_uppercases_key_and_prefix() {
        // SAFETY: unique var name.
        unsafe {
            std::env::set_var("MY_RBSEC3_PREFIX_MIXED_KEY", "cased");
        }
        let src = EnvSource::new("my_rbsec3_prefix");
        let val = src
            .get("mixed_key")
            .expect("lowercased lookup should upcase before lookup");
        assert_eq!(val.expose(), "cased");
        // SAFETY: removes what we set.
        unsafe {
            std::env::remove_var("MY_RBSEC3_PREFIX_MIXED_KEY");
        }
    }

    #[test]
    fn env_source_empty_prefix_reads_bare_key() {
        // SAFETY: unique var name.
        unsafe {
            std::env::set_var("RBSEC4_BARE_KEY_VAL", "bare");
        }
        let src = EnvSource::new("");
        let val = src
            .get("RBSEC4_BARE_KEY_VAL")
            .expect("bare (no prefix) key should be found");
        assert_eq!(val.expose(), "bare");
        // SAFETY: removes what we set.
        unsafe {
            std::env::remove_var("RBSEC4_BARE_KEY_VAL");
        }
    }

    #[test]
    fn file_source_reads_secret_file() {
        let dir = tempdir().expect("tempdir");
        let file_path = dir.path().join("db_password");
        let mut f = std::fs::File::create(&file_path).expect("create file");
        writeln!(f, "s3cr3t").expect("write");

        let src = FileSource::new(dir.path());
        let val = src.get("db_password").expect("file should be readable");
        // Trailing newline must be stripped.
        assert_eq!(val.expose(), "s3cr3t");
    }

    #[test]
    fn file_source_missing_file_returns_io_error() {
        let dir = tempdir().expect("tempdir");
        let src = FileSource::new(dir.path());
        let err = src
            .get("nonexistent_key_rbsec6")
            .expect_err("missing file should return error");
        assert!(matches!(err, SecretError::Io { .. }));
    }

    #[test]
    fn secret_value_expose_returns_inner_string() {
        let val = SecretValue::new("my-secret".to_owned());
        assert_eq!(val.expose(), "my-secret");
    }

    #[test]
    fn from_env_helper_creates_working_env_source() {
        // SAFETY: unique var name.
        unsafe {
            std::env::set_var("RB_RBSEC8_HELPER_KEY", "helper_val");
        }
        let src = from_env("RB_RBSEC8_HELPER");
        let val = src.get("KEY").expect("from_env source should find var");
        assert_eq!(val.expose(), "helper_val");
        // SAFETY: removes what we set.
        unsafe {
            std::env::remove_var("RB_RBSEC8_HELPER_KEY");
        }
    }
}
