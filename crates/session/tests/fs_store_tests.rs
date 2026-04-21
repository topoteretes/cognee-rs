#![cfg(feature = "fs")]

use std::sync::Arc;

use cognee_session::{FsSessionStore, SessionManager, SessionStore};

fn setup_store(dir: &std::path::Path) -> Arc<FsSessionStore> {
    Arc::new(FsSessionStore::new(dir))
}

#[tokio::test]
async fn create_and_retrieve_qa_entries() {
    let dir = tempfile::tempdir().unwrap();
    let store = setup_store(dir.path());

    store
        .create_qa_entry("s1", None, "What is Rust?", "A language.", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", None, "Tell me more.", "It is fast.", None)
        .await
        .unwrap();

    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].question, "What is Rust?");
    assert_eq!(entries[1].question, "Tell me more.");
}

#[tokio::test]
async fn get_latest_entries_respects_limit() {
    let dir = tempfile::tempdir().unwrap();
    let store = setup_store(dir.path());

    for i in 0..5 {
        store
            .create_qa_entry("s1", None, &format!("q{i}"), &format!("a{i}"), None)
            .await
            .unwrap();
    }

    let entries = store.get_latest_qa_entries("s1", None, 3).await.unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].question, "q2");
    assert_eq!(entries[1].question, "q3");
    assert_eq!(entries[2].question, "q4");
}

#[tokio::test]
async fn entries_isolated_by_session_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = setup_store(dir.path());

    store
        .create_qa_entry("session-a", None, "qa", "aa", None)
        .await
        .unwrap();
    store
        .create_qa_entry("session-b", None, "qb", "ab", None)
        .await
        .unwrap();

    let a = store.get_all_qa_entries("session-a", None).await.unwrap();
    let b = store.get_all_qa_entries("session-b", None).await.unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert_eq!(a[0].question, "qa");
    assert_eq!(b[0].question, "qb");
}

#[tokio::test]
async fn entries_isolated_by_user_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = setup_store(dir.path());

    store
        .create_qa_entry("s1", Some("user-1"), "q1", "a1", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", Some("user-2"), "q2", "a2", None)
        .await
        .unwrap();

    let u1 = store
        .get_all_qa_entries("s1", Some("user-1"))
        .await
        .unwrap();
    let u2 = store
        .get_all_qa_entries("s1", Some("user-2"))
        .await
        .unwrap();
    assert_eq!(u1.len(), 1);
    assert_eq!(u2.len(), 1);
    assert_eq!(u1[0].question, "q1");
    assert_eq!(u2[0].question, "q2");
}

#[tokio::test]
async fn delete_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = setup_store(dir.path());

    store
        .create_qa_entry("s1", None, "q1", "a1", None)
        .await
        .unwrap();

    assert!(store.delete_session("s1", None).await.unwrap());
    assert!(
        store
            .get_all_qa_entries("s1", None)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!store.delete_session("s1", None).await.unwrap());
}

#[tokio::test]
async fn delete_single_qa_entry() {
    let dir = tempfile::tempdir().unwrap();
    let store = setup_store(dir.path());

    let id1 = store
        .create_qa_entry("s1", None, "q1", "a1", None)
        .await
        .unwrap();
    let _id2 = store
        .create_qa_entry("s1", None, "q2", "a2", None)
        .await
        .unwrap();

    assert!(store.delete_qa_entry("s1", None, &id1).await.unwrap());

    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].question, "q2");
}

#[tokio::test]
async fn prune_removes_all_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = setup_store(dir.path());

    // Create entries across multiple sessions and users
    store
        .create_qa_entry("s1", Some("user-1"), "q1", "a1", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", Some("user-2"), "q2", "a2", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s2", None, "q3", "a3", None)
        .await
        .unwrap();

    // Prune all session data
    store.prune().await.unwrap();

    // Verify all entries are gone
    assert!(
        store
            .get_all_qa_entries("s1", Some("user-1"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .get_all_qa_entries("s1", Some("user-2"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .get_all_qa_entries("s2", None)
            .await
            .unwrap()
            .is_empty()
    );

    // Verify the store is still functional after prune
    store
        .create_qa_entry("s3", None, "q4", "a4", None)
        .await
        .unwrap();
    let entries = store.get_all_qa_entries("s3", None).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].question, "q4");
}

#[tokio::test]
async fn prune_on_empty_store_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let store = setup_store(dir.path());

    // Prune when nothing exists should succeed
    store.prune().await.unwrap();

    // Store remains functional
    store
        .create_qa_entry("s1", None, "q1", "a1", None)
        .await
        .unwrap();
    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert_eq!(entries.len(), 1);
}

#[tokio::test]
async fn session_manager_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = setup_store(dir.path());
    let sm = SessionManager::new(store as Arc<dyn SessionStore>);

    sm.save_qa(Some("s1"), None, "q1", "a1", None)
        .await
        .unwrap();
    sm.save_qa(Some("s1"), None, "q2", "a2", None)
        .await
        .unwrap();

    let messages = sm.load_history_messages(Some("s1"), None).await.unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].content, "q1");
    assert_eq!(messages[1].content, "a1");
    assert_eq!(messages[2].content, "q2");
    assert_eq!(messages[3].content, "a2");
}
