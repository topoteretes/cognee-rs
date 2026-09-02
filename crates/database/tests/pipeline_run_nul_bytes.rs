#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression: NUL bytes must be stripped on the `SeaOrmPipelineRunRepository`
//! write paths.
//!
//! Postgres rejects `0x00` in `text`, and rejects the `\u0000` escape that
//! `serde_json` emits for it inside a `json` column. The
//! `pipeline_runs.run_info` and `pipeline_run_payload_fields.value` columns are
//! both `json`, and both carry arbitrary text: `run_info_for_errored` embeds the
//! error string (which routinely quotes offending source text extracted from a
//! PDF), and `TaskContext::publish_payload_field` lets a task publish anything
//! at all.
//!
//! What makes an unsanitized write here *silent* rather than loud: every
//! production caller reaches these methods through the run watchers in
//! `cognee_core::pipeline_run_registry`, which log-and-ignore a failed write.
//! An Errored row that fails to insert leaves the run recorded as `Started`
//! with the error never stored anywhere.
//!
//! These methods build their `ActiveModel` field-by-field, so the sanitizing
//! `From<&PipelineRun> for pipeline_run::ActiveModel` conversion never sees
//! their payloads — the guard has to live in the methods themselves.
//!
//! The SQLite cases below run in every lane. SQLite happily stores an embedded
//! NUL, so they assert on what came *back* out of the column rather than on the
//! write succeeding; that is what makes them fail without the fix. The
//! `postgres` module pins the failure mode itself, and is skipped when
//! `TEST_POSTGRES_URL` is unset.

use std::sync::Arc;

use cognee_database::{
    DatabaseConnection, PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository,
    connect, initialize, ops,
};
use cognee_models::Dataset;
use serde_json::{Value, json};
use uuid::Uuid;

/// A `run_info` payload shaped like `run_info_for_errored`, with a literal NUL
/// inside the error string — the shape a PDF-extraction failure produces.
fn dirty_errored_run_info(data_id: Uuid) -> Value {
    json!({
        "data": [data_id.to_string()],
        "error": "parse failed near: sour\u{0}ce te\u{0}xt",
    })
}

/// Assert a JSON value carries no NUL anywhere — in a string, an array element,
/// a nested object value, or an object *key*.
///
/// Walks the `Value` itself rather than its rendering. Inspecting the rendering
/// for the six-character escape `\u0000` false-fails on legitimate content: a
/// string that really contains a backslash followed by `u0000` renders with the
/// backslash doubled, which still contains that substring. If no key and no
/// string holds an actual NUL codepoint, `serde_json` cannot emit a NUL escape
/// either, so walking the value is both stricter and sufficient.
fn assert_no_nul(value: &Value, what: &str) {
    if let Some((path, offender)) = find_nul(value, "$".to_string()) {
        panic!("{what} still carries a NUL byte at {path}: {offender:?}");
    }
}

/// The path of the first NUL-bearing object key or string value, paired with
/// the offending string; `None` when the value is clean. Numbers, booleans and
/// null hold no codepoints, so they cannot carry one.
fn find_nul(value: &Value, path: String) -> Option<(String, String)> {
    match value {
        Value::String(text) => text.contains('\u{0}').then(|| (path, text.clone())),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(index, item)| find_nul(item, format!("{path}[{index}]"))),
        Value::Object(entries) => entries.iter().find_map(|(key, entry)| {
            let child = format!("{path}.{}", key.escape_debug());
            if key.contains('\u{0}') {
                return Some((format!("{child} (key)"), key.clone()));
            }
            find_nul(entry, child)
        }),
        _ => None,
    }
}

async fn sqlite_db() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    Arc::new(db)
}

/// Pre-create a dataset row so the FK on `pipeline_runs.dataset_id` passes.
async fn create_dataset(db: &DatabaseConnection, id: Uuid) {
    let dataset = Dataset::new("nul-test".to_string(), Uuid::new_v4(), None, id);
    ops::datasets::create_dataset(db, dataset)
        .await
        .expect("create_dataset for FK setup");
}

// ---------------------------------------------------------------------------
// log_pipeline_run
// ---------------------------------------------------------------------------

#[tokio::test]
async fn log_pipeline_run_strips_nul_from_run_info() {
    let db = sqlite_db().await;
    let dataset_id = Uuid::new_v4();
    create_dataset(&db, dataset_id).await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));

    let data_id = Uuid::new_v4();
    let pipeline_run_id = Uuid::new_v4();
    repo.log_pipeline_run(
        pipeline_run_id,
        Uuid::new_v4(),
        "nul_pipeline",
        Some(dataset_id),
        PipelineRunStatus::Errored,
        Some(dirty_errored_run_info(data_id)),
    )
    .await
    .expect("the Errored row must be written, not dropped");

    let row = repo
        .get_pipeline_run(pipeline_run_id)
        .await
        .expect("get_pipeline_run")
        .expect("row present");
    let run_info = row.run_info.expect("run_info populated");
    assert_no_nul(&run_info, "stored run_info");

    // The surrounding text must survive intact — only the NUL is removed.
    assert_eq!(
        run_info["error"],
        json!("parse failed near: source text"),
        "sanitizing must strip the NUL and nothing else"
    );
    assert_eq!(run_info["data"], json!([data_id.to_string()]));
}

/// Object *keys* are sanitized too — `sanitize_json` rewrites them, and a NUL
/// in a key is just as fatal to the Postgres json parser as one in a value.
#[tokio::test]
async fn log_pipeline_run_strips_nul_from_nested_keys_and_values() {
    let db = sqlite_db().await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));

    let pipeline_run_id = Uuid::new_v4();
    repo.log_pipeline_run(
        pipeline_run_id,
        Uuid::new_v4(),
        "nul_pipeline",
        // Ad-hoc run: no dataset, so no FK row to seed.
        None,
        PipelineRunStatus::Errored,
        Some(json!({
            "ke\u{0}y": {"nested": ["a\u{0}b", {"deep": "c\u{0}d"}]},
        })),
    )
    .await
    .expect("log_pipeline_run");

    let run_info = repo
        .get_pipeline_run(pipeline_run_id)
        .await
        .expect("get_pipeline_run")
        .expect("row present")
        .run_info
        .expect("run_info populated");

    assert_no_nul(&run_info, "stored run_info");
    assert_eq!(
        run_info,
        json!({"key": {"nested": ["ab", {"deep": "cd"}]}}),
        "sanitizing must recurse through keys, arrays and nested objects"
    );
}

// ---------------------------------------------------------------------------
// set_payload_field
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_payload_field_strips_nul_from_value() {
    let db = sqlite_db().await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    let run_id = Uuid::new_v4();

    repo.set_payload_field(run_id, "extracted", json!({"text": "pdf\u{0}text"}))
        .await
        .expect("set_payload_field");

    let payload = repo.get_payload(run_id).await.expect("get_payload");
    let stored = payload.get("extracted").expect("key present");
    assert_no_nul(stored, "stored payload value");
    assert_eq!(stored, &json!({"text": "pdftext"}));
}

// ---------------------------------------------------------------------------
// The helper itself: literal `\u0000` text is not a NUL
// ---------------------------------------------------------------------------

/// `assert_no_nul` judges the *value*, not its rendering. A string that
/// legitimately contains the six characters `\u0000` — a backslash followed by
/// `u0000`, as an escaped error message or a quoted source snippet routinely
/// does — renders with the backslash doubled, so a substring check over the
/// rendering would flag it as a NUL escape. Nothing was ever stripped, and the
/// content must survive the sanitizer untouched.
#[tokio::test]
async fn literal_backslash_u0000_text_is_not_mistaken_for_a_nul() {
    // A backslash followed by `u0000`, not a NUL codepoint.
    let literal = "parse failed near: \\u0000";
    assert!(
        !literal.contains('\u{0}'),
        "the fixture must carry no real NUL, or it proves nothing"
    );

    let clean = json!({
        "error": literal,
        "nested": [{"\\u0000key": literal}],
    });
    // The regression: the old rendering-based check panicked right here.
    assert_no_nul(&clean, "literal-escape payload");

    // And it round-trips through the sanitizing write path unchanged.
    let db = sqlite_db().await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    let run_id = Uuid::new_v4();
    repo.set_payload_field(run_id, "extracted", clean.clone())
        .await
        .expect("set_payload_field");

    let payload = repo.get_payload(run_id).await.expect("get_payload");
    let stored = payload.get("extracted").expect("key present");
    assert_no_nul(stored, "stored payload value");
    assert_eq!(
        stored, &clean,
        "sanitizing must leave literal escape text alone"
    );
}

// ---------------------------------------------------------------------------
// reset_orphans
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reset_orphans_strips_nul_from_reason() {
    let db = sqlite_db().await;
    let dataset_id = Uuid::new_v4();
    create_dataset(&db, dataset_id).await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));

    // One stuck `Started` row for `reset_orphans` to find.
    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "orphan_pipeline",
        Some(dataset_id),
        PipelineRunStatus::Started,
        None,
    )
    .await
    .expect("seed orphan");

    let reset = repo
        .reset_orphans("server_restart\u{0}orphan")
        .await
        .expect("reset_orphans");
    assert_eq!(reset, 1, "the stuck Started row must be reset");

    // Select the Errored row explicitly rather than relying on `created_at`
    // ordering, which can tie with the seed row's timestamp.
    let runs = repo
        .get_pipeline_runs_by_dataset(dataset_id)
        .await
        .expect("get_pipeline_runs_by_dataset");
    let errored: Vec<_> = runs
        .iter()
        .filter(|r| matches!(r.status, PipelineRunStatus::Errored))
        .collect();
    assert_eq!(errored.len(), 1, "exactly one Errored row expected");

    let run_info = errored[0]
        .run_info
        .as_ref()
        .expect("reset_orphans always writes a reason");
    assert_no_nul(run_info, "stored reset_orphans reason");
    assert_eq!(run_info, &json!({"reason": "server_restartorphan"}));
}

// ---------------------------------------------------------------------------
// Postgres: the failure mode itself
// ---------------------------------------------------------------------------

/// Gated on the `postgres` feature so that, without a Postgres driver compiled
/// in, this neither compiles nor runs — a bare `connect("postgres://…")` would
/// panic instead of skipping. Provisions its OWN throwaway database, so it can
/// never touch a developer's data and needs no `#[serial]` guard.
#[cfg(feature = "postgres")]
mod postgres {
    use super::*;

    /// On Postgres an unsanitized `run_info` does not merely round-trip dirty —
    /// the INSERT is rejected outright (`unsupported Unicode escape sequence`),
    /// which is the loss this whole guard exists to prevent.
    ///
    /// Skipped when `TEST_POSTGRES_URL` is unset.
    #[tokio::test]
    async fn errored_run_info_with_nul_is_written_to_postgres() {
        let Some(base_url) = cognee_test_utils::test_postgres_url() else {
            eprintln!(
                "TEST_POSTGRES_URL not set — skipping errored_run_info_with_nul_is_written_to_postgres"
            );
            return;
        };
        let tmp = cognee_test_utils::create_temp_postgres_db(&base_url)
            .await
            .expect("create temp Postgres database");
        let db = Arc::new(connect(tmp.url()).await.expect("connect to temp Postgres"));
        initialize(db.as_ref()).await.expect("migrate");

        let dataset_id = Uuid::new_v4();
        create_dataset(db.as_ref(), dataset_id).await;
        let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));

        let data_id = Uuid::new_v4();
        let pipeline_run_id = Uuid::new_v4();
        repo.log_pipeline_run(
            pipeline_run_id,
            Uuid::new_v4(),
            "nul_pipeline",
            Some(dataset_id),
            PipelineRunStatus::Errored,
            Some(dirty_errored_run_info(data_id)),
        )
        .await
        .expect("Postgres must accept the Errored row; a NUL must not lose it");

        let row = repo
            .get_pipeline_run(pipeline_run_id)
            .await
            .expect("get_pipeline_run")
            .expect("row present");
        assert!(
            matches!(row.status, PipelineRunStatus::Errored),
            "the error must be recorded as Errored, not left as Started"
        );
        let run_info = row.run_info.expect("run_info populated");
        assert_no_nul(&run_info, "stored run_info");
        assert_eq!(run_info["error"], json!("parse failed near: source text"));

        // The payload-field path shares the same `json` column type.
        repo.set_payload_field(
            pipeline_run_id,
            "extracted",
            json!({"text": "pdf\u{0}text"}),
        )
        .await
        .expect("Postgres must accept the payload field");
        let payload = repo
            .get_payload(pipeline_run_id)
            .await
            .expect("get_payload");
        assert_eq!(payload.get("extracted"), Some(&json!({"text": "pdftext"})));
    }
}
