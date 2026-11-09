#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `SessionLifecycleDb::list_recent_session_ids_for_dataset`: the session
//! listing behind the visualization's session events (Python's
//! `session_events._list_recent_session_ids`, COG-6121). Port of Python's
//! `test_session_event_scoping.py` listing cases, run against the real SQL
//! predicate on SQLite and, when `TEST_POSTGRES_URL` is set, on Postgres.
#![cfg(feature = "sqlite")]

use chrono::{DateTime, Duration, TimeZone, Utc};
use cognee_database::{DatabaseConnection, SessionLifecycleDb, connect, initialize};
use sea_orm::{ConnectionTrait, Statement};
use uuid::Uuid;

const DATASET_A: Uuid = Uuid::from_u128(0xaaaaaaaa_1111_4111_8111_1111111111ab);
const DATASET_B: Uuid = Uuid::from_u128(0xbbbbbbbb_2222_4222_8222_2222222222cd);
const USER: Uuid = Uuid::from_u128(0xcccccccc_3333_4333_8333_3333333333ef);
const MAX_SESSIONS_SCANNED: u32 = 10;

fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 3, 9, 0, 0).unwrap()
}

/// A session row for `user`, last active `age_seconds` before the base time.
async fn add(
    db: &DatabaseConnection,
    session_id: &str,
    dataset_id: Option<Uuid>,
    age_seconds: i64,
    user: Uuid,
) {
    db.ensure_and_touch_session(session_id, user, dataset_id)
        .await
        .unwrap();
    db.execute(Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE session_records SET last_activity_at = $1 WHERE session_id = $2 AND user_id = $3",
        [
            (base_time() - Duration::seconds(age_seconds)).into(),
            session_id.into(),
            user.simple().to_string().into(),
        ],
    ))
    .await
    .unwrap();
}

async fn listed(db: &DatabaseConnection, dataset_id: Option<Uuid>) -> Vec<String> {
    db.list_recent_session_ids_for_dataset(USER, dataset_id, MAX_SESSIONS_SCANNED)
        .await
        .unwrap()
}

/// Every scoping case, each on a clean table.
async fn run_all(db: &DatabaseConnection) {
    let reset = || async {
        db.execute_unprepared("DELETE FROM session_records")
            .await
            .unwrap();
    };

    // A session attributed to B is excluded for A and included for B.
    reset().await;
    add(db, "session-a", Some(DATASET_A), 0, USER).await;
    add(db, "session-b", Some(DATASET_B), 0, USER).await;
    assert_eq!(listed(db, Some(DATASET_A)).await, ["session-a"]);
    assert_eq!(listed(db, Some(DATASET_B)).await, ["session-b"]);

    // A NULL dataset is excluded unless the session id carries the suffix.
    reset().await;
    add(db, "opaque-session", None, 0, USER).await;
    add(db, &format!("default_session_{DATASET_A}"), None, 0, USER).await;
    assert_eq!(
        listed(db, Some(DATASET_A)).await,
        [format!("default_session_{DATASET_A}")]
    );

    // The `_` of the suffix is literal, not a LIKE wildcard; and `%` in the
    // stored id has no special meaning either.
    reset().await;
    add(db, &format!("default_sessionX{DATASET_A}"), None, 0, USER).await;
    add(db, &format!("{DATASET_A}"), None, 1, USER).await;
    add(db, &format!("x%_{DATASET_A}"), None, 2, USER).await;
    assert_eq!(
        listed(db, Some(DATASET_A)).await,
        [format!("x%_{DATASET_A}")]
    );

    // The limit applies after scoping, so the newest for that dataset
    // survive even though B is newer throughout.
    reset().await;
    for index in 0..i64::from(MAX_SESSIONS_SCANNED) + 2 {
        add(db, &format!("b-{index}"), Some(DATASET_B), index, USER).await;
    }
    for index in 0..3 {
        add(
            db,
            &format!("a-{index}"),
            Some(DATASET_A),
            100 + index,
            USER,
        )
        .await;
    }
    assert_eq!(listed(db, Some(DATASET_A)).await, ["a-0", "a-1", "a-2"]);
    let b = listed(db, Some(DATASET_B)).await;
    assert_eq!(b.len(), MAX_SESSIONS_SCANNED as usize);
    assert_eq!(b.first().map(String::as_str), Some("b-0"), "newest first");

    // Another user's session on the same dataset is never listed — not by
    // its dataset_id and not by the suffix.
    reset().await;
    let other = Uuid::new_v4();
    add(
        db,
        &format!("default_session_{DATASET_A}"),
        Some(DATASET_A),
        0,
        other,
    )
    .await;
    add(db, "theirs", Some(DATASET_A), 0, other).await;
    add(db, "mine", Some(DATASET_A), 5, USER).await;
    assert_eq!(listed(db, Some(DATASET_A)).await, ["mine"]);
    assert_eq!(listed(db, None).await, ["mine"]);

    // No dataset keeps the caller's unscoped listing.
    reset().await;
    add(db, "session-a", Some(DATASET_A), 1, USER).await;
    add(db, "opaque-session", None, 0, USER).await;
    add(db, "someone-else", None, 0, other).await;
    assert_eq!(listed(db, None).await, ["opaque-session", "session-a"]);
}

#[tokio::test]
async fn recent_session_ids_are_scoped_to_the_caller_and_dataset_on_sqlite() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");
    run_all(&db).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn recent_session_ids_are_scoped_to_the_caller_and_dataset_on_postgres() {
    let Some(base_url) = cognee_test_utils::test_postgres_url() else {
        eprintln!("TEST_POSTGRES_URL not set — skipping the Postgres session-listing case");
        return;
    };
    let tmp = cognee_test_utils::create_temp_postgres_db(&base_url)
        .await
        .expect("create temp Postgres database");
    let url = tmp.url().to_string();
    let outcome = tokio::spawn(async move {
        let db = connect(&url).await.expect("connect to temp Postgres");
        initialize(&db).await.expect("migrate relational schema");
        run_all(&db).await;
        db.close().await.ok();
    })
    .await;
    tmp.cleanup().await;
    if let Err(join_err) = outcome {
        std::panic::resume_unwind(join_err.into_panic());
    }
}
