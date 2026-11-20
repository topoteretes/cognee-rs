#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `data.system_metadata` (Python `Data.system_metadata`, SDK-1053): the
//! cognee-owned route stamp round-trips through the entity, survives the
//! re-add refresh (which writes it only when given one), and has its own
//! targeted setter.
//!
//! Runs on in-memory SQLite, so it needs no external database.
#![cfg(feature = "sqlite")]

use cognee_database::ops::data::{
    DataRowRefresh, create_data, get_data, refresh_data_row, update_data_system_metadata,
    update_data_token_count,
};
use cognee_database::{DatabaseError, connect, initialize};
use cognee_models::Data;
use sea_orm::{ConnectionTrait, Statement};
use uuid::Uuid;

const STAMP: &str = r#"{"source":"code_repo","repo_path":"/src/app"}"#;

fn data(system_metadata: Option<&str>) -> Data {
    let id = Uuid::new_v4();
    let mut builder = Data::builder(
        id,
        format!("data-{id}"),
        "file:///tmp/raw",
        "file:///tmp/raw",
        "txt",
        "text/plain",
        format!("hash-{id}"),
        Uuid::new_v4(),
    )
    .external_metadata(r#"{"user":"field"}"#);
    if let Some(meta) = system_metadata {
        builder = builder.system_metadata(meta);
    }
    builder.build()
}

async fn db() -> cognee_database::DatabaseConnection {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");
    db
}

#[tokio::test]
async fn system_metadata_round_trips_through_the_entity() {
    let db = db().await;
    let stamped = data(Some(STAMP));
    let plain = data(None);
    create_data(&db, stamped.clone()).await.unwrap();
    create_data(&db, plain.clone()).await.unwrap();

    let row = get_data(&db, stamped.id).await.unwrap().expect("row");
    assert_eq!(row.system_metadata.as_deref(), Some(STAMP));
    assert_eq!(
        row.external_metadata.as_deref(),
        Some(r#"{"user":"field"}"#),
        "never merged into the user field"
    );
    let row = get_data(&db, plain.id).await.unwrap().expect("row");
    assert_eq!(row.system_metadata, None);
}

/// A row Python wrote: SQLAlchemy's `JSON` column is text on SQLite, and an
/// explicit `None` is stored as the JSON text `null`.
#[tokio::test]
async fn reads_values_python_wrote() {
    let db = db().await;
    let a = data(None);
    let b = data(None);
    create_data(&db, a.clone()).await.unwrap();
    create_data(&db, b.clone()).await.unwrap();
    for (id, value) in [(a.id, r#"{"source": "code"}"#), (b.id, "null")] {
        db.execute(Statement::from_sql_and_values(
            db.get_database_backend(),
            "UPDATE data SET system_metadata = ? WHERE id = ?",
            [value.into(), cognee_database::uuid_hex::to_hex(id).into()],
        ))
        .await
        .unwrap();
    }
    let row = get_data(&db, a.id).await.unwrap().unwrap();
    assert_eq!(
        row.system_metadata.as_deref(),
        Some(r#"{"source": "code"}"#)
    );
    let row = get_data(&db, b.id).await.unwrap().unwrap();
    assert_eq!(row.system_metadata.as_deref(), Some("null"));
}

#[tokio::test]
async fn update_data_system_metadata_touches_only_its_column() {
    let db = db().await;
    let d = data(None);
    create_data(&db, d.clone()).await.unwrap();
    update_data_token_count(&db, d.id, 7).await.unwrap();

    let written = update_data_system_metadata(&db, d.id, Some(STAMP))
        .await
        .expect("set");
    let row = get_data(&db, d.id).await.unwrap().unwrap();
    assert_eq!(row.system_metadata.as_deref(), Some(STAMP));
    assert_eq!(row.updated_at, Some(written));
    assert_eq!(row.token_count, 7, "token_count survives");
    assert_eq!(
        row.external_metadata.as_deref(),
        Some(r#"{"user":"field"}"#)
    );

    update_data_system_metadata(&db, d.id, None).await.unwrap();
    let row = get_data(&db, d.id).await.unwrap().unwrap();
    assert_eq!(row.system_metadata, None, "None clears the stamp");
}

#[tokio::test]
async fn update_data_system_metadata_on_a_missing_row_is_not_found() {
    let db = db().await;
    let err = update_data_system_metadata(&db, Uuid::new_v4(), Some(STAMP))
        .await
        .expect_err("no row");
    assert!(matches!(err, DatabaseError::NotFound(_)), "got {err:?}");
}

fn refresh_of(d: &Data, system_metadata: Option<&str>) -> DataRowRefresh {
    DataRowRefresh {
        name: d.name.clone(),
        raw_data_location: d.raw_data_location.clone(),
        original_data_location: d.original_data_location.clone(),
        extension: d.extension.clone(),
        mime_type: d.mime_type.clone(),
        original_extension: None,
        original_mime_type: None,
        loader_engine: None,
        raw_content_hash: None,
        data_size: d.data_size,
        label: Some("relabelled".to_string()),
        node_set: None,
        external_metadata: None,
        system_metadata: system_metadata.map(str::to_string),
    }
}

#[tokio::test]
async fn refresh_data_row_writes_system_metadata() {
    let db = db().await;
    let d = data(Some(STAMP));
    create_data(&db, d.clone()).await.unwrap();
    refresh_data_row(&db, d.id, &refresh_of(&d, Some(r#"{"source":"code"}"#)))
        .await
        .unwrap();
    let row = get_data(&db, d.id).await.unwrap().unwrap();
    assert_eq!(row.system_metadata.as_deref(), Some(r#"{"source":"code"}"#));
}

/// `system_metadata` is a patch: `None` leaves the stored stamp alone, as
/// Python's re-add of unchanged content without a stamp never writes the
/// column (`ingest_data.py:589-590`). The other columns are still written.
#[tokio::test]
async fn refresh_data_row_without_system_metadata_leaves_the_column() {
    let db = db().await;
    let d = data(None);
    create_data(&db, d.clone()).await.unwrap();
    // Written after the caller's snapshot: the refresh must not clobber it.
    update_data_system_metadata(&db, d.id, Some(STAMP))
        .await
        .unwrap();

    let written = refresh_data_row(&db, d.id, &refresh_of(&d, None))
        .await
        .unwrap();
    let row = get_data(&db, d.id).await.unwrap().unwrap();
    assert_eq!(row.system_metadata.as_deref(), Some(STAMP));
    assert_eq!(row.label.as_deref(), Some("relabelled"));
    assert_eq!(row.updated_at, Some(written));
}
