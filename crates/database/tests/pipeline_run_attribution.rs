// The auth tables (`users`, `principals`, `user_api_key`) plus the
// `cognee_database::auth::*` repositories moved to the closed
// `cognee-access-control` crate (T2-move §4 S2). The OSS
// `list_recent_with_attribution` projection no longer joins `users`, so
// `owner_email` is always `None` on the OSS side. These tests verified
// the now-removed join; T3 will re-home an updated version that
// exercises the closed-side repository.
#![cfg(any())]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Tests for `PipelineRunRepository::list_recent_with_attribution`.
//!
//! Verifies the `pipeline_runs ⨝ datasets ⨝ users` LEFT JOIN behaviour: rows
//! whose dataset has been deleted still surface (with `dataset_name` /
//! `owner_email` set to None), and the optional dataset filter narrows the
//! result set correctly.

use std::sync::Arc;

use cognee_database::auth::{CreateUserPayload, SeaOrmUserAuthRepository, UserAuthRepository};
use cognee_database::entities::principal;
use cognee_database::{
    DatabaseConnection, PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository,
    connect, initialize, ops, uuid_hex,
};
use cognee_models::Dataset;
use sea_orm::{ActiveValue::Set, EntityTrait};
use uuid::Uuid;

async fn make_db() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    Arc::new(db)
}

async fn ensure_principal(db: &DatabaseConnection, id: Uuid, kind: &str) {
    let hex = uuid_hex::to_hex(id);
    if principal::Entity::find_by_id(hex.clone())
        .one(db)
        .await
        .expect("principal find")
        .is_some()
    {
        return;
    }
    let am = principal::ActiveModel {
        id: Set(hex),
        principal_type: Set(kind.into()),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(None),
    };
    principal::Entity::insert(am)
        .exec(db)
        .await
        .expect("insert principal");
}

async fn seed_user(db: &DatabaseConnection, id: Uuid, email: &str) {
    ensure_principal(db, id, "user").await;
    let repo = SeaOrmUserAuthRepository { db: db.clone() };
    repo.create(CreateUserPayload {
        id,
        email: email.into(),
        hashed_password: String::new(),
        is_active: true,
        is_superuser: false,
        is_verified: true,
        tenant_id: None,
        parent_user_id: None,
    })
    .await
    .expect("create user");
}

async fn seed_dataset(db: &DatabaseConnection, dataset_id: Uuid, owner_id: Uuid, name: &str) {
    let dataset = Dataset::new(name.to_string(), owner_id, None, dataset_id);
    ops::datasets::create_dataset(db, dataset)
        .await
        .expect("create dataset");
}

#[tokio::test]
async fn list_recent_with_attribution_returns_orphan_with_nulls() {
    let db = make_db().await;
    let owner_id = Uuid::new_v4();
    seed_user(&db, owner_id, "owner@example.com").await;

    let dataset_a = Uuid::new_v4();
    seed_dataset(&db, dataset_a, owner_id, "dataset-a").await;

    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));

    // Two attached rows, one orphan (its dataset is never created).
    let orphan_dataset = Uuid::new_v4();
    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "cognify_pipeline",
        Some(dataset_a),
        PipelineRunStatus::Started,
        None,
    )
    .await
    .expect("log row 1");
    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "memify_pipeline",
        Some(dataset_a),
        PipelineRunStatus::Completed,
        None,
    )
    .await
    .expect("log row 2");
    // Insert a synthetic orphan row directly via raw SQL so we bypass the
    // `pipeline_runs.dataset_id` FK constraint (the activity router needs to
    // surface rows whose dataset has been deleted; in production the FK is
    // ON DELETE CASCADE so this scenario only arises before a migration adds
    // the FK or in cross-SDK schemas without it).
    {
        use sea_orm::{ConnectionTrait, Statement};
        let row_id = uuid_hex::to_hex(Uuid::new_v4());
        let pipeline_run_hex = uuid_hex::to_hex(Uuid::new_v4());
        let pipeline_id_hex = uuid_hex::to_hex(Uuid::new_v4());
        let orphan_hex = uuid_hex::to_hex(orphan_dataset);
        let now = chrono::Utc::now().to_rfc3339();
        // SQLite does not enforce FKs unless `PRAGMA foreign_keys=ON` is
        // explicitly set; sqlx-sqlite leaves the default ON, so we toggle it
        // off for this insert and back on afterwards.
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "PRAGMA foreign_keys = OFF".to_string(),
        ))
        .await
        .expect("disable fk");
        db.execute(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO pipeline_runs (id, created_at, status, pipeline_run_id, pipeline_name, pipeline_id, dataset_id, run_info) VALUES ($1, $2, 'DATASET_PROCESSING_ERRORED', $3, 'add_pipeline', $4, $5, NULL)",
            [
                row_id.clone().into(),
                now.into(),
                pipeline_run_hex.into(),
                pipeline_id_hex.into(),
                orphan_hex.into(),
            ],
        ))
        .await
        .expect("orphan insert");
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "PRAGMA foreign_keys = ON".to_string(),
        ))
        .await
        .expect("re-enable fk");
    }

    let rows = repo
        .list_recent_with_attribution(None, 10)
        .await
        .expect("list_recent_with_attribution");
    assert_eq!(rows.len(), 3, "all three rows surface (orphan included)");

    // Order is DESC by created_at — the orphan is the most recent insert.
    let by_id: std::collections::HashMap<Uuid, _> = rows
        .iter()
        .map(|r| (r.dataset_id.unwrap_or(Uuid::nil()), r.clone()))
        .collect();
    let orphan = by_id.get(&orphan_dataset).cloned().expect("orphan present");
    assert!(orphan.dataset_name.is_none(), "orphan has no dataset_name");
    assert!(orphan.owner_email.is_none(), "orphan has no owner_email");

    let attached = by_id.get(&dataset_a).cloned().expect("attached row");
    assert_eq!(attached.dataset_name.as_deref(), Some("dataset-a"));
    assert_eq!(attached.owner_email.as_deref(), Some("owner@example.com"));
    assert_eq!(attached.owner_id, Some(owner_id));
}

#[tokio::test]
async fn list_recent_with_attribution_filters_by_dataset() {
    let db = make_db().await;
    let owner_id = Uuid::new_v4();
    seed_user(&db, owner_id, "owner2@example.com").await;
    let ds_a = Uuid::new_v4();
    let ds_b = Uuid::new_v4();
    seed_dataset(&db, ds_a, owner_id, "alpha").await;
    seed_dataset(&db, ds_b, owner_id, "beta").await;

    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "cognify_pipeline",
        Some(ds_a),
        PipelineRunStatus::Started,
        None,
    )
    .await
    .expect("a");
    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "cognify_pipeline",
        Some(ds_b),
        PipelineRunStatus::Started,
        None,
    )
    .await
    .expect("b");

    let only_a = repo
        .list_recent_with_attribution(Some(ds_a), 10)
        .await
        .expect("filter by ds_a");
    assert_eq!(only_a.len(), 1);
    assert_eq!(only_a[0].dataset_id, Some(ds_a));
    assert_eq!(only_a[0].dataset_name.as_deref(), Some("alpha"));
}

#[tokio::test]
async fn list_recent_with_attribution_orders_desc_by_created_at() {
    let db = make_db().await;
    let owner_id = Uuid::new_v4();
    seed_user(&db, owner_id, "owner3@example.com").await;
    let ds = Uuid::new_v4();
    seed_dataset(&db, ds, owner_id, "alpha").await;

    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "first",
        Some(ds),
        PipelineRunStatus::Initiated,
        None,
    )
    .await
    .expect("first");
    // Sleep so the second row gets a strictly-greater created_at.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        "second",
        Some(ds),
        PipelineRunStatus::Started,
        None,
    )
    .await
    .expect("second");

    let rows = repo
        .list_recent_with_attribution(Some(ds), 10)
        .await
        .expect("list");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].pipeline_name, "second", "DESC by created_at");
    assert_eq!(rows[1].pipeline_name, "first");
}
