# Task 08-03 — Align `run_info` JSON shape with Python

**Status**: not yet implemented (⬜)
**Owner**: _unassigned_
**Depends on**: 08-02.
**Blocks**:
- [Task 08-04 — INITIATED from executor](04-initiated-from-executor.md) (the new INITIATED row uses the same `run_info` plumbing; this task lands the shape for the three runtime states first).
- [Task 08-07 — Library wiring](07-library-pipeline-wiring.md) (the new `DbPipelineWatcher` factor-out uses the same shared write helper introduced here).
- [Task 08-09 — Tests](09-tests.md) (cross-SDK parity test asserts exact `run_info` JSON).

**Parent doc**: [08 — Pipeline Run Status Persistence](../08-pipeline-run-status.md)
**Locked decisions**: 5 (`run_info` JSON shape matches Python byte-for-byte), 6 (`run_info["data"]` items are `Value::String`), 11 (single point of truth — keep watcher impls aligned).

---

## 1. Goal

Update the existing `ScopedRunWatcher` impl so the three runtime statuses (`STARTED`, `COMPLETED`, `ERRORED`) write `run_info` JSON that is **byte-identical** to Python's:

| State | Python `run_info` | Today's Rust `run_info` | After this task |
|-------|-------------------|--------------------------|------------------|
| STARTED | `{"data": <data_info>}` | `None` (no run_info) | `{"data": <data_info>}` |
| COMPLETED | `{"data": <data_info>}` | `None` | `{"data": <data_info>}` |
| ERRORED | `{"data": <data_info>, "error": str(e)}` | `{"error": <msg>}` (no `data` key) | `{"data": <data_info>, "error": <msg>}` |

`data_info` is the helper from task 08-02. The `data_ids` carrier on `PipelineRunInfo` (also from 08-02) feeds the helper.

**INITIATED is not handled in this task** — task 08-04 adds the executor emission and writes `{}` for that state.

## 2. Rationale

Python's `log_pipeline_run_start.py`, `_complete.py`, and `_error.py` all write `{"data": data_info, ...}`. Today the Rust watcher writes `None` (lines 121, 152 of [`scoped_watcher.rs`](../../crates/core/src/pipeline_run_registry/scoped_watcher.rs)) and `{"error": error}` without the `"data"` key (line 185). Downstream Python consumers (notably `cognee/modules/metrics/operations/get_pipeline_run_metrics.py`) read `run_info["data"]` and would see `null` for Rust-written rows.

Aligning the shape here is what makes the cross-SDK harness in task 08-09 viable.

## 3. Pre-conditions

- Tasks 08-01 and 08-02 committed.
- `data_info` helper available at `cognee_core::pipeline_run_registry::data_info`.
- `PipelineRunInfo.data_ids: Vec<Uuid>` field present (task 08-02).
- Repository `log_pipeline_run` accepts `Option<serde_json::Value>` for `run_info` (already true at [`repository.rs:39-46`](../../crates/database/src/pipelines/repository.rs)).

## 4. Step-by-step

### 4.1 Extract a small `run_info` builder

Add to [`crates/core/src/pipeline_run_registry/data_info.rs`](../../crates/core/src/pipeline_run_registry/data_info.rs) (created in task 08-02):

```rust
use serde_json::{Map, Value};

/// Build `run_info` for the `STARTED` / `COMPLETED` rows.
///
/// Matches Python:
///   `run_info = {"data": data_info(data)}`
pub fn run_info_for_running(data_ids: &[Uuid]) -> Value {
    let mut m = Map::with_capacity(1);
    m.insert("data".into(), data_info(data_ids));
    Value::Object(m)
}

/// Build `run_info` for the `ERRORED` row.
///
/// Matches Python:
///   `run_info = {"data": data_info(data), "error": str(e)}`
pub fn run_info_for_errored(data_ids: &[Uuid], error: &str) -> Value {
    let mut m = Map::with_capacity(2);
    m.insert("data".into(), data_info(data_ids));
    m.insert("error".into(), Value::String(error.to_string()));
    Value::Object(m)
}

/// Build `run_info` for the `INITIATED` row. Reserved for task 08-04.
///
/// Matches Python (`log_pipeline_run_initiated.py`): `run_info = {}`
pub fn run_info_for_initiated() -> Value {
    Value::Object(Map::new())
}
```

Re-export from `mod.rs`:

```rust
pub use data_info::{data_info, run_info_for_errored, run_info_for_initiated, run_info_for_running};
```

Unit tests in the same file:

```rust
#[test]
fn started_run_info_matches_python() {
    let id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
    let v = run_info_for_running(&[id]);
    assert_eq!(
        v.to_string(),
        "{\"data\":[\"00000000-0000-0000-0000-000000000001\"]}"
    );
}

#[test]
fn errored_run_info_includes_data_and_error() {
    let id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
    let v = run_info_for_errored(&[id], "boom");
    let obj = v.as_object().expect("object");
    assert_eq!(obj.get("data").unwrap().as_array().unwrap().len(), 1);
    assert_eq!(obj.get("error").unwrap().as_str(), Some("boom"));
}

#[test]
fn started_run_info_with_empty_data_emits_none_literal() {
    let v = run_info_for_running(&[]);
    assert_eq!(v.to_string(), "{\"data\":\"None\"}");
}
```

### 4.2 Update `ScopedRunWatcher::on_pipeline_run_started`

Edit [`crates/core/src/pipeline_run_registry/scoped_watcher.rs`](../../crates/core/src/pipeline_run_registry/scoped_watcher.rs) around line 111-140:

```rust
async fn on_pipeline_run_started(&self, run: &PipelineRunInfo) {
    let run_info = Some(super::run_info_for_running(&run.data_ids));
    let db_result = self
        .db
        .log_pipeline_run(
            run.run_id,
            run.pipeline_id,
            &run.pipeline_name,
            run.dataset_id,
            core_to_db_status(&run.status),
            run_info,
        )
        .await;
    if let Err(e) = db_result {
        tracing::warn!(
            run_id = %self.run_id,
            "ScopedRunWatcher: DB write for Started failed (non-fatal): {e}"
        );
    }
    self.sink.set_phase(RunPhase::Running);
    self.sink.publish(RunEvent {
        run_id: self.run_id,
        kind: RunEventKind::Started,
        payload: serde_json::Value::Null,
        at: Utc::now(),
    });
}
```

### 4.3 Update `ScopedRunWatcher::on_pipeline_run_completed`

Around line 142-171:

```rust
async fn on_pipeline_run_completed(&self, run: &PipelineRunInfo, _output_count: usize) {
    let run_info = Some(super::run_info_for_running(&run.data_ids));
    let db_result = self
        .db
        .log_pipeline_run(
            run.run_id,
            run.pipeline_id,
            &run.pipeline_name,
            run.dataset_id,
            DbStatus::Completed,
            run_info,
        )
        .await;
    if let Err(e) = db_result {
        tracing::warn!(
            run_id = %self.run_id,
            "ScopedRunWatcher: DB write for Completed failed (non-fatal): {e}"
        );
    }
    self.sink.set_phase(RunPhase::Completed);
    self.sink.publish(RunEvent {
        run_id: self.run_id,
        kind: RunEventKind::Completed,
        payload: serde_json::Value::Null,
        at: Utc::now(),
    });
}
```

### 4.4 Update `ScopedRunWatcher::on_pipeline_run_errored`

Around line 183-217:

```rust
async fn on_pipeline_run_errored(&self, run: &PipelineRunInfo, error: &str) {
    let run_info = Some(super::run_info_for_errored(&run.data_ids, error));
    let db_result = self
        .db
        .log_pipeline_run(
            run.run_id,
            run.pipeline_id,
            &run.pipeline_name,
            run.dataset_id,
            DbStatus::Errored,
            run_info,
        )
        .await;
    if let Err(e) = db_result {
        tracing::warn!(
            run_id = %self.run_id,
            "ScopedRunWatcher: DB write for Errored failed (non-fatal): {e}"
        );
    }
    self.sink.set_phase(RunPhase::Errored {
        message: error.to_string(),
    });
    self.sink.publish(RunEvent {
        run_id: self.run_id,
        kind: RunEventKind::Errored {
            message: error.to_string(),
        },
        payload: serde_json::Value::Null,
        at: Utc::now(),
    });
}
```

Drop the now-unused `use serde_json::json;` at the top of the file.

### 4.5 `DefaultPipelineRunRegistry`'s direct writes

Inspect [`default_impl.rs`](../../crates/core/src/pipeline_run_registry/default_impl.rs) `run_work_inline` (~line 271) and `register_background` (~line 365). If they call `log_pipeline_run` *outside* of `ScopedRunWatcher` (i.e. directly on `self.repo`), update those sites too to use the new helpers. Today the inline path writes the `STARTED` row before the work future runs (see lines around 290-310) — that call must now pass `Some(run_info_for_running(&spec.data_ids))`.

Concretely, search:

```bash
rg "\.log_pipeline_run\(" crates/core/src/pipeline_run_registry/
```

Every call site picks up the corresponding helper.

### 4.6 No watcher-trait change

The `PipelineWatcher` trait stays unchanged in this task. Adding `on_pipeline_run_initiated` is task 08-04's responsibility.

### 4.7 Build + test

```bash
cargo check --all-targets
cargo test -p cognee-core --lib -- data_info
cargo test -p cognee-core --test scoped_watcher_payload_persistence
```

The existing scoped-watcher tests at [`crates/core/tests/scoped_watcher_payload_persistence.rs`](../../crates/core/tests/scoped_watcher_payload_persistence.rs) will need their `run_info` assertions updated — `None` becomes `Some({"data": …})`. Task 09 covers the new assertion shape; this task only fixes the existing assertions so the file compiles.

## 5. Verification

```bash
# 1. Compiles.
cargo check --all-targets

# 2. New helper unit tests pass.
cargo test -p cognee-core --lib -- data_info::tests

# 3. Existing scoped-watcher tests pass against the new shape (update them
#    in this task if they assert run_info == None).
cargo test -p cognee-core --test scoped_watcher_payload_persistence

# 4. HTTP-server activity smoke test still passes (response unchanged because
#    the wire DTO does not include run_info).
cargo test -p cognee-http-server --test activity_router

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/core/src/pipeline_run_registry/data_info.rs`](../../crates/core/src/pipeline_run_registry/data_info.rs) — add `run_info_for_running`, `run_info_for_errored`, `run_info_for_initiated`.
- [`crates/core/src/pipeline_run_registry/mod.rs`](../../crates/core/src/pipeline_run_registry/mod.rs) — re-export the new helpers.
- [`crates/core/src/pipeline_run_registry/scoped_watcher.rs`](../../crates/core/src/pipeline_run_registry/scoped_watcher.rs) — three lifecycle methods use the helpers; drop unused `json!` import.
- [`crates/core/src/pipeline_run_registry/default_impl.rs`](../../crates/core/src/pipeline_run_registry/default_impl.rs) — any direct `log_pipeline_run` call gets the helper too.
- [`crates/core/tests/scoped_watcher_payload_persistence.rs`](../../crates/core/tests/scoped_watcher_payload_persistence.rs) — fix any assertion that asserted `run_info == None`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Existing tests assert `run_info == None` and break | High — desired; tests are stale. | Update the assertions in this task to `Some(json!({"data": "None"}))` (or the appropriate ids). |
| `data_ids` is empty on HTTP-driven paths (task 08-02 §4.5 left this empty) → `run_info` is `{"data": "None"}` for HTTP runs | Acknowledged — fixed in task 08-07 when library wiring populates the carrier. | Documented in 08-02; cross-SDK test in 08-09 will exercise the library path. |
| The watcher writes `run_info` synchronously inside the lifecycle method; a slow DB makes the rest of the run wait | Low — `log_pipeline_run` is already awaited today (decision 10). | No change in latency profile. |
| `run_info["data"]` serialises differently on Postgres vs SQLite due to JSONB normalisation | Low — Postgres `JSON` (not `JSONB`) preserves text; SQLite stores text verbatim. | The `Json` SeaORM column is `JSON`, not `JSONB`. Cross-SDK test in 08-09 will catch any divergence. |
| Backwards-incompatible read for any HTTP consumer that pattern-matches on `run_info` | Low — the wire DTO at `activity.rs:75-88` does **not** include `run_info`. | No external consumer relies on the JSON shape today. |

## 8. Out of scope

- Adding `on_pipeline_run_initiated` to the watcher trait (task 08-04).
- Writing `run_info["pipeline_run_id"]` or other Python-private keys. Python only writes `{"data": …}` and `{"data": …, "error": …}` — Rust matches exactly.
- Adding a structured `RunInfo` Rust type. The wire format is `serde_json::Value`; a typed alias would not buy parity.
- Backfilling existing rows. Pre-existing rows keep their old shape (`None` / `{"error": …}`); only new rows from this point follow Python's shape.
