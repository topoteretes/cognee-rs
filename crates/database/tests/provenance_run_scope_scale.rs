#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Scale test for the run-scoped provenance queries.
//!
//! `get_unique_nodes_for_run`'s doc comment states the reason it is a correlated
//! `NOT EXISTS` over the *predicate* rather than over a list of the selected row
//! ids:
//!
//! > a large run owns millions of rows and an `IN (…)` of ids would blow past
//! > SQLite's `SQLITE_MAX_VARIABLE_NUMBER` and Postgres' 65 535.
//!
//! That rationale was enforced by nothing but the comment. This test seeds a run
//! whose row count is above SQLite's 32 766 bind-variable cap and drives
//! selection, exclusivity, marker collection and deletion through it, so the
//! id-list formulation cannot be reintroduced without a red test.
//!
//! Row count is `COGNEE_SCALE_ROWS` (default 40 000) so the same test can be run
//! smaller when profiling.

// ── Floor guard ────────────────────────────────────────────────────────────
// The suite is behind `sqlite` because it drives a real in-memory SQLite, and
// `cognee-database` declares no default features: built without it this target
// compiles down to nothing and libtest prints `running 0 tests … ok`. That is
// the silent no-op `scripts/ci/assert_pg_suite_ran.sh` exists to make
// impossible, and it is how this file spent its first life — `cargo test -p
// cognee-database --test provenance_run_scope_scale` reported green while
// running zero cases. `cargo test --workspace` (what CI runs) turns the feature
// on by unification, so the healthy path is unaffected; the one case below
// keeps the unhealthy one loud.
#[cfg(not(feature = "sqlite"))]
#[test]
fn suite_compiled_to_zero_cases_without_the_sqlite_feature() {
    panic!(
        "provenance_run_scope_scale needs cognee-database's `sqlite` \
         feature and was built without it, so every case below was compiled \
         out. Run it as part of the workspace (`cargo test --workspace`, which \
         unifies the feature in) or pass `--features sqlite`."
    );
}

#[cfg(feature = "sqlite")]
mod scale {
    use std::collections::BTreeSet;
    use std::time::Instant;

    use chrono::Utc;
    use cognee_database::ops::datasets::create_dataset;
    use cognee_database::ops::graph_storage::{
        RunScope, delete_nodes_for_run, get_data_ids_for_run, get_nodes_for_run,
        get_unique_nodes_for_run, upsert_nodes,
    };
    use cognee_database::{GraphNode, connect, initialize};
    use cognee_models::Dataset;
    use serde_json::json;
    use uuid::Uuid;

    /// SQLite's modern `SQLITE_MAX_VARIABLE_NUMBER`. The default row count is above
    /// it on purpose: an `IN (:id, …)` over the selected rows would not compile.
    const SQLITE_BIND_CAP: usize = 32_766;

    fn row_count() -> usize {
        std::env::var("COGNEE_SCALE_ROWS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(40_000)
    }

    /// How many of the run's nodes share their slug with a row outside the scope,
    /// and must therefore survive exclusivity.
    const SHARED: usize = 500;
    /// Distinct files the run touched.
    const FILES: usize = 8;

    #[tokio::test]
    async fn run_scope_queries_work_above_the_bind_variable_cap() {
        let rows = row_count();
        assert!(
            rows > SQLITE_BIND_CAP,
            "COGNEE_SCALE_ROWS={rows} is below SQLite's bind cap; the test would \
             not exercise what it exists for"
        );

        let db = connect("sqlite::memory:").await.expect("connect");
        initialize(&db).await.expect("migrate");

        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        let other_dataset = Uuid::new_v4();
        for (id, name) in [(dataset, "big"), (other_dataset, "other")] {
            create_dataset(&db, Dataset::new(name.into(), user, None, id))
                .await
                .expect("dataset");
        }
        let run = Uuid::new_v4();
        let files: Vec<Uuid> = (0..FILES).map(|_| Uuid::new_v4()).collect();

        let node = |slug: Uuid, data: Uuid, ds: Uuid, r: Option<Uuid>| GraphNode {
            id: Uuid::new_v4(),
            slug,
            user_id: user,
            data_id: data,
            dataset_id: ds,
            pipeline_run_id: r,
            label: Some("n".into()),
            node_type: "Entity".into(),
            indexed_fields: json!({ "index_fields": ["name"] }),
            attributes: None,
            created_at: Utc::now(),
        };

        // The run's own rows.
        let slugs: Vec<Uuid> = (0..rows).map(|_| Uuid::new_v4()).collect();
        let mine: Vec<GraphNode> = slugs
            .iter()
            .enumerate()
            .map(|(i, slug)| node(*slug, files[i % FILES], dataset, Some(run)))
            .collect();

        // Claimants outside the scope for the first SHARED slugs: half in another
        // dataset, half pre-ownership rows in the same dataset.
        let outside: Vec<GraphNode> = slugs
            .iter()
            .take(SHARED)
            .enumerate()
            .map(|(i, slug)| {
                if i % 2 == 0 {
                    node(*slug, files[0], other_dataset, Some(Uuid::new_v4()))
                } else {
                    node(*slug, files[0], dataset, None)
                }
            })
            .collect();

        let t = Instant::now();
        for chunk in mine.chunks(2_000) {
            upsert_nodes(&db, chunk).await.expect("seed run rows");
        }
        upsert_nodes(&db, &outside)
            .await
            .expect("seed outside rows");
        println!(
            "seeded {rows} run rows + {SHARED} outside rows in {:?}",
            t.elapsed()
        );

        let scope = RunScope::whole_run(run, dataset);

        // --- selection -------------------------------------------------------
        let t = Instant::now();
        let selected = get_nodes_for_run(&db, &scope).await.expect("select");
        println!("selection: {} rows in {:?}", selected.len(), t.elapsed());
        assert_eq!(selected.len(), rows, "selection lost rows at scale");

        // --- the affected files ----------------------------------------------
        let t = Instant::now();
        let data_ids = get_data_ids_for_run(&db, &scope).await.expect("data ids");
        println!("data ids: {} in {:?}", data_ids.len(), t.elapsed());
        assert_eq!(
            data_ids.iter().copied().collect::<BTreeSet<_>>(),
            files.iter().copied().collect::<BTreeSet<_>>()
        );

        // --- exclusivity ------------------------------------------------------
        // This is the query the bind-variable argument is about.
        let t = Instant::now();
        let unique = get_unique_nodes_for_run(&db, &scope)
            .await
            .expect("exclusivity at scale");
        println!("exclusivity: {} rows in {:?}", unique.len(), t.elapsed());
        assert_eq!(
            unique.len(),
            rows - SHARED,
            "exclusivity miscounted at scale"
        );
        let unique_slugs: BTreeSet<Uuid> = unique.iter().map(|n| n.slug).collect();
        for shared in slugs.iter().take(SHARED) {
            assert!(
                !unique_slugs.contains(shared),
                "a slug claimed outside the scope was reported exclusive"
            );
        }

        // --- deletion ---------------------------------------------------------
        let t = Instant::now();
        let deleted = delete_nodes_for_run(&db, &scope).await.expect("delete");
        println!("deletion: {deleted} rows in {:?}", t.elapsed());
        assert_eq!(deleted as usize, rows);
        assert!(
            get_nodes_for_run(&db, &scope)
                .await
                .expect("post-delete")
                .is_empty()
        );
        // The outside rows are untouched.
        let survivors = get_nodes_for_run(&db, &RunScope::whole_run(run, other_dataset))
            .await
            .expect("other dataset");
        assert!(survivors.is_empty(), "wrong run in the other dataset");
    }

    // ---------------------------------------------------------------------------
    // KNOWN DEFECT, deliberately not asserted here — see the commit message.
    // ---------------------------------------------------------------------------
    //
    // The predicate formulation buys freedom from the bind-variable cap (proved
    // above) at the cost of a *correlated* subquery, which is only cheap if the
    // correlation column is indexed. It is not:
    //
    //   * `nodes` has `idx_nodes_dataset_slug` on (dataset_id, slug). The subquery
    //     correlates on `slug` alone — its only `dataset_id` term sits inside the
    //     `outside` disjunction — so `slug` is not an index prefix and SQLite
    //     plans `SCAN n2`.
    //   * `edges` has no index mentioning `slug` at all.
    //
    // Measured on this branch, in-memory SQLite, 40 000 rows: the
    // `get_unique_nodes_for_run` call above alone takes ~52 s, and the curve is
    // quadratic (5 000 -> 0.75 s, 10 000 -> 2.97 s, 20 000 -> 11.8 s). Seeding a
    // plain `CREATE INDEX ... ON nodes (slug)` and one on `edges (slug)` takes
    // that call to ~0.22 s and this whole file from 53 s to 1.16 s.
    //
    // This is a real defect on the cognify rollback path, with a user waiting,
    // and `get_unique_nodes_for_run`'s own doc comment justifies the design with
    // "a large run owns millions of rows". It wants a migration adding the two
    // indexes, plus the Python-parity question that goes with a schema change —
    // all of which is outside a test-only change. It is recorded here rather than
    // asserted because a permanently-failing test would be retried (CI sets
    // `retries = 1`) and never pass, and would red-board every later change on
    // this branch. Add the assertion back in the same commit that adds the
    // indexes.
}
