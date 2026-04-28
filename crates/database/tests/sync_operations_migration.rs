//! Verify the `sync_operations` migration creates the table and supports the
//! repository surface against a fresh in-memory SQLite DB.

use std::sync::Arc;

use cognee_database::{
    DatabaseConnection, SeaOrmSyncOperationRepository, SyncOperationRepository, connect, initialize,
};
use uuid::Uuid;

async fn make_db() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    Arc::new(db)
}

#[tokio::test]
async fn migration_creates_table_idempotently() {
    let db = make_db().await;
    // Re-running `initialize` against the same connection must be a no-op.
    initialize(&db).await.expect("re-init idempotent");

    let repo = SeaOrmSyncOperationRepository::new(Arc::clone(&db));

    let user_id = Uuid::new_v4();
    let run_id = Uuid::new_v4().to_string();
    let dataset_ids = vec![Uuid::new_v4(), Uuid::new_v4()];
    let dataset_names = vec!["a".to_string(), "b".to_string()];

    repo.create_operation(&run_id, &dataset_ids, &dataset_names, user_id)
        .await
        .expect("create_operation");

    let row = repo
        .get_by_run_id(&run_id)
        .await
        .expect("lookup")
        .expect("row present");
    assert_eq!(row.run_id, run_id);
    assert_eq!(row.user_id, user_id);
    assert_eq!(row.status, "started");
    assert_eq!(row.progress_percentage, 0);
    assert_eq!(row.dataset_ids, dataset_ids);
    assert_eq!(row.dataset_names, dataset_names);

    repo.mark_started(&run_id).await.expect("mark_started");
    repo.update_progress(&run_id, 42)
        .await
        .expect("update_progress");
    let row = repo
        .get_by_run_id(&run_id)
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(row.status, "in_progress");
    assert_eq!(row.progress_percentage, 42);
    assert!(row.started_at.is_some());

    let running = repo.running_for_user(user_id).await.expect("running");
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].run_id, run_id);

    repo.mark_completed(&run_id, 5, 10, 1000, 2000, None)
        .await
        .expect("mark_completed");
    let row = repo
        .get_by_run_id(&run_id)
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(row.status, "completed");
    assert_eq!(row.progress_percentage, 100);
    assert_eq!(row.records_uploaded, 5);
    assert_eq!(row.records_downloaded, 10);
    assert_eq!(row.bytes_uploaded, 1000);
    assert_eq!(row.bytes_downloaded, 2000);
    assert!(row.completed_at.is_some());

    // Completed runs no longer surface via running_for_user.
    let running = repo.running_for_user(user_id).await.expect("running");
    assert!(running.is_empty());
}

#[tokio::test]
async fn mark_failed_records_error_message() {
    let db = make_db().await;
    let repo = SeaOrmSyncOperationRepository::new(Arc::clone(&db));
    let user_id = Uuid::new_v4();
    let run_id = Uuid::new_v4().to_string();
    repo.create_operation(&run_id, &[], &[], user_id)
        .await
        .expect("create");
    repo.mark_failed(&run_id, "server_shutdown")
        .await
        .expect("mark_failed");
    let row = repo
        .get_by_run_id(&run_id)
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(row.status, "failed");
    assert_eq!(row.error_message.as_deref(), Some("server_shutdown"));
}
