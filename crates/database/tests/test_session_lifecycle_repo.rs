#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! LIB-05 repository tests for `SessionLifecycleDb`.
//!
//! Validates the trait impl on `DatabaseConnection` against the SeaORM
//! schema landed in LIB-03. Each test runs in its own in-memory SQLite
//! database (`sqlite::memory:`) so they don't interfere even when
//! `cargo test` runs them in parallel.
//!
//! The abandoned-status test temporarily mutates
//! `SESSION_ABANDON_AFTER_SECONDS` via `std::env::set_var`. To keep
//! that env-var change isolated from other tests, that test uses
//! `serial_test::serial` and unsets the var on the way out.
//!
//! Python source-of-truth references:
//!   * `cognee/modules/session_lifecycle/metrics.py` (write paths +
//!     read paths + effective-status).
//!   * `cognee/api/v1/sessions/routers/get_sessions_router.py`
//!     (aggregate_stats, cost_by_model row shapes).

use chrono::{Duration, Utc};
use cognee_database::{
    DatabaseConnection, SessionLifecycleDb, SessionListFilters, connect, initialize,
};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use uuid::Uuid;

async fn make_db() -> DatabaseConnection {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");
    db
}

fn hex(u: Uuid) -> String {
    u.simple().to_string()
}

#[tokio::test]
async fn test_ensure_and_touch_session_upserts() {
    let db = make_db().await;
    let user = Uuid::new_v4();
    let sid = "cc_proj_aaaaaaaaaaaa";

    db.ensure_and_touch_session(sid, user, None)
        .await
        .expect("first ensure");

    // Fetch initial state.
    let r1 = db
        .get_session_row(sid, user, &[], false)
        .await
        .expect("get_session_row")
        .expect("row exists");
    let started1 = r1.record.started_at;
    let last1 = r1.record.last_activity_at;
    assert_eq!(r1.record.status, "running");
    assert_eq!(r1.record.tokens_in, 0);

    // Sleep enough that the new `last_activity_at` is strictly later.
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;

    db.ensure_and_touch_session(sid, user, None)
        .await
        .expect("second ensure");

    let r2 = db
        .get_session_row(sid, user, &[], false)
        .await
        .expect("get_session_row")
        .expect("row exists");
    assert_eq!(
        r2.record.started_at, started1,
        "started_at must be sticky across re-touches"
    );
    assert!(
        r2.record.last_activity_at > last1,
        "last_activity_at must move forward; got {} → {}",
        last1,
        r2.record.last_activity_at
    );
    assert_eq!(r2.record.status, "running");
    // No double-row.
    let count_rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT COUNT(*) AS n FROM session_records".to_string(),
        ))
        .await
        .expect("count");
    let n: i64 = count_rows[0].try_get("", "n").expect("read count");
    assert_eq!(n, 1, "ensure must upsert, not duplicate");
}

#[tokio::test]
async fn test_ensure_and_touch_session_backfills_dataset_id() {
    let db = make_db().await;
    let user = Uuid::new_v4();
    let dataset = Uuid::new_v4();
    let sid = "cc_proj_bbbbbbbbbbbb";

    // First call: no dataset.
    db.ensure_and_touch_session(sid, user, None)
        .await
        .expect("ensure no ds");
    let r1 = db
        .get_session_row(sid, user, &[], false)
        .await
        .expect("get")
        .expect("row");
    assert!(r1.record.dataset_id.is_none(), "no dataset on first call");

    // Second call: provides dataset → backfill.
    db.ensure_and_touch_session(sid, user, Some(dataset))
        .await
        .expect("ensure with ds");
    let r2 = db
        .get_session_row(sid, user, &[], false)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(r2.record.dataset_id.as_deref(), Some(hex(dataset).as_str()));

    // Third call with a *different* dataset must NOT overwrite the
    // backfilled one (COALESCE semantics).
    let other_dataset = Uuid::new_v4();
    db.ensure_and_touch_session(sid, user, Some(other_dataset))
        .await
        .expect("ensure with other ds");
    let r3 = db
        .get_session_row(sid, user, &[], false)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(
        r3.record.dataset_id.as_deref(),
        Some(hex(dataset).as_str()),
        "COALESCE must keep the original dataset, not overwrite"
    );
}

#[tokio::test]
async fn test_accumulate_usage_increments() {
    let db = make_db().await;
    let user = Uuid::new_v4();
    let sid = "cc_proj_cccccccccccc";

    db.ensure_and_touch_session(sid, user, None)
        .await
        .expect("ensure");

    db.accumulate_usage(sid, user, Some("gpt-4o"), 100, 50, 0.012, false)
        .await
        .expect("accumulate 1");
    db.accumulate_usage(sid, user, Some("gpt-4o"), 25, 5, 0.003, false)
        .await
        .expect("accumulate 2");
    // A second model should attribute separately on session_model_usage
    // but still aggregate into session_records.
    db.accumulate_usage(sid, user, Some("gpt-4o-mini"), 10, 1, 0.0001, true)
        .await
        .expect("accumulate 3");

    let r = db
        .get_session_row(sid, user, &[], false)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(r.record.tokens_in, 135);
    assert_eq!(r.record.tokens_out, 56);
    assert!((r.record.cost_usd - 0.0151).abs() < 1e-9);
    assert_eq!(r.record.error_count, 1);
    assert_eq!(r.record.last_model.as_deref(), Some("gpt-4o-mini"));

    // Per-model rows: gpt-4o has the sum of both calls; gpt-4o-mini has
    // only the third.
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT model, tokens_in, tokens_out, cost_usd \
             FROM session_model_usage ORDER BY model"
                .to_string(),
        ))
        .await
        .expect("model rows");
    assert_eq!(rows.len(), 2);
    let m0: String = rows[0].try_get("", "model").expect("m0");
    assert_eq!(m0, "gpt-4o");
    let ti0: i32 = rows[0].try_get("", "tokens_in").expect("ti0");
    let to0: i32 = rows[0].try_get("", "tokens_out").expect("to0");
    let c0: f64 = rows[0].try_get("", "cost_usd").expect("c0");
    assert_eq!(ti0, 125);
    assert_eq!(to0, 55);
    assert!((c0 - 0.015).abs() < 1e-9);

    let m1: String = rows[1].try_get("", "model").expect("m1");
    assert_eq!(m1, "gpt-4o-mini");
    let ti1: i32 = rows[1].try_get("", "tokens_in").expect("ti1");
    assert_eq!(ti1, 10);

    // Terminal-session gate: flip to completed, attempt another
    // accumulate, verify no change. Mirrors Python `metrics.py:172-181`.
    db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        format!(
            "UPDATE session_records SET status = 'completed' \
             WHERE session_id = '{sid}' AND user_id = '{}'",
            hex(user)
        ),
    ))
    .await
    .expect("flip status");

    db.accumulate_usage(sid, user, Some("gpt-4o"), 9999, 9999, 9.99, true)
        .await
        .expect("post-terminal accumulate is a no-op on session_records");
    let r2 = db
        .get_session_row(sid, user, &[], false)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(r2.record.tokens_in, 135, "session row must stay frozen");
    assert_eq!(r2.record.tokens_out, 56);
    assert_eq!(r2.record.error_count, 1);
}

#[tokio::test]
async fn test_list_session_rows_pagination() {
    let db = make_db().await;
    let user = Uuid::new_v4();

    // Seed five sessions; small sleep between to give them
    // distinguishable last_activity_at timestamps for stable ordering.
    for i in 0..5 {
        let sid = format!("cc_p_{i:012}");
        db.ensure_and_touch_session(&sid, user, None)
            .await
            .expect("ensure");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Page 1: limit 2, offset 0.
    let p1 = db
        .list_session_rows(SessionListFilters {
            user_id: user,
            permitted_dataset_ids: vec![],
            since: None,
            status_filter: None,
            limit: 2,
            offset: 0,
            order_by: "last_activity_at".to_string(),
            descending: true,
        })
        .await
        .expect("page1");
    assert_eq!(p1.total, 5);
    assert_eq!(p1.limit, 2);
    assert_eq!(p1.offset, 0);
    assert_eq!(p1.sessions.len(), 2);
    assert!(p1.has_more());

    // Page 3 (offset 4): only 1 row left, has_more false.
    let p3 = db
        .list_session_rows(SessionListFilters {
            user_id: user,
            permitted_dataset_ids: vec![],
            since: None,
            status_filter: None,
            limit: 2,
            offset: 4,
            order_by: "last_activity_at".to_string(),
            descending: true,
        })
        .await
        .expect("page3");
    assert_eq!(p3.total, 5);
    assert_eq!(p3.sessions.len(), 1);
    assert!(!p3.has_more());

    // Bogus order_by must fall back to last_activity_at (sortable lookup).
    let p_fallback = db
        .list_session_rows(SessionListFilters {
            user_id: user,
            permitted_dataset_ids: vec![],
            since: None,
            status_filter: None,
            limit: 5,
            offset: 0,
            order_by: "DROP TABLE session_records".to_string(),
            descending: true,
        })
        .await
        .expect("fallback");
    assert_eq!(p_fallback.sessions.len(), 5);
}

#[tokio::test]
#[serial_test::serial]
async fn test_list_session_rows_status_filter_with_abandoned() {
    // Set the abandonment threshold to 1s so any row whose
    // last_activity_at is older than 1s reports as abandoned at read
    // time. We set it via env var because the helper reads it on each
    // call (Python parity).
    // SAFETY: the test is marked serial_test::serial so other tests
    // don't observe this env mutation.
    unsafe { std::env::set_var("SESSION_ABANDON_AFTER_SECONDS", "1") };

    async {
        let db = make_db().await;
        let user = Uuid::new_v4();
        let stale_sid = "cc_p_stale_aaaaaaaaaa";
        let fresh_sid = "cc_p_fresh_bbbbbbbbbb";

        // Insert a stale running row (last_activity_at well in the past).
        let user_hex = hex(user);
        let now = Utc::now();
        let old_ts = now - Duration::seconds(60);

        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO session_records (\
                    session_id, user_id, dataset_id, status, started_at, \
                    last_activity_at, ended_at, tokens_in, tokens_out, \
                    cost_usd, error_count, last_model\
                 ) VALUES (\
                    '{stale_sid}', '{user_hex}', NULL, 'running', \
                    '{ts}', '{ts}', NULL, 0, 0, 0.0, 0, NULL\
                 )",
                ts = old_ts.to_rfc3339()
            ),
        ))
        .await
        .expect("seed stale row");

        // And a fresh running row via the regular path.
        db.ensure_and_touch_session(fresh_sid, user, None)
            .await
            .expect("ensure fresh");

        // status_filter = "abandoned" should match exactly the stale row,
        // even though its stored status column is still 'running'.
        let page = db
            .list_session_rows(SessionListFilters {
                user_id: user,
                permitted_dataset_ids: vec![],
                since: None,
                status_filter: Some("abandoned".to_string()),
                limit: 50,
                offset: 0,
                order_by: "last_activity_at".to_string(),
                descending: true,
            })
            .await
            .expect("list abandoned");

        assert_eq!(page.sessions.len(), 1, "exactly the stale row matches");
        assert_eq!(page.sessions[0].record.session_id, stale_sid);
        assert_eq!(
            page.sessions[0].record.status, "running",
            "stored status must NOT be mutated by the read"
        );
        assert_eq!(
            page.sessions[0].effective_status, "abandoned",
            "effective_status must be inferred at read time"
        );

        // status_filter = "running" must match the fresh row only.
        let page_running = db
            .list_session_rows(SessionListFilters {
                user_id: user,
                permitted_dataset_ids: vec![],
                since: None,
                status_filter: Some("running".to_string()),
                limit: 50,
                offset: 0,
                order_by: "last_activity_at".to_string(),
                descending: true,
            })
            .await
            .expect("list running");
        assert_eq!(page_running.sessions.len(), 1);
        assert_eq!(page_running.sessions[0].record.session_id, fresh_sid);
        assert_eq!(page_running.sessions[0].effective_status, "running");
    }
    .await;

    // Unset the env var on the success path. If assertions panicked
    // above, this won't run — but `serial_test::serial` ensures no
    // other test observes the leaked env var anyway, and the next
    // test run starts in a fresh process.
    unsafe { std::env::remove_var("SESSION_ABANDON_AFTER_SECONDS") };
}

#[tokio::test]
async fn test_list_session_rows_visibility() {
    let db = make_db().await;
    let alice = Uuid::new_v4();
    let bob = Uuid::new_v4();
    let shared_dataset = Uuid::new_v4();

    // Alice owns one session.
    db.ensure_and_touch_session("cc_alice_111111111111", alice, None)
        .await
        .expect("alice 1");
    // Bob owns one session attached to shared_dataset.
    db.ensure_and_touch_session("cc_bob_22222222222222", bob, Some(shared_dataset))
        .await
        .expect("bob 1");
    // Bob owns another session NOT in the shared dataset.
    db.ensure_and_touch_session("cc_bob_33333333333333", bob, None)
        .await
        .expect("bob 2");

    // Alice with no permitted datasets → sees only her own.
    let p1 = db
        .list_session_rows(SessionListFilters {
            user_id: alice,
            permitted_dataset_ids: vec![],
            since: None,
            status_filter: None,
            limit: 50,
            offset: 0,
            order_by: "last_activity_at".to_string(),
            descending: true,
        })
        .await
        .expect("alice list");
    assert_eq!(p1.sessions.len(), 1);
    assert_eq!(p1.sessions[0].record.session_id, "cc_alice_111111111111");

    // Alice + permitted=[shared_dataset] → sees her own AND bob's
    // shared-dataset session.
    let p2 = db
        .list_session_rows(SessionListFilters {
            user_id: alice,
            permitted_dataset_ids: vec![shared_dataset],
            since: None,
            status_filter: None,
            limit: 50,
            offset: 0,
            order_by: "last_activity_at".to_string(),
            descending: true,
        })
        .await
        .expect("alice list with permission");
    assert_eq!(p2.sessions.len(), 2);
    let ids: std::collections::HashSet<_> = p2
        .sessions
        .iter()
        .map(|s| s.record.session_id.as_str())
        .collect();
    assert!(ids.contains("cc_alice_111111111111"));
    assert!(ids.contains("cc_bob_22222222222222"));
    assert!(!ids.contains("cc_bob_33333333333333"));
}

#[tokio::test]
async fn test_aggregate_stats_buckets() {
    let db = make_db().await;
    let user = Uuid::new_v4();

    // Seed:
    //  * 2 completed (cost 0.10 + 0.20, tokens 100/200)
    //  * 1 failed (cost 0.05, tokens 50/0)
    //  * 1 running fresh
    //  * 1 running stale (would be abandoned with low threshold; we
    //    keep default 1800s here so it remains 'running' for this
    //    test)
    let sids = [
        ("cc_done1", "completed", 0.10_f64, 100_i32, 200_i32),
        ("cc_done2", "completed", 0.20_f64, 100_i32, 200_i32),
        ("cc_fail", "failed", 0.05_f64, 50_i32, 0_i32),
        ("cc_run1", "running", 0.0_f64, 0_i32, 0_i32),
        ("cc_run2", "running", 0.0_f64, 0_i32, 0_i32),
    ];

    let user_hex = hex(user);
    let now = Utc::now();
    for (sid, status, cost, ti, to) in sids {
        let started = now - Duration::seconds(60);
        let ended = if status != "running" {
            Some(now - Duration::seconds(10))
        } else {
            None
        };
        let last_act = ended.unwrap_or(now);
        let ended_lit = match ended {
            Some(e) => format!("'{}'", e.to_rfc3339()),
            None => "NULL".to_string(),
        };
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "INSERT INTO session_records (\
                    session_id, user_id, dataset_id, status, started_at, \
                    last_activity_at, ended_at, tokens_in, tokens_out, \
                    cost_usd, error_count, last_model\
                 ) VALUES ('{sid}', '{user_hex}', NULL, '{status}', \
                          '{s}', '{l}', {ended_lit}, {ti}, {to}, {cost}, 0, NULL)",
                s = started.to_rfc3339(),
                l = last_act.to_rfc3339()
            ),
        ))
        .await
        .expect("seed");
    }

    let stats = db
        .aggregate_stats(user, &[], None)
        .await
        .expect("aggregate");

    assert_eq!(stats.sessions, 5);
    assert_eq!(stats.completed, 2);
    assert_eq!(stats.failed, 1);
    assert_eq!(stats.running, 2);
    assert_eq!(stats.abandoned, 0);
    assert!((stats.total_spend_usd - 0.35).abs() < 1e-9);
    assert!((stats.avg_spend_per_session_usd - 0.07).abs() < 1e-9);
    assert_eq!(stats.tokens_in, 250);
    assert_eq!(stats.tokens_out, 400);
    assert_eq!(stats.tokens_total, 650);

    // success_rate = completed / decided where decided = completed +
    // failed + abandoned = 3, so 2/3.
    assert!(
        (stats.success_rate - (2.0 / 3.0)).abs() < 1e-9,
        "expected 2/3, got {}",
        stats.success_rate
    );

    // Durations: 4 sessions used (running fresh has end == now, so its
    // duration is positive; running stale has end == now too in this
    // setup since we set `ended` only when status != running and
    // `last_act` for running rows is `now`). Just sanity-check the
    // counts and that values are non-negative.
    assert!(stats.agent_time_s >= 0.0);
    assert!(stats.avg_session_s >= 0.0);
}

#[tokio::test]
async fn test_cost_by_model_groups_correctly() {
    let db = make_db().await;
    let user = Uuid::new_v4();

    // Two sessions, both touch gpt-4o; one also touches gpt-4o-mini.
    db.ensure_and_touch_session("cc_s1_aaaaaaaaaaaa", user, None)
        .await
        .expect("ensure 1");
    db.ensure_and_touch_session("cc_s2_bbbbbbbbbbbb", user, None)
        .await
        .expect("ensure 2");

    db.accumulate_usage(
        "cc_s1_aaaaaaaaaaaa",
        user,
        Some("gpt-4o"),
        100,
        50,
        0.10,
        false,
    )
    .await
    .expect("acc s1 4o");
    db.accumulate_usage(
        "cc_s1_aaaaaaaaaaaa",
        user,
        Some("gpt-4o-mini"),
        20,
        10,
        0.005,
        false,
    )
    .await
    .expect("acc s1 mini");
    db.accumulate_usage(
        "cc_s2_bbbbbbbbbbbb",
        user,
        Some("gpt-4o"),
        80,
        40,
        0.08,
        false,
    )
    .await
    .expect("acc s2 4o");

    let rows = db.cost_by_model(user, &[], None).await.expect("cost");
    assert_eq!(rows.len(), 2);

    // Order: by total cost DESC. gpt-4o = 0.18, gpt-4o-mini = 0.005.
    assert_eq!(rows[0].model, "gpt-4o");
    assert_eq!(rows[0].session_count, 2, "DISTINCT sessions for gpt-4o");
    assert!((rows[0].cost_usd - 0.18).abs() < 1e-9);
    assert_eq!(rows[0].tokens_in, 180);
    assert_eq!(rows[0].tokens_out, 90);

    assert_eq!(rows[1].model, "gpt-4o-mini");
    assert_eq!(rows[1].session_count, 1);
    assert!((rows[1].cost_usd - 0.005).abs() < 1e-9);
    assert_eq!(rows[1].tokens_in, 20);
    assert_eq!(rows[1].tokens_out, 10);
}
