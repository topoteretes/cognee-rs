# Implementation Plan: Environment Variable Coverage (Gap 8)

This document provides the step-by-step implementation plan for improving environment variable coverage in the Rust SDK, organized by priority.

---

## Goal

Increase Rust SDK env var coverage from ~42% to ~70%+ by adding the most impactful missing variables to `crates/lib/src/config.rs` (central config), while leaving provider-specific variables in their respective crates where appropriate.

---

## Design Principles

### Central config vs crate-local config

Not all env vars belong in `Settings` (lib/config.rs). The decision rule:

| Belongs in `Settings` | Stays in crate-local config |
|---|---|
| Cross-cutting concerns (logging, ACL, caching) | Provider-specific details only used by one crate |
| Values that the CLI or top-level API needs to pass down | Values read lazily at engine construction time |
| Values that appear in config JSON serialization | Internal implementation details |

**Concrete examples:**
- `EMBEDDING_API_KEY`, `EMBEDDING_ENDPOINT`, `EMBEDDING_API_VERSION` -- stay in `crates/embedding/src/config.rs` (only used by `EmbeddingConfig::from_env()`)
- `CACHE_BACKEND`, `SESSION_TTL_SECONDS` -- go in `Settings` (needed by component manager to select session store)
- `LOG_LEVEL` -- go in `Settings` (cross-cutting)

### Aliasing strategy

Follow the existing pattern: `str_alias("PRIMARY_NAME", "LEGACY_ALIAS")` where the primary name matches Python exactly and the alias provides backward compatibility.

---

## Priority 1: High (Core SDK functionality)

### Step 1.1: Add `LLM_MAX_COMPLETION_TOKENS` alias

The Python field is `llm_max_completion_tokens` (maps to env var `LLM_MAX_COMPLETION_TOKENS`). Rust currently reads `LLM_MAX_TOKENS`. Add `LLM_MAX_COMPLETION_TOKENS` as the primary, with `LLM_MAX_TOKENS` as fallback alias.

**File:** `crates/lib/src/config.rs`

```rust
// Replace:
if let Some(v) = str_var("LLM_MAX_TOKENS")
    && let Ok(n) = v.parse::<u32>()
{
    self.llm_max_completion_tokens = n;
}

// With:
if let Some(v) = str_alias("LLM_MAX_COMPLETION_TOKENS", "LLM_MAX_TOKENS")
    && let Ok(n) = v.parse::<u32>()
{
    self.llm_max_completion_tokens = n;
}
```

### Step 1.2: Add `LLM_STREAMING` env var

The Rust `Settings` struct already has `llm_streaming: bool` but `overlay_from_env()` does not read it. Add:

```rust
// In overlay_from_env(), after LLM_MAX_TOKENS block:
if let Some(v) = str_var("LLM_STREAMING") {
    let v = v.to_lowercase();
    self.llm_streaming = v == "true" || v == "1" || v == "yes";
}
```

### Step 1.3: Add session/cache env vars to `Settings`

Add new fields to `Settings` and corresponding overlay reads:

```rust
// New fields in Settings struct:
pub cache_backend: String,          // "fs" | "redis" | "seaorm"
pub cache_host: String,             // default: "localhost"
pub cache_port: u16,                // default: 6379
pub cache_username: String,
pub cache_password: String,
pub session_ttl_seconds: u64,       // default: 604800 (7 days)
pub enable_caching: bool,           // default: true
pub auto_feedback: bool,            // default: false

// New defaults:
cache_backend: "fs".to_string(),
cache_host: "localhost".to_string(),
cache_port: 6379,
cache_username: String::new(),
cache_password: String::new(),
session_ttl_seconds: 604800,
enable_caching: true,
auto_feedback: false,

// New overlay reads:
if let Some(v) = str_var("CACHE_BACKEND") {
    self.cache_backend = v;
}
if let Some(v) = str_var("CACHE_HOST") {
    self.cache_host = v;
}
if let Some(v) = str_var("CACHE_PORT")
    && let Ok(n) = v.parse::<u16>()
{
    self.cache_port = n;
}
if let Some(v) = str_var("CACHE_USERNAME") {
    self.cache_username = v;
}
if let Some(v) = str_var("CACHE_PASSWORD") {
    self.cache_password = v;
}
if let Some(v) = str_var("SESSION_TTL_SECONDS")
    && let Ok(n) = v.parse::<u64>()
{
    self.session_ttl_seconds = n;
}
if let Some(v) = str_var("CACHING") {
    let v = v.to_lowercase();
    self.enable_caching = v == "true" || v == "1" || v == "yes";
}
if let Some(v) = str_var("AUTO_FEEDBACK") {
    let v = v.to_lowercase();
    self.auto_feedback = v == "true" || v == "1" || v == "yes";
}
```

### Step 1.4: Add auth/ACL env vars

```rust
// New fields:
pub default_user_email: String,
pub default_user_password: String,
pub enable_access_control: bool,

// Defaults:
default_user_email: String::new(),
default_user_password: String::new(),
enable_access_control: false,

// Overlay:
if let Some(v) = str_var("DEFAULT_USER_EMAIL") {
    self.default_user_email = v;
}
if let Some(v) = str_var("DEFAULT_USER_PASSWORD") {
    self.default_user_password = v;
}
if let Some(v) = str_var("ENABLE_BACKEND_ACCESS_CONTROL") {
    let v = v.to_lowercase();
    self.enable_access_control = v == "true" || v == "1" || v == "yes";
}
```

### Step 1.5: Add logging env vars

```rust
// New fields:
pub log_level: String,

// Default:
log_level: "info".to_string(),

// Overlay:
if let Some(v) = str_var("LOG_LEVEL") {
    self.log_level = v;
}
// COGNEE_LOGS_DIR maps to existing logs_root_directory
if let Some(v) = str_var("COGNEE_LOGS_DIR") {
    self.logs_root_directory = v;
}
// CACHE_ROOT_DIRECTORY maps to existing cache_root_directory
if let Some(v) = str_var("CACHE_ROOT_DIRECTORY") {
    self.cache_root_directory = v;
}
```

### Step 1.6: Add `EMBEDDING_PROVIDER` to central config

Currently `EMBEDDING_PROVIDER` is only read in `crates/embedding/src/config.rs` and `crates/chunking/src/config.rs`, not in `Settings`. Adding it to central config allows CLI and component manager to make decisions based on the embedding provider.

```rust
// New field:
pub embedding_provider: String,

// Default:
embedding_provider: "onnx".to_string(),

// Overlay:
if let Some(v) = str_var("EMBEDDING_PROVIDER") {
    self.embedding_provider = v;
}
```

---

## Priority 2: Medium (Feature parity)

### Step 2.1: Add rate limiting env vars

These fields prepare for future rate limiting implementation (Gap TBD). Add fields but no behavioral changes yet.

```rust
// New fields:
pub llm_rate_limit_enabled: bool,
pub llm_rate_limit_requests: u32,
pub llm_rate_limit_interval: u32,
pub embedding_rate_limit_enabled: bool,
pub embedding_rate_limit_requests: u32,
pub embedding_rate_limit_interval: u32,

// Defaults (matching Python):
llm_rate_limit_enabled: false,
llm_rate_limit_requests: 60,
llm_rate_limit_interval: 60,
embedding_rate_limit_enabled: false,
embedding_rate_limit_requests: 60,
embedding_rate_limit_interval: 60,
```

Plus corresponding overlay reads for `LLM_RATE_LIMIT_ENABLED`, `LLM_RATE_LIMIT_REQUESTS`, `LLM_RATE_LIMIT_INTERVAL`, `EMBEDDING_RATE_LIMIT_ENABLED`, `EMBEDDING_RATE_LIMIT_REQUESTS`, `EMBEDDING_RATE_LIMIT_INTERVAL`.

### Step 2.2: Add vector DB auth env vars

```rust
// New fields:
pub vector_db_username: String,
pub vector_db_password: String,
pub vector_db_host: String,

// Plus overlay reads for VECTOR_DB_USERNAME, VECTOR_DB_PASSWORD, VECTOR_DB_HOST.
```

### Step 2.3: Add storage backend env vars

```rust
// New fields:
pub storage_backend: String,        // "local" | "s3"
pub storage_bucket_name: String,

// Overlay reads for STORAGE_BACKEND, STORAGE_BUCKET_NAME.
```

### Step 2.4: Add observability env vars

```rust
// New fields:
pub cognee_tracing_enabled: bool,
pub otel_service_name: String,
pub otel_exporter_otlp_endpoint: String,
pub otel_exporter_otlp_headers: String,

// Defaults:
cognee_tracing_enabled: false,
otel_service_name: "cognee".to_string(),
otel_exporter_otlp_endpoint: String::new(),
otel_exporter_otlp_headers: String::new(),
```

### Step 2.5: Add feature flag env vars

```rust
// New fields:
pub enable_last_accessed: bool,     // default: false

// Overlay:
if let Some(v) = str_var("ENABLE_LAST_ACCESSED") {
    let v = v.to_lowercase();
    self.enable_last_accessed = v == "true" || v == "1" || v == "yes";
}
```

---

## Priority 3: Low (Platform-specific, can defer)

These should NOT be implemented now. They are either for platforms/vendors not yet supported in Rust or are Python-framework-specific:

| Category | Env Vars | Reason to Defer |
|---|---|---|
| BAML | `BAML_LLM_*` (6 vars) | BAML framework is Python-specific (uses `baml_py`) |
| AWS/S3 | `AWS_*` (7 vars) | S3 storage is a stub (`DataInput::S3Path` returns error) |
| Langfuse | `LANGFUSE_*` (3 vars) | Specific vendor integration, no Rust Langfuse SDK |
| Tavily | `TAVILY_API_KEY` | Specific vendor web scraper |
| Log rotation | `COGNEE_LOG_FILE`, `COGNEE_LOG_MAX_BYTES`, `COGNEE_LOG_BACKUP_COUNT` | Rust uses `tracing` crate which handles rotation differently |
| Python-specific | `LLM_INSTRUCTOR_MODE`, `STRUCTURED_OUTPUT_FRAMEWORK`, `MOCK_CODE_SUMMARY` | Instructor/BAML selection is Python-specific |

---

## Env vars that stay in crate-local config (no changes needed)

These are already correctly handled in their respective crates and should NOT be duplicated in `Settings`:

| Env Var | Crate | Why crate-local is correct |
|---|---|---|
| `EMBEDDING_API_KEY` | `crates/embedding/src/config.rs` | Only used by embedding engine construction |
| `EMBEDDING_ENDPOINT` | `crates/embedding/src/config.rs` | Only used by embedding engine construction |
| `EMBEDDING_API_VERSION` | `crates/embedding/src/config.rs` | Only used by embedding engine construction |
| `EMBEDDING_MAX_COMPLETION_TOKENS` | `crates/embedding/src/config.rs` | Only used by embedding engine construction |
| `HUGGINGFACE_TOKENIZER` | `crates/embedding/src/config.rs` | Only used by embedding + chunking |
| `MOCK_EMBEDDING` | `crates/embedding/src/config.rs` | Testing override |
| `COGNEE_TOKEN_COUNTER` | `crates/chunking/src/config.rs` | Only used by chunking tokenizer selection |
| `COGNEE_DEBUG_LLM_REQUEST` | `crates/llm/src/adapters/openai.rs` | Debug toggle for LLM HTTP logging |
| `TRANSCRIPTION_MODEL` | `crates/llm/src/adapters/openai.rs` | Whisper model override |
| `LLM_VISION_MODEL` | `crates/llm/src/adapters/openai.rs` | Vision model override |

---

## Files to Modify

| File | Action |
|---|---|
| `crates/lib/src/config.rs` | Add ~25 new fields, ~25 new overlay reads, update `Default` impl |
| `crates/lib/src/component_manager.rs` | Use `cache_backend`/`cache_host`/`cache_port` for session store selection |
| `crates/session/src/lib.rs` | Accept config for backend selection (if not already parameterized) |

---

## Verification

After implementation, run:
```bash
scripts/check_all.sh
```

Add a test in `crates/lib/src/config.rs` (following existing `overlay_picks_up_ontology_*` pattern) for at least:
- `CACHE_BACKEND` overlay
- `LLM_MAX_COMPLETION_TOKENS` alias fallback to `LLM_MAX_TOKENS`
- `LLM_STREAMING` boolean parsing
- `ENABLE_BACKEND_ACCESS_CONTROL` boolean parsing

---

## Expected Coverage After Implementation

| Priority | Vars Added | Coverage Impact |
|---|---|---|
| P1 (Steps 1.1-1.6) | ~20 vars | 42% -> 61% |
| P2 (Steps 2.1-2.5) | ~15 vars | 61% -> 75% |
| Total | ~35 vars | **42% -> 75%** |

The remaining ~25% are P3 deferrals (BAML, AWS, Langfuse, log rotation, Python-specific frameworks) that are not actionable until the corresponding features exist in Rust.
