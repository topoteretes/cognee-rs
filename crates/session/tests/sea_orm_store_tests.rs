#![cfg(feature = "sea-orm-store")]

use std::sync::Arc;

use cognee_session::{SeaOrmSessionStore, SessionManager, SessionStore};
use sea_orm::Database;

async fn setup_store() -> Arc<SeaOrmSessionStore> {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    // SeaOrmSessionStore::new runs its own migration
    Arc::new(SeaOrmSessionStore::new(Arc::new(db)).await.unwrap())
}

#[tokio::test]
async fn create_and_retrieve_qa_entries() {
    let store = setup_store().await;

    store
        .create_qa_entry("s1", None, "What is Rust?", "A language.", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", None, "Tell me more.", "It is fast.", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", None, "And safe?", "Yes, memory safe.", None)
        .await
        .unwrap();

    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].question, "What is Rust?");
    assert_eq!(entries[1].question, "Tell me more.");
    assert_eq!(entries[2].question, "And safe?");
}

#[tokio::test]
async fn get_latest_entries_respects_limit() {
    let store = setup_store().await;

    for i in 0..5 {
        store
            .create_qa_entry("s1", None, &format!("q{i}"), &format!("a{i}"), None)
            .await
            .unwrap();
    }

    let entries = store.get_latest_qa_entries("s1", None, 3).await.unwrap();
    assert_eq!(entries.len(), 3);
    // Should be the last 3, oldest-first
    assert_eq!(entries[0].question, "q2");
    assert_eq!(entries[1].question, "q3");
    assert_eq!(entries[2].question, "q4");
}

#[tokio::test]
async fn entries_isolated_by_session_id() {
    let store = setup_store().await;

    store
        .create_qa_entry("session-a", None, "qa", "aa", None)
        .await
        .unwrap();
    store
        .create_qa_entry("session-b", None, "qb", "ab", None)
        .await
        .unwrap();

    let a_entries = store.get_all_qa_entries("session-a", None).await.unwrap();
    let b_entries = store.get_all_qa_entries("session-b", None).await.unwrap();

    assert_eq!(a_entries.len(), 1);
    assert_eq!(a_entries[0].question, "qa");
    assert_eq!(b_entries.len(), 1);
    assert_eq!(b_entries[0].question, "qb");
}

#[tokio::test]
async fn entries_isolated_by_user_id() {
    let store = setup_store().await;

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
    assert_eq!(u1[0].question, "q1");
    assert_eq!(u2.len(), 1);
    assert_eq!(u2[0].question, "q2");
}

#[tokio::test]
async fn delete_session() {
    let store = setup_store().await;

    store
        .create_qa_entry("s1", None, "q1", "a1", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", None, "q2", "a2", None)
        .await
        .unwrap();

    let deleted = store.delete_session("s1", None).await.unwrap();
    assert!(deleted);

    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert!(entries.is_empty());

    // Deleting again returns false
    let deleted_again = store.delete_session("s1", None).await.unwrap();
    assert!(!deleted_again);
}

#[tokio::test]
async fn delete_single_qa_entry() {
    let store = setup_store().await;

    let id1 = store
        .create_qa_entry("s1", None, "q1", "a1", None)
        .await
        .unwrap();
    let _id2 = store
        .create_qa_entry("s1", None, "q2", "a2", None)
        .await
        .unwrap();

    let deleted = store.delete_qa_entry("s1", None, &id1).await.unwrap();
    assert!(deleted);

    let entries = store.get_all_qa_entries("s1", None).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].question, "q2");
}

#[tokio::test]
async fn prune_removes_all_sessions() {
    let store = setup_store().await;

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
    let store = setup_store().await;

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
async fn session_manager_load_history_returns_user_assistant_pairs() {
    let store = setup_store().await;

    store
        .create_qa_entry("s1", None, "Hello?", "Hi there!", None)
        .await
        .unwrap();
    store
        .create_qa_entry("s1", None, "What is 2+2?", "4", None)
        .await
        .unwrap();

    let sm = SessionManager::new(store as Arc<dyn SessionStore>);
    let messages = sm.load_history_messages(Some("s1"), None).await.unwrap();

    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].content, "Hello?");
    assert_eq!(messages[1].content, "Hi there!");
    assert_eq!(messages[2].content, "What is 2+2?");
    assert_eq!(messages[3].content, "4");
}

#[tokio::test]
async fn session_manager_load_empty_history() {
    let store = setup_store().await;
    let sm = SessionManager::new(store as Arc<dyn SessionStore>);
    let messages = sm
        .load_history_messages(Some("nonexistent"), None)
        .await
        .unwrap();
    assert!(messages.is_empty());
}

#[tokio::test]
async fn session_manager_save_and_load_round_trip() {
    let store = setup_store().await;
    let sm = SessionManager::new(store as Arc<dyn SessionStore>);

    sm.save_qa(Some("s1"), None, "q1", "a1", Some("ctx1"))
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
