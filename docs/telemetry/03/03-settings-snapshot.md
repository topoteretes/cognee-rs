# Task 03-03 — `Config::telemetry_snapshot()` settings allowlist

**Status**: ✅ implemented in commit `cde4024`
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 03-04 — Pipeline lifecycle events](04-pipeline-lifecycle-events.md) (the snapshot is merged into the `Pipeline Run *` payloads).

> **Implementation notes (post-landing).** Two minor user-visible
> divergences from this sub-doc, both aligned with its stated intent
> (Python `get_current_settings()` parity):
>
> 1. **Wire-format key renames** — the Rust struct fields were renamed
>    to match the wire-format keys directly, so no rename layer is
>    needed. `Config::graph_database_provider` →
>    `Settings::graph_db_provider`, `Config::db_provider` →
>    `Settings::relational_db_provider`,
>    `Config::embedding_model_name` → `Settings::embedding_model`.
> 2. **Struct name** — the helper lives on a struct named `Settings`
>    (not `Config` as written throughout this sub-doc). Read every
>    `Config::telemetry_snapshot()` reference below as
>    `Settings::telemetry_snapshot()`.

**Parent doc**: [03 — Pipeline / Task / API Operation Events](../03-pipeline-task-api-events.md)
**Locked decision**: #5 — settings snapshot is a hand-curated allowlist; never serialize the full `Config` struct.

---

## 1. Goal

Add `Config::telemetry_snapshot(&self) -> serde_json::Map<String,
serde_json::Value>` returning the redacted, hand-curated subset of
configuration that is safe to ship to the analytics proxy as the
`| config` merge on `Pipeline Run *` events.

| Wire-format key | Source field on `Config` | Type |
|---|---|---|
| `sdk_runtime` | constant `"rust"` (matches gap-02 decision 2) | `&'static str` |
| `vector_db_provider` | `Config::vector_db_provider` | `String` |
| `graph_db_provider` | `Config::graph_database_provider` (renamed on the wire) | `String` |
| `relational_db_provider` | `Config::db_provider` (renamed on the wire) | `String` |
| `llm_provider` | `Config::llm_provider` | `String` |
| `llm_model` | `Config::llm_model` | `String` |
| `embedding_provider` | `Config::embedding_provider` | `String` |
| `embedding_model` | `Config::embedding_model_name` (renamed on the wire) | `String` |
| `embedding_dimensions` | `Config::embedding_dimensions` | `u32` |
| `chunk_strategy` | `Config::chunk_strategy` | `String` |
| `token_counter` | sourced separately — see [§4.2](#42-token_counter-source) | `String` |

Wire-format key names follow Python's
[`get_current_settings()`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/config/get_current_settings.py)
output keys (e.g. `graph_db_provider`, not `graph_database_provider`).
The Rust struct fields differ in spelling, so the snapshot helper
explicitly renames them.

## 2. Rationale — what is allowed and what is not

**Allowed:** provider/model identifiers and feature flags. These are
either:

- Values a maintainer would pick from a fixed set of options
  (`"openai"`, `"ollama"`, `"qdrant"`, `"lancedb"`, `"sqlite"`,
  `"postgres"`, `"sentence"`, `"paragraph"`, etc.) — finite vocabulary.
- Numerical settings whose distribution is interesting for product
  analytics (`embedding_dimensions`).

**Disallowed:** anything that could carry a deployment-specific
secret or URL:

- `llm_api_key`, `embedding_api_key`, `vector_db_key`,
  `vector_db_password`, `db_password`, `cache_password` — credentials.
- `llm_endpoint`, `embedding_endpoint`, `vector_db_url`,
  `vector_db_host`, `graph_database_url`, `relational_db_url`,
  `db_host`, `migration_db_url`, `cache_host` — endpoints / hostnames.
- `default_user_email`, `default_user_password` — bootstrap creds.
- File paths (`graph_file_path`, `embedding_model_path`, etc.) — may
  embed user-specific directory layouts.

The allowlist is **deny-by-default**: a new `Config` field added in
the future is **not** reported on the wire until someone explicitly
adds it to `telemetry_snapshot()`. A snapshot test (see
[§5](#5-verification)) locks the wire shape so additions require a
test update.

## 3. Pre-conditions

- A clean `cargo check --all-targets` on `main`.
- Gap 02 closed (`cognee_telemetry::send_telemetry` available).
- Sub-agent A confirms the `Config` field names listed in
  [§1](#1-goal) match the current
  [`crates/lib/src/config.rs`](../../crates/lib/src/config.rs)
  layout. If the struct has shifted, sub-agent A updates the table
  in this doc and reports `STATUS: needs-update`.

## 4. Step-by-step

### 4.1 Add the helper method on `Config`

Edit [`crates/lib/src/config.rs`](../../crates/lib/src/config.rs).
Add a new `impl Config` block near the existing public-method
impls (after `resolved_relational_db_url` around line 524 is a
natural spot):

```rust
impl Config {
    /// Returns the redacted property dict merged into `Pipeline Run *`
    /// analytics events.
    ///
    /// **Allowlist-only.** Mirrors Python's `get_current_settings()`
    /// shape but covers only provider/model identifiers and a few
    /// dimension/strategy fields — see
    /// [`docs/telemetry/03/03-settings-snapshot.md`](https://github.com/topoteretes/cognee-rust/blob/main/docs/telemetry/03/03-settings-snapshot.md)
    /// for the rationale on what is omitted (URLs, credentials,
    /// file paths).
    ///
    /// Adding a field here is intentional — there is a snapshot test
    /// that will fail until it is acknowledged.
    pub fn telemetry_snapshot(&self) -> serde_json::Map<String, serde_json::Value> {
        use serde_json::Value;
        let mut m = serde_json::Map::new();
        m.insert("sdk_runtime".into(), Value::String("rust".into()));
        m.insert(
            "vector_db_provider".into(),
            Value::String(self.vector_db_provider.clone()),
        );
        m.insert(
            "graph_db_provider".into(),
            Value::String(self.graph_database_provider.clone()),
        );
        m.insert(
            "relational_db_provider".into(),
            Value::String(self.db_provider.clone()),
        );
        m.insert(
            "llm_provider".into(),
            Value::String(self.llm_provider.clone()),
        );
        m.insert("llm_model".into(), Value::String(self.llm_model.clone()));
        m.insert(
            "embedding_provider".into(),
            Value::String(self.embedding_provider.clone()),
        );
        m.insert(
            "embedding_model".into(),
            Value::String(self.embedding_model_name.clone()),
        );
        m.insert(
            "embedding_dimensions".into(),
            Value::Number(self.embedding_dimensions.into()),
        );
        m.insert(
            "chunk_strategy".into(),
            Value::String(self.chunk_strategy.clone()),
        );
        m
    }
}
```

> **Note on `token_counter`:** the field does not exist on `Config`
> today (it is a `CognifyConfig` choice). See [§4.2](#42-token_counter-source)
> for the resolution.

### 4.2 `token_counter` source

`token_counter` is conceptually a chunking-pipeline choice (sourced
via `TokenCounterKind::from_env()` from `EMBEDDING_PROVIDER` and
`COGNEE_TOKEN_COUNTER` env vars per
[`.claude/CLAUDE.md`](../../../.claude/CLAUDE.md) "cognee-chunking").
It does not live on `Config`.

**Resolution:** the pipeline event emitter in
[task 03-04](04-pipeline-lifecycle-events.md) is the right caller to
add `token_counter` because it has access to the `CognifyConfig` (via
the `Pipeline` it is executing). To keep the responsibility cleanly
split:

- This task (03-03) ships `Config::telemetry_snapshot()` **without**
  the `token_counter` key.
- Task 03-04's emitter merges `token_counter` into the snapshot from
  whichever source is reachable in `execute()`. If no source is
  reachable, the key is omitted (matches Python's behaviour when
  `cognify_config` is not on the call stack).

This split is intentional — the Config-only snapshot is reusable by
other emitters that don't have a `CognifyConfig`.

### 4.3 Re-export from `cognee-lib` if needed

If sub-agent A finds that `cognee-core` (where the pipeline emitter
lives in task 03-04) does **not** depend on `cognee-lib`, expose the
helper as a free function in a sibling crate. Two options:

- **Preferred:** add `Config::telemetry_snapshot` directly on
  `cognee-lib`'s `Config` and have task 03-04's emitter accept the
  pre-built `serde_json::Map` as a parameter (`execute()` plumbs it
  through). This avoids any new dependency.
- **Fallback:** define a `pub fn telemetry_snapshot(config: &Config)`
  free function in `cognee-telemetry` (which is a leaf crate that
  every emitter site already pulls in) and pass `&Config` from the
  caller.

Pick whichever requires no new crate dep edge. Document the choice in
the commit body.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. Unit-test the snapshot output (added in this task).
cargo test -p cognee-lib config::tests::telemetry_snapshot

# 3. Doc gen.
cargo doc -p cognee-lib --no-deps

# 4. Full check.
scripts/check_all.sh
```

### Inline unit tests (add to `crates/lib/src/config.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_snapshot_only_emits_allowlisted_keys() {
        let cfg = Config::default();
        let snap = cfg.telemetry_snapshot();
        let keys: std::collections::BTreeSet<&str> =
            snap.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> = [
            "sdk_runtime",
            "vector_db_provider",
            "graph_db_provider",
            "relational_db_provider",
            "llm_provider",
            "llm_model",
            "embedding_provider",
            "embedding_model",
            "embedding_dimensions",
            "chunk_strategy",
        ]
        .iter()
        .copied()
        .collect();
        assert_eq!(
            keys, expected,
            "telemetry_snapshot must not leak fields outside the allowlist"
        );
    }

    #[test]
    fn telemetry_snapshot_redacts_credentials_and_urls() {
        let mut cfg = Config::default();
        cfg.llm_api_key = "sk-secret".into();
        cfg.embedding_api_key = "sk-also-secret".into();
        cfg.vector_db_password = "vector-pass".into();
        cfg.db_password = "db-pass".into();
        cfg.relational_db_url = "postgres://user:pass@host/db".into();
        cfg.embedding_endpoint = "https://internal.example/v1/embed".into();

        let snap = cfg.telemetry_snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        for forbidden in [
            "sk-secret",
            "sk-also-secret",
            "vector-pass",
            "db-pass",
            "postgres://",
            "internal.example",
        ] {
            assert!(
                !json.contains(forbidden),
                "telemetry_snapshot leaked credential/URL substring: {forbidden}"
            );
        }
    }

    #[test]
    fn telemetry_snapshot_carries_sdk_runtime_rust() {
        let cfg = Config::default();
        let snap = cfg.telemetry_snapshot();
        assert_eq!(
            snap.get("sdk_runtime"),
            Some(&serde_json::Value::String("rust".into()))
        );
    }
}
```

The first test is the **breakage-by-design** snapshot lock: any
maintainer adding a field to `telemetry_snapshot()` must also extend
the `expected` set, which forces a code review on the wire shape.

## 6. Files modified

- [`crates/lib/src/config.rs`](../../crates/lib/src/config.rs) — add
  `pub fn telemetry_snapshot(&self) -> serde_json::Map<...>` and
  three inline unit tests.
- (Optional) [`crates/telemetry/src/lib.rs`](../../crates/telemetry/src/lib.rs)
  — only if the fallback approach in [§4.3](#43-re-export-from-cognee-lib-if-needed)
  is chosen.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Future config field accidentally surfaces credentials/URLs on the wire | Low — allowlist is closed-form. | Two unit tests (allowlist exact-match + credential substring scan) lock the wire shape. |
| `Config` field renamed without updating the snapshot | Compile error — the snapshot reads named fields. | Compiler enforces. |
| Snapshot diverges from Python's `get_current_settings()` keys | Will happen if either side adds a field unilaterally. | E2E parity test in cross-SDK harness can grow to cover this; not blocking for this gap. |
| `embedding_dimensions: u32` truncates above `i64::MAX` | Impossible in practice (the value is a tokenizer dim, ≤ 4096). | Use `serde_json::Number::from(u32)` which is infallible. |

## 8. Out of scope

- `token_counter` — emitted by [task 03-04](04-pipeline-lifecycle-events.md)
  from `CognifyConfig`, not from this helper.
- Run-time tunables that are not Python-parity (e.g. Rust-only
  `embedding_batch_size` is intentionally **not** added — keep the
  snapshot scope conservative).
- Migrating other settings dumps (e.g. logging utility's redacted
  config snapshot) onto this helper — that is a separate refactor.
