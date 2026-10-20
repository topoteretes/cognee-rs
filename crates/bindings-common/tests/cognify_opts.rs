#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Cognify `opts` parsing shared by every binding: `graphSchema` /
//! `chunkAttachment` reach the [`CognifyConfig`], every known key is
//! type-checked, an invalid combination fails validation on the binding
//! boundary, and an unknown key is reported rather than silently vanishing.
//!
//! [`CognifyConfig`]: cognee::cognify::CognifyConfig

use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::TempDir;

use cognee::cognify::{ChunkAttachment, CognifyConfig};
use cognee::config::Settings;
use cognee_bindings_common::ops::pipeline::{
    add_and_cognify, apply_cognify_opts, check_cognify_opts, cognify, cognify_config_with_opts,
    existing_data_ids, unknown_cognify_opts,
};
use cognee_bindings_common::{CogneeServices, HandleState, SdkError};

fn schema() -> Value {
    json!({
        "title": "Graph",
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "metadata": {
                "type": "object",
                "default": { "identity_fields": ["name"], "index_fields": ["name"] }
            }
        },
        "required": ["name"]
    })
}

fn apply(opts: Value) -> Result<CognifyConfig, SdkError> {
    apply_cognify_opts(CognifyConfig::default(), &opts)
}

fn validation_message(result: Result<CognifyConfig, SdkError>) -> String {
    match result {
        Err(SdkError::Validation(message)) => message,
        Err(other) => panic!("expected a validation error, got {other}"),
        Ok(cfg) => panic!(
            "expected a validation error, got a config (graph_schema={:?})",
            cfg.graph_schema
        ),
    }
}

/// Hermetic settings: every store under a temp dir, a dummy key so the strict
/// LLM resolve succeeds without network I/O, and the mock embedding provider.
fn settings_under(dir: &TempDir) -> Settings {
    let root = dir.path();
    Settings {
        sections: [
            (
                "relational.url".to_string(),
                json!(format!(
                    "sqlite://{}?mode=rwc",
                    root.join("cognee.db").display()
                )),
            ),
            ("embedding.provider".to_string(), json!("mock")),
            ("llm.api_key".to_string(), json!("sk-test")),
        ]
        .into(),
        data_root_directory: root.join("data").to_string_lossy().into_owned(),
        system_root_directory: root.join("sys").to_string_lossy().into_owned(),
        ..Settings::default()
    }
}

async fn services_under(state: &HandleState) -> Arc<CogneeServices> {
    state.services().await.expect("warm")
}

// ---------------------------------------------------------------------------
// Parsing.
// ---------------------------------------------------------------------------

#[test]
fn graph_schema_and_chunk_attachment_reach_the_config() {
    let cfg = apply(json!({ "graphSchema": schema(), "chunkAttachment": "all" })).expect("config");
    assert_eq!(cfg.graph_schema, Some(schema()));
    assert_eq!(cfg.chunk_attachment, Some(ChunkAttachment::All));
    cfg.validate()
        .expect("a valid schema with attachment validates");

    // A JSON string carries the same schema (C strings, CLI files).
    let cfg = apply(json!({ "graphSchema": schema().to_string(), "chunkAttachment": "direct" }))
        .expect("config");
    assert_eq!(cfg.graph_schema, Some(schema()));
    assert_eq!(cfg.chunk_attachment, Some(ChunkAttachment::Direct));

    // `graphModel` is Python's name for the same thing.
    let cfg = apply(json!({ "graphModel": schema() })).expect("config");
    assert_eq!(cfg.graph_schema, Some(schema()));
}

#[test]
fn null_and_empty_schema_mean_the_default_model() {
    for opts in [
        json!({ "graphSchema": null, "chunkAttachment": null }),
        json!({ "graphSchema": {} }),
        json!({ "graphModel": "{}" }),
        Value::Null,
        json!({}),
    ] {
        let cfg = apply(opts.clone()).unwrap_or_else(|e| panic!("{opts}: {e}"));
        assert_eq!(cfg.graph_schema, None, "{opts}");
        assert_eq!(cfg.chunk_attachment, None, "{opts}");
    }
}

#[test]
fn invalid_values_are_validation_errors() {
    let cases = [
        (
            json!({ "graphSchema": schema(), "chunkAttachment": "everything" }),
            "expected 'direct', 'all', or None",
        ),
        (
            json!({ "graphSchema": schema(), "chunkAttachment": "Direct" }),
            "expected 'direct', 'all', or None",
        ),
        (
            json!({ "graphSchema": schema(), "chunkAttachment": true }),
            "`chunkAttachment` must be",
        ),
        (json!({ "graphSchema": [1, 2] }), "graph_schema must be"),
        (json!({ "graphSchema": "{not json" }), "not valid JSON"),
        (
            json!({ "graphSchema": schema(), "graphModel": schema() }),
            "not both",
        ),
        (json!({ "chunkSize": "512" }), "`chunkSize` must be"),
        (json!({ "chunkSize": -1 }), "`chunkSize` must be"),
        (json!({ "chunkOverlap": 1.5 }), "`chunkOverlap` must be"),
        (json!({ "summarization": "yes" }), "`summarization` must be"),
        (json!({ "temporalCognify": 1 }), "`temporalCognify` must be"),
        (json!({ "triplet": "true" }), "`triplet` must be"),
        (json!([1]), "must be an object"),
    ];
    for (opts, expected) in cases {
        let message = validation_message(apply(opts.clone()));
        assert!(message.contains(expected), "{opts}: {message}");
    }
}

#[test]
fn unknown_keys_are_reported_and_known_ones_are_not() {
    let opts = json!({
        "tenant": null,
        "chunkSize": 10,
        "chunkOverlap": 1,
        "summarization": true,
        "temporalCognify": false,
        "triplet": false,
        "graphBackend": null,
        "graphSchema": null,
        "graphModel": null,
        "chunkAttachment": null,
        "chunkAtachment": "all",
        "runInBackground": true,
    });
    assert_eq!(
        unknown_cognify_opts(&opts),
        vec!["chunkAtachment".to_string(), "runInBackground".to_string()]
    );
    // Reported, not fatal: Python forwards unknown kwargs instead of raising.
    let cfg = apply(opts).expect("unknown keys warn, they do not fail");
    assert_eq!(cfg.chunk_attachment, None);
    assert!(unknown_cognify_opts(&Value::Null).is_empty());
}

/// Captures everything a `fmt` subscriber writes, so a test can read the
/// warnings an op logs.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn text(&self) -> String {
        // lock poison is unrecoverable
        String::from_utf8(self.0.lock().unwrap().clone()).expect("utf-8 log output")
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // lock poison is unrecoverable
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

const UNKNOWN_WARNING: &str = "ignoring unknown cognify option(s)";

/// The unknown-key warning is what keeps a misspelled option from vanishing,
/// so it must fire and name the key — and only once per op: `cognify` and
/// `add_and_cognify` run `check_cognify_opts` and later
/// `cognify_config_with_opts` on the same opts, and only the first warns.
#[tokio::test]
async fn an_unknown_key_is_warned_about_once_by_name() {
    let dir = TempDir::new().expect("tempdir");
    let state = HandleState::from_settings(settings_under(&dir));
    let svc = services_under(&state).await;

    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let opts = json!({ "chunkAtachment": "all", "chunkSize": 512 });
    check_cognify_opts(&svc, &opts).expect("an unknown key is not an error");
    let text = logs.text();
    assert_eq!(text.matches(UNKNOWN_WARNING).count(), 1, "{text}");
    // Exactly the misspelled key: the known `chunkSize` is not reported.
    assert!(text.contains(r#"unknown=["chunkAtachment"]"#), "{text}");

    // The second application on the same opts stays silent.
    cognify_config_with_opts(&svc, &opts)
        .await
        .expect("an unknown key is not an error");
    apply(opts).expect("an unknown key is not an error");
    let text = logs.text();
    assert_eq!(text.matches(UNKNOWN_WARNING).count(), 1, "{text}");

    // Known keys alone warn about nothing.
    check_cognify_opts(&svc, &json!({ "chunkSize": 512 })).expect("valid");
    assert_eq!(logs.text().matches(UNKNOWN_WARNING).count(), 1);
}

// ---------------------------------------------------------------------------
// Validation through the services-backed entry points.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cognify_config_with_opts_runs_config_validation() {
    let dir = TempDir::new().expect("tempdir");
    let state = HandleState::from_settings(settings_under(&dir));
    let svc = services_under(&state).await;

    let cfg = cognify_config_with_opts(
        &svc,
        &json!({ "graphSchema": schema(), "chunkAttachment": "all" }),
    )
    .await
    .expect("valid options build a config");
    assert_eq!(cfg.graph_schema, Some(schema()));
    assert_eq!(cfg.chunk_attachment, Some(ChunkAttachment::All));

    for (opts, expected) in [
        (
            json!({ "chunkAttachment": "all" }),
            "chunk_attachment requires graph_schema",
        ),
        (
            json!({ "graphSchema": schema(), "temporalCognify": true }),
            "not supported with temporal_cognify",
        ),
        (
            json!({ "graphSchema": { "title": "graph", "properties": {} } }),
            "Invalid graph schema",
        ),
        (
            json!({ "chunkSize": 0 }),
            "max_chunk_size must be greater than 0",
        ),
    ] {
        let message = validation_message(cognify_config_with_opts(&svc, &opts).await);
        assert!(message.contains(expected), "{opts}: {message}");
        let message = match check_cognify_opts(&svc, &opts) {
            Err(SdkError::Validation(message)) => message,
            other => panic!("{opts}: expected a validation error, got {other:?}"),
        };
        assert!(message.contains(expected), "{opts}: {message}");
    }
}

/// `cognify` checks its options before resolving the dataset, so an invalid
/// option is the error even on a dataset that does not exist.
#[tokio::test]
async fn cognify_op_rejects_invalid_options_up_front() {
    let dir = TempDir::new().expect("tempdir");
    let state = HandleState::from_settings(settings_under(&dir));
    let error = cognify(
        &state,
        "no_such_dataset",
        &json!({ "graphSchema": schema(), "chunkAttachment": "everything" }),
    )
    .await
    .expect_err("an invalid chunkAttachment must fail");
    assert!(
        matches!(&error, SdkError::Validation(message) if message.contains("chunk_attachment")),
        "{error}"
    );
}

/// `add_and_cognify` checks its cognify options before the add, so a bad one
/// does not get the documents ingested by a call that was always going to fail.
#[tokio::test]
async fn add_and_cognify_rejects_invalid_options_before_adding() {
    let dir = TempDir::new().expect("tempdir");
    let state = HandleState::from_settings(settings_under(&dir));
    let error = add_and_cognify(
        &state,
        json!({ "type": "text", "text": "Alice works at Acme." }),
        "opts_ds",
        &json!({ "chunkAttachment": "direct" }),
    )
    .await
    .expect_err("chunkAttachment without graphSchema must fail");
    assert!(
        matches!(&error, SdkError::Validation(message)
            if message.contains("chunk_attachment requires graph_schema")),
        "{error}"
    );

    let svc = services_under(&state).await;
    let owner_id = state.owner_id().await.expect("owner");
    assert!(
        existing_data_ids(&svc, "opts_ds", owner_id, None)
            .await
            .expect("lookup")
            .is_empty(),
        "nothing may be added when the cognify options are invalid"
    );
}
