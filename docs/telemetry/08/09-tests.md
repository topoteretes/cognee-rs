# Task 08-09 — Tests for gap 08

**Status**: implemented in commit 08c5140 (sub-doc named a new file `crates/http-server/tests/activity_pipeline_runs.rs`; orchestrator extended the existing `test_activity_pipeline_runs.rs` instead — same coverage. Reset-helper tests live in `crates/lib/tests/pipeline_runs_reset.rs` from 08-05 — cognee-database can't depend on cognee-lib's reset API. Cross-SDK `test_pipeline_runs_parity.py` lives under `e2e-cross-sdk/harness/`; not executed locally, CI runs via docker compose)
**Owner**: _unassigned_
**Depends on**: tasks 08-01 through 08-08 — every implementation task must be in place before tests land.
**Blocks**:
- [Task 08-10 — Docs and CI](10-docs-and-ci.md).

**Parent doc**: [08 — Pipeline Run Status Persistence](../08-pipeline-run-status.md)
**Locked decisions**: all (this task locks in the invariants every previous task ships).

---

## 1. Goal

Land the test surface that locks in gap 08's invariants:

1. **`SeaOrmPipelineRunRepository`** — four-state round-trip, dataset_id=None round-trip, exact `run_info` JSON shape, reader helpers, qualification verdict.
2. **Executor lifecycle** — `pipeline::execute` writes four rows in order, populates `run_info["data"]` correctly, surfaces `ERRORED` with `{"data": ..., "error": ...}`.
3. **HTTP-server activity endpoint** — returns the four-state trail with the right enum strings and per-row attribution.
4. **CLI lifecycle** — `cognee-cli cognify` against a fresh dataset writes four rows; running it again returns "already complete" and writes zero new rows.
5. **Cross-SDK parity** — Python `cognify` + Rust read; Rust `cognify` + Python read; `run_info` JSON byte-identical.

All env-mutating tests are serialised.

## 2. Rationale

Each implementation task could land its own tests inline, but batching them here lets sub-agent C run a single `cargo test` / `pytest` pass per crate and catch cross-task regressions (e.g. task 08-04's INITIATED row breaking task 08-06's reader helpers).

## 3. Pre-conditions

- Tasks 08-01 through 08-08 committed.
- `cognee-test-utils` includes mocks for `StorageTrait`, `GraphDBTrait`, `VectorDB` so cognify integration tests can run without a real LLM. Confirm via `rg "MockStorage\|MockGraphDB\|MockVectorDB" crates/test-utils/`.
- `e2e-cross-sdk/Dockerfile` already builds both Rust + Python SDKs into a single image; the new test reuses that image.

## 4. Step-by-step

### 4.1 Extend `crates/database/tests/pipeline_run_repository.rs`

Add or extend the following tests (the file already exists from prior work; only delta is added):

#### `log_pipeline_run_persists_with_none_dataset_id`

Asserts the silent-drop branch removed in task 08-01 is gone:

```rust
#[tokio::test]
#[serial]
async fn log_pipeline_run_persists_with_none_dataset_id() {
    let db = inmem_database().await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::new(db));
    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();
    let row_id = repo
        .log_pipeline_run(prid, pid, "ad_hoc_pipeline", None, PipelineRunStatus::Started, None)
        .await
        .unwrap();
    let rows = repo.list_recent(None, 10).await.unwrap();
    assert_eq!(rows.len(), 1, "row must persist even when dataset_id is None");
    assert_eq!(rows[0].id, row_id);
    assert!(rows[0].dataset_id.is_none(), "dataset_id should be None");
}
```

#### `four_state_lifecycle_round_trip`

```rust
#[tokio::test]
#[serial]
async fn four_state_lifecycle_round_trip() {
    let db = inmem_database().await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::new(db));
    let prid = Uuid::new_v4();
    let pid = Uuid::new_v4();
    let did = Uuid::new_v4();

    for status in [
        PipelineRunStatus::Initiated,
        PipelineRunStatus::Started,
        PipelineRunStatus::Completed,
    ] {
        repo.log_pipeline_run(prid, pid, "cognify_pipeline", Some(did), status, None)
            .await
            .unwrap();
    }

    let latest = repo
        .get_pipeline_run_by_dataset(did, "cognify_pipeline")
        .await
        .unwrap()
        .expect("row");
    assert_eq!(latest.status, PipelineRunStatus::Completed);
}
```

Plus the `Errored` variant in a separate test.

#### `run_info_shape_matches_python`

```rust
#[tokio::test]
#[serial]
async fn run_info_shape_matches_python() {
    use cognee_core::pipeline_run_registry::{run_info_for_errored, run_info_for_initiated, run_info_for_running};
    use serde_json::json;

    let id1 = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();

    assert_eq!(run_info_for_initiated(), json!({}));
    assert_eq!(run_info_for_running(&[id1]), json!({"data": ["00000000-0000-0000-0000-000000000001"]}));
    assert_eq!(run_info_for_running(&[]), json!({"data": "None"}));
    assert_eq!(
        run_info_for_errored(&[id1], "boom"),
        json!({"data": ["00000000-0000-0000-0000-000000000001"], "error": "boom"})
    );
}
```

#### `reader_helpers_return_latest`

For each of `get_pipeline_run`, `get_pipeline_run_by_dataset`, `get_pipeline_runs_by_dataset`:

- Write three rows (INITIATED → STARTED → COMPLETED) with the same `pipeline_run_id`; assert the helper returns `COMPLETED`.
- Write two pipeline names for the same dataset; assert `get_pipeline_runs_by_dataset` returns two rows, one per name, each the latest.
- Verify `get_pipeline_run` returns `None` for an unknown `pipeline_run_id`.

#### `reset_pipeline_run_status_writes_initiated_with_empty_run_info`

```rust
#[tokio::test]
#[serial]
async fn reset_pipeline_run_status_writes_initiated_with_empty_run_info() {
    use cognee_lib::api::pipeline_runs::reset_pipeline_run_status;
    use serde_json::json;

    let db = inmem_database().await;
    let repo: Arc<dyn PipelineRunRepository> =
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::new(db)));
    let user_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();

    reset_pipeline_run_status(repo.clone(), user_id, dataset_id, "cognify_pipeline")
        .await
        .unwrap();

    let rows = repo.list_recent(Some(dataset_id), 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, PipelineRunStatus::Initiated);
    assert_eq!(rows[0].run_info.as_ref().unwrap(), &json!({}));
}
```

### 4.2 New `crates/core/tests/pipeline_run_lifecycle.rs`

Executor-level test that confirms `pipeline::execute` writes the four-state trail to the real repo through `DbPipelineWatcher` (task 08-07):

```rust
#[tokio::test]
#[serial]
async fn execute_writes_four_state_trail() {
    let db = inmem_database().await;
    let repo: Arc<dyn PipelineRunRepository> =
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::new(db)));
    let watcher = DbPipelineWatcher::new(repo.clone());

    let pipeline = trivial_pipeline_that_succeeds();
    let inputs = vec![/* mock data */];
    let ctx = test_context();

    let _ = cognee_core::pipeline::execute(&pipeline, inputs, &ctx, &watcher)
        .await
        .unwrap();

    let rows = repo.list_recent(None, 10).await.unwrap();
    assert_eq!(rows.len(), 3, "INITIATED + STARTED + COMPLETED");
    // Verify ordering by created_at.
    assert_eq!(rows[2].status, PipelineRunStatus::Initiated);
    assert_eq!(rows[1].status, PipelineRunStatus::Started);
    assert_eq!(rows[0].status, PipelineRunStatus::Completed);
}

#[tokio::test]
#[serial]
async fn execute_writes_errored_trail_on_failure() {
    // Same setup, pipeline whose task panics.
    // Expect INITIATED → STARTED → ERRORED with run_info["error"] populated.
}
```

### 4.3 New `crates/http-server/tests/activity_pipeline_runs.rs`

```rust
#[tokio::test]
#[serial]
async fn pipeline_runs_endpoint_returns_four_state_trail() {
    let state = make_test_state().await;
    let dataset = state.fixtures.dataset();

    // Dispatch a cognify run that succeeds.
    dispatch_cognify(&state, dataset.id).await.unwrap();

    let resp = state
        .client
        .get("/api/v1/activity/pipeline-runs")
        .query(&[("dataset_id", &dataset.id.to_string())])
        .send()
        .await
        .unwrap();
    let dtos: Vec<PipelineRunListItemDTO> = resp.json().await.unwrap();
    assert_eq!(dtos.len(), 3, "INITIATED + STARTED + COMPLETED");
    let statuses: Vec<&str> = dtos.iter()
        .map(|d| d.status.as_deref().unwrap())
        .collect();
    // Newest first.
    assert_eq!(
        statuses,
        vec![
            "DATASET_PROCESSING_COMPLETED",
            "DATASET_PROCESSING_STARTED",
            "DATASET_PROCESSING_INITIATED",
        ]
    );
}
```

### 4.4 New `crates/cli/tests/cli_pipeline_runs.rs`

```rust
#[tokio::test]
#[serial]
async fn cli_cognify_writes_pipeline_runs() {
    let workdir = tempfile::tempdir().unwrap();
    let bin = cargo_bin("cognee-cli");

    // Stage a tiny dataset, run cognify, query the SQLite DB directly.
    cmd!(bin, "add", "--dataset", "test", "hello world").env("COGNEE_HOME", workdir.path()).run().unwrap();
    cmd!(bin, "cognify", "--dataset", "test").env("COGNEE_HOME", workdir.path()).run().unwrap();

    let db = open_sqlite(&workdir.path().join("cognee.db"));
    let count: i64 = db.query_row("SELECT COUNT(*) FROM pipeline_runs", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 3, "INITIATED + STARTED + COMPLETED");
}

#[tokio::test]
#[serial]
async fn cli_cognify_short_circuits_on_completed() {
    // Run cognify twice. First call writes 3 rows; second call writes 0 new rows.
    // Stdout should contain "already complete" for the second call.
}
```

### 4.5 New `crates/cognify/tests/cognify_qualification.rs`

```rust
#[tokio::test]
#[serial]
async fn cognify_short_circuits_on_completed() {
    // Set up cognify with mock everything + real SeaOrmPipelineRunRepository.
    // Seed pipeline_runs with a COMPLETED row for (dataset_id, "cognify_pipeline").
    // Call cognify; assert result.already_completed == true and no new rows.
}

#[tokio::test]
#[serial]
async fn cognify_rejects_on_started() {
    // Seed pipeline_runs with a STARTED row (no COMPLETED yet).
    // Call cognify; expect Err(CognifyError::PipelineAlreadyRunning).
}

#[tokio::test]
#[serial]
async fn cognify_proceeds_on_errored() {
    // Seed pipeline_runs with an ERRORED row.
    // Call cognify; expect success and four new rows (the seed + new lifecycle).
}

#[tokio::test]
#[serial]
async fn cognify_proceeds_on_initiated_reset_row() {
    // Use reset_pipeline_run_status to write an INITIATED row.
    // Call cognify; expect it to proceed.
}
```

### 4.6 New `e2e-cross-sdk/harness/test_pipeline_runs_parity.py`

```python
"""Cross-SDK pipeline_runs parity (gap 08 decision 8).

Verifies:
1. Schema parity (PRAGMA table_info) — column types and nullability match.
2. Cross-write/read — Python writes a four-state trail, Rust reads it via
   `cognee-cli internal pipeline-runs list` (added as a thin debug subcommand).
3. run_info parity — JSON byte-identical at every state.
4. pipeline_run_id derivation — Python's `generate_pipeline_run_id` matches
   Rust's `pipeline_run_id` helper.
"""

import json
import sqlite3
import subprocess
from pathlib import Path

import pytest

from harness.helpers import setup_workspace, rust_cli, python_cli


@pytest.fixture
def workspace(tmp_path) -> Path:
    return setup_workspace(tmp_path)


def test_schema_columns_match(workspace):
    """PRAGMA table_info(pipeline_runs) returns the same columns / types
    after Python and Rust have both run their migrations."""
    rust_cli(workspace, ["add", "--text", "hello"])  # Rust applies migrations.
    python_cli(workspace, ["add", "hello"])           # Python applies its own.

    conn = sqlite3.connect(workspace / "cognee.db")
    info = conn.execute("PRAGMA table_info(pipeline_runs)").fetchall()
    cols = {row[1]: (row[2], row[3]) for row in info}  # name -> (type, notnull)
    assert cols["dataset_id"][1] == 0, "dataset_id must be nullable"
    assert "run_info" in cols
    # ... other column assertions ...


def test_cross_read_python_writes_rust_reads(workspace):
    """Python cognifies; Rust read surfaces the four-state trail."""
    python_cli(workspace, ["add", "hello world"])
    python_cli(workspace, ["cognify"])

    out = rust_cli(workspace, ["internal", "pipeline-runs", "list", "--json"])
    rows = json.loads(out.stdout)
    assert len(rows) == 4, f"expected 4 rows, got {len(rows)}"
    statuses = [r["status"] for r in rows]
    assert statuses == [
        "DATASET_PROCESSING_COMPLETED",
        "DATASET_PROCESSING_STARTED",
        "DATASET_PROCESSING_INITIATED",
        # ...the add pipeline's COMPLETED row...
    ]


def test_run_info_json_byte_identical(workspace):
    """run_info from Python rows parses to the exact shape Rust writes."""
    python_cli(workspace, ["add", "hello"])
    python_cli(workspace, ["cognify"])
    conn = sqlite3.connect(workspace / "cognee.db")
    rows = conn.execute(
        "SELECT status, run_info FROM pipeline_runs ORDER BY created_at"
    ).fetchall()
    for status, run_info_blob in rows:
        info = json.loads(run_info_blob)
        if status == "DATASET_PROCESSING_INITIATED":
            assert info == {}
        elif status in ("DATASET_PROCESSING_STARTED", "DATASET_PROCESSING_COMPLETED"):
            assert "data" in info
            assert isinstance(info["data"], (list, str))
        elif status == "DATASET_PROCESSING_ERRORED":
            assert set(info.keys()) == {"data", "error"}


def test_pipeline_run_id_derivation_matches():
    """Rust and Python derive identical pipeline_run_id from the same inputs."""
    pid = "00000000-0000-0000-0000-000000000001"
    did = "00000000-0000-0000-0000-000000000002"
    rust_out = subprocess.check_output(
        ["cognee-cli", "internal", "pipeline-run-id", "--pipeline-id", pid, "--dataset-id", did],
        text=True,
    ).strip()
    py_out = subprocess.check_output(
        ["python", "-c",
         "from cognee.modules.pipelines.utils.generate_pipeline_run_id import generate_pipeline_run_id;"
         "from uuid import UUID;"
         f"print(generate_pipeline_run_id(UUID('{pid}'), UUID('{did}')))"],
        text=True,
    ).strip()
    assert rust_out == py_out
```

### 4.7 New `cognee-cli internal pipeline-runs list` debug subcommand

Required by the cross-SDK test. Add a minimal subcommand in [`crates/cli/src/commands/`](../../crates/cli/src/commands/) that prints `pipeline_runs` rows as JSON. Surface behind an `internal` parent group so it doesn't pollute the user-facing help.

```rust
// crates/cli/src/commands/internal/pipeline_runs.rs
#[derive(Args)]
pub struct ListArgs {
    #[arg(long)]
    json: bool,
}

pub async fn list(args: ListArgs, db: Arc<DatabaseConnection>) -> Result<(), CliError> {
    let repo = SeaOrmPipelineRunRepository::new(db);
    let rows = repo.list_recent(None, 50).await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        for r in rows {
            println!("{} {} {}", r.created_at, r.status, r.pipeline_name);
        }
    }
    Ok(())
}
```

Plus a `cognee-cli internal pipeline-run-id --pipeline-id <uuid> --dataset-id <uuid>` echo subcommand for the derivation parity test.

### 4.8 Wire into `e2e-cross-sdk/docker-compose.yml`

The existing `docker compose up --build` already collects every test under `harness/`. The new test file picks up automatically. Verify by reading `pytest.ini` / `pyproject.toml` discovery settings; if there's an explicit allowlist, add the new file.

## 5. Verification

```bash
# Rust unit/integration tests
cargo test -p cognee-database --test pipeline_run_repository
cargo test -p cognee-core --test pipeline_run_lifecycle
cargo test -p cognee-http-server --test activity_pipeline_runs
cargo test -p cognee-cli --test cli_pipeline_runs
cargo test -p cognee-cognify --test cognify_qualification

# Cross-SDK harness
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit && cd -

# Full check
scripts/check_all.sh
```

## 6. Files modified

- [`crates/database/tests/pipeline_run_repository.rs`](../../crates/database/tests/pipeline_run_repository.rs) — extended (5+ new tests).
- [`crates/core/tests/pipeline_run_lifecycle.rs`](../../crates/core/tests/pipeline_run_lifecycle.rs) — **NEW**.
- [`crates/http-server/tests/activity_pipeline_runs.rs`](../../crates/http-server/tests/activity_pipeline_runs.rs) — **NEW**.
- [`crates/cli/tests/cli_pipeline_runs.rs`](../../crates/cli/tests/cli_pipeline_runs.rs) — **NEW**.
- [`crates/cognify/tests/cognify_qualification.rs`](../../crates/cognify/tests/cognify_qualification.rs) — **NEW**.
- [`crates/cli/src/commands/internal/`](../../crates/cli/src/commands/) — **NEW** `internal pipeline-runs list` + `internal pipeline-run-id` subcommands.
- [`e2e-cross-sdk/harness/test_pipeline_runs_parity.py`](../../e2e-cross-sdk/harness/test_pipeline_runs_parity.py) — **NEW**.
- (Possibly) `e2e-cross-sdk/Dockerfile` — confirm the harness picks up the new test file via existing patterns; no change expected.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `inmem_database()` test helper races on `serial_test` boundary | Low — each test owns its DB. | All tests use a fresh `SqlitePool::connect("sqlite::memory:")`. |
| Cross-SDK test depends on the new `internal pipeline-runs list` subcommand existing in the docker image | High — required for parity. | Add the subcommand in this task; document in the Dockerfile if a separate build step is needed. |
| CLI E2E test is slow (≥5s due to cargo build + tokio setup) | Medium | Use `assert_cmd` with the pre-built binary; reuse the existing CLI E2E test pattern. |
| The cross-SDK test breaks because Python's pipeline_run_id derivation depends on Python `str(uuid.UUID(...))` which is `36`-char hyphenated; Rust's `Uuid::to_string()` matches | Low — both languages produce the same string. | The `test_pipeline_run_id_derivation_matches` test enforces this. |
| The `Errored` arm test runs a deliberately panicking pipeline and asserts the row content; the panic propagates and kills the test runner | Medium | Use `panic::catch_unwind` or a task that returns `Err` (not `panic!`). The executor wraps task errors in `Result`. |
| `dataset_id IS NULL` test row in cross-SDK harness can't be inserted by Rust before task 08-01 lands | Acknowledged — that's why this task depends on 08-01. | Sequencing enforced. |
| Python's `cognee.cognify()` writes 4 rows under one `pipeline_run_id`; the Rust read needs to filter to the right run | Low | Test seeds a deterministic dataset id and asserts the four-row count for that id. |

## 8. Out of scope

- Performance tests (write throughput, query latency). The change is correctness-focused.
- Property tests / fuzzing of UUID derivation. Deterministic helpers are unit-tested directly.
- Tests for the LIB-06 sidecar table. Decision 9 keeps it Rust-only; no cross-SDK coverage.
- Tests for ingestion's `AddPipeline` short-circuit. Decision 3 excludes ingestion from the qualification gate.
- Load testing the qualification check under concurrent re-cognify calls. The race is acceptable (matches Python).
