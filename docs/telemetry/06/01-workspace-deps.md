# Task 06-01 — Workspace dependencies for file logging

**Status**: ✅ implemented in commit ca62d29
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 06-02 — Logging config](02-logging-config.md) (the new crate uses `tracing-appender`).
- [Task 06-05 — init_logging](05-init-logging.md) (uses `tracing-subscriber`'s `json` feature).

**Parent doc**: [06 — File-Based Logging with Rotation](../06-file-logging-rotation.md)
**Locked decisions**: decision 1 (time-based rotation via `tracing-appender`), decision 3 (JSON format toggle).

---

## 1. Goal

Add the two workspace-level dependency changes that unblock the rest
of gap 06:

1. Add `tracing-appender = "0.2"` to the `[workspace.dependencies]`
   table.
2. Add the `"json"` feature to the existing `tracing-subscriber`
   workspace dep so `fmt::layer().json()` is callable.

No code changes — this is a pure `Cargo.toml` edit. Later tasks pull
the dep into `crates/logging/Cargo.toml` (task 06-02) and turn it on
in init code (task 06-05).

## 2. Rationale

- `tracing-appender::RollingFileAppender` is the v1 rotation
  mechanism (decision 1). It must be a workspace dep so
  `crates/logging` can declare `tracing-appender.workspace = true`
  and inherit the pinned version.
- `tracing-subscriber`'s default workspace features are
  `["env-filter", "fmt"]`. The JSON formatter lives behind the
  `"json"` feature flag and is not currently compiled in. Decision 3
  requires `COGNEE_LOG_FORMAT=json` to be a runtime toggle, so the
  feature must be on.
- Landing the dep changes as a standalone commit lets task 06-02 do a
  pure-source addition with no `Cargo.lock` churn other than the new
  crate node.

## 3. Pre-conditions

- Clean `cargo check --all-targets` on `main`.
- No outstanding edits to [`Cargo.toml`](../../../Cargo.toml).
- `tracing-subscriber` currently declared at
  [`Cargo.toml:119`](../../../Cargo.toml#L119) as
  `{ version = "0.3", features = ["env-filter", "fmt"] }`.

## 4. Step-by-step

### 4.1 Add `tracing-appender` to `[workspace.dependencies]`

Edit [`Cargo.toml`](../../../Cargo.toml). Insert alphabetically near
the other `tracing-*` entries (between `tracing-opentelemetry` and
`tracing-subscriber`):

```toml
tracing-appender = "0.2"
```

### 4.2 Enable the `json` feature on `tracing-subscriber`

Edit the existing line at [`Cargo.toml:119`](../../../Cargo.toml#L119):

```toml
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json"] }
```

### 4.3 Refresh the lockfile

```bash
cargo update -p tracing-subscriber
cargo update -p tracing-appender
```

These should be no-ops on `tracing-subscriber` (just feature toggle)
and add the new `tracing-appender` node + its transitive
`crossbeam-channel` / `time` deps to `Cargo.lock`.

## 5. Verification

```bash
# 1. Compile everything (no consumers yet but lock must be consistent).
cargo check --all-targets

# 2. Sanity-check that the new dep resolves.
cargo tree -p tracing-appender | head

# 3. Sanity-check json feature is on.
cargo tree -e features -p tracing-subscriber | grep -E "json|fmt" | head

# 4. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`Cargo.toml`](../../../Cargo.toml) — one new line, one feature
  addition.
- `Cargo.lock` — automatic.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `tracing-appender 0.2` requires a newer MSRV than the workspace | Very low — `tracing-appender` 0.2.x targets the same MSRV as `tracing-subscriber` 0.3 (Rust 1.65+); current workspace is on edition 2024 (Rust 1.85+). | If it does fail, the build will surface immediately at `cargo check`. |
| `json` feature pulls in `serde_json` transitively bloating binaries that did not use it | Low — `serde_json` is already a direct dependency of `cognee-models`, `cognee-llm`, and many others; adding it as a tracing-subscriber transitive is free. | n/a |
| `Cargo.lock` churn unrelated to the dep change | Low | Commit the lockfile in the same commit; `cargo update -p <crate>` keeps the diff focused. |

## 8. Out of scope

- Adding any code that imports `tracing_appender`. That belongs in
  task 06-02 (`crates/logging` declares the dep) and task 06-05
  (init helper uses `RollingFileAppender`).
- Bumping the `tracing` or `tracing-subscriber` major versions.
  Gap 06 piggybacks on the existing pin.
- Adding `tracing-rolling-file` (the size-based alternative). Decision
  1 deferred this to a follow-up.
