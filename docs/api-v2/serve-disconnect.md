# API v2: `serve()` / `disconnect()`

**Python source:** `cognee/api/v1/serve/` (~8 files, 570 LOC total)  
**Rust status:** **Implemented** (feature-gated `cloud`, CLI-enabled by default)  
**Implementation plan:** [impl/serve-disconnect-plan.md](impl/serve-disconnect-plan.md)

---

## 1. What `serve()` does

`serve()` is an async orchestrator that connects the local Cognee SDK to a remote Cognee Cloud instance or a local backend. It has **two modes**:

### Direct Mode (local/remote with URL)
Called when `url` is explicitly provided or `COGNEE_SERVICE_URL` env var is set:
```python
await cognee.serve(url="http://localhost:8000")
await cognee.serve(url="https://tenant.cognee.ai", api_key="ck_...")
```

**Direct mode flow** (Python: `/tmp/cognee-python/cognee/api/v1/serve/serve.py:75–101`):
1. Strips trailing `/` from URL
2. Creates `CloudClient(service_url, api_key)` — thin async HTTP wrapper
3. Performs health check via GET `/health` (optional warning on failure)
4. Saves credentials to `~/.cognee/cloud_credentials.json` for reconnection
5. Sets module-level singleton `_remote_client` (state.py)
6. Returns `CloudClient` instance; prints "Connected to Cognee (local|remote) at {url}"

### Cloud Mode (Auth0 Device Code Flow)
Called when `url` is **not** provided — runs the full OAuth 2.0 Device Code Flow:
```python
await cognee.serve()  # Triggers interactive auth
```

**Cloud mode flow** (Python: `serve.py:104–230`):

**Step 1: Load cached credentials** (if exists and not expired)
- Tries to load from `~/.cognee/cloud_credentials.json`
- If `access_token` is valid and `service_url` is reachable → returns immediately with cached client
- If token expired but `refresh_token` exists → attempts refresh via `refresh_access_token()` helper
- On refresh failure or unreachable URL → proceeds to Step 2

**Step 2: Device Code Flow** (RFC 8628)
- Calls `device_code_login()` which:
  - Makes POST to `https://{auth0_domain}/oauth/device/code` with `client_id`, `scope`, `audience`
  - Returns `device_code`, `user_code`, `verification_uri_complete`, expiry window (~15 min default)
  - Prints URL and user code to terminal; prints "Waiting for authorization..."
  - Polls `https://{auth0_domain}/oauth/token` every 5 seconds with grant type `urn:ietf:params:oauth:grant-type:device_code`
  - Handles errors: `authorization_pending` (continue), `slow_down` (backoff), `expired_token` (timeout), `access_denied` (user rejected)
  - On success → returns `TokenResponse` with `access_token`, `refresh_token` (if offline_access), `id_token`, `expires_in`

**Step 3: Extract email from JWT**
- Decodes `id_token` JWT payload (base64 decode, no signature verification) to extract `email` claim
- Needs `openid profile email` in OAuth scope for this to work

**Step 4: Discover or create tenant** (via Management API)
- Calls `get_current_tenant(mgmt_url, access_token)` → GET `/api/tenants/current`
- If tenant exists → use it
- If not found (404) → call `create_tenant(mgmt_url, access_token, email)`
  - Generates deterministic tenant name from email: `tenant-{uuid5(NAMESPACE_URL, email)}`
  - POST `/api/tenants?tenant_name={...}`
  - Polls `get_current_tenant()` for up to 5 minutes (polling every 5s) until tenant is provisioned

**Step 5: Fetch service URL** (tenant's Cognee instance)
- Calls `get_service_url(mgmt_url, access_token)` → GET `/api/tenants/current/service-url`
- Returns `{service_url}` or `{url}` field from JSON response
- Raises error if empty (tenant still provisioning)

**Step 6: Get or create API key**
- Calls `get_or_create_api_key(mgmt_url, access_token)`
- GET `/api/api-keys` → if list exists and has items, return first key
- If not found or empty → POST `/api/api-keys` with retries (3 attempts, exponential backoff 2^n seconds)
- Returns `{key}` or `{api_key}` field

**Step 7: Save and connect**
- Saves full `CloudCredentials` dataclass to `~/.cognee/cloud_credentials.json` (600 mode)
- Creates `CloudClient(service_url, api_key)`
- Health check (warning if fails)
- Sets `_remote_client` singleton
- Prints "Connected to Cognee Cloud at {service_url}" and "Tenant: {tenant_name} ({email})"
- Returns `CloudClient`

### Return value
Both modes return a **`CloudClient` instance** — async HTTP wrapper with methods:
- `async remember(data, dataset_name="main_dataset", **kwargs) → dict` — POST `/api/v1/remember`
- `async recall(query_text, query_type=None, **kwargs) → list` — POST `/api/v1/recall`
- `async improve(dataset="main_dataset", **kwargs) → dict` — POST `/api/v1/improve`
- `async forget(**kwargs) → dict` — POST `/api/v1/forget`
- `async close()` — closes aiohttp ClientSession

### How it integrates with the SDK
Once `serve()` sets `_remote_client`, all V2 operations (remember, recall, improve, forget) check `is_remote_mode()` and route to the cloud client instead of running locally. The SDK uses a **module-level singleton** pattern (state.py) — no explicit context passing.

---

## 2. What `disconnect()` does

Tears down the cloud connection and reverts to local execution.

**Code** (Python: `/tmp/cognee-python/cognee/api/v1/serve/disconnect.py:8–36`):

```python
async def disconnect(clear_saved: bool = False) -> None:
```

**Steps:**
1. Calls `get_remote_client()` — checks if singleton is set
2. If connected:
   - Calls `await client.close()` — closes aiohttp ClientSession
   - Sets `_remote_client = None`
   - Logs "Disconnected from Cognee Cloud"
   - Prints "Disconnected from Cognee Cloud. Operations now run locally."
3. If not connected → prints "Not connected to Cognee Cloud."
4. If `clear_saved=True`:
   - Calls `clear_credentials()` — deletes `~/.cognee/cloud_credentials.json` file
   - Prints "Saved credentials cleared."

**Semantics:**
- **Non-destructive by default** — credentials remain saved, so `serve()` can reconnect without re-authenticating
- **Explicit cleanup** — pass `clear_saved=True` to revoke/forget saved creds (e.g., on logout)
- **Idempotent** — safe to call when already disconnected

---

## 3. Building blocks (Python)

### A. `CloudClient` (`cloud_client.py`)
**Purpose:** Thin async HTTP proxy for remote Cognee Cloud instance.  
**Type:** Class, not a trait.  
**Key methods:**
- `__init__(service_url: str, api_key: str)` — stores URL and API key
- `_get_session() → aiohttp.ClientSession` — lazy singleton HTTP session with `X-Api-Key` header auth
- `close()` — closes session
- `_health_check() → bool` — GET `/health`, returns `status == 200`
- `remember()`, `recall()`, `improve()`, `forget()` — V2 operations, POST to `/api/v1/{op}`

**External service:** Cognee Cloud instance at any HTTP(S) URL.

### B. `credentials.py`
**Purpose:** Persist and restore OAuth tokens and service metadata.  
**Type:** Functions + `CloudCredentials` dataclass.  
**Dataclass fields:**
```python
@dataclass
class CloudCredentials:
    access_token: str
    refresh_token: Optional[str] = None
    expires_at: float = 0.0                    # Unix timestamp, checked via `time.time()`
    service_url: str = ""
    api_key: str = ""
    management_url: str = ""
    tenant_id: str = ""
    tenant_name: str = ""
    email: str = ""
```

**Storage:** `~/.cognee/cloud_credentials.json` (600 permissions, JSON format)  
**Functions:**
- `load_credentials() → Optional[CloudCredentials]` — reads and deserializes JSON
- `save_credentials(creds: CloudCredentials) → None` — serializes and writes (creates dir if missing)
- `clear_credentials() → None` — deletes file
- `is_token_expired(creds: CloudCredentials) → bool` — checks `expires_at - 60` (60s buffer)
- `get_credentials_path() → Path` — returns `~/.cognee/cloud_credentials.json`

**External service:** File system only.

### C. `device_auth.py`
**Purpose:** RFC 8628 OAuth 2.0 Device Code Flow implementation.  
**Defaults:**
- `DEFAULT_AUTH0_DOMAIN = "cognee.eu.auth0.com"`
- `DEFAULT_AUTH0_AUDIENCE = "cognee:api"`
- `DEFAULT_SCOPE = "openid profile email offline_access"`

**Env vars (overrides):**
- `COGNEE_AUTH0_DOMAIN`
- `COGNEE_AUTH0_DEVICE_CLIENT_ID` (required, no default)
- `COGNEE_AUTH0_AUDIENCE`

**Functions:**
- `device_code_login(domain=None, client_id=None, audience=None, scope=DEFAULT_SCOPE) → TokenResponse` — main entry point
  - POST `/oauth/device/code` with `client_id`, `scope`, `audience`
  - Prints URL + user code (or just URL if `verification_uri_complete` is provided)
  - Polls `/oauth/token` with grant type `urn:ietf:params:oauth:grant-type:device_code` every 5s
  - Handles `authorization_pending`, `slow_down`, `expired_token`, `access_denied` errors
  - Returns token on success
  
- `refresh_access_token(refresh_token: str, domain=None, client_id=None) → TokenResponse` — refresh expired token
  - POST `/oauth/token` with grant type `refresh_token`
  - Returns new token set
  
- `extract_email_from_id_token(id_token: str) → Optional[str]` — JWT decoding (no validation)
  - Splits JWT on `.`, decodes payload base64 (adds padding), parses JSON
  - Extracts `email` claim

**Return type:**
```python
@dataclass
class TokenResponse:
    access_token: str
    refresh_token: Optional[str] = None
    id_token: Optional[str] = None
    token_type: str = "Bearer"
    expires_in: int = 3600
```

**External services:**
- Auth0 control plane: `https://{auth0_domain}/oauth/device/code`, `/oauth/token`
- Prints to stdout (no UI integration)

### D. `management_api.py`
**Purpose:** Tenant discovery, provisioning, and API key management via Cognee Cloud Management API.  
**Default URL:** `https://api.dev.cloud.topoteretes.com`
**Env var override:** `COGNEE_CLOUD_URL`

**Dataclass:**
```python
@dataclass
class Tenant:
    id: str
    name: str
```

**Functions:**
- `get_current_tenant(management_url: str, access_token: str) → Optional[Tenant]`
  - GET `/api/tenants/current`
  - Returns `Tenant` or `None` if 404
  - Raises on other HTTP errors

- `create_tenant(management_url: str, access_token: str, email: str, poll_timeout: int = 300, poll_interval: int = 5) → Tenant`
  - Generates tenant name: `tenant-{uuid5(NAMESPACE_URL, email)}` (matches frontend convention)
  - POST `/api/tenants?tenant_name={...}`
  - Polls `get_current_tenant()` for 5 minutes (every 5s) until tenant appears
  - Prints "Provisioning tenant (this may take a minute)..."
  - Raises `TimeoutError` if not ready within timeout

- `get_service_url(management_url: str, access_token: str) → str`
  - GET `/api/tenants/current/service-url`
  - Returns `service_url` or `url` field
  - Raises if empty

- `get_or_create_api_key(management_url: str, access_token: str, max_retries: int = 3) → str`
  - GET `/api/api-keys` — return first key if list not empty
  - If empty/404 → POST `/api/api-keys` with retries (exponential backoff)
  - Returns `key` or `api_key` field

**External service:**
- Cognee Cloud Management API: `https://api.dev.cloud.topoteretes.com` (configurable)
- Endpoints: `/api/tenants/current`, `/api/tenants`, `/api/tenants/current/service-url`, `/api/api-keys`

### E. `state.py`
**Purpose:** Module-level singleton for the remote client.  
**Type:** Module-level variables + accessor functions.

**Module state:**
```python
_remote_client: Optional["CloudClient"] = None
```

**Functions:**
- `get_remote_client() → Optional[CloudClient]` — returns singleton
- `set_remote_client(client: Optional[CloudClient]) → None` — sets singleton
- `is_remote_mode() → bool` — returns `_remote_client is not None`

**No traits, no persistence — pure in-memory runtime state.**

---

## 4. Rust status per building block

| Component | Status | Location | Notes |
|-----------|--------|----------|-------|
| **HTTP Server Framework** | N/A | — | `serve()` is a client-side orchestrator, not an HTTP server. Reuses `reqwest` only. |
| **OAuth2 Device Code Flow** | Implemented (C2 `8624a3f`) | `crates/cloud/src/device_auth.rs` | `reqwest`-based RFC 8628 flow with manual JWT-payload decode (no signature verification, matching Python). |
| **Credentials Storage** | Implemented (C1 `ac8c86f`) | `crates/cloud/src/credentials.rs` | `~/.cognee/cloud_credentials.json`, `0o600` on Unix, identical on-disk schema to Python. |
| **CloudClient HTTP Wrapper** | Implemented (C3 `e94e9f4`) | `crates/cloud/src/cloud_client.rs` | `reqwest::Client` with `X-Api-Key` default header; remember/recall/improve/forget + `/health`. |
| **Tenant Management API Client** | Implemented (C2 `8624a3f`) | `crates/cloud/src/management_api.rs` | Tenant discovery + provisioning (polling) + service-URL + API-key. Deterministic `uuid5(NAMESPACE_URL, email)` tenant names. |
| **Auth0 Integration** | Implemented (C1 `ac8c86f`) | `crates/cloud/src/config.rs` | Env-var-driven defaults (`COGNEE_AUTH0_DOMAIN` / `_DEVICE_CLIENT_ID` / `_AUDIENCE`, `COGNEE_CLOUD_URL`). |
| **Module-level Singleton (remote_client)** | Implemented (C3 `e94e9f4`) | `crates/cloud/src/state.rs` | `std::sync::LazyLock<tokio::sync::RwLock<Option<Arc<CloudClient>>>>` with Python-compat aliases. |
| **`serve()` / `disconnect()` orchestrators** | Implemented (C4 `7c04dcb`) | `crates/cloud/src/{serve,disconnect}.rs` | Direct + cloud mode, cached-credentials short-circuit, refresh path, tenant provisioning. |
| **`cognee-lib` re-exports + feature gate** | Implemented (C4 `7c04dcb`) | `crates/lib/src/lib.rs` | `cognee::{serve, disconnect, ServeConfig, ...}` gated on `cloud` (default-enabled). |
| **CLI subcommand** | Implemented (C5 this commit) | `crates/cli/src/cli.rs`, `crates/cli/src/commands/{serve,disconnect}.rs` | `cognee-cli serve [--url … \| --auth0-* …]` and `cognee-cli disconnect [--wipe-credentials]`. |

---

## 5. Gaps — what Rust needs

### 5.1 HTTP Server Framework (if serving locally)
**Clarification:** The Python `serve()` does **not start an HTTP server**. It *connects* to an existing Cognee Cloud instance (or local backend). If Rust is meant to have local multi-tenant support via HTTP, a server is needed. If it's only SDK-level cloud integration, it's not.

**Assumed scope:** SDK-level cloud client (no local HTTP server). If local server is needed, add one of:
- **`axum`** — modern, composable, async-first (recommended for Rust ecosystem)
- **`actix-web`** — mature, high-performance
- **`warp`** — filter-based, lightweight

### 5.2 OAuth2 Device Code Flow
- Add dependency: `oauth2` crate (IETF spec-compliant)
- Or manually implement with `reqwest` (as Python does)
- Implement `device_code_login()` and `refresh_access_token()` equivalents
- JWT decoding: use `jsonwebtoken` or just base64 decode + serde_json (Python does no validation)

### 5.3 Credentials Storage Module
- Create new crate: `cognee-cloud-credentials` or add to `cognee-cli`
- Trait: `CredentialsStore` with impls:
  - `FileSystemStore` (saves to `~/.cognee/cloud_credentials.json`)
  - `MockStore` (testing)
- Dataclass equivalent: `CloudCredentials` struct with fields matching Python
- Functions: `load()`, `save()`, `clear()`, `is_expired()`
- File permissions: 600 (via `std::fs::Permissions`)

### 5.4 CloudClient HTTP Wrapper
- Create new crate: `cognee-cloud-client` or add to `cognee-cli`
- Struct: `CloudClient(service_url: String, api_key: String)`
- Methods:
  - `new()` → Self
  - `async remember()`, `async recall()`, `async improve()`, `async forget()` → V2 operations
  - `async _health_check() → bool`
  - `async close()`
- Use `reqwest::Client` with persistent session and `X-Api-Key` header

### 5.5 Management API Client
- Add to `cognee-cloud-client` or separate crate
- Functions:
  - `async get_current_tenant(url, token) → Option<Tenant>`
  - `async create_tenant(url, token, email) → Tenant` (with polling)
  - `async get_service_url(url, token) → String`
  - `async get_or_create_api_key(url, token) → String`
- Uses same `reqwest::Client` with Bearer token in `Authorization` header

### 5.6 Auth0 Integration
- Env vars (new):
  - `COGNEE_AUTH0_DOMAIN` (default: `cognee.eu.auth0.com`)
  - `COGNEE_AUTH0_DEVICE_CLIENT_ID` (required for cloud mode)
  - `COGNEE_AUTH0_AUDIENCE` (default: `cognee:api`)
  - `COGNEE_CLOUD_URL` (default: `https://api.dev.cloud.topoteretes.com`)
- Config struct to hold these, auto-populate from env

### 5.7 Remote Client Singleton
- Use `once_cell::sync::Lazy<Mutex<Option<Arc<CloudClient>>>>`  or  `tokio::sync::Mutex`
- Or pass `Arc<CloudClient>` through pipeline context (preferred for testability)
- Functions: `get_remote_client()`, `set_remote_client()`

### 5.8 CLI Integration
- Add `Serve` and `Disconnect` variants to `Commands` enum in `crates/cli/src/cli.rs`
- Structures:
  ```rust
  Commands::Serve(ServeArgs),
  Commands::Disconnect(DisconnectArgs),
  ```
- Args:
  - `ServeArgs { url: Option<String>, api_key: Option<String>, ... }`
  - `DisconnectArgs { clear_saved: bool }`
- Handlers in `crates/cli/src/commands/{serve,disconnect}.rs`

### 5.9 State Integration
- `cognee-lib` exports `serve()` and `disconnect()` functions
- These manage the module-level singleton
- V2 operation functions (remember, recall, improve, forget) check singleton and route to cloud client if set

---

## 6. Effort estimate & scope note

**Estimated effort: L (Large, 5–10 weeks for one engineer)**

### Breakdown:
- OAuth2 Device Code Flow (60–90h): research Auth0 API, implement device polling, JWT decode, error handling
- Credentials storage + file management (20–30h): create module, tests, cross-platform path handling
- CloudClient HTTP wrapper (30–40h): implement 4 V2 methods (remember, recall, improve, forget), multipart form handling
- Management API client (20–30h): tenant discovery, provisioning loop, polling
- CLI integration + testing (40–60h): add commands, wire into singleton, integration tests, mocking
- Documentation + examples (20–30h)

**Total: ~250–300 engineering hours (~5–7 weeks)**

### Key challenges:
1. **Async/await complexity** — managing session lifecycle, token refresh, polling loops in async Rust
2. **Error handling** — OAuth state machine has many failure paths (slow_down, expired_token, access_denied)
3. **File I/O + permissions** — safe credential storage with 600 mode
4. **Singleton pattern** — Rust's type system makes module-level state awkward; prefer passing context
5. **Testing** — mocking Auth0, Management API, and credential store; no user interaction in tests

### Scope consideration: Is this out-of-scope?

**Arguably YES for a library-level port**, because:
- `serve()` / `disconnect()` are **cloud integrations**, not core knowledge graph operations
- They add **authentication complexity** (OAuth2) and **external service dependencies** (Auth0, Management API)
- The Python reference implements them as **optional V2 features** — users can ignore them and run locally
- The Rust port already has full local pipeline (add → cognify → search) and is feature-complete for on-device use

**Keep in scope IF:**
- The Rust port is marketed as a "drop-in replacement" (stated goal: 90%+ parity)
- Cognee Cloud is a primary deployment target
- Users expect SDK-level cloud connectivity

**Deprioritize IF:**
- The Rust port is focused on edge/embedded (Android, embedded Linux)
- Users are expected to run local instances or implement cloud integration themselves
- Team bandwidth is limited

### Recommendation:
Implement this as a **separate optional crate** (`cognee-cloud`) with feature flag `cloud-integration` so that:
- Core library remains lightweight
- Cloud integration is opt-in (e.g., `cargo build --features cloud-integration`)
- CLI can expose `serve` / `disconnect` subcommands only if feature is enabled

---

## Summary Table

| Aspect | Python | Rust |
|--------|--------|------|
| **Mode: Direct** | Yes, fully implemented | ~~Missing~~ — done in C4 (`7c04dcb`) |
| **Mode: OAuth Cloud** | Yes, fully implemented | ~~Missing~~ — done in C2 + C4 (`8624a3f`, `7c04dcb`) |
| **Credentials storage** | `~/.cognee/cloud_credentials.json` | ~~Missing~~ — done in C1 (`ac8c86f`); byte-compatible JSON, `0o600` on Unix |
| **Device Code Flow** | Implemented, RFC 8628 compliant | ~~Missing~~ — done in C2 (`8624a3f`); manual JWT-payload decode (no signature verification), matching Python |
| **CloudClient HTTP wrapper** | `aiohttp.ClientSession` with `X-Api-Key` | ~~Missing~~ — done in C3 (`e94e9f4`); `reqwest::Client` with `X-Api-Key` default header |
| **Management API client** | Tenant discovery, provisioning, API key retrieval | ~~Missing~~ — done in C2 (`8624a3f`); `uuid5(NAMESPACE_URL, email)` tenant names |
| **Singleton pattern** | Module-level `_remote_client` | ~~Missing~~ — done in C3 (`e94e9f4`); `std::sync::LazyLock<tokio::sync::RwLock<Option<Arc<CloudClient>>>>` |
| **CLI subcommands** | Yes (`serve`, `disconnect`) | ~~Missing~~ — done in C5 (this commit); `cognee-cli serve` / `disconnect`, `cloud` feature-gated (default on) |
| **External services** | Auth0, Cognee Cloud Management API | Reachable (same endpoints); can share `~/.cognee/cloud_credentials.json` with Python SDK |
| **Test coverage** | Manual (cloud services), fixtures | ~~Would need mocks~~ — ~95 unit + 9 integration tests via `mockito`; manual live-Auth0 path documented but gated |

---

## Implementation notes

### Commit breakdown

| Commit | Scope |
|---|---|
| `ac8c86f` (C1) | Scaffold the `cognee-cloud` crate: `error.rs`, `config.rs` (Auth0 / cloud URL defaults + env overrides), `credentials.rs` (on-disk JSON store, `~/.cognee/cloud_credentials.json`, `0o600`). |
| `8624a3f` (C2) | `device_auth.rs` (RFC 8628 Device Code flow, JWT-payload decode, refresh flow) and `management_api.rs` (tenant discovery / provisioning, service-URL, API-key retrieval). |
| `e94e9f4` (C3) | `cloud_client.rs` (reqwest proxy with `X-Api-Key` header for remember / recall / improve / forget + `/health`) and `state.rs` (`LazyLock<RwLock<Option<Arc<CloudClient>>>>` singleton with Python-compat aliases). |
| `7c04dcb` (C4) | `serve.rs` (direct + cloud mode orchestrator with cached-creds short-circuit + refresh path + tenant provisioning) and `disconnect.rs` (tear-down + optional wipe). Also wires `cognee-lib` re-exports behind the `cloud` feature (default-enabled). |
| C5 (this commit) | `cognee-cli serve` / `disconnect` subcommands (feature-gated on `cloud`) + integration tests in `crates/cloud/tests/` + `crates/cli/tests/cli_serve_subcommand.rs`. |

### Summary

The new `cognee-cloud` crate ports `cognee/api/v1/serve/` byte-for-byte where the on-disk or wire format matters. Credentials live at `~/.cognee/cloud_credentials.json` (owner-only on Unix) and use the same JSON schema as the Python SDK, so both clients can share one file. Auth0 uses an RFC 8628 device-code flow with a manual base64url JWT-payload decode — no signature verification, matching Python's `extract_email_from_id_token`. Tenant provisioning mirrors Python's deterministic `uuid5(NAMESPACE_URL, email)` naming. The HTTP proxy sends every request with an `X-Api-Key` header (not `Bearer`) per the Python reference. Process-wide routing uses `std::sync::LazyLock<tokio::sync::RwLock<Option<Arc<CloudClient>>>>`: `LazyLock` is stable on edition 2024, and the `tokio` lock keeps awaiting non-blocking. `ServeConfig` is a builder with two construction helpers — `direct(url)` for local/remote connections and `cloud()` for the full device flow. The CLI surface (`cognee-cli serve` / `disconnect`) is feature-gated on `cloud`, which is default-enabled on both `cognee-lib` and `cognee-cli`.

### Deviations from plan

- **Auth header is `X-Api-Key`, not `Bearer`.** The plan occasionally says `Bearer` (§2), but the Python reference uses `X-Api-Key` throughout the cloud-proxy requests (management API calls still use `Authorization: Bearer <access_token>`). Rust matches Python here for wire parity.
- **`CloudClient::new` returns `CloudResult<Arc<Self>>`, not an infallible constructor.** This lets invalid API-key header bytes (newlines / non-ASCII) surface cleanly rather than panic. Every call site was updated to `?` the result.
- **Both `set_client` / `set_remote_client` are exported.** The prompt and plan use slightly different names (`set_client` in the Rust-native surface, `set_remote_client` in the Python-compat surface). Both now alias the same write operation so downstream callers can pick whichever reads better.
- **Constructor validates header chars.** Per previous point — the plan's infallible signature was incompatible with `reqwest::HeaderValue::from_str` error handling.
- **`extract_email_from_id_token` returns `CloudResult<String>`, not `Option<String>`.** The Python reference returns `None` on decode / missing-claim failures; the Rust version surfaces them as `CloudError::InvalidToken` so the cloud-mode orchestrator can decide to `ok()` the result where `None` is acceptable (serve.rs does exactly this when the access token is valid but no email claim is present).

### Deferred items

None. All 13 steps of the implementation plan landed.

---

## Files Referenced

**Python source:**
- `/tmp/cognee-python/cognee/api/v1/serve/serve.py` (230 lines)
- `/tmp/cognee-python/cognee/api/v1/serve/disconnect.py` (35 lines)
- `/tmp/cognee-python/cognee/api/v1/serve/cloud_client.py` (158 lines)
- `/tmp/cognee-python/cognee/api/v1/serve/credentials.py` (68 lines)
- `/tmp/cognee-python/cognee/api/v1/serve/device_auth.py` (186 lines)
- `/tmp/cognee-python/cognee/api/v1/serve/management_api.py` (155 lines)
- `/tmp/cognee-python/cognee/api/v1/serve/state.py` (26 lines)

**Rust codebase:**
- `/home/dmytro/dev/cognee/cognee-rust/crates/cli/src/cli.rs:14–26` — `Commands` enum (no Serve/Disconnect)
- `/home/dmytro/dev/cognee/cognee-rust/crates/cli/src/main.rs:26–35` — command dispatch
- `/home/dmytro/dev/cognee/cognee-rust/Cargo.toml` — no HTTP server or OAuth2 deps
