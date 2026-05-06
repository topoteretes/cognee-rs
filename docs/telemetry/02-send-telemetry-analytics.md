# `send_telemetry()` Product-Analytics Client — Gap Analysis & Design

> **Scope:** This document covers the single gap of porting Python's
> `cognee.shared.utils.send_telemetry` product-analytics HTTP client into
> Rust. For the broader telemetry landscape (OTLP export, span scaffolding,
> pipeline-status persistence) see the parent [gap-analysis.md](./gap-analysis.md).

## Overview

Python `cognee` ships a **custom HTTP product-analytics proxy** that emits
fire-and-forget events on every pipeline run, task lifecycle transition, and
public API endpoint invocation. Each event carries a multi-layered identity
scheme that is intentionally designed to survive `forget(everything=True)`,
pip reinstalls, and `User` recreation, while never exposing the raw LLM API
key.

Rust currently has **no equivalent**. The only thing the Rust port emits in
this space is a single `tracing::info!` call on the `cognee.telemetry` target
inside `forget()` — see
[`crates/lib/src/api/forget.rs:103-123`](../../crates/lib/src/api/forget.rs#L103-L123).
That event is a structured log line, not an HTTP POST to a proxy, and is gated
behind the `telemetry` cargo feature defined in
[`crates/lib/Cargo.toml:41`](../../crates/lib/Cargo.toml#L41).

This doc proposes a Rust module that mirrors the Python implementation
byte-for-byte where it matters (PBKDF2 identity derivation, payload schema,
file-based persistent IDs) so a single user moving between SDKs produces the
same `api_key_tracking_id`.

---

## Python implementation

All code lives in
[`/tmp/cognee-python/cognee/shared/utils.py`](file:///tmp/cognee-python/cognee/shared/utils.py)
(reference clone). The relevant constants:

```python
# utils.py:21
proxy_url = "https://test.prometh.ai"

# utils.py:24
TELEMETRY_REQUEST_TIMEOUT: int = int(os.getenv("TELEMETRY_REQUEST_TIMEOUT", "5"))

# utils.py:25-27
_TELEMETRY_API_KEY_TRACKING_SALT_ENV = "TELEMETRY_API_KEY_TRACKING_SALT"
_DEFAULT_TELEMETRY_API_KEY_TRACKING_SALT = b"cognee.telemetry.api-key-tracking.v1"
_TELEMETRY_API_KEY_TRACKING_ITERATIONS = 100_000

# utils.py:41-46
_PERSISTENT_ID_DIR  = pathlib.Path.home() / ".cognee"
_PERSISTENT_ID_FILE = _PERSISTENT_ID_DIR / ".persistent_id"
_ANON_ID_DIR  = pathlib.Path(__file__).parent.parent.parent.resolve()  # project root
_ANON_ID_FILE = _ANON_ID_DIR / ".anon_id"
```

### Payload schema

`send_telemetry()` (utils.py:176-228) builds and dispatches:

```jsonc
{
  "anonymous_id": "<uuid4>",
  "event_name":   "<event>",
  "user_properties": {
    "user_id":             "<UUID>",
    "persistent_id":       "<uuid4>",
    "api_key_tracking_id": "ak_<32hex>",
    "api_key_hash":        "ak_<32hex>"   // bw-compat alias, identical value
  },
  "properties": {
    "time":                "MM/DD/YYYY",   // current_time.strftime("%m/%d/%Y")
    "user_id":             "<UUID>",
    "anonymous_id":        "<uuid4>",
    "persistent_id":       "<uuid4>",
    "api_key_tracking_id": "ak_<32hex>",
    "api_key_hash":        "ak_<32hex>",
    /* …spread of caller-supplied additional_properties (sanitized)… */
  }
}
```

POST'd via `aiohttp.ClientSession` to `proxy_url` with `total=TELEMETRY_REQUEST_TIMEOUT`
(default 5 s). Response status != 200 is logged at DEBUG; transport errors
(`aiohttp.ClientError`, `asyncio.TimeoutError`) are likewise swallowed.

The HTTP request is fire-and-forget: `loop.create_task(_send_telemetry_request(payload))`
(utils.py:228) detaches the future from the calling coroutine.

### Three identity layers

| Layer | Source | Stability | Code |
|---|---|---|---|
| `anonymous_id` | `<project_root>/.anon_id` (uuid4 created on first call), or `TRACKING_ID` env override | Resets on git re-clone / pip reinstall | utils.py:49-73 |
| `persistent_id` | `~/.cognee/.persistent_id` (uuid4, seeded from anon if present) | Survives `forget(everything=True)`, virtualenv recreation, User deletion | utils.py:76-104 |
| `api_key_tracking_id` | PBKDF2-HMAC-SHA256(`LLM_API_KEY`, salt, iter=100_000, dklen=16) → `"ak_" + hex` | Stable across machines for the same API key | utils.py:139-168 |
| `user_id` (transient) | Caller-supplied `User.id`; passed through directly | Changes when User is recreated | utils.py:176 |

**`anonymous_id` (project-root .anon_id):**

```python
# utils.py:49-73
def get_anonymous_id() -> str:
    tracking_id = os.getenv("TRACKING_ID", None)
    if tracking_id:
        return tracking_id
    try:
        if not os.path.isdir(str(_ANON_ID_DIR)):
            os.makedirs(str(_ANON_ID_DIR), exist_ok=True)
        if not _ANON_ID_FILE.is_file():
            anonymous_id = str(uuid4())
            _ANON_ID_FILE.write_text(anonymous_id, encoding="utf-8")
        else:
            anonymous_id = _ANON_ID_FILE.read_text(encoding="utf-8").strip()
    except Exception as e:
        logger.warning("Could not create or read anonymous id file: %s", e)
        return "unknown-anonymous-id"
    return anonymous_id
```

**`persistent_id` (`~/.cognee/.persistent_id`):**

```python
# utils.py:76-104
def get_persistent_id() -> str:
    try:
        if _PERSISTENT_ID_FILE.is_file():
            return _PERSISTENT_ID_FILE.read_text(encoding="utf-8").strip()
        # Seed from anonymous_id if it exists (ties the two together)
        persistent_id = get_anonymous_id()
        if persistent_id == "unknown-anonymous-id":
            persistent_id = str(uuid4())
        _PERSISTENT_ID_DIR.mkdir(parents=True, exist_ok=True)
        _PERSISTENT_ID_FILE.write_text(persistent_id, encoding="utf-8")
        return persistent_id
    except Exception as e:
        logger.warning("Could not create or read persistent id file: %s", e)
        return get_anonymous_id()
```

**`api_key_tracking_id` (PBKDF2-HMAC-SHA256, this is the load-bearing parity bit):**

```python
# utils.py:139-168
def _get_api_key_tracking_id() -> str:
    import hashlib
    key = os.getenv("LLM_API_KEY", "")
    if not key:
        return ""
    configured_salt = os.getenv(_TELEMETRY_API_KEY_TRACKING_SALT_ENV)
    salt = (
        configured_salt.encode("utf-8")
        if configured_salt
        else _DEFAULT_TELEMETRY_API_KEY_TRACKING_SALT  # b"cognee.telemetry.api-key-tracking.v1"
    )
    derived = hashlib.pbkdf2_hmac(
        "sha256",
        key.encode("utf-8"),
        salt,
        _TELEMETRY_API_KEY_TRACKING_ITERATIONS,  # 100_000
        dklen=16,
    )
    return f"ak_{derived.hex()}"
```

A backward-compat alias `_get_api_key_fingerprint()` (utils.py:171-173)
returns the same value, and the payload exposes it under both
`api_key_tracking_id` and `api_key_hash`.

### Property sanitization

`_sanitize_nested_properties()` (utils.py:107-124) recursively walks the
caller's `additional_properties` dict/list, and for any key in the watch list
(`["url"]` is the only one passed by `send_telemetry`) it replaces the
string value with `str(uuid5(NAMESPACE_OID, value))`. This prevents bare URLs
(which may be tenant-identifiable) from being shipped to the proxy.

### Opt-out / env gating

```python
# utils.py:194-199
if os.getenv("TELEMETRY_DISABLED"):
    return
env = os.getenv("ENV")
if env in ["test", "dev"]:
    return
```

Note that *any* truthy value (even `"0"` or `"false"`) for `TELEMETRY_DISABLED`
disables telemetry — Python only checks for non-empty.

### Env vars (the full list)

| Var | Default | Effect |
|---|---|---|
| `TELEMETRY_DISABLED` | unset | If set to *any* non-empty value, all events are dropped |
| `ENV` | unset | If `"test"` or `"dev"`, all events are dropped |
| `TELEMETRY_REQUEST_TIMEOUT` | `5` (seconds) | HTTP total timeout |
| `TELEMETRY_API_KEY_TRACKING_SALT` | `b"cognee.telemetry.api-key-tracking.v1"` | PBKDF2 salt; deployments override for private namespace |
| `TRACKING_ID` | unset | Overrides `anonymous_id` lookup entirely |
| `LLM_API_KEY` | unset | Empty → `api_key_tracking_id` is `""` |

---

## Catalog of `send_telemetry()` call sites in Python

Sourced via `grep -rn "send_telemetry" /tmp/cognee-python/cognee/`. Tests
omitted. Legend: P = pipeline lifecycle, T = task lifecycle, A = API endpoint,
S = SDK function.

| # | Event name | Source | Lvl | Notable additional_properties |
|--:|---|---|---|---|
| 1 | `Pipeline Run Started` | [run_tasks_with_telemetry.py:27](file:///tmp/cognee-python/cognee/modules/pipelines/operations/run_tasks_with_telemetry.py#L27) | P | `pipeline_name`, `cognee_version`, `tenant_id`, **+ get_current_settings()** |
| 2 | `Pipeline Run Completed` | run_tasks_with_telemetry.py:42 | P | (same as above) |
| 3 | `Pipeline Run Errored` | run_tasks_with_telemetry.py:59 | P | (same as above) |
| 4 | `${task_type} Task Started` | [run_tasks_base.py:135](file:///tmp/cognee-python/cognee/modules/pipelines/operations/run_tasks_base.py#L135) | T | `task_name`, `cognee_version`, `tenant_id` |
| 5 | `${task_type} Task Completed` | run_tasks_base.py:192 | T | (same) |
| 6 | `${task_type} Task Errored` | run_tasks_base.py:210 | T | (same) |
| 7 | `cognee.search EXECUTION STARTED` | [search.py:74](file:///tmp/cognee-python/cognee/modules/search/methods/search.py#L74) | S | `cognee_version`, `tenant_id` |
| 8 | `cognee.search EXECUTION COMPLETED` | search.py:115 | S | (same) |
| 9 | `code_description_to_code_part_search EXECUTION STARTED` | description_to_codepart_search.py:58 | S | — |
| 10 | `code_description_to_code_part_search EXECUTION FAILED` | description_to_codepart_search.py:141 | S | — |
| 11 | `cognee.cognify DEFAULT TASKS CREATION ERRORED` | get_cascade_graph_tasks.py:47 | S | — |
| 12 | `cognee.session.add_qa` | [session_manager.py:171](file:///tmp/cognee-python/cognee/infrastructure/session/session_manager.py#L171) | S | `session_id`, `data_size_bytes`, `has_feedback`, `has_graph_elements` |
| 13 | `Add API Endpoint Invoked` | [get_add_router.py:83](file:///tmp/cognee-python/cognee/api/v1/add/routers/get_add_router.py#L83) | A | `endpoint`, `node_set`, `cognee_version` |
| 14 | `Cognify API Endpoint Invoked` | get_cognify_router.py:123 | A | `endpoint`, `cognee_version` |
| 15 | `Search API Endpoint Invoked` (GET) | get_search_router.py:74 | A | `endpoint`, `cognee_version` |
| 16 | `Search API Endpoint Invoked` (POST) | get_search_router.py:133 | A | + `search_type`, `datasets`, `dataset_ids` |
| 17 | `cognee.recall` | [recall.py:402](file:///tmp/cognee-python/cognee/api/v1/recall/recall.py#L402) | S | `query_length`, `scope`, `auto_route`, `top_k`, `search_type`, `session_id`, `datasets`, `dataset_ids` |
| 18 | `Recall API Endpoint Invoked` (×2) | get_recall_router.py:63, 104 | A | endpoint context |
| 19 | `cognee.remember` | [remember.py:624](file:///tmp/cognee-python/cognee/api/v1/remember/remember.py#L624) | S | `mode`, `data_size_bytes`, `item_count`, `session_id` |
| 20 | `Remember API Endpoint Invoked` (×2) | get_remember_router.py:62, 133 | A | endpoint context |
| 21 | `cognee.forget` | [forget.py:79](file:///tmp/cognee-python/cognee/api/v1/forget/forget.py#L79) | S | `target`, `dataset`, `data_id`, `cognee_version` |
| 22 | `Forget API Endpoint Invoked` | get_forget_router.py:46 | A | endpoint context |
| 23 | `cognee.improve` | [improve.py:91](file:///tmp/cognee-python/cognee/api/v1/improve/improve.py#L91) | S | improve params |
| 24 | `Improve API Endpoint Invoked` | get_improve_router.py:63 | A | endpoint context |
| 25 | `Update API Endpoint Invoked` | get_update_router.py:74 | A | endpoint context |
| 26 | `Memify API Endpoint Invoked` | get_memify_router.py:86 | A | endpoint context |
| 27 | `Sync API Endpoint Invoked` (×2) | get_sync_router.py:97, 204 | A | endpoint context |
| 28 | `Datasets API Endpoint Invoked` (×7) | get_datasets_router.py:108, 156, 224, 262, 331, 405, 453 | A | endpoint context, dataset id |
| 29 | `Ontology API Endpoint Invoked` (×3) | get_ontology_router.py:39, 98, 126 | A | endpoint context |
| 30 | `LLM API Endpoint Invoked` (×2) | get_llm_router.py:101, 155 | A | endpoint context |
| 31 | `API Keys API Endpoint Invoked` (×3) | get_api_key_management_router.py:27, 64, 90 | A | endpoint context |
| 32 | `Permissions API Endpoint Invoked` (×7) | get_permissions_router.py:65, 108, 158, 201, 246, 282, 317 | A | endpoint context |
| 33 | `Visualize API Endpoint Invoked` (×2) | get_visualize_router.py:52, 100 | A | endpoint context |
| 34 | `Delete API Endpoint Invoked` | get_delete_router.py:44 | A | endpoint context |

Total: ~50+ unique callsites (router endpoints + ~10 SDK-level).

---

## Rust current state

There is **no** `send_telemetry`-equivalent in the Rust workspace today.
What exists that we can reuse:

- **`telemetry` cargo feature** in [`crates/lib/Cargo.toml:41`](../../crates/lib/Cargo.toml#L41)
  and [`crates/core/Cargo.toml:7`](../../crates/core/Cargo.toml#L7) — currently
  only gates `tracing` spans; can be extended to gate the new HTTP client.
- **`tracing::info!(target: "cognee.telemetry", …)` placeholder** in
  [`crates/lib/src/api/forget.rs:103-123`](../../crates/lib/src/api/forget.rs#L103-L123) —
  the only existing call site, emits a structured log only.
- **`reqwest`** workspace dep with `rustls-tls` already present
  ([Cargo.toml:73](../../Cargo.toml#L73)) — drop-in for the proxy POST.
- **`sha2`** workspace dep ([Cargo.toml:80](../../Cargo.toml#L80)) — usable
  by `pbkdf2` for the HMAC-SHA256 PRF.
- **`uuid`** with `v4`+`v5` ([Cargo.toml:102](../../Cargo.toml#L102)) — covers
  the random `anonymous_id`/`persistent_id` and the URL-sanitization uuid5.
- **`dirs`** ([Cargo.toml:53](../../Cargo.toml#L53)) — for cross-platform home
  dir lookup (`~/.cognee/.persistent_id`).
- **`tokio` runtime** is already pervasive — `tokio::spawn` for fire-and-forget
  is trivial.
- **`tracing`** + **`tracing-subscriber`** present — debug-level fall-back
  logs match Python's behavior.

What is **not** present:

- `pbkdf2` crate — needs adding (`pbkdf2 = "0.12"`).
- `hmac` crate — `pbkdf2` v0.12 takes the PRF as a generic; we need
  `hmac = "0.12"` to plug in `Hmac<Sha256>`.

---

## Detailed gap analysis

| Capability | Python | Rust today | Required work |
|---|---|---|---|
| HTTP POST to `https://test.prometh.ai` | aiohttp.ClientSession | none | reqwest async client wrapper |
| Fire-and-forget dispatch | `loop.create_task(...)` | none | `tokio::spawn` detached |
| Timeout (`TELEMETRY_REQUEST_TIMEOUT`) | env, default 5 s | none | `reqwest::Client::builder().timeout()` |
| `anonymous_id` from `<project_root>/.anon_id` | yes | none | Cwd-anchored lookup; honor `TRACKING_ID` |
| `persistent_id` from `~/.cognee/.persistent_id` | yes | none | `dirs::home_dir()`-anchored lookup |
| `api_key_tracking_id` PBKDF2-HMAC-SHA256 (100k, dklen=16) | yes | none | New helper using `pbkdf2`+`hmac` crates |
| Salt env var override | yes | none | Read `TELEMETRY_API_KEY_TRACKING_SALT` |
| Sanitize `url` keys → uuid5 | yes | none | Recursive `serde_json::Value` walker |
| `TELEMETRY_DISABLED` opt-out | yes | none | Trivial env check |
| `ENV in {test,dev}` opt-out | yes | none | Trivial env check |
| Payload schema parity | yes | none | Match field-by-field including `api_key_hash` alias |
| Per-callsite event emissions (~50 sites) | yes | only `forget.rs` (as a log) | Replace stub with real emit; add ~50 callsites |

---

## Proposed design

### Module placement

Add a new module **`cognee_utils::telemetry`** inside the existing
[`crates/utils`](../../crates/utils) crate. Rationale:

- `cognee-utils` is the lightest dependency in the workspace (no DB, no
  models) — telemetry needs to be importable from *every* other crate
  including `lib`, `cli`, `http-server`, `core`, `cognify`, `search`, etc.
  without circular deps.
- A new top-level `crates/telemetry` would also work, but `utils` already
  hosts retry-with-jitter and ID helpers, both of which the telemetry module
  will lean on.
- We deliberately do **not** put it in `crates/core` because some downstream
  callers (e.g. CLI argument-time emits) don't depend on the pipeline runtime.

### Crate layout

```
crates/utils/src/telemetry/
├── mod.rs          // re-exports + public API surface
├── ids.rs          // anonymous_id, persistent_id, api_key_tracking_id
├── sanitize.rs     // _sanitize_nested_properties equivalent
├── payload.rs      // strongly-typed TelemetryPayload (serde::Serialize)
├── client.rs       // reqwest singleton + send_telemetry_request
└── env.rs          // env-var parsing + opt-out checks
```

### Cargo additions (utils)

```toml
[dependencies]
# (existing) tokio, log, rand, uuid
reqwest.workspace      = true
serde.workspace        = true
serde_json.workspace   = true
sha2.workspace         = true
hmac    = "0.12"
pbkdf2  = { version = "0.12", default-features = false }
hex     = "0.4"
dirs.workspace         = true
chrono.workspace       = true
tracing.workspace      = true

[features]
default   = []
telemetry = []   # gates the HTTP client; ID helpers always compiled
```

The umbrella `cognee-lib` and `cognee-cli` crates should add `telemetry` to
their default feature lists per the workspace convention
(see project CLAUDE.md "Feature strategy").

### Public API

```rust
//! crates/utils/src/telemetry/mod.rs

use serde_json::Value;
use uuid::Uuid;

/// Fire-and-forget product-analytics event.
///
/// Mirrors Python `cognee.shared.utils.send_telemetry`. Returns immediately;
/// the HTTP POST is dispatched on a detached tokio task with a 5-second
/// (configurable) total timeout. Errors are swallowed at debug level.
///
/// No-op when:
/// - the `telemetry` cargo feature is disabled at compile time,
/// - `TELEMETRY_DISABLED` is set to a non-empty value,
/// - `ENV` is `"test"` or `"dev"`.
pub fn send_telemetry(
    event_name: &str,
    user_id:    impl Into<UserIdRef>,        // accepts Uuid, &str, or "sdk"
    additional_properties: Option<Value>,    // serde_json object
);

/// Internals — exposed for tests and for callers that need to construct
/// a payload without sending it.
pub mod ids {
    pub fn get_anonymous_id() -> String;
    pub fn get_persistent_id() -> String;
    pub fn get_api_key_tracking_id() -> String;
}
```

### Async dispatch pattern

```rust
// crates/utils/src/telemetry/client.rs (sketch)
pub(crate) fn dispatch(payload: TelemetryPayload) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        // Caller is not in a tokio context — emit a debug log and drop.
        tracing::debug!(target: "cognee.telemetry",
            "send_telemetry called outside tokio runtime; dropping");
        return;
    };
    handle.spawn(async move {
        let client = http_client();   // once_cell::sync::Lazy<reqwest::Client>
        let timeout = std::time::Duration::from_secs(
            env::telemetry_request_timeout()
        );
        match client.post(PROXY_URL).json(&payload).timeout(timeout).send().await {
            Ok(resp) if !resp.status().is_success() => {
                tracing::debug!(target: "cognee.telemetry",
                    status = %resp.status(), "proxy returned non-200");
            }
            Err(e) => {
                tracing::debug!(target: "cognee.telemetry", error = %e,
                    "telemetry request failed");
            }
            _ => {}
        }
    });
}
```

The `reqwest::Client` is held in a `once_cell::sync::Lazy` so we don't pay
TLS-handshake startup on every event. (`once_cell` is already in the
dependency graph via several crates.)

No global "send-from-anywhere" queue is needed — Python doesn't have one
either, and the tokio handle is reachable from every async call site we care
about. The synchronous CLI entry points already wrap calls in a `Runtime`.

### ID derivation (the parity-critical piece)

```rust
// crates/utils/src/telemetry/ids.rs (sketch)
use hmac::Hmac;
use pbkdf2::pbkdf2;
use sha2::Sha256;

const DEFAULT_SALT: &[u8] = b"cognee.telemetry.api-key-tracking.v1";
const ITERATIONS:   u32   = 100_000;
const DKLEN:        usize = 16;

pub fn get_api_key_tracking_id() -> String {
    let key = std::env::var("LLM_API_KEY").unwrap_or_default();
    if key.is_empty() {
        return String::new();
    }
    let salt: Vec<u8> = std::env::var("TELEMETRY_API_KEY_TRACKING_SALT")
        .map(|s| s.into_bytes())
        .unwrap_or_else(|_| DEFAULT_SALT.to_vec());
    let mut out = [0u8; DKLEN];
    // pbkdf2 returns Result<(), InvalidLength>; with a fixed dklen ≤ 32
    // (Sha256 output) it cannot fail.
    pbkdf2::<Hmac<Sha256>>(key.as_bytes(), &salt, ITERATIONS, &mut out)
        .expect("dklen 16 ≤ Sha256 output 32 — invariant holds");
    format!("ak_{}", hex::encode(out))
}
```

`anonymous_id`/`persistent_id` use `std::fs` synchronously (Python does too,
and these are < 64-byte reads) with the same path layout:

- `anonymous_id` → resolve project root via `std::env::current_dir()` then
  walk upward until we find a `Cargo.toml` (matching Python's
  `pathlib.Path(__file__).parent.parent.parent.resolve()` semantics in spirit;
  Rust has no equivalent of `__file__` so we use cwd-rooted lookup which
  matches the Python behavior when invoked from a checkout). Fallback
  to `current_dir()` itself if no Cargo.toml is found. Filename: `.anon_id`.
- `persistent_id` → `dirs::home_dir().join(".cognee").join(".persistent_id")`.

Both honor a `TRACKING_ID` env override (Python only checks for `anonymous_id`).

### Property sanitization

```rust
// crates/utils/src/telemetry/sanitize.rs (sketch)
use serde_json::{Value, Map};
use uuid::{Uuid, uuid};

const NAMESPACE_OID: Uuid = uuid!("6ba7b812-9dad-11d1-80b4-00c04fd430c8");

pub fn sanitize(value: &mut Value, names: &[&str]) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if names.contains(&k.as_str()) {
                    if let Value::String(s) = v {
                        *v = Value::String(
                            Uuid::new_v5(&NAMESPACE_OID, s.as_bytes()).to_string(),
                        );
                        continue;
                    }
                }
                sanitize(v, names);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| sanitize(i, names)),
        _ => {}
    }
}
```

`send_telemetry` calls this with `names = &["url"]` to match Python.

### Integration with `forget.rs`

Replace the existing tracing-only block in
[`crates/lib/src/api/forget.rs:103-123`](../../crates/lib/src/api/forget.rs#L103-L123)
with:

```rust
#[cfg(feature = "telemetry")]
cognee_utils::telemetry::send_telemetry(
    "cognee.forget",
    owner_id,
    Some(serde_json::json!({
        "target":         target_label,
        "dataset":        dataset_dbg,
        "data_id":        data_id_dbg,
        "cognee_version": env!("CARGO_PKG_VERSION"),
    })),
);
```

The same pattern is then applied to the other callsites cataloged above as
they are ported.

---

## Cross-SDK identity parity

Critical requirement: a developer who runs `pip install cognee` then later
the Rust SDK against the same `LLM_API_KEY` and the same home directory
**must** observe identical `api_key_tracking_id` and `persistent_id` values
on the proxy side. Otherwise we double-count installs.

| Element | Required match | Risk |
|---|---|---|
| PBKDF2 algorithm | HMAC-SHA256 | both `hashlib.pbkdf2_hmac("sha256", …)` and Rust `Hmac<Sha256>` over `pbkdf2` are NIST SP 800-132 conformant |
| Iterations | `100_000` exactly | hard-coded constant in both |
| Default salt bytes | `b"cognee.telemetry.api-key-tracking.v1"` (38 bytes UTF-8) | hard-coded in both; do **not** UTF-8 normalize |
| Salt encoding when env var set | UTF-8 of the env-var string | both call `.encode("utf-8")` / `.into_bytes()` |
| Output length | 16 bytes | dklen=16 in Python; `[0u8; 16]` buffer in Rust |
| Encoding | lowercase hex | Python `derived.hex()` defaults to lowercase; `hex::encode` is lowercase by default |
| Prefix | literal `"ak_"` | both |
| `persistent_id` location | `~/.cognee/.persistent_id` | both use `Path.home() / ".cognee"` |
| `persistent_id` format | uuid4 string, `to_string()` (`urn:uuid:` prefix omitted) | Python `str(uuid4())` matches Rust `Uuid::new_v4().to_string()` (both produce hyphenated lowercase 36-char form) |

### Byte-level test plan

```rust
#[test]
fn pbkdf2_matches_python_reference() {
    // Generated with Python:
    //   import hashlib
    //   key = b"sk-test-key-12345"
    //   salt = b"cognee.telemetry.api-key-tracking.v1"
    //   hashlib.pbkdf2_hmac("sha256", key, salt, 100_000, 16).hex()
    //   -> "<fill-in-from-python-run>"
    let key  = b"sk-test-key-12345";
    let salt = b"cognee.telemetry.api-key-tracking.v1";
    let mut out = [0u8; 16];
    pbkdf2::<Hmac<Sha256>>(key, salt, 100_000, &mut out).unwrap();
    assert_eq!(hex::encode(out), "<fixture-from-python>");
}

#[test]
fn tracking_id_full_format() {
    std::env::set_var("LLM_API_KEY", "sk-test-key-12345");
    std::env::remove_var("TELEMETRY_API_KEY_TRACKING_SALT");
    let id = ids::get_api_key_tracking_id();
    assert!(id.starts_with("ak_"));
    assert_eq!(id.len(), 3 + 32);  // "ak_" + 16-byte hex
    assert_eq!(id, format!("ak_{}", "<fixture-from-python>"));
}
```

A small `scripts/generate_python_fixtures.py` helper is recommended so we can
re-derive fixtures should the salt or iterations ever change. This keeps the
fixture files trivially regenerable and serves as evidence of byte-parity.

---

## Action items

Each item below has a dedicated implementation sub-document under [`02/`](02/) with rationale, prerequisites, step-by-step source-level changes, verification commands, files modified, and risks. **The sub-docs are authoritative**: where they refine details based on the locked design decisions (especially decision 1 — `telemetry` is **on** by default for `cognee-lib`/`cognee-cli` — and decision 6 — code lives in a new `cognee-telemetry` crate, not inside `cognee-utils`), follow the sub-doc rather than the high-level summary here.

| # | Action item | Sub-doc | Depends on | Status |
|---|---|---|---|---|
| 1 | Add workspace dependencies (`pbkdf2 = "0.12"`, `hmac = "0.12"`, `hex = "0.4"`, `once_cell = "1"`) to `[workspace.dependencies]`. Confirm `reqwest`, `sha2`, `serde_json`, `dirs`, `chrono`, `tracing`, `uuid` are already present. | [02/01-workspace-deps.md](02/01-workspace-deps.md) | — | ⬜ |
| 2 | Create the new `cognee-telemetry` workspace crate (manifest, `lib.rs` skeleton, feature wiring, register in workspace `members`). Scaffold only — implementations land in tasks 3–6. | [02/02-telemetry-crate-scaffold.md](02/02-telemetry-crate-scaffold.md) | 1 | ⬜ |
| 3 | Implement the three identity layers: `get_anonymous_id` (`<project_root>/.anon_id` + `TRACKING_ID` override), `get_persistent_id` (`~/.cognee/.persistent_id`), and `get_api_key_tracking_id` (PBKDF2-HMAC-SHA256, 100 000 iter, dklen 16, byte-parity to Python). | [02/03-id-derivation.md](02/03-id-derivation.md) | 2 | ⬜ |
| 4 | Implement `TelemetryPayload` (serde-serialized, exact Python field names including `api_key_hash` alias) and the recursive `sanitize_nested_properties` helper that hashes `url` keys via `uuid5(NAMESPACE_OID, value)`. | [02/04-payload-and-sanitize.md](02/04-payload-and-sanitize.md) | 2 | ⬜ |
| 5 | Implement the HTTP client (`reqwest` singleton, fire-and-forget `tokio::spawn`), opt-out checks (`TELEMETRY_DISABLED`, `ENV in {test,dev}`), and the runtime-fallback path (decision 5 — log warning + spin up a one-shot single-thread `Runtime` when no tokio handle is present). | [02/05-client-dispatch-and-optout.md](02/05-client-dispatch-and-optout.md) | 3, 4 | ⬜ |
| 6 | Wire the `telemetry` feature through `cognee-telemetry`, `cognee-lib` (default ON per decision 1), `cognee-cli` (default ON), `android-default` (OFF). Implement the noop fallback so the public API compiles when the feature is off. Export `cognee_lib::telemetry::send_telemetry`. | [02/06-public-api-and-noop.md](02/06-public-api-and-noop.md) | 2, 5 | ⬜ |
| 7 | Replace the placeholder in [`crates/lib/src/api/forget.rs:103-123`](../../crates/lib/src/api/forget.rs#L103-L123) with a real `send_telemetry` call. Port all SDK call sites (`recall`, `remember`, `improve`, search orchestrator, session add_qa) and HTTP router endpoints (~30 handlers). Pipeline + task lifecycle (`Pipeline Run Started/Completed/Errored`, `${task_type} Task Started/Completed/Errored`) is split out to gap [03](../03-pipeline-task-api-events.md). | [02/07-callsite-migration.md](02/07-callsite-migration.md) | 6 | ⬜ |
| 8 | Unit tests under `crates/telemetry/src/`: PBKDF2 byte-parity vs Python fixture, `ak_` format invariants, custom-salt divergence, empty `LLM_API_KEY`, sanitize URL replacement, anonymous/persistent file create + read-stable, opt-out env checks. | [02/08-unit-tests.md](02/08-unit-tests.md) | 3, 4, 5 | ⬜ |
| 9 | Integration tests with `mockito` (workspace already uses it; do **not** add `wiremock`): full payload schema parity vs Python (jsonschema), opt-out via `TELEMETRY_DISABLED`, fire-and-forget timeout (proxy stalls, dispatch returns < 100 ms). | [02/09-integration-tests.md](02/09-integration-tests.md) | 5, 7 | ⬜ |
| 10 | Cross-SDK parity test: extend `e2e-cross-sdk/` with a shared mock that records both Python and Rust telemetry payloads for the same `LLM_API_KEY` and home directory; assert identical `api_key_tracking_id` and `persistent_id`. | [02/10-cross-sdk-parity.md](02/10-cross-sdk-parity.md) | 7, 9 | ⬜ |
| 11 | User-facing docs: `docs/observability/send_telemetry.md` (env vars, opt-out recipes, privacy note, salt rotation, payload schema, troubleshooting). Plus rustdoc on the public API and a README pointer. | [02/11-user-docs.md](02/11-user-docs.md) | 5, 6, 7 | ⬜ |
| 12 | CI updates: ensure both `--features telemetry` and `--no-default-features` lanes cover the new crate; add a network-isolation lane that asserts no outbound HTTP fires when `TELEMETRY_DISABLED=1`. | [02/12-ci-updates.md](02/12-ci-updates.md) | 7, 8, 9 | ⬜ |

### Suggested execution order

A clean PR sequence based on the dependency graph above:

1. **PR 1** (foundation): tasks 01 + 02 — workspace deps, new crate scaffold.
2. **PR 2** (parity-critical core): tasks 03 + 04 — ID derivation (PBKDF2 byte-parity is the load-bearing piece) and payload/sanitize.
3. **PR 3** (transport + public surface): tasks 05 + 06 — client/dispatch/opt-out, feature wiring through `cognee-lib` + `cognee-cli`, noop fallback. The noop must land with the real impl so default-off builds keep working.
4. **PR 4** (callsites + tests): tasks 07 + 08 + 09 — replace `forget.rs` placeholder, port the catalog, unit + integration tests.
5. **PR 5** (cross-SDK + ops): tasks 10 + 11 + 12 — Python ↔ Rust parity test, user docs, CI lanes.

## Design decisions (locked)

These supersede the earlier "Open questions" — answers were obtained from the project owner on 2026-05-06 and are the binding contract for all per-task sub-docs under [`02/`](02/).

| # | Decision | Resolution | Implication |
|---|---|---|---|
| 1 | `telemetry` cargo feature default | **ON** by default in `cognee-lib` and `cognee-cli`; **OFF** in `android-default` (mirrors Python's enabled-by-default with `TELEMETRY_DISABLED` kill switch) | Plain `cargo build` of `cognee-cli` ships telemetry; Android builds opt out. Users disable at runtime via `TELEMETRY_DISABLED=1` or compile-time via `--no-default-features`. |
| 2 | Proxy URL | **Reuse** Python's `https://test.prometh.ai`. Add a new property field `sdk_runtime: "rust"` (alongside `cognee_version`) so dashboards can distinguish SDK origin without losing cross-SDK identity grouping | Same proxy, same identity space — one user moving between SDKs counts as one. |
| 3 | `Pipeline Run Started` settings dump | **Hand-curated subset** — provider names, model names, feature flags only. **Never** serialize the full `Config` struct (would leak deployment URLs/keys) | Define a `current_settings_for_telemetry()` helper that explicitly lists allowed fields. Detail belongs in gap [03](../03-pipeline-task-api-events.md), but the principle is locked here. |
| 4 | Emission ownership | **SDK function = single source of truth.** HTTP routers and CLI subcommands add a thin `endpoint` property (e.g. `endpoint = "POST /api/v1/forget"`) but do **not** duplicate the SDK-level event | Avoids double-counting. Routers wrap the SDK call and `additional_properties` is merged with the router's contribution. |
| 5 | Tokio runtime fallback | When `tokio::runtime::Handle::try_current()` returns `Err`, log a warning at `WARN` level (`tracing::warn!`) and spin up a one-shot `tokio::runtime::Builder::new_current_thread().enable_io().enable_time().build()` to dispatch the request, blocking up to `TELEMETRY_REQUEST_TIMEOUT` | Embedded/Android entry points still emit events; the warning surfaces the inefficiency so callers can switch to async if they care. |
| 6 | Module placement | New workspace crate **`cognee-telemetry`** (sibling of `cognee-utils`, `cognee-observability`, etc.) | Keeps the dep set (`pbkdf2`, `hmac`, `reqwest`) out of `cognee-utils`'s blast radius. Mirrors the per-concern crate split that gap 01 used for `cognee-observability`. |
| 7 | Sub-doc grouping | Twelve sub-docs under [`02/`](02/) following the gap 01 pattern (00-runbook + 01..12 per-task) | Action items in this parent doc are grouped (e.g. "client + dispatch + opt-out" → one sub-doc) so the count matches gap 01 even though Python's surface is wider. |
| 8 | Investigation depth | Same as gap 01: per-file line-number citations, full step-by-step diffs, verification commands, files modified, risks | Sub-docs must be self-contained enough that a fresh Claude Code session can drive each task with no prior context. |
| 9 | Pipeline / task lifecycle events | **Out of scope** for this gap — moved to gap [03](../03-pipeline-task-api-events.md). This gap covers the *transport* (`send_telemetry` itself + identity layers + opt-out + the existing `forget.rs` placeholder + a representative slice of SDK + router callsites) | Keeps gap 02 shippable in one initiative; gap 03 then reuses the transport for the high-volume pipeline events. |
| 10 | HTTP-mocking library | **`mockito`** (already a dev-dep of `cognee-cli` and `cognee-cloud`) — do **not** introduce `wiremock` | Avoids a redundant test dep; one HTTP-mock library across the workspace. |
| 11 | Provenance of API key | The `api_key_tracking_id` derivation reads `LLM_API_KEY` from the process environment at *event-emission time*, **not** at startup | Matches Python's lazy read in `_get_api_key_tracking_id()`. Allows tests to set the env in-test without re-importing. |
| 12 | Salt overrides for deployments | Honour `TELEMETRY_API_KEY_TRACKING_SALT` (UTF-8 bytes of the env value) — same name and semantics as Python | A deployment can put its fleet in a private namespace; default public salt is intentionally well-known so OSS installs converge to one analytics namespace. |

---

## Privacy & compliance notes

What is sent on every event:

- A randomly-generated machine UUID (`anonymous_id`, `persistent_id`).
- A PBKDF2-derived hash of the LLM API key (`api_key_tracking_id`). Cost of
  brute force at 100k iterations × HMAC-SHA256: deriving a single candidate
  is ~50 ms on commodity CPU; recovering a 40-char OpenAI-style key with
  reasonable entropy is computationally infeasible. The 16-byte output is
  smaller than SHA256's full digest, but still has 2^128 codomain.
- The Cognee user UUID supplied by the caller.
- Caller-supplied `additional_properties`, with all `url` keys hashed via
  `uuid5(NAMESPACE_OID, value)` before transmission.
- The cognee version string.

What is **not** sent:

- The raw `LLM_API_KEY`.
- The last N characters of the API key (the previous `_get_api_key_fingerprint`
  attribute that exposed a key tail has been replaced; both names now resolve
  to the salted PBKDF2 hash).
- Any path strings other than those the caller explicitly puts in
  `additional_properties` (no auto-redaction of arbitrary keys beyond `url`).

How a user disables it:

```bash
export TELEMETRY_DISABLED=1     # any non-empty value
# or
export ENV=test
```

Compile-time disable:

```bash
cargo build --no-default-features  # cognee-lib/cognee-cli no-default-features
```

How the salt protects against rainbow tables: a deployment can override
`TELEMETRY_API_KEY_TRACKING_SALT` to put their fleet in a private namespace.
This makes their `api_key_tracking_id` values incomparable with the public
namespace, frustrating join attacks across deployments. The default public
salt is intentionally well-known so that local installs converge to a single
analytics namespace.

---

## Open questions

These were superseded by the [Design decisions (locked)](#design-decisions-locked) table above on 2026-05-06. Kept here as a paper trail of the original questions and the rationale considered before locking.

1. ~~**Default state.**~~ Resolved by decision 1 — ON by default in `cognee-lib`/`cognee-cli`, OFF in `android-default`.
2. ~~**Proxy URL.**~~ Resolved by decision 2 — reuse `https://test.prometh.ai`, add `sdk_runtime: "rust"` field.
3. ~~**Settings dump.**~~ Resolved by decision 3 — hand-curated subset; never serialize full `Config`.
4. ~~**CLI vs library callsites.**~~ Resolved by decision 4 — SDK function is single source of truth; routers/CLI add `endpoint` property.
5. ~~**Tokio runtime requirement.**~~ Resolved by decision 5 — log warning + spin up a one-shot single-thread runtime.

---

## Testing strategy

| Layer | Test | Location |
|---|---|---|
| Unit | PBKDF2 byte parity vs Python fixture | `crates/utils/src/telemetry/ids.rs` `#[cfg(test)]` |
| Unit | `ak_` format invariants | same |
| Unit | Custom salt yields different ID | same |
| Unit | Empty `LLM_API_KEY` → empty string | same |
| Unit | `_sanitize_nested_properties` URL replacement | `crates/utils/src/telemetry/sanitize.rs` |
| Unit | Anonymous/persistent file create + read-stable | tempdir + override `HOME` |
| Unit | Opt-out via `TELEMETRY_DISABLED` and `ENV=test` | dispatch returns without spawning |
| Integration | Full payload schema parity vs Python (jsonschema) | `crates/utils/tests/telemetry_schema.rs` with wiremock |
| Integration | Fire-and-forget timeout — proxy stalls 30 s, dispatch returns < 100 ms | wiremock with delay |
| E2E cross-SDK | Same key → same `api_key_tracking_id` Python & Rust | `e2e-cross-sdk/test_telemetry_parity.py` |
| Manual | Live POST to staging proxy, verify it appears in dashboard | one-shot dev script |

All tests must run by default with `MOCK_EMBEDDING=true` and without network
access; the wiremock-based integration tests bind to `127.0.0.1:0`. The
cross-SDK test runs inside the existing Docker Compose harness in
[`e2e-cross-sdk/`](../../e2e-cross-sdk/).

---

## References

- Python implementation: [`/tmp/cognee-python/cognee/shared/utils.py`](file:///tmp/cognee-python/cognee/shared/utils.py)
  (constants L21-27, anon ID L41-73, persistent ID L76-104, sanitize L107-124,
  HTTP request L127-137, PBKDF2 L139-168, `send_telemetry` L176-228)
- Python tests: [`/tmp/cognee-python/cognee/tests/unit/shared/test_telemetry_tracking.py`](file:///tmp/cognee-python/cognee/tests/unit/shared/test_telemetry_tracking.py)
- Rust placeholder: [`crates/lib/src/api/forget.rs:103-123`](../../crates/lib/src/api/forget.rs#L103-L123)
- Existing telemetry feature flag: [`crates/lib/Cargo.toml:37-41`](../../crates/lib/Cargo.toml#L37-L41), [`crates/core/Cargo.toml:7`](../../crates/core/Cargo.toml#L7)
- Workspace deps that already cover most of the implementation:
  [`Cargo.toml:42-103`](../../Cargo.toml#L42-L103)
- Parent gap analysis (do not edit): [`gap-analysis.md`](./gap-analysis.md)
- Project conventions: [`/.claude/CLAUDE.md`](../../.claude/CLAUDE.md)
  (Feature strategy section; "no `unwrap()` in non-test code")
- PBKDF2 standard: NIST SP 800-132
- Crates: [`pbkdf2`](https://crates.io/crates/pbkdf2), [`hmac`](https://crates.io/crates/hmac), [`sha2`](https://crates.io/crates/sha2), [`reqwest`](https://crates.io/crates/reqwest), [`dirs`](https://crates.io/crates/dirs)
