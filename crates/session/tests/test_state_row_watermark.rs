//! The one state-row watermark implementation shared by the improve stages.
//!
//! Port of Python's
//! `cognee/tests/unit/infrastructure/session/test_state_row_watermark.py`:
//! the generic `StateRowWatermark` (lookup, count/value storage, one row per
//! session, the stale-watermark policy) and the concrete watermarks built on
//! it (Q&A persistence, trace persistence, trace extraction, per-dataset
//! distillation).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_session::{
    SESSION_DISTILL_STATE_KIND, SESSION_PERSIST_STATE_ID, SESSION_PERSIST_STATE_KIND,
    SESSION_PERSIST_WATERMARK, SessionContextRow, SessionError, SessionManager, SessionQAEntry,
    SessionQAUpdate, SessionStore, StateRowWatermark, TRACE_EXTRACTION_STATE_ID,
    TRACE_EXTRACTION_STATE_KIND, TRACE_EXTRACTION_WATERMARK, TRACE_PERSIST_STATE_ID,
    TRACE_PERSIST_STATE_KIND, TRACE_PERSIST_WATERMARK, coerce_active_context_entries,
    distill_watermark, get_distilled_entry_ids, save_distilled_entry_ids,
};
use serde_json::{Value, json};

const USER: Option<&str> = Some("u");
const SESSION: Option<&str> = Some("s");

/// In-memory context-row store with the CRUD surface the watermarks use
/// (Python's `FakeSessionManager`): create appends, update merges in place.
#[derive(Default)]
struct FakeStore {
    rows: Mutex<Vec<SessionContextRow>>,
}

impl FakeStore {
    fn rows(&self) -> Vec<SessionContextRow> {
        self.rows.lock().unwrap().clone() // lock poison is unrecoverable
    }
}

#[async_trait]
impl SessionStore for FakeStore {
    async fn create_qa_entry(
        &self,
        _: &str,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> Result<String, SessionError> {
        unreachable!("not used by the watermarks")
    }
    async fn get_latest_qa_entries(
        &self,
        _: &str,
        _: Option<&str>,
        _: usize,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        Ok(vec![])
    }
    async fn get_all_qa_entries(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<Vec<SessionQAEntry>, SessionError> {
        Ok(vec![])
    }
    async fn delete_session(&self, _: &str, _: Option<&str>) -> Result<bool, SessionError> {
        Ok(false)
    }
    async fn delete_qa_entry(
        &self,
        _: &str,
        _: Option<&str>,
        _: &str,
    ) -> Result<bool, SessionError> {
        Ok(false)
    }
    async fn prune(&self) -> Result<(), SessionError> {
        Ok(())
    }
    async fn update_qa_entry(
        &self,
        _: &str,
        _: Option<&str>,
        _: &str,
        _: SessionQAUpdate,
    ) -> Result<bool, SessionError> {
        Ok(false)
    }

    async fn create_session_context_entry(
        &self,
        _: &str,
        _: Option<&str>,
        entry: SessionContextRow,
    ) -> Result<(), SessionError> {
        self.rows.lock().unwrap().push(entry); // lock poison is unrecoverable
        Ok(())
    }
    async fn get_session_context_entries(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<Vec<SessionContextRow>, SessionError> {
        Ok(self.rows())
    }
    async fn update_session_context_entry(
        &self,
        _: &str,
        _: Option<&str>,
        entry_id: &str,
        merge: SessionContextRow,
    ) -> Result<bool, SessionError> {
        let mut rows = self.rows.lock().unwrap(); // lock poison is unrecoverable
        match rows
            .iter_mut()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(entry_id))
        {
            Some(row) => {
                row.extend(merge);
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

fn setup() -> (Arc<FakeStore>, SessionManager) {
    let store = Arc::new(FakeStore::default());
    let manager = SessionManager::new(store.clone());
    (store, manager)
}

fn row(value: Value) -> SessionContextRow {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

const WATERMARK: StateRowWatermark = StateRowWatermark::new("wm", "wm_state", "done_count");

// ------------------------------------------------------------------ generic helper

#[test]
fn find_row_prefers_id_then_falls_back_to_kind() {
    let by_id = row(json!({"id": "wm", "kind": "other", "done_count": 1}));
    let by_kind = row(json!({"id": "something-else", "kind": "wm_state", "done_count": 2}));

    assert_eq!(
        WATERMARK.find_row(&[by_kind.clone(), by_id.clone()]),
        Some(&by_id)
    );
    assert_eq!(
        WATERMARK.find_row(std::slice::from_ref(&by_kind)),
        Some(&by_kind)
    );
    assert_eq!(WATERMARK.find_row(&[row(json!({"id": "x"}))]), None);
    assert_eq!(WATERMARK.find_row(&[]), None);
}

#[test]
fn scoped_watermark_never_matches_a_sibling_row_by_kind() {
    let scoped = StateRowWatermark::scoped("wm:ds-a".into(), "wm_state", "done_count");
    let sibling = row(json!({"id": "wm:ds-b", "kind": "wm_state", "done_count": 7}));

    assert_eq!(scoped.find_row(std::slice::from_ref(&sibling)), None);
    assert_eq!(scoped.count_from_rows(&[sibling]), 0);
}

#[test]
fn count_from_rows_is_lenient() {
    let cases: [(Option<Value>, u64); 7] = [
        (None, 0),
        (Some(json!({"id": "wm"})), 0),
        (Some(json!({"id": "wm", "done_count": null})), 0),
        (Some(json!({"id": "wm", "done_count": "not-a-number"})), 0),
        (Some(json!({"id": "wm", "done_count": -3})), 0),
        (Some(json!({"id": "wm", "done_count": "4"})), 4),
        (Some(json!({"id": "wm", "done_count": 9})), 9),
    ];
    for (value, expected) in cases {
        let rows: Vec<SessionContextRow> = value.clone().map(row).into_iter().collect();
        assert_eq!(WATERMARK.count_from_rows(&rows), expected, "{value:?}");
    }
}

#[tokio::test]
async fn count_roundtrip_keeps_one_row_per_session() {
    let (store, manager) = setup();

    assert_eq!(
        WATERMARK.read_count(&manager, SESSION, USER).await.unwrap(),
        0
    );
    WATERMARK
        .write_count(&manager, SESSION, USER, 4)
        .await
        .unwrap();
    assert_eq!(
        WATERMARK.read_count(&manager, SESSION, USER).await.unwrap(),
        4
    );
    WATERMARK
        .write_count(&manager, SESSION, USER, 7)
        .await
        .unwrap();
    assert_eq!(
        WATERMARK.read_count(&manager, SESSION, USER).await.unwrap(),
        7
    );
    WATERMARK
        .write_count(&manager, SESSION, USER, -1)
        .await
        .unwrap();
    assert_eq!(
        WATERMARK.read_count(&manager, SESSION, USER).await.unwrap(),
        0
    );

    let rows = store.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], json!("wm"));
    assert_eq!(rows[0]["kind"], json!("wm_state"));
    assert!(rows[0].contains_key("updated_at"));
}

#[tokio::test]
async fn value_roundtrip_stores_opaque_values() {
    let (store, manager) = setup();

    assert_eq!(
        WATERMARK.read_value(&manager, SESSION, USER).await.unwrap(),
        None
    );
    WATERMARK
        .write_value(&manager, SESSION, USER, json!(["a", "b"]))
        .await
        .unwrap();
    assert_eq!(
        WATERMARK.read_value(&manager, SESSION, USER).await.unwrap(),
        Some(json!(["a", "b"]))
    );
    assert_eq!(store.rows().len(), 1);
}

#[tokio::test]
async fn state_rows_never_reach_rendered_context() {
    let (store, manager) = setup();
    WATERMARK
        .write_count(&manager, SESSION, USER, 3)
        .await
        .unwrap();
    SESSION_PERSIST_WATERMARK
        .write_count(&manager, SESSION, USER, 3)
        .await
        .unwrap();
    TRACE_PERSIST_WATERMARK
        .write_count(&manager, SESSION, USER, 3)
        .await
        .unwrap();
    TRACE_EXTRACTION_WATERMARK
        .write_count(&manager, SESSION, USER, 3)
        .await
        .unwrap();
    distill_watermark("ds")
        .write_value(&manager, SESSION, USER, json!(["e1"]))
        .await
        .unwrap();

    assert!(
        coerce_active_context_entries(&store.rows())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn resolve_effective_applies_the_stale_policy() {
    assert_eq!(StateRowWatermark::resolve_effective(3, 5, ""), 3);
    assert_eq!(StateRowWatermark::resolve_effective(5, 5, ""), 5);
    // Above the current total: the session was cleared and rebuilt, start over.
    assert_eq!(StateRowWatermark::resolve_effective(10, 2, "s"), 0);
}

// ------------------------------------------------------------------ concrete watermarks

#[tokio::test]
async fn qa_persist_watermark_uses_the_legacy_row_shape() {
    let (store, manager) = setup();
    SESSION_PERSIST_WATERMARK
        .write_count(&manager, SESSION, USER, 5)
        .await
        .unwrap();

    let rows = store.rows();
    assert_eq!(SESSION_PERSIST_STATE_ID, "session_persist_watermark");
    assert_eq!(rows[0]["id"], json!(SESSION_PERSIST_STATE_ID));
    assert_eq!(rows[0]["kind"], json!(SESSION_PERSIST_STATE_KIND));
    assert_eq!(rows[0]["persisted_qa_count"], json!(5));
    assert_eq!(
        SESSION_PERSIST_WATERMARK
            .read_count(&manager, SESSION, USER)
            .await
            .unwrap(),
        5
    );
}

#[tokio::test]
async fn qa_persist_reads_rows_written_before_the_shared_helper() {
    let (_store, manager) = setup();
    manager
        .create_session_context_entry(
            SESSION,
            USER,
            row(json!({
                "id": "session_persist_watermark",
                "kind": "session_persist_watermark_state",
                "persisted_qa_count": 6,
            })),
        )
        .await
        .unwrap();
    assert_eq!(
        SESSION_PERSIST_WATERMARK
            .read_count(&manager, SESSION, USER)
            .await
            .unwrap(),
        6
    );
}

#[tokio::test]
async fn trace_persist_watermark_has_its_own_row() {
    let (store, manager) = setup();
    SESSION_PERSIST_WATERMARK
        .write_count(&manager, SESSION, USER, 2)
        .await
        .unwrap();
    TRACE_PERSIST_WATERMARK
        .write_count(&manager, SESSION, USER, 9)
        .await
        .unwrap();

    assert_eq!(
        SESSION_PERSIST_WATERMARK
            .read_count(&manager, SESSION, USER)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        TRACE_PERSIST_WATERMARK
            .read_count(&manager, SESSION, USER)
            .await
            .unwrap(),
        9
    );
    let ids: BTreeSet<String> = store
        .rows()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        BTreeSet::from([
            SESSION_PERSIST_STATE_ID.to_string(),
            TRACE_PERSIST_STATE_ID.to_string()
        ])
    );
    assert_ne!(TRACE_PERSIST_STATE_KIND, SESSION_PERSIST_STATE_KIND);
}

#[test]
fn trace_extraction_watermark_reads_the_legacy_stage4_row() {
    let legacy_row = row(json!({
        "id": TRACE_EXTRACTION_STATE_ID,
        "kind": TRACE_EXTRACTION_STATE_KIND,
        "processed_trace_count": 12,
    }));
    assert_eq!(
        TRACE_EXTRACTION_WATERMARK.count_from_rows(&[legacy_row]),
        12
    );
    assert_eq!(TRACE_EXTRACTION_WATERMARK.field, "processed_trace_count");
}

fn ids(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn distill_watermark_is_scoped_per_dataset() {
    let (store, manager) = setup();
    save_distilled_entry_ids(&manager, SESSION, USER, "ds-a", owned(&["e2", "e1", "e1"]))
        .await
        .unwrap();

    assert_eq!(
        get_distilled_entry_ids(&manager, SESSION, USER, "ds-a")
            .await
            .unwrap(),
        ids(&["e1", "e2"])
    );
    assert!(
        get_distilled_entry_ids(&manager, SESSION, USER, "ds-b")
            .await
            .unwrap()
            .is_empty()
    );

    save_distilled_entry_ids(&manager, SESSION, USER, "ds-b", owned(&["e9"]))
        .await
        .unwrap();
    assert_eq!(
        get_distilled_entry_ids(&manager, SESSION, USER, "ds-a")
            .await
            .unwrap(),
        ids(&["e1", "e2"])
    );
    assert_eq!(
        get_distilled_entry_ids(&manager, SESSION, USER, "ds-b")
            .await
            .unwrap(),
        ids(&["e9"])
    );
    let rows = store.rows();
    assert!(
        rows.iter()
            .all(|r| r["kind"] == json!(SESSION_DISTILL_STATE_KIND))
    );
    assert_eq!(rows.len(), 2);
}

#[tokio::test]
async fn distill_watermark_replaces_rather_than_appends() {
    let (store, manager) = setup();
    save_distilled_entry_ids(&manager, SESSION, USER, "ds", owned(&["e1", "e2"]))
        .await
        .unwrap();
    save_distilled_entry_ids(&manager, SESSION, USER, "ds", owned(&["e2", "e3"]))
        .await
        .unwrap();

    // Ids of entries that have since disappeared drop out, so the row stays bounded.
    assert_eq!(
        get_distilled_entry_ids(&manager, SESSION, USER, "ds")
            .await
            .unwrap(),
        ids(&["e2", "e3"])
    );
    assert_eq!(store.rows().len(), 1);
}

#[tokio::test]
async fn distill_watermark_tolerates_malformed_values() {
    let (_store, manager) = setup();
    distill_watermark("ds")
        .write_value(&manager, SESSION, USER, json!("not-a-list"))
        .await
        .unwrap();
    assert!(
        get_distilled_entry_ids(&manager, SESSION, USER, "ds")
            .await
            .unwrap()
            .is_empty()
    );
}
