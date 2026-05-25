# Codemap: rb-github

Library crate for GitHub App authentication and API interactions. Handles JWT signing for App-level authentication, installation token minting with in-process caching, webhook signature verification with replay protection, and GitHub REST API calls for repository and installation management.

## Module tree

```
crates/rb-github/src/
├── lib.rs                  # Crate root: re-exports all public types
├── app_config_store.rs     # Postgres-backed encrypted App credential storage
├── app_jwt.rs              # RS256 JWT minting for GitHub App authentication
├── client.rs               # GhApp: central handle for all GitHub API operations
├── error.rs                # GhError enum (JWT, HTTP, API, signature, replay)
├── installation_token.rs   # Installation access token request
├── loader.rs               # GhAppLoader: hot-reloadable GhApp handle
├── manifest_exchange.rs    # GitHub App manifest-to-credentials exchange flow
├── repos.rs                # Repository listing (RepoItem, RepoPage)
├── secret.rs               # Secret<T>: Debug/Display-safe wrapper
├── state_token.rs          # SHA-256 token hashing for OAuth state
├── token_cache.rs          # Per-installation token cache with single-flight mint
└── webhook/
    ├── mod.rs              # Webhook module root
    ├── events.rs           # Typed webhook event structs (installation, repos)
    ├── replay.rs           # ReplayCache: TTL-bounded delivery dedup
    └── verify.rs           # HMAC-SHA256 webhook signature verification
```

## Public API surface

### Core types

| Type | Kind | Description |
|------|------|-------------|
| `GhApp` | struct | Central handle — `verify_webhook()`, `check_identity()`, `installation_token()`, `fetch_repo()`, `list_installation_repos()`, `fetch_installation()`, `start_token_sweep()` |
| `GhAppLoader` | struct | Mutable holder for current `GhApp` with hot-reload — `current()`, `set()` |
| `GhError` | enum | Error type — `JwtMint`, `Http`, `ApiError { status, body }`, `Base64`, `InvalidKey`, `BadSignatureFormat`, `SignatureMismatch`, `Replay` |
| `Secret<T>` | struct | Prevents inner value from appearing in `Debug`/`Display` output |

### App configuration (encrypted storage)

| Type | Kind | Description |
|------|------|-------------|
| `AppConfig` | struct | Decrypted view of a `github_app_config` row |
| `AppConfigStore` | struct | Postgres-backed store — `load_active()`, `insert_replacing()` |
| `AppConfigError` | enum | `KeyMissing`, `KeyNotBase64`, `KeyWrongLength`, `Crypto`, `Db`, `UnknownKeyId`, `MalformedNonce` |
| `EncryptionKey` | struct | AES-256-GCM key — `from_env()`, `from_base64()`, `key_id()` |
| `NewAppConfig` | struct | Insert row with plaintext credentials |

### Token management

| Type | Kind | Description |
|------|------|-------------|
| `TokenCache` | struct | Per-installation in-process cache — `get_or_mint()` |
| `CachedToken` | struct | Token + expiry timestamp |
| `TokenMinter` | trait | `mint(installation_id) -> MintFuture` |
| `MintFuture<'a>` | type alias | `Pin<Box<dyn Future<Output = Result<CachedToken, GhError>> + Send + 'a>>` |

### GitHub API responses

| Type | Kind | Description |
|------|------|-------------|
| `AppIdentity` | struct | App identity from `GET /app` |
| `AppOwner` | struct | App owner info |
| `InstallationInfo` | struct | Installation metadata |
| `RepoInfo` | struct | Repository info (`full_name`, `default_branch`) |
| `RepoItem` | struct | Single repository entry from installation repos API |
| `RepoPage` | struct | Paginated repo list (`total_count`, `repositories`) |
| `ManifestConversion` | struct | Manifest-to-credentials exchange response |

### Webhook types

| Type | Kind | Description |
|------|------|-------------|
| `ReplayCache` | struct | TTL-bounded `X-GitHub-Delivery` dedup — `try_insert_new()` |
| `InstallationEvent` | enum | `Created`, `Deleted`, `Suspend`, `Unsuspend`, `Other` |
| `InstallationPayload` | struct | Installation event payload |
| `InstallationRepositoriesEvent` | enum | `Added`, `Removed`, `Other` |
| `InstallationReposPayload` | struct | Installation repositories event payload |
| `Account` | struct | GitHub account (login, kind, id) |
| `Installation` | struct | Installation info (id, account) |
| `RepoRef` | struct | Repo reference (id, full_name) |

### Free functions

| Function | Description |
|----------|-------------|
| `try_build_gh_app(cfg: &AppConfig) -> Result<GhApp, GhError>` | Build live `GhApp` from decrypted config |
| `exchange_manifest_code(http, base_url, code) -> Result<ManifestConversion>` | Exchange manifest code for App credentials |
| `hash_token(token: &[u8]) -> String` | SHA-256 hex digest |
| `verify_signature(body, sig_header, secret) -> Result<()>` | HMAC-SHA256 webhook signature check |

### Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `CURRENT_ENCRYPTION_KEY_ID` | `"gh-app-v1"` | Key ID for AES-256-GCM encryption |
| `SAFETY_MARGIN` | 10 minutes | Token refresh safety margin |
| `DEFAULT_GITHUB_API_BASE` | `"https://api.github.com"` | Default GitHub API base URL |

## External dependencies (rb-* crates)

| Crate | Role |
|-------|------|
| `rb-secrets` | `Secret<T>` wrapper for sensitive values |
| `rb-tracing` | OpenTelemetry integration |

## Dependents

| Consumer | How it uses rb-github |
|----------|----------------------|
| `services/control-api` | `GhAppLoader` in AppState; GitHub OAuth callback, webhook handling, installation management, repo listing |
| `services/ingest-clone` | Installation token for authenticated `git clone` |

## Related docs

- [ADR-010: Tenant-scoped GitHub App install](../decisions/ADR-010-github-app-tenant-install.md)
- [Runbook: github-install-rebind](../runbooks/github-install-rebind.md)
- [API reference: GitHub integration endpoints](../api-reference.md#github-integration-endpoints)
