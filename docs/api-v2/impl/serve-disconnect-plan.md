# Implementation Plan: `serve()` / `disconnect()`

**Gap doc:** [../serve-disconnect.md](../serve-disconnect.md)  
**Python reference:** `cognee/api/v1/serve/`  
**Rust entry point:** `crates/cloud/` + `cognee-lib` re-exports (see C4 wiring).

---

## Status

Implemented: yes
Commits:
  - `ac8c86f` (C1: scaffold / error / config / credentials)
  - `8624a3f` (C2: device_auth + management_api)
  - `e94e9f4` (C3: cloud_client + state singleton)
  - `7c04dcb` (C4: serve + disconnect + cognee-lib wiring)
  - C5 (CLI subcommands + integration tests + final docs) — see `git log` for SHA
Date: 2026-04-24

Steps 1–13 of the plan are complete:

- [x] Step 1 — Scaffolding: `crates/cloud/` with `Cargo.toml` and empty `lib.rs` (C1)
- [x] Step 2 — `error.rs`: `CloudError` enum (C1)
- [x] Step 3 — `config.rs`: env-var loading (C1)
- [x] Step 4 — `credentials.rs`: on-disk credential store (C1)
- [x] Step 5 — `device_auth.rs`: OAuth2 Device Code Flow (RFC 8628) (C2)
- [x] Step 6 — `management_api.rs`: tenant & API-key client (C2)
- [x] Step 7 — `cloud_client.rs`: HTTP proxy for V2 operations (C3)
- [x] Step 8 — `state.rs`: async-safe singleton (C3)
- [x] Step 9 — `serve.rs`: orchestrator (C4)
- [x] Step 10 — `disconnect.rs` (C4)
- [x] Step 11 — `lib.rs`: public surface (C1 through C4; final re-exports in C4)
- [x] Step 12 — Wire into `cognee-lib` (C4; feature-gated on `cloud`, default-enabled)
- [x] Step 13 — CLI integration (C5: `cognee-cli serve` / `disconnect`, gated on `cloud`)

Test plan (§4) items all have implementations: `mockito`-based unit tests in each module, plus integration tests under `crates/cloud/tests/serve_disconnect_round_trip.rs` and CLI E2E under `crates/cli/tests/cli_serve_subcommand.rs`. Manual live-Auth0 verification remains opt-in and gated on user-supplied credentials; no CI changes were needed because the unit/integration tests are fully mocked.

---

## 1. Goal & Scope

Port Python's `cognee.serve()` and `cognee.disconnect()` to the Rust SDK while keeping the shape, behaviour, and on-disk credential format 100% compatible with the Python implementation. The two SDKs must be able to share `~/.cognee/cloud_credentials.json`.

### User-visible API

```rust
use cognee::{serve, disconnect, ServeConfig};

// Direct mode — local/remote URL provided explicitly
cognee::serve(ServeConfig::direct("http://localhost:8000")).await?;
cognee::serve(ServeConfig::direct("https://tenant.cognee.ai").api_key("ck_...")).await?;

// Cloud mode — OAuth2 Device Code Flow (interactive)
cognee::serve(ServeConfig::cloud()).await?;

// Or the ergonomic one-liners:
cognee::serve_url("http://localhost:8000", None).await?;   // direct
cognee::serve_cloud().await?;                              // device code flow
cognee::disconnect(false).await?;                          // keeps creds
cognee::disconnect(true).await?;                           // also wipes creds
```

All three variants return `Arc<CloudClient>`. After `serve()` returns, a module-level singleton is set. The SDK's V2 operations (`remember`, `recall`, `improve`, `forget`) check the singleton and route to the cloud client when it is populated. `disconnect()` tears the singleton down. Both functions are `async` (tokio), matching the Python coroutine semantics (Python: `serve.py:17` — `async def serve(...)`, `disconnect.py:8` — `async def disconnect(...)`). Callers decide whether to block (`tokio::runtime::Runtime::block_on`) or spawn.

### Two modes — Python references

| Mode | Trigger | Python lines |
|------|---------|---|
| Direct | `url` arg or `COGNEE_SERVICE_URL` set | `serve.py:61–65`, `_serve_direct` at `serve.py:75–101` |
| Cloud | No URL | `_serve_cloud` at `serve.py:104–230` |

---

## 2. Design Overview

### Crate structure — new `crates/cloud/` (`cognee-cloud`)

Adding an unconditional OAuth2 + Auth0 dependency chain to `cognee-lib` is overkill for users running only the local pipeline. Follow the gap-doc recommendation: ship cloud integration as a separate crate behind a feature flag.

```
crates/cloud/
├── Cargo.toml
└── src/
    ├── lib.rs              # public surface: CloudClient, serve helpers, errors
    ├── cloud_client.rs     # ↔ cloud_client.py  (async reqwest wrapper)
    ├── credentials.rs      # ↔ credentials.py   (CloudCredentials + IO)
    ├── device_auth.rs      # ↔ device_auth.py   (RFC 8628 device-code flow)
    ├── management_api.rs   # ↔ management_api.py (tenant + API-key client)
    ├── state.rs            # ↔ state.py         (singleton)
    ├── serve.rs            # ↔ serve.py         (orchestrator)
    ├── disconnect.rs       # ↔ disconnect.py
    ├── config.rs           # env-var loading (Auth0 domain/client_id/audience, mgmt URL)
    └── error.rs            # CloudError enum (thiserror)
```

One Rust file per Python file; names match so a grep across either tree finds the counterpart. `cognee-lib` then adds:

```toml
# crates/lib/Cargo.toml
[features]
cloud = ["dep:cognee-cloud"]
default = [..., "cloud"]          # ship enabled by default (drop-in parity goal)

[dependencies]
cognee-cloud = { path = "../cloud", optional = true }
```

`crates/lib/src/api/serve.rs` is a thin re-export module:

```rust
#[cfg(feature = "cloud")]
pub use cognee_cloud::{serve, disconnect, CloudClient, CloudError, ServeConfig};
```

This keeps the public symbol path `cognee::serve` working while letting embedded/Android builds opt out with `--no-default-features`.

### Dependency choices

| Concern | Choice | Rationale |
|---|---|---|
| HTTP client | `reqwest` (already workspace dep with `rustls-tls`, `json`, `multipart`, `stream`) | Matches `aiohttp.ClientSession` + `FormData` used by Python. No new dep. |
| JWT decode | **Manual base64 + serde_json** — no `jsonwebtoken` crate | Python does *no* signature verification (`device_auth.py:171–185`). Using `jsonwebtoken` adds a dep and pulls in signature math we don't want. `base64` (workspace) + `serde_json` are already available. |
| Cred dir discovery | `dirs` crate (already workspace dep) | Gives `dirs::home_dir()` cross-platform. Python uses `Path.home() / ".cognee"` (`credentials.py:14`). |
| File permissions | `std::os::unix::fs::PermissionsExt` (cfg-gated to Unix) | Matches `os.chmod(path, 0o600)` (`credentials.py:53`). Windows: best-effort, skip chmod, rely on ACL defaults. |
| Singleton | `tokio::sync::RwLock<Option<Arc<CloudClient>>>` inside `once_cell::sync::Lazy` | Async-safe, supports read/write. Python uses a module global (`state.py:12`). |
| UUID v5 for tenant name | `uuid` v5 (workspace, already enabled) | Exactly matches `uuid5(NAMESPACE_URL, email)` from `management_api.py:31–36`. |
| Async runtime | `tokio` (workspace) | Matches existing convention. |

### Credential file format

Byte-for-byte compatible with Python. Python writes via `json.dumps(asdict(creds), indent=2)` (`credentials.py:52`) with `0o600` perms. Schema (see `credentials.py:19–28`):

```json
{
  "access_token": "...",
  "refresh_token": "...",
  "expires_at": 1714060000.123,
  "service_url": "...",
  "api_key": "...",
  "management_url": "...",
  "tenant_id": "...",
  "tenant_name": "...",
  "email": "..."
}
```

Rust `CloudCredentials` struct uses `#[derive(Serialize, Deserialize)]` with exact field names and `expires_at: f64` so the file round-trips without drift. The location is computed at runtime from `dirs::home_dir().join(".cognee").join("cloud_credentials.json")` — matching Python's `Path.home() / ".cognee" / "cloud_credentials.json"` (`credentials.py:14–15`).

### Config / env vars

Exactly mirror Python names (see `device_auth.py:30–45`, `management_api.py:21–22`, `serve.py:61–62,131–133`):

| Env var | Default | Source |
|---|---|---|
| `COGNEE_SERVICE_URL` | (unset) | `serve.py:61` |
| `COGNEE_API_KEY` | `""` | `serve.py:62` |
| `COGNEE_AUTH0_DOMAIN` | `cognee.eu.auth0.com` | `device_auth.py:16,31` |
| `COGNEE_AUTH0_DEVICE_CLIENT_ID` | **required** in cloud mode | `device_auth.py:34–41` |
| `COGNEE_AUTH0_AUDIENCE` | `cognee:api` | `device_auth.py:17,45` |
| `COGNEE_CLOUD_URL` | `https://api.dev.cloud.topoteretes.com` | `management_api.py:18,22` |

Bundle them in a `CloudConfig` struct with a `from_env()` constructor. Per-call overrides from `ServeConfig` take precedence over env vars, matching Python's keyword-argument override pattern (`serve.py:22–24, 155–158`).

---

## 3. Step-by-Step Implementation

Steps are ordered so each compiles and tests cleanly on top of the previous one.

### Step 1 — Scaffolding: `crates/cloud/` with `Cargo.toml` and empty `lib.rs`

**New files:**
- `crates/cloud/Cargo.toml`
- `crates/cloud/src/lib.rs`
- Top-level `Cargo.toml` — add `"crates/cloud"` to `[workspace].members`.

```toml
# crates/cloud/Cargo.toml
[package]
name = "cognee-cloud"
version.workspace = true
edition.workspace = true

[dependencies]
async-trait.workspace = true
base64.workspace = true
chrono.workspace = true
dirs.workspace = true
reqwest = { workspace = true, features = ["json", "multipart", "stream"] }
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true
url.workspace = true
uuid.workspace = true
once_cell = "1.19"   # new workspace dep — add to root Cargo.toml

[dev-dependencies]
mockito = "1"        # HTTP mocking for unit tests (new dev-dep)
tempfile.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
```

`lib.rs` starts empty with `pub mod …;` lines added as steps are completed.

**Dependencies:** none.

---

### Step 2 — `error.rs`: `CloudError` enum

**File:** `crates/cloud/src/error.rs` (new)

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CloudError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("missing env var: {0}")]
    MissingEnv(&'static str),

    #[error("device code flow timed out")]
    DeviceCodeTimeout,

    #[error("device code expired — please try again")]
    DeviceCodeExpired,

    #[error("authorization denied by user")]
    AuthDenied,

    #[error("token polling error: {0}")]
    TokenPolling(String),

    #[error("tenant provisioning timed out after {0}s")]
    TenantProvisionTimeout(u64),

    #[error("failed to create API key after {0} retries")]
    ApiKeyCreation(u32),

    #[error("management API returned {status}: {body}")]
    ManagementApi { status: u16, body: String },

    #[error("service URL empty — tenant may still be provisioning")]
    EmptyServiceUrl,

    #[error("could not extract email from id_token (add 'email' to Auth0 scope)")]
    MissingEmailClaim,

    #[error("invalid credentials file: {0}")]
    InvalidCredentials(String),

    #[error("remote operation '{op}' failed ({status}): {body}")]
    RemoteOp { op: &'static str, status: u16, body: String },
}

pub type CloudResult<T> = Result<T, CloudError>;
```

Covers every `raise` in the Python tree (`device_auth.py:76,131,133,135,137,160`; `management_api.py:53,82,95,110,113,154`; `cloud_client.py:88,111,135,156`).

**Dependencies:** Step 1.

---

### Step 3 — `config.rs`: env-var loading

**File:** `crates/cloud/src/config.rs` (new)

```rust
use std::env;
use crate::error::{CloudError, CloudResult};

pub const DEFAULT_AUTH0_DOMAIN: &str = "cognee.eu.auth0.com";
pub const DEFAULT_AUTH0_AUDIENCE: &str = "cognee:api";
pub const DEFAULT_SCOPE: &str = "openid profile email offline_access";
pub const DEFAULT_MANAGEMENT_URL: &str = "https://api.dev.cloud.topoteretes.com";

#[derive(Debug, Clone)]
pub struct CloudConfig {
    pub auth0_domain: String,
    pub auth0_client_id: String,   // required; empty means not loaded yet
    pub auth0_audience: String,
    pub management_url: String,
}

impl CloudConfig {
    pub fn from_env() -> Self {
        Self {
            auth0_domain: env::var("COGNEE_AUTH0_DOMAIN")
                .unwrap_or_else(|_| DEFAULT_AUTH0_DOMAIN.into()),
            auth0_client_id: env::var("COGNEE_AUTH0_DEVICE_CLIENT_ID").unwrap_or_default(),
            auth0_audience: env::var("COGNEE_AUTH0_AUDIENCE")
                .unwrap_or_else(|_| DEFAULT_AUTH0_AUDIENCE.into()),
            management_url: env::var("COGNEE_CLOUD_URL")
                .unwrap_or_else(|_| DEFAULT_MANAGEMENT_URL.into())
                .trim_end_matches('/').into(),
        }
    }

    /// Validate that a client_id is present (needed for cloud-mode serve()).
    pub fn require_client_id(&self) -> CloudResult<&str> {
        if self.auth0_client_id.is_empty() {
            Err(CloudError::MissingEnv("COGNEE_AUTH0_DEVICE_CLIENT_ID"))
        } else {
            Ok(&self.auth0_client_id)
        }
    }
}
```

Mirrors `device_auth.py:30–45` and `management_api.py:21–22` exactly.

**Dependencies:** Step 2.

---

### Step 4 — `credentials.rs`: on-disk credential store

**File:** `crates/cloud/src/credentials.rs` (new) — ports `credentials.py` line-for-line.

```rust
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use tokio::fs;
use crate::error::{CloudError, CloudResult};

const CREDS_FILENAME: &str = "cloud_credentials.json";
const EXPIRY_BUFFER_SECS: f64 = 60.0;   // credentials.py:67

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudCredentials {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub expires_at: f64,              // unix seconds, matches Python float
    pub service_url: String,
    pub api_key: String,
    pub management_url: String,
    pub tenant_id: String,
    pub tenant_name: String,
    pub email: String,
}

pub fn credentials_path() -> CloudResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        CloudError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "home directory not found",
        ))
    })?;
    Ok(home.join(".cognee").join(CREDS_FILENAME))
}

pub async fn load_credentials() -> Option<CloudCredentials> {
    // Python credentials.py:35–46 — ignores all errors and returns None.
    let path = credentials_path().ok()?;
    let bytes = fs::read(&path).await.ok()?;
    serde_json::from_slice::<CloudCredentials>(&bytes).ok()
}

pub async fn save_credentials(creds: &CloudCredentials) -> CloudResult<()> {
    let path = credentials_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    // serde_json::to_string_pretty produces 2-space indent — matches
    // Python's json.dumps(..., indent=2) (credentials.py:52).
    let body = serde_json::to_string_pretty(creds)?;
    fs::write(&path, body).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = fs::metadata(&path).await?.permissions();
        p.set_mode(0o600);
        fs::set_permissions(&path, p).await?;
    }
    Ok(())
}

pub async fn clear_credentials() -> CloudResult<()> {
    let path = credentials_path()?;
    match fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub fn is_token_expired(creds: &CloudCredentials) -> bool {
    // credentials.py:64–67
    if creds.expires_at == 0.0 { return true; }
    let now = chrono::Utc::now().timestamp() as f64;
    now > (creds.expires_at - EXPIRY_BUFFER_SECS)
}
```

**Dependencies:** Step 2.

**Compatibility note:** Python's dataclass serializes all fields unconditionally with `asdict()` (`credentials.py:52`). The Rust `#[serde(skip_serializing_if = "Option::is_none")]` on `refresh_token` produces files without that key in the direct-mode case; Python re-reads the file via `CloudCredentials(**{k: v for k, v in data.items() if k in __dataclass_fields__})` (`credentials.py:42–43`), so missing keys default to `None`. Compatible both ways. If strict byte-parity matters, drop the attribute and serialize `null` explicitly.

---

### Step 5 — `device_auth.rs`: OAuth2 Device Code Flow (RFC 8628)

**File:** `crates/cloud/src/device_auth.rs` (new) — ports `device_auth.py` entirely.

Key types:

```rust
use std::time::Duration;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use tokio::time::{sleep, Instant};
use crate::{config::{CloudConfig, DEFAULT_SCOPE}, error::{CloudError, CloudResult}};

#[derive(Debug, Clone, Default)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub token_type: String,      // default "Bearer"
    pub expires_in: u64,         // default 3600
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResp {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    #[serde(default = "default_expires")] expires_in: u64,
    #[serde(default = "default_interval")] interval: u64,
}
fn default_expires() -> u64 { 900 }
fn default_interval() -> u64 { 5 }

#[derive(Debug, Deserialize)]
struct TokenResp {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<u64>,
    error: Option<String>,
    error_description: Option<String>,
}
```

Main flow (ports `device_auth.py:48–137`):

```rust
pub async fn device_code_login(cfg: &CloudConfig, scope: Option<&str>) -> CloudResult<TokenResponse> {
    let client_id = cfg.require_client_id()?;
    let audience = &cfg.auth0_audience;
    let scope = scope.unwrap_or(DEFAULT_SCOPE);
    let base = format!("https://{}", cfg.auth0_domain);

    let http = reqwest::Client::new();

    // Step 1: POST /oauth/device/code  (device_auth.py:67–78)
    let resp = http.post(format!("{base}/oauth/device/code"))
        .form(&[("client_id", client_id), ("scope", scope), ("audience", audience)])
        .send().await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(CloudError::ManagementApi { status, body });
    }
    let dc: DeviceCodeResp = resp.json().await?;

    // Step 2: show the user what to do (device_auth.py:88–97)
    let verification_uri = dc.verification_uri_complete.as_deref().unwrap_or(&dc.verification_uri);
    println!();
    println!("  To authenticate with Cognee Cloud, open this URL in your browser:");
    println!();
    println!("    {verification_uri}");
    println!();
    if dc.verification_uri_complete.is_none() {
        println!("  Then enter code: {}", dc.user_code);
        println!();
    }
    println!("  Waiting for authorization...");

    // Step 3: poll /oauth/token (device_auth.py:99–135)
    let deadline = Instant::now() + Duration::from_secs(dc.expires_in);
    let mut interval = Duration::from_secs(dc.interval);

    while Instant::now() < deadline {
        sleep(interval).await;

        let resp = http.post(format!("{base}/oauth/token"))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &dc.device_code),
                ("client_id", client_id),
            ])
            .send().await?;
        let status = resp.status();
        let body: TokenResp = resp.json().await?;

        if status.is_success() {
            println!("  Authenticated successfully!");
            return Ok(TokenResponse {
                access_token: body.access_token.unwrap_or_default(),
                refresh_token: body.refresh_token,
                id_token: body.id_token,
                token_type: body.token_type.unwrap_or_else(|| "Bearer".into()),
                expires_in: body.expires_in.unwrap_or(3600),
            });
        }

        match body.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval = (interval + Duration::from_secs(5)).min(Duration::from_secs(30));
            }
            Some("expired_token") => return Err(CloudError::DeviceCodeExpired),
            Some("access_denied") => return Err(CloudError::AuthDenied),
            Some(other) => return Err(CloudError::TokenPolling(
                format!("{other}: {}", body.error_description.unwrap_or_default())
            )),
            None => return Err(CloudError::TokenPolling("malformed token response".into())),
        }
    }
    Err(CloudError::DeviceCodeTimeout)
}

pub async fn refresh_access_token(
    cfg: &CloudConfig,
    refresh_token: &str,
) -> CloudResult<TokenResponse> {
    // device_auth.py:140–168
    let client_id = cfg.require_client_id()?;
    let http = reqwest::Client::new();
    let resp = http.post(format!("https://{}/oauth/token", cfg.auth0_domain))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
        ])
        .send().await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(CloudError::ManagementApi { status, body });
    }
    let body: TokenResp = resp.json().await?;
    Ok(TokenResponse {
        access_token: body.access_token.unwrap_or_default(),
        // Python: body.get("refresh_token", refresh_token) (device_auth.py:164)
        refresh_token: body.refresh_token.or_else(|| Some(refresh_token.to_string())),
        id_token: body.id_token,
        token_type: body.token_type.unwrap_or_else(|| "Bearer".into()),
        expires_in: body.expires_in.unwrap_or(3600),
    })
}

pub fn extract_email_from_id_token(id_token: &str) -> Option<String> {
    // device_auth.py:171–185 — split on '.', URL-safe base64 decode middle part,
    // parse JSON, extract "email".
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() < 2 { return None; }
    let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    json.get("email")?.as_str().map(str::to_owned)
}
```

**Polling backoff note:** Python `device_auth.py:128` increments `interval += 5` capped at 30 on `slow_down`. Rust mirrors that exactly. This is not exponential — it's the RFC 8628 linear bump. Don't "improve" it or we drift from Python.

**Dependencies:** Steps 2, 3.

---

### Step 6 — `management_api.rs`: tenant & API-key client

**File:** `crates/cloud/src/management_api.rs` (new) — ports `management_api.py`.

```rust
use std::time::Duration;
use serde::Deserialize;
use tokio::time::sleep;
use uuid::Uuid;
use crate::error::{CloudError, CloudResult};

#[derive(Debug, Clone)]
pub struct Tenant { pub id: String, pub name: String }

fn email_to_tenant_name(email: &str) -> String {
    // management_api.py:31–36 — uuid5(NAMESPACE_URL, email)
    let id = Uuid::new_v5(&Uuid::NAMESPACE_URL, email.as_bytes());
    format!("tenant-{id}")
}

fn auth_header(access_token: &str) -> String { format!("Bearer {access_token}") }

pub async fn get_current_tenant(
    management_url: &str,
    access_token: &str,
) -> CloudResult<Option<Tenant>> {
    // management_api.py:39–55
    let http = reqwest::Client::new();
    let resp = http.get(format!("{management_url}/api/tenants/current"))
        .header("Authorization", auth_header(access_token))
        .send().await?;
    match resp.status().as_u16() {
        404 => Ok(None),
        200 => {
            #[derive(Deserialize)]
            struct R { #[serde(default)] id: serde_json::Value, #[serde(default)] name: String }
            let r: R = resp.json().await?;
            let id = match r.id {
                serde_json::Value::String(s) => s,
                serde_json::Value::Number(n) => n.to_string(),
                _ => String::new(),
            };
            Ok(Some(Tenant { id, name: r.name }))
        }
        status => {
            let body = resp.text().await.unwrap_or_default();
            Err(CloudError::ManagementApi { status, body })
        }
    }
}

pub async fn create_tenant(
    management_url: &str,
    access_token: &str,
    email: &str,
    poll_timeout: Duration,     // Python default 300s — management_api.py:62
    poll_interval: Duration,    // Python default   5s — management_api.py:63
) -> CloudResult<Tenant> {
    let name = email_to_tenant_name(email);
    tracing::info!("Creating tenant '{name}' for {email}");

    let http = reqwest::Client::new();
    let resp = http.post(format!("{management_url}/api/tenants"))
        .query(&[("tenant_name", name.as_str())])
        .header("Authorization", auth_header(access_token))
        .send().await?;
    let status = resp.status().as_u16();
    if !matches!(status, 200 | 201 | 202) {
        let body = resp.text().await.unwrap_or_default();
        return Err(CloudError::ManagementApi { status, body });
    }

    println!("  Provisioning tenant (this may take a minute)...");
    let deadline = tokio::time::Instant::now() + poll_timeout;
    while tokio::time::Instant::now() < deadline {
        sleep(poll_interval).await;
        if let Some(t) = get_current_tenant(management_url, access_token).await? {
            if !t.id.is_empty() {
                tracing::info!("Tenant ready: {} ({})", t.name, t.id);
                return Ok(t);
            }
        }
    }
    Err(CloudError::TenantProvisionTimeout(poll_timeout.as_secs()))
}

pub async fn get_service_url(
    management_url: &str,
    access_token: &str,
) -> CloudResult<String> {
    // management_api.py:98–115
    let http = reqwest::Client::new();
    let resp = http.get(format!("{management_url}/api/tenants/current/service-url"))
        .header("Authorization", auth_header(access_token))
        .send().await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(CloudError::ManagementApi { status, body });
    }
    let v: serde_json::Value = resp.json().await?;
    let url = v.get("service_url").and_then(|x| x.as_str())
        .or_else(|| v.get("url").and_then(|x| x.as_str()))
        .unwrap_or("").trim_end_matches('/').to_string();
    if url.is_empty() { return Err(CloudError::EmptyServiceUrl); }
    Ok(url)
}

pub async fn get_or_create_api_key(
    management_url: &str,
    access_token: &str,
    max_retries: u32,   // Python default 3 — management_api.py:121
) -> CloudResult<String> {
    // management_api.py:118–154
    let http = reqwest::Client::new();
    let auth = auth_header(access_token);

    // GET first
    let resp = http.get(format!("{management_url}/api/api-keys"))
        .header("Authorization", &auth).send().await?;
    if resp.status().is_success() {
        let keys: serde_json::Value = resp.json().await?;
        if let Some(arr) = keys.as_array() {
            if let Some(first) = arr.first() {
                let key = first.get("key").and_then(|x| x.as_str())
                    .or_else(|| first.get("api_key").and_then(|x| x.as_str()))
                    .unwrap_or("");
                if !key.is_empty() { return Ok(key.to_string()); }
            }
        }
    }

    // POST with retries
    for attempt in 0..max_retries {
        let resp = http.post(format!("{management_url}/api/api-keys"))
            .header("Authorization", &auth).send().await?;
        let status = resp.status().as_u16();
        if matches!(status, 200 | 201) {
            let v: serde_json::Value = resp.json().await?;
            let key = v.get("key").and_then(|x| x.as_str())
                .or_else(|| v.get("api_key").and_then(|x| x.as_str()))
                .unwrap_or("").to_string();
            if !key.is_empty() { return Ok(key); }
        }
        if attempt < max_retries - 1 {
            sleep(Duration::from_secs(1u64 << attempt)).await; // 2^attempt — management_api.py:152
        }
    }
    Err(CloudError::ApiKeyCreation(max_retries))
}
```

**Dependencies:** Step 2.

---

### Step 7 — `cloud_client.rs`: HTTP proxy for V2 operations

**File:** `crates/cloud/src/cloud_client.rs` (new) — ports `cloud_client.py`.

```rust
use std::sync::Arc;
use reqwest::multipart::{Form, Part};
use serde_json::{json, Map, Value};
use uuid::Uuid;
use crate::error::{CloudError, CloudResult};

#[derive(Debug)]
pub struct CloudClient {
    pub service_url: String,
    pub api_key: String,
    client: reqwest::Client,   // ~ aiohttp.ClientSession
}

impl CloudClient {
    pub fn new(service_url: impl Into<String>, api_key: impl Into<String>) -> Arc<Self> {
        let service_url = service_url.into().trim_end_matches('/').to_string();
        let api_key = api_key.into();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "X-Api-Key",
            reqwest::header::HeaderValue::from_str(&api_key)
                .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("")),
        );
        let client = reqwest::Client::builder().default_headers(headers).build()
            .expect("reqwest client build should succeed on a valid header value");
        Arc::new(Self { service_url, api_key, client })
    }

    /// Drop the HTTP client. Python closes the aiohttp session; reqwest
    /// cleans up when the struct is dropped, so this is a no-op, but it
    /// matches the Python API surface (cloud_client.py:33–36).
    pub async fn close(&self) {}

    /// GET /health — cloud_client.py:38–45
    pub async fn health_check(&self) -> bool {
        self.client.get(format!("{}/health", self.service_url))
            .send().await
            .map(|r| r.status() == 200)
            .unwrap_or(false)
    }

    /// POST /api/v1/remember — cloud_client.py:49–89
    pub async fn remember(
        &self,
        data: RememberData,
        dataset_name: &str,
        run_in_background: bool,
        custom_prompt: Option<&str>,
    ) -> CloudResult<Value> {
        let mut form = Form::new().text("datasetName", dataset_name.to_string());
        if run_in_background { form = form.text("run_in_background", "true"); }
        if let Some(p) = custom_prompt { form = form.text("custom_prompt", p.to_string()); }
        match data {
            RememberData::Text(s) => {
                form = form.part("data",
                    Part::bytes(s.into_bytes()).file_name("data.txt").mime_str("text/plain")?);
            }
            RememberData::Texts(items) => {
                for s in items {
                    form = form.part("data",
                        Part::bytes(s.into_bytes()).file_name("data.txt").mime_str("text/plain")?);
                }
            }
            RememberData::Files(paths) => {
                for path in paths {
                    let bytes = tokio::fs::read(&path).await?;
                    let name = path.file_name().and_then(|n| n.to_str())
                        .unwrap_or("upload").to_string();
                    form = form.part("data", Part::bytes(bytes).file_name(name));
                }
            }
        }
        self.post_multipart("remember", "/api/v1/remember", form).await
    }

    /// POST /api/v1/recall — cloud_client.py:91–112
    pub async fn recall(
        &self,
        query_text: &str,
        query_type: Option<&str>,
        datasets: Option<Vec<String>>,
        top_k: Option<usize>,
        system_prompt: Option<&str>,
    ) -> CloudResult<Value> {
        let mut body = Map::new();
        body.insert("query".into(), Value::String(query_text.into()));
        if let Some(t) = query_type { body.insert("search_type".into(), Value::String(t.into())); }
        if let Some(d) = datasets { body.insert("datasets".into(), json!(d)); }
        if let Some(k) = top_k { body.insert("top_k".into(), json!(k)); }
        if let Some(p) = system_prompt { body.insert("system_prompt".into(), Value::String(p.into())); }
        self.post_json("recall", "/api/v1/recall", Value::Object(body)).await
    }

    /// POST /api/v1/improve — cloud_client.py:114–135
    pub async fn improve(&self, dataset: ImproveDataset, run_in_background: bool, node_name: Option<&str>) -> CloudResult<Value> {
        let mut body = Map::new();
        match dataset {
            ImproveDataset::Id(id) => { body.insert("dataset_id".into(), Value::String(id.to_string())); }
            ImproveDataset::Name(n) => { body.insert("dataset_name".into(), Value::String(n)); }
        }
        if run_in_background { body.insert("run_in_background".into(), Value::Bool(true)); }
        if let Some(n) = node_name { body.insert("node_name".into(), Value::String(n.into())); }
        self.post_json("improve", "/api/v1/improve", Value::Object(body)).await
    }

    /// POST /api/v1/forget — cloud_client.py:137–157
    pub async fn forget(&self, everything: bool, dataset: Option<String>, data_id: Option<String>) -> CloudResult<Value> {
        let mut body = Map::new();
        if everything { body.insert("everything".into(), Value::Bool(true)); }
        if let Some(d) = dataset { body.insert("dataset".into(), Value::String(d)); }
        if let Some(d) = data_id { body.insert("data_id".into(), Value::String(d)); }
        self.post_json("forget", "/api/v1/forget", Value::Object(body)).await
    }

    // ---- helpers ----

    async fn post_multipart(&self, op: &'static str, path: &str, form: Form) -> CloudResult<Value> {
        let resp = self.client.post(format!("{}{}", self.service_url, path))
            .multipart(form).send().await?;
        self.read_json_or_error(op, resp).await
    }

    async fn post_json(&self, op: &'static str, path: &str, body: Value) -> CloudResult<Value> {
        let resp = self.client.post(format!("{}{}", self.service_url, path))
            .json(&body).send().await?;
        self.read_json_or_error(op, resp).await
    }

    async fn read_json_or_error(&self, op: &'static str, resp: reqwest::Response) -> CloudResult<Value> {
        let status = resp.status();
        if status.as_u16() >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(CloudError::RemoteOp { op, status: status.as_u16(), body });
        }
        Ok(resp.json().await?)
    }
}

// Input types — richer than Python's "Any" but map cleanly onto the existing cases.
#[derive(Debug)]
pub enum RememberData {
    Text(String),
    Texts(Vec<String>),
    Files(Vec<std::path::PathBuf>),
}

#[derive(Debug)]
pub enum ImproveDataset {
    Id(Uuid),
    Name(String),
}
```

Python returns `dict` for remember/improve/forget and `list` for recall. We return `serde_json::Value` in all four cases, matching Python's dynamic typing. Typed wrappers can be added later if consumers ask for them.

**Dependencies:** Step 2.

---

### Step 8 — `state.rs`: async-safe singleton

**File:** `crates/cloud/src/state.rs` (new) — ports `state.py`.

```rust
use std::sync::Arc;
use once_cell::sync::Lazy;
use tokio::sync::RwLock;
use crate::cloud_client::CloudClient;

static REMOTE_CLIENT: Lazy<RwLock<Option<Arc<CloudClient>>>> = Lazy::new(|| RwLock::new(None));

pub async fn get_remote_client() -> Option<Arc<CloudClient>> {
    REMOTE_CLIENT.read().await.clone()
}

pub async fn set_remote_client(client: Option<Arc<CloudClient>>) {
    *REMOTE_CLIENT.write().await = client;
}

pub async fn is_remote_mode() -> bool {
    REMOTE_CLIENT.read().await.is_some()
}
```

**Dependencies:** Step 7.

**Future integration point:** the existing V2 API functions in `crates/lib/src/api/{remember,recall,improve,forget}.rs` should call `cognee_cloud::state::get_remote_client()` at their entry and, if populated, forward to the corresponding `CloudClient` method instead of executing locally. That wiring is out of scope for this plan but this step provides the hook.

---

### Step 9 — `serve.rs`: orchestrator

**File:** `crates/cloud/src/serve.rs` (new) — ports `serve.py` (230 lines).

```rust
use std::env;
use std::sync::Arc;
use crate::{
    cloud_client::CloudClient,
    config::CloudConfig,
    credentials::{CloudCredentials, is_token_expired, load_credentials, save_credentials},
    device_auth::{device_code_login, extract_email_from_id_token, refresh_access_token},
    error::{CloudError, CloudResult},
    management_api::{create_tenant, get_current_tenant, get_or_create_api_key, get_service_url},
    state::set_remote_client,
};

#[derive(Debug, Default, Clone)]
pub struct ServeConfig {
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub management_url: Option<String>,
    pub auth0_domain: Option<String>,
    pub auth0_client_id: Option<String>,
    pub auth0_audience: Option<String>,
}

impl ServeConfig {
    pub fn direct(url: impl Into<String>) -> Self {
        Self { url: Some(url.into()), ..Default::default() }
    }
    pub fn cloud() -> Self { Self::default() }
    pub fn api_key(mut self, k: impl Into<String>) -> Self { self.api_key = Some(k.into()); self }
    // …plus `.management_url()`, `.auth0_domain()`, etc.
}

pub async fn serve(sc: ServeConfig) -> CloudResult<Arc<CloudClient>> {
    // serve.py:61–72 — resolve URL from arg or env.
    let service_url = sc.url.or_else(|| env::var("COGNEE_SERVICE_URL").ok());
    let api_key = sc.api_key.unwrap_or_else(|| env::var("COGNEE_API_KEY").unwrap_or_default());

    match service_url {
        Some(url) => serve_direct(&url, &api_key).await,
        None => serve_cloud(&sc).await,
    }
}

// serve.py:75–101
async fn serve_direct(service_url: &str, api_key: &str) -> CloudResult<Arc<CloudClient>> {
    let service_url = service_url.trim_end_matches('/');
    let client = CloudClient::new(service_url, api_key);

    if !client.health_check().await {
        tracing::warn!("Instance at {service_url} did not respond to health check");
    }

    // Persist for reconnect — serve.py:88–96
    let creds = CloudCredentials {
        service_url: service_url.to_string(),
        api_key: api_key.to_string(),
        email: "local".into(),
        ..Default::default()
    };
    save_credentials(&creds).await?;

    set_remote_client(Some(client.clone())).await;
    let mode = if service_url.contains("localhost") || service_url.contains("127.0.0.1")
        { "local" } else { "remote" };
    println!("  Connected to Cognee ({mode}) at {service_url}");
    Ok(client)
}

// serve.py:104–230
async fn serve_cloud(sc: &ServeConfig) -> CloudResult<Arc<CloudClient>> {
    let mut cfg = CloudConfig::from_env();
    // Apply per-call overrides.
    if let Some(d) = &sc.auth0_domain    { cfg.auth0_domain = d.clone(); }
    if let Some(c) = &sc.auth0_client_id { cfg.auth0_client_id = c.clone(); }
    if let Some(a) = &sc.auth0_audience  { cfg.auth0_audience = a.clone(); }
    if let Some(m) = &sc.management_url  { cfg.management_url = m.trim_end_matches('/').into(); }

    // Step 1: saved credentials (serve.py:136–173)
    if let Some(mut creds) = load_credentials().await {
        if !creds.service_url.is_empty() && !creds.api_key.is_empty() {
            if !is_token_expired(&creds) {
                let client = CloudClient::new(&creds.service_url, &creds.api_key);
                if client.health_check().await {
                    set_remote_client(Some(client.clone())).await;
                    println!("  Connected to Cognee Cloud at {}", creds.service_url);
                    return Ok(client);
                }
                tracing::warn!("Saved service URL unreachable, re-authenticating");
                client.close().await;
            } else if let Some(rt) = creds.refresh_token.clone() {
                match refresh_access_token(&cfg, &rt).await {
                    Ok(tok) => {
                        creds.access_token = tok.access_token;
                        if let Some(new_rt) = tok.refresh_token { creds.refresh_token = Some(new_rt); }
                        creds.expires_at = chrono::Utc::now().timestamp() as f64 + tok.expires_in as f64;
                        save_credentials(&creds).await?;
                        let client = CloudClient::new(&creds.service_url, &creds.api_key);
                        if client.health_check().await {
                            set_remote_client(Some(client.clone())).await;
                            println!("  Connected to Cognee Cloud at {}", creds.service_url);
                            return Ok(client);
                        }
                        client.close().await;
                    }
                    Err(e) => tracing::warn!("Token refresh failed, re-authenticating: {e}"),
                }
            }
        }
    }

    // Step 2: Device Code Flow (serve.py:175–180)
    println!("  Authenticating with Cognee Cloud...");
    let token = device_code_login(&cfg, None).await?;

    // Step 3: extract email (serve.py:183)
    let email = token.id_token.as_deref().and_then(extract_email_from_id_token);

    // Step 4: discover or create tenant (serve.py:186–193)
    let tenant = match get_current_tenant(&cfg.management_url, &token.access_token).await? {
        Some(t) => t,
        None => {
            let email_ref = email.as_deref().ok_or(CloudError::MissingEmailClaim)?;
            create_tenant(
                &cfg.management_url,
                &token.access_token,
                email_ref,
                std::time::Duration::from_secs(300),
                std::time::Duration::from_secs(5),
            ).await?
        }
    };

    // Step 5: service URL (serve.py:196)
    let service_url = get_service_url(&cfg.management_url, &token.access_token).await?;

    // Step 6: API key (serve.py:199)
    let api_key = get_or_create_api_key(&cfg.management_url, &token.access_token, 3).await?;

    // Step 7: save & connect (serve.py:202–230)
    let creds = CloudCredentials {
        access_token: token.access_token.clone(),
        refresh_token: token.refresh_token,
        expires_at: chrono::Utc::now().timestamp() as f64 + token.expires_in as f64,
        service_url: service_url.clone(),
        api_key: api_key.clone(),
        management_url: cfg.management_url.clone(),
        tenant_id: tenant.id.clone(),
        tenant_name: tenant.name.clone(),
        email: email.clone().unwrap_or_default(),
    };
    save_credentials(&creds).await?;

    let client = CloudClient::new(&service_url, &api_key);
    if !client.health_check().await {
        tracing::warn!("Service URL {service_url} not responding — may still be starting");
    }
    set_remote_client(Some(client.clone())).await;
    println!("  Connected to Cognee Cloud at {service_url}");
    if let Some(e) = &email { println!("  Tenant: {} ({})", tenant.name, e); }

    Ok(client)
}
```

**Dependencies:** Steps 2–8.

---

### Step 10 — `disconnect.rs`

**File:** `crates/cloud/src/disconnect.rs` (new) — ports `disconnect.py`.

```rust
use crate::{
    credentials::clear_credentials,
    error::CloudResult,
    state::{get_remote_client, set_remote_client},
};

pub async fn disconnect(clear_saved: bool) -> CloudResult<()> {
    if let Some(client) = get_remote_client().await {
        client.close().await;
        set_remote_client(None).await;
        tracing::info!("Disconnected from Cognee Cloud");
        println!("  Disconnected from Cognee Cloud. Operations now run locally.");
    } else {
        println!("  Not connected to Cognee Cloud.");
    }
    if clear_saved {
        clear_credentials().await?;
        println!("  Saved credentials cleared.");
    }
    Ok(())
}
```

**Dependencies:** Steps 4, 7, 8.

---

### Step 11 — `lib.rs`: public surface

**File:** `crates/cloud/src/lib.rs`

```rust
//! Cognee Cloud client — OAuth2 Device Code Flow, tenant discovery, and
//! a remote HTTP proxy for the V2 operations (remember/recall/improve/forget).

pub mod cloud_client;
pub mod config;
pub mod credentials;
pub mod device_auth;
pub mod disconnect;
pub mod error;
pub mod management_api;
pub mod serve;
pub mod state;

pub use cloud_client::{CloudClient, ImproveDataset, RememberData};
pub use config::CloudConfig;
pub use credentials::{CloudCredentials, clear_credentials, load_credentials, save_credentials};
pub use device_auth::{TokenResponse, device_code_login, refresh_access_token};
pub use disconnect::disconnect;
pub use error::{CloudError, CloudResult};
pub use management_api::{Tenant, create_tenant, get_current_tenant, get_or_create_api_key, get_service_url};
pub use serve::{ServeConfig, serve};
pub use state::{get_remote_client, is_remote_mode, set_remote_client};
```

---

### Step 12 — Wire into `cognee-lib`

**Modify:**

- `crates/lib/Cargo.toml` — add optional `cognee-cloud` dep and `cloud` feature (on by default to keep parity with Python).

```toml
[features]
default = [ ..., "cloud" ]
cloud = ["dep:cognee-cloud"]

[dependencies]
cognee-cloud = { path = "../cloud", optional = true }
```

- `crates/lib/src/api/mod.rs` — add `pub mod serve;`, re-exports.

- **New file** `crates/lib/src/api/serve.rs`:

```rust
//! Cloud connect/disconnect — re-exports from `cognee-cloud`.

#[cfg(feature = "cloud")]
pub use cognee_cloud::{
    CloudClient, CloudCredentials, CloudError, ServeConfig, disconnect, serve,
};

#[cfg(feature = "cloud")]
pub async fn serve_url(
    url: impl Into<String>,
    api_key: Option<impl Into<String>>,
) -> Result<std::sync::Arc<CloudClient>, CloudError> {
    let mut cfg = ServeConfig::direct(url);
    if let Some(k) = api_key { cfg = cfg.api_key(k); }
    serve(cfg).await
}

#[cfg(feature = "cloud")]
pub async fn serve_cloud() -> Result<std::sync::Arc<CloudClient>, CloudError> {
    serve(ServeConfig::cloud()).await
}
```

- `crates/lib/src/lib.rs` — add:

```rust
#[cfg(feature = "cloud")]
pub use crate::api::serve::{serve, serve_cloud, serve_url, disconnect, CloudClient, ServeConfig};
```

**Dependencies:** Step 11.

---

### Step 13 — CLI integration (optional, shippable after Step 12)

**Modify:** `crates/cli/src/cli.rs` and `crates/cli/src/main.rs` and add `crates/cli/src/commands/{serve,disconnect}.rs`.

```rust
// cli.rs additions
#[derive(Debug, Subcommand)]
pub enum Commands {
    // …existing…
    Serve(ServeArgs),
    Disconnect(DisconnectArgs),
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Direct URL — skips Auth0. Also overrides COGNEE_SERVICE_URL.
    #[arg(long)] pub url: Option<String>,
    #[arg(long = "api-key")] pub api_key: Option<String>,
    #[arg(long = "management-url")] pub management_url: Option<String>,
    #[arg(long = "auth0-domain")] pub auth0_domain: Option<String>,
    #[arg(long = "auth0-client-id")] pub auth0_client_id: Option<String>,
    #[arg(long = "auth0-audience")] pub auth0_audience: Option<String>,
}

#[derive(Debug, Args)]
pub struct DisconnectArgs {
    #[arg(long = "clear-saved", default_value_t = false)]
    pub clear_saved: bool,
}
```

Handler just bridges to `cognee_lib::serve(...)` / `cognee_lib::disconnect(...)`. Gate the enum variants and handlers behind `#[cfg(feature = "cloud")]` so non-cloud builds still compile.

**Dependencies:** Step 12.

---

## 4. Test Plan

### Unit tests — mock HTTP with `mockito`

Each module gets a peer `#[cfg(test)] mod tests` block.

| Module | Test | Approach |
|---|---|---|
| `credentials.rs` | round-trip save → load | `tempfile::TempDir`; set `$HOME` via the `temp-env` crate (or factor path resolution into a trait for injection). Assert every field round-trips incl. `refresh_token = None`. |
| `credentials.rs` | `is_token_expired` boundary | Set `expires_at = now + 30` → expired (60s buffer). `expires_at = now + 120` → not expired. `0.0` → expired. |
| `credentials.rs` | file perms 0o600 on Unix | `#[cfg(unix)]` check `metadata().permissions().mode() & 0o777 == 0o600`. |
| `device_auth.rs` | successful flow | `mockito::Server` stubs `/oauth/device/code` then `/oauth/token` (first returns `authorization_pending`, then 200). Assert returned `TokenResponse`. Use very short `interval` (override in the mocked response) to keep the test fast. |
| `device_auth.rs` | `slow_down` bumps interval | Assert second poll happens ≥ 5s later than first (measure via `tokio::time::pause()` + `advance`). |
| `device_auth.rs` | `expired_token` → `DeviceCodeExpired` | Stub returns `{"error":"expired_token"}` → assert error variant. |
| `device_auth.rs` | `access_denied` → `AuthDenied` | Same pattern. |
| `device_auth.rs` | JWT email extraction | Craft a fake `header.payload.sig` where payload is `{"email":"x@y.com"}` base64url-encoded without padding. Assert `Some("x@y.com")`. Malformed input returns `None`. |
| `management_api.rs` | `get_current_tenant` returns None on 404 | mockito 404. |
| `management_api.rs` | `create_tenant` polls until tenant appears | mockito: POST `/api/tenants` → 202, then GET `/api/tenants/current` returns 404 twice then 200. Use `tokio::time::pause()` so the 5s interval doesn't block the test. |
| `management_api.rs` | `email_to_tenant_name` determinism | Assert equal output for same email across runs; different output for different emails. Cross-check against the value Python produces for a known email (hard-coded fixture). |
| `management_api.rs` | `get_service_url` accepts either `service_url` or `url` field | mockito returns `{"url":"…"}` — verify accepted. |
| `management_api.rs` | `get_or_create_api_key` retry backoff | mockito: 500 twice then 201. Confirm three attempts; use paused time to assert backoff 1s, 2s. |
| `cloud_client.rs` | `remember` builds multipart with correct fields | mockito captures the request body (or use `wiremock` which exposes body matchers). Assert `Content-Disposition: form-data; name="data"; filename="data.txt"` present. |
| `cloud_client.rs` | recall/improve/forget serialize JSON correctly | mockito `expect_body_string_contains` on `"dataset_id"` etc. |
| `cloud_client.rs` | `X-Api-Key` header sent | mockito header matcher. |
| `cloud_client.rs` | non-2xx → `CloudError::RemoteOp` | Stub 500. |
| `state.rs` | set/get/clear through `Arc` | Straightforward. |
| `serve.rs` (direct) | happy path | mockito serves `/health` → 200. Temp `$HOME`. Verify credentials saved, singleton populated. |
| `serve.rs` (direct) | `localhost` prints "local" | Redirect stdout or capture via a dependency-injected writer. Simpler: skip output assertion and rely on behaviour. |
| `serve.rs` (cloud, refresh path) | valid cache → skip device flow | Pre-seed credentials file with non-expired token; mock only `/health`. Assert no Auth0 calls happen. |
| `serve.rs` (cloud, expired + refresh OK) | refresh path used | Pre-seed expired creds with `refresh_token`. Mock refresh endpoint returns new token. Assert updated file and `/health` hit. |
| `serve.rs` (cloud, full flow) | device code → tenant → service URL → api key | Full mockito server with all endpoints wired; pause time to skip polls. Assert final `CloudClient` uses returned `service_url` / `api_key`. |
| `disconnect.rs` | clears singleton | Install a dummy client via `set_remote_client`, call `disconnect(false)`, expect `None`. |
| `disconnect.rs` | `clear_saved=true` deletes file | Temp `$HOME`, write fake creds, call `disconnect(true)`, assert file removed. |
| `disconnect.rs` | idempotent | Call twice without setup — no panic, no error. |

### Manual verification — requires a live Auth0 tenant and Cognee Cloud

Cannot be unit-tested:

1. Real device-code flow against `cognee.eu.auth0.com` (set `COGNEE_AUTH0_DEVICE_CLIENT_ID`, run `cognee serve`, click through the browser).
2. Real tenant provisioning polling (5-minute window against `api.dev.cloud.topoteretes.com`).
3. Cross-SDK cache sharing: run Python `cognee.serve()`, then Rust `cognee serve` (no args) — should pick up the cached creds and skip auth. And vice versa.
4. Refresh flow end-to-end: wait until `expires_at` passes, reconnect, observe refresh.
5. `remember` multipart upload with a real binary file against a live service.

### CI integration

- Unit tests run in `scripts/run_tests_with_openai.sh` (no cloud creds needed — all mocked).
- Gate manual tests behind env var `COGNEE_CLOUD_E2E=1` and skip otherwise, matching the existing pattern (`crates/embedding` tests check for `COGNEE_E2E_EMBED_MODEL_PATH` and skip).

---

## 5. Effort Breakdown

Budget totals **~260 hours / 5.5 weeks** for one engineer, matching the gap-doc range.

| Module / work item | Hours | Notes |
|---|---:|---|
| Step 1: crate scaffolding, workspace wiring | 4 | Also update workspace Cargo.toml and add `once_cell`. |
| Step 2: `error.rs` | 4 | Mirror all raise sites. |
| Step 3: `config.rs` | 4 | Plus env-override plumbing tests. |
| Step 4: `credentials.rs` + cross-platform path + perms + round-trip tests | 20 | Cross-SDK byte-level compat with Python is the tricky bit. |
| Step 5: `device_auth.rs` (Device Code Flow + refresh + JWT decode) + mock tests | 50 | Largest single module. Timing-sensitive tests need `tokio::time::pause`. |
| Step 6: `management_api.rs` (tenant + API key) + polling tests | 30 | Long polls in test need paused time. |
| Step 7: `cloud_client.rs` (4 V2 ops + multipart + error mapping) + mock tests | 40 | Multipart body wiring is fiddly; need to match Python's field names exactly. |
| Step 8: `state.rs` singleton | 4 |  |
| Step 9: `serve.rs` orchestrator (direct + cloud + refresh branches) + tests | 40 | Lots of integration glue; mock every branch. |
| Step 10: `disconnect.rs` + tests | 6 |  |
| Step 11: `lib.rs` exports | 2 |  |
| Step 12: `cognee-lib` integration, feature flag, re-exports | 10 | Compile under all feature-combos in CI. |
| Step 13: CLI subcommands | 16 | Small; output polish + help text. |
| Docs: README for `crates/cloud`, examples under `examples/` | 20 | At least one runnable example per mode. |
| CI + full-check suite adjustments | 10 | `scripts/check_all.sh`, ensure feature matrix builds. |
| Contingency (~15%) | 30 |  |
| **Total** | **~290** |  |

---

## 6. Out of Scope

The following are **deliberately excluded** from this plan and should be tracked as follow-up issues:

1. **Keyring / OS-keychain credential storage** — Python also uses filesystem JSON. Parity first; secure-storage alternative later (e.g. `keyring` crate).
2. **Automatic browser launch** — Python prints the URL and expects the user to open it. Rust does the same. No `open`/`webbrowser` crate dependency.
3. **Starting a local HTTP server** — `serve()` is a *client-side connector*, not an HTTP server. Despite the name, no `axum`/`actix`/`warp` is needed. If Rust ever needs to expose local endpoints, that is a separate gap.
4. **Routing V2 operations (`remember`/`recall`/`improve`/`forget`) through the cloud client** — Step 8 provides the `get_remote_client()` hook, but wiring each of the four API functions to consult it and forward is a separate, larger task. The current plan delivers the connector and singleton; actual routing lives under each operation's implementation plan.
5. **Retry/backoff on transient HTTP errors in `CloudClient`** — Python does none. We match Python. Users can wrap calls themselves.
6. **Typed response structs for `remember`/`recall`/`improve`/`forget`** — Return `serde_json::Value` (Python returns dict/list). Can be tightened later without breaking the wire format.
7. **Cloud-mode E2E CI job** — No test Auth0 tenant available in CI. Unit tests via mockito cover the full state machine; end-to-end is manual.
8. **Windows file-permission hardening** — On Unix we `chmod 600`. On Windows we rely on default user profile ACLs (same as Python, which silently no-ops the chmod there).
9. **Alternative transports** — WebSocket, server-sent events, gRPC. All V2 ops are plain HTTPS, matching Python.

---

## 7. Scope Recommendation

**Ship as a separate crate `cognee-cloud` behind the `cloud` feature flag on `cognee-lib`.**

Rationale:

- Auth0 + OAuth2 + a dozen HTTP endpoints is a cohesive concern that does not belong in the core knowledge-graph pipeline.
- Embedded / Android builds (`android-default` feature) can now opt out: `cargo build -p cognee-lib --no-default-features --features android-default` pulls *zero* cloud code.
- The SDK-as-drop-in-replacement goal (≥90% Python parity) is served by enabling `cloud` in the **default** feature set — users running `cargo add cognee-lib` get `cognee::serve()` out of the box.
- Keeps CI matrix linear: build with and without `cloud`.
- The crate is self-contained — no dependencies on `cognee-cognify`, `cognee-search`, or any pipeline crate. Only `reqwest`, `tokio`, `serde`, `uuid`, `dirs`, `base64`, `once_cell`, `thiserror`, `chrono`, `tracing`.
- Follows the pattern already established by `cognee-ontology`, `cognee-session`, etc. — one crate per concern.

### Critical Files for Implementation

- `/home/dmytro/dev/cognee/cognee-rust/crates/cloud/src/serve.rs` *(new)*
- `/home/dmytro/dev/cognee/cognee-rust/crates/cloud/src/device_auth.rs` *(new)*
- `/home/dmytro/dev/cognee/cognee-rust/crates/cloud/src/management_api.rs` *(new)*
- `/home/dmytro/dev/cognee/cognee-rust/crates/cloud/src/cloud_client.rs` *(new)*
- `/home/dmytro/dev/cognee/cognee-rust/crates/lib/Cargo.toml` *(modify: add `cloud` feature + optional dep)*