//! Per-request `GhApp` handle that supports hot reload (Phase 2 of the
//! Manifest flow).
//!
//! Route handlers read the current `GhApp` via [`GhAppLoader::current`] on
//! every request — an [`ArcSwap`] load-acquire, no DB round trip. When the
//! upcoming Phase 3 callback ([`GhAppLoader::set`]) writes a fresh
//! `GhApp` into the loader, in-flight requests keep the old `Arc<GhApp>`
//! alive until they release it; later requests pick up the new value.

use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::GhApp;

/// Mutable handle to the current process-wide `GhApp` instance.
///
/// Cloning the loader is cheap — it shares the inner `ArcSwap` via `Arc`.
/// `None` means no App is configured; route handlers should respond 503 in
/// that case (existing `GithubAppNotConfigured` semantics).
#[derive(Clone)]
pub struct GhAppLoader {
    inner: Arc<ArcSwap<Option<Arc<GhApp>>>>,
}

impl GhAppLoader {
    /// Construct a loader seeded with `initial`. Pass `None` when no App
    /// configuration has been resolved yet.
    #[must_use]
    pub fn new(initial: Option<Arc<GhApp>>) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from(Arc::new(initial))),
        }
    }

    /// Return the current `GhApp` handle, if any. The returned `Arc<GhApp>`
    /// keeps the underlying instance alive for the duration of the caller's
    /// scope; replacements via [`GhAppLoader::set`] do not invalidate
    /// already-held clones.
    #[must_use]
    pub fn current(&self) -> Option<Arc<GhApp>> {
        self.inner.load().as_ref().clone()
    }

    /// Replace the current handle. In-flight readers that already called
    /// [`GhAppLoader::current`] continue using the previous value until they
    /// drop it; future calls observe `next`.
    pub fn set(&self, next: Option<Arc<GhApp>>) {
        self.inner.store(Arc::new(next));
    }
}

impl std::fmt::Debug for GhAppLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let configured = self.current().is_some();
        f.debug_struct("GhAppLoader")
            .field("configured", &configured)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Secret;
    use jsonwebtoken::EncodingKey;

    fn fake_gh_app() -> GhApp {
        // Generated once: 2048-bit RSA key in DER form, base64-decoded; here
        // we just pass a JwtSecret-style HMAC key since GhApp::new does not
        // validate the key shape. Tests below never mint a JWT.
        let key = EncodingKey::from_secret(b"test-key");
        GhApp::new(1, key, Secret::new(b"webhook-secret".to_vec()))
    }

    #[test]
    fn new_with_none_reports_unconfigured() {
        let loader = GhAppLoader::new(None);
        assert!(loader.current().is_none());
    }

    #[test]
    fn new_with_some_reports_configured() {
        let initial = Arc::new(fake_gh_app());
        let loader = GhAppLoader::new(Some(Arc::clone(&initial)));
        let got = loader.current().expect("Some");
        assert_eq!(got.app_id, 1);
        assert!(Arc::ptr_eq(&got, &initial));
    }

    #[test]
    fn set_replaces_current_handle() {
        let loader = GhAppLoader::new(Some(Arc::new(fake_gh_app())));
        let replacement = Arc::new(fake_gh_app());
        loader.set(Some(Arc::clone(&replacement)));
        let got = loader.current().expect("Some");
        assert!(Arc::ptr_eq(&got, &replacement));
    }

    #[test]
    fn set_to_none_clears_handle() {
        let loader = GhAppLoader::new(Some(Arc::new(fake_gh_app())));
        loader.set(None);
        assert!(loader.current().is_none());
    }

    #[test]
    fn clone_shares_inner() {
        // Both clones observe a swap performed via either handle — the
        // ArcSwap is shared by reference, not duplicated on clone.
        let loader_a = GhAppLoader::new(None);
        let loader_b = loader_a.clone();
        let app = Arc::new(fake_gh_app());
        loader_b.set(Some(Arc::clone(&app)));
        let got = loader_a.current().expect("Some");
        assert!(Arc::ptr_eq(&got, &app));
    }
}
