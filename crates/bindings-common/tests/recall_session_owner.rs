#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! A binding's recall reads a session the caller may read through a dataset
//! grant under the session's owner (SDK-1040 P3).
//!
//! Session-cache entries are keyed by the session's owner; Python's local
//! `recall()` resolves that owner from the caller's dataset read grants
//! (`_resolve_session_cache_user_id`, `recall.py:181-250`). The grants come
//! from the handle's `ReadableDatasets` hook; without one only the caller's
//! own cache is read.

use std::sync::{Arc, Mutex};

use cognee::config::Settings;
use cognee::database::RelationalDb;
use cognee::models::Dataset;
use cognee_bindings_common::ops::retrieval;
use cognee_bindings_common::{HandleState, ReadableDatasets, SdkError};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

const SESSION: &str = "alice-agent-session";

/// Hermetic settings: every store under a temp dir, a dummy key and the mock
/// embedding provider, so warming does no network I/O.
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

/// Grants whatever the test sets, recording who asked.
#[derive(Default)]
struct Grants {
    readable: Mutex<Vec<Uuid>>,
    asked: Mutex<Vec<Uuid>>,
}

#[async_trait::async_trait]
impl ReadableDatasets for Grants {
    async fn readable_dataset_ids(
        &self,
        _db: &Arc<dyn RelationalDb>,
        user_id: Uuid,
        _tenant_id: Option<Uuid>,
    ) -> Result<Vec<Uuid>, SdkError> {
        self.asked.lock().unwrap().push(user_id);
        Ok(self.readable.lock().unwrap().clone())
    }
}

/// Gives Alice, another user, a session on a dataset of hers holding one QA
/// entry, in the handle's database and session cache. Returns the dataset.
async fn seed_alice_session(state: &HandleState) -> Uuid {
    let svc = state.services().await.unwrap();
    let alice = Uuid::new_v4();
    let dataset = svc
        .database
        .create_dataset(Dataset::new(
            "agent_memory".to_string(),
            alice,
            None,
            Uuid::new_v4(),
        ))
        .await
        .unwrap()
        .id;
    svc.database
        .ensure_and_touch_session(SESSION, alice, Some(dataset))
        .await
        .unwrap();
    svc.session_store
        .create_qa_entry(
            SESSION,
            Some(&alice.to_string()),
            "how should I answer rust questions",
            "cite the book",
            None,
        )
        .await
        .unwrap();
    dataset
}

async fn recall_session(state: &HandleState) -> Vec<Value> {
    let result = retrieval::recall(
        state,
        "how should I answer",
        &json!({ "sessionId": SESSION, "scope": ["session"] }),
    )
    .await
    .unwrap();
    result["items"].as_array().expect("items").clone()
}

#[tokio::test]
async fn a_granted_dataset_reads_the_session_owners_cache() {
    let dir = tempfile::tempdir().unwrap();
    let grants = Arc::new(Grants::default());
    let state = HandleState::from_settings(settings_under(&dir))
        .with_readable_datasets(Arc::clone(&grants) as Arc<dyn ReadableDatasets>);
    let dataset = seed_alice_session(&state).await;

    // No grant: the caller's own (empty) cache.
    assert_eq!(recall_session(&state).await, Vec::<Value>::new());
    let caller = state.owner_id().await.unwrap();
    assert_eq!(
        *grants.asked.lock().unwrap(),
        [caller],
        "asked as the caller"
    );

    // A read grant on the session's dataset: Alice's entries.
    grants.readable.lock().unwrap().push(dataset);
    let items = recall_session(&state).await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["content"]["answer"], "cite the book", "{items:?}");
}

#[tokio::test]
async fn graph_only_recall_does_not_look_up_grants() {
    let dir = tempfile::tempdir().unwrap();
    let grants = Arc::new(Grants::default());
    let state = HandleState::from_settings(settings_under(&dir))
        .with_readable_datasets(Arc::clone(&grants) as Arc<dyn ReadableDatasets>);
    state.services().await.unwrap();
    let _ = retrieval::recall(
        &state,
        "anything",
        &json!({ "sessionId": SESSION, "scope": ["graph"] }),
    )
    .await;
    assert!(grants.asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn without_a_hook_the_caller_reads_its_own_cache() {
    let dir = tempfile::tempdir().unwrap();
    let state = HandleState::from_settings(settings_under(&dir));
    seed_alice_session(&state).await;
    assert_eq!(recall_session(&state).await, Vec::<Value>::new());
}
