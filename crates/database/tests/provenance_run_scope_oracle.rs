#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Differential test: the run-scoped provenance SQL vs. an independent,
//! obviously-correct in-memory reference.
//!
//! The reference below is written from the *specification* (rollback plan §2.4)
//! and the module's doc comments, not from the SQL:
//!
//! > Select the ownership rows for **this run** in **this dataset**, optionally
//! > narrowed to **these files**. Delete the artifacts of any selected row whose
//! > identity is not also claimed by a row *outside* the selection.
//!
//! and
//!
//! > "Outside the selection" covers rows from other runs, rows predating
//! > ownership tracking, rows in other datasets, and — for the item-scoped case —
//! > rows belonging to *surviving* files in the same run.
//!
//! So the reference states selection positively, defines "outside" as the plain
//! logical negation of it (`!selected`), and answers exclusivity with an O(n²)
//! scan. It never mentions `NULL`, `NOT EXISTS`, three-valued logic or `IN`
//! lists — those are the SQL's problem, and the point of the test is that the
//! SQL has to arrive at the same answer as a formulation that has never heard
//! of them.
//!
//! Thousands of randomly generated row sets and scopes are then run through
//! both. The pools of ids are deliberately *tiny* (3 datasets, 3 runs + NULL,
//! 4 data items, 5 slugs) so that collisions — the same slug claimed from
//! several runs/datasets/files, which is the only interesting case — happen
//! constantly rather than never.
//!
//! Runs on in-memory SQLite (the predicate is standard SQL, identical on
//! Postgres).

// ── Floor guard ────────────────────────────────────────────────────────────
// The suite is behind `sqlite` because it drives a real in-memory SQLite, and
// `cognee-database` declares no default features: built without it this target
// compiles down to nothing and libtest prints `running 0 tests … ok`. That is
// the silent no-op `scripts/ci/assert_pg_suite_ran.sh` exists to make
// impossible, and it is how this file spent its first life — `cargo test -p
// cognee-database --test provenance_run_scope_oracle` reported green while
// running zero cases. `cargo test --workspace` (what CI runs) turns the feature
// on by unification, so the healthy path is unaffected; the one case below
// keeps the unhealthy one loud.
#[cfg(not(feature = "sqlite"))]
#[test]
fn suite_compiled_to_zero_cases_without_the_sqlite_feature() {
    panic!(
        "provenance_run_scope_oracle needs cognee-database's `sqlite` \
         feature and was built without it, so every case below was compiled \
         out. Run it as part of the workspace (`cargo test --workspace`, which \
         unifies the feature in) or pass `--features sqlite`."
    );
}

#[cfg(feature = "sqlite")]
mod oracle {
    use std::collections::BTreeSet;

    use chrono::Utc;
    use cognee_database::ops::datasets::create_dataset;
    use cognee_database::ops::graph_storage::{
        RunScope, delete_edges_for_run, delete_nodes_for_run, get_data_ids_for_run,
        get_edges_for_run, get_nodes_for_run, get_relationship_names_claimed_outside_run,
        get_unique_edges_for_run, get_unique_nodes_for_run, upsert_edges, upsert_nodes,
    };
    use cognee_database::{DatabaseConnection, GraphEdge, GraphNode, connect, initialize};
    use cognee_models::Dataset;
    use sea_orm::{ConnectionTrait, Statement};
    use serde_json::json;
    use uuid::Uuid;

    // ---------------------------------------------------------------------------
    // The reference implementation — derived from the specification, not the SQL
    // ---------------------------------------------------------------------------

    /// The subset of an ownership row the predicate actually looks at.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Row {
        id: Uuid,
        /// The graph store's real artifact id — the thing exclusivity is about.
        slug: Uuid,
        data: Uuid,
        dataset: Uuid,
        /// `None` = written before run ownership existed.
        run: Option<Uuid>,
    }

    /// What a sweep was asked to cover.
    #[derive(Debug, Clone)]
    struct Scope {
        run: Uuid,
        dataset: Uuid,
        /// `None` = the whole run; `Some(v)` = narrowed to those files (`Some(vec![])`
        /// narrows to no file at all).
        data: Option<Vec<Uuid>>,
    }

    /// "the ownership rows for **this run** in **this dataset**, optionally narrowed
    /// to **these files**" — stated positively, exactly as the plan states it.
    fn is_selected(row: &Row, scope: &Scope) -> bool {
        let run_matches = match row.run {
            // A row that predates ownership tracking belongs to no run, so it is
            // not one of "this run's" rows.
            None => false,
            Some(r) => r == scope.run,
        };
        let dataset_matches = row.dataset == scope.dataset;
        let data_matches = match &scope.data {
            None => true,
            Some(narrowing) => narrowing.contains(&row.data),
        };
        run_matches && dataset_matches && data_matches
    }

    /// "a row *outside* the selection" — the plain negation. Nothing more.
    fn is_outside(row: &Row, scope: &Scope) -> bool {
        !is_selected(row, scope)
    }

    /// The rows the sweep owns.
    fn reference_selection(rows: &[Row], scope: &Scope) -> BTreeSet<Uuid> {
        rows.iter()
            .filter(|r| is_selected(r, scope))
            .map(|r| r.id)
            .collect()
    }

    /// The selected rows whose identity is claimed by nothing outside the selection.
    ///
    /// Deliberately O(n²) and deliberately dumb: for each selected row, walk every
    /// row in the table and look for an outside one wearing the same slug.
    fn reference_exclusive(rows: &[Row], scope: &Scope) -> BTreeSet<Uuid> {
        let mut out = BTreeSet::new();
        for candidate in rows {
            if !is_selected(candidate, scope) {
                continue;
            }
            let mut claimed_outside = false;
            for other in rows {
                if is_outside(other, scope) && other.slug == candidate.slug {
                    claimed_outside = true;
                    break;
                }
            }
            if !claimed_outside {
                out.insert(candidate.id);
            }
        }
        out
    }

    /// The distinct files the selection touches — whose completion markers a sweep
    /// has to clear. No exclusivity: a file whose every artifact survives still had
    /// its work rolled back.
    fn reference_data_ids(nodes: &[Row], edges: &[Row], scope: &Scope) -> BTreeSet<Uuid> {
        nodes
            .iter()
            .chain(edges)
            .filter(|r| is_selected(r, scope))
            .map(|r| r.data)
            .collect()
    }

    /// The relationship names still claimed by edge rows outside the selection.
    /// Same negation, applied to the whole edge table rather than per candidate.
    fn reference_names_outside(edges: &[Row], names: &[String], scope: &Scope) -> BTreeSet<String> {
        edges
            .iter()
            .zip(names)
            .filter(|(r, _)| is_outside(r, scope))
            .map(|(_, n)| n.clone())
            .collect()
    }

    // ---------------------------------------------------------------------------
    // A tiny deterministic PRNG (no new dependency; reproducible failures)
    // ---------------------------------------------------------------------------

    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed | 1)
        }
        fn next_u64(&mut self) -> u64 {
            // xorshift64*
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next_u64() % n as u64) as usize
        }
        /// True with probability `num`/`den`.
        fn chance(&mut self, num: u64, den: u64) -> bool {
            self.next_u64() % den < num
        }
        fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
            xs[self.below(xs.len())]
        }
    }

    // ---------------------------------------------------------------------------
    // The world the random cases are drawn from
    // ---------------------------------------------------------------------------

    struct World {
        db: DatabaseConnection,
        user: Uuid,
        /// Datasets that exist in the DB and can hold rows.
        datasets: Vec<Uuid>,
        /// Run ids that rows may carry (`None` = pre-ownership row).
        runs: Vec<Uuid>,
        data: Vec<Uuid>,
        slugs: Vec<Uuid>,
        rel_names: Vec<String>,
        /// A run id no row ever carries, and a dataset id no row ever carries.
        absent_run: Uuid,
        absent_data: Uuid,
    }

    impl World {
        async fn new() -> Self {
            let db = connect("sqlite::memory:").await.expect("connect");
            initialize(&db).await.expect("migrate");
            let user = Uuid::new_v4();
            let datasets: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
            for (i, id) in datasets.iter().enumerate() {
                create_dataset(&db, Dataset::new(format!("ds{i}"), user, None, *id))
                    .await
                    .expect("dataset");
            }
            Self {
                db,
                user,
                datasets,
                runs: (0..3).map(|_| Uuid::new_v4()).collect(),
                data: (0..4).map(|_| Uuid::new_v4()).collect(),
                slugs: (0..5).map(|_| Uuid::new_v4()).collect(),
                rel_names: vec!["is_a".into(), "contains".into(), "mentions".into()],
                absent_run: Uuid::new_v4(),
                absent_data: Uuid::new_v4(),
            }
        }

        async fn clear(&self) {
            self.db
                .execute_unprepared("DELETE FROM nodes; DELETE FROM edges;")
                .await
                .expect("clear tables");
        }

        /// A fresh row, or — a third of the time — a deliberate *twin* of one
        /// already generated: same slug, same run, same dataset, different file.
        ///
        /// Uniform random rows make the most important clause of §2.4 ("rows
        /// belonging to *surviving* files in the same run") vanishingly rare, and a
        /// corpus that never contains a case cannot test it. The coverage
        /// assertions at the end of the test are what caught that.
        fn random_row(&self, rng: &mut Rng, existing: &[Row]) -> Row {
            if !existing.is_empty() && rng.chance(1, 3) {
                let base = existing[rng.below(existing.len())];
                let mut data = rng.pick(&self.data);
                if data == base.data {
                    // Force a different file so the twin is a *sibling*, not a
                    // duplicate.
                    data = self.data[(self.data.iter().position(|d| *d == data).unwrap_or(0) + 1)
                        % self.data.len()];
                }
                return Row {
                    id: Uuid::new_v4(),
                    slug: base.slug,
                    data,
                    dataset: base.dataset,
                    run: base.run,
                };
            }
            Row {
                id: Uuid::new_v4(),
                slug: rng.pick(&self.slugs),
                data: rng.pick(&self.data),
                // Biased towards one dataset/run so that collisions inside a single
                // (run, dataset) are common rather than a curiosity.
                dataset: if rng.chance(3, 5) {
                    self.datasets[0]
                } else {
                    rng.pick(&self.datasets)
                },
                // ~1 row in 5 predates ownership tracking.
                run: if rng.chance(1, 5) {
                    None
                } else if rng.chance(1, 2) {
                    Some(self.runs[0])
                } else {
                    Some(rng.pick(&self.runs))
                },
            }
        }

        fn random_scope(&self, rng: &mut Rng) -> Scope {
            let run = if rng.chance(1, 12) {
                self.absent_run
            } else {
                rng.pick(&self.runs)
            };
            let dataset = if rng.chance(3, 5) {
                self.datasets[0]
            } else {
                rng.pick(&self.datasets)
            };
            let data = match rng.below(6) {
                // whole run
                0 | 1 => None,
                // narrowed to nothing at all
                2 => Some(Vec::new()),
                // narrowed to every file that exists
                3 => Some(self.data.clone()),
                // narrowed to a random non-empty subset, sometimes including a file
                // no row carries
                _ => {
                    let mut picked: Vec<Uuid> = self
                        .data
                        .iter()
                        .copied()
                        .filter(|_| rng.chance(1, 2))
                        .collect();
                    if rng.chance(1, 6) {
                        picked.push(self.absent_data);
                    }
                    Some(picked)
                }
            };
            Scope { run, dataset, data }
        }

        fn node_of(&self, row: &Row) -> GraphNode {
            GraphNode {
                id: row.id,
                slug: row.slug,
                user_id: self.user,
                data_id: row.data,
                dataset_id: row.dataset,
                pipeline_run_id: row.run,
                label: Some("n".into()),
                node_type: "Entity".into(),
                indexed_fields: json!({ "index_fields": ["name"] }),
                attributes: None,
                created_at: Utc::now(),
            }
        }

        fn edge_of(&self, row: &Row, name: &str) -> GraphEdge {
            GraphEdge {
                id: row.id,
                slug: row.slug,
                user_id: self.user,
                data_id: row.data,
                dataset_id: row.dataset,
                pipeline_run_id: row.run,
                source_node_id: Uuid::new_v4(),
                destination_node_id: Uuid::new_v4(),
                relationship_name: name.to_string(),
                label: None,
                attributes: None,
                created_at: Utc::now(),
            }
        }
    }

    fn scope_of<'a>(scope: &'a Scope) -> RunScope<'a> {
        match &scope.data {
            None => RunScope::whole_run(scope.run, scope.dataset),
            Some(ids) => RunScope::for_data(scope.run, scope.dataset, ids),
        }
    }

    fn ids_of_nodes(rows: &[GraphNode]) -> BTreeSet<Uuid> {
        rows.iter().map(|r| r.id).collect()
    }

    fn ids_of_edges(rows: &[GraphEdge]) -> BTreeSet<Uuid> {
        rows.iter().map(|r| r.id).collect()
    }

    // ---------------------------------------------------------------------------
    // The differential test
    // ---------------------------------------------------------------------------

    /// Number of random (rows, scope) cases. Each is a full round trip through
    /// SQLite, so this is the knob to turn if the test gets slow.
    const CASES: usize = 20_000;

    #[tokio::test]
    async fn sql_predicate_agrees_with_naive_reference_on_random_cases() {
        let world = World::new().await;
        let mut rng = Rng::new(0x5EED_1234_ABCD_0001);

        // Coverage counters. A differential test that never generated an
        // interesting case would pass while proving nothing, so the shape of the
        // corpus is asserted at the end.
        let mut cov_nonempty_selection = 0usize;
        let mut cov_exclusivity_bit = 0usize; // a selected row was claimed outside
        let mut cov_null_run_claimed = 0usize; // and the claimant was a NULL-run row
        let mut cov_sibling_file_claimed = 0usize; // ...a kept file in the same run
        let mut cov_other_dataset_claimed = 0usize;
        let mut cov_empty_narrowing = 0usize;
        let mut cov_full_narrowing = 0usize;

        for case in 0..CASES {
            world.clear().await;

            let n_nodes = rng.below(9);
            let n_edges = rng.below(9);
            let mut node_rows: Vec<Row> = Vec::with_capacity(n_nodes);
            for _ in 0..n_nodes {
                let r = world.random_row(&mut rng, &node_rows);
                node_rows.push(r);
            }
            let mut edge_rows: Vec<Row> = Vec::with_capacity(n_edges);
            for _ in 0..n_edges {
                let r = world.random_row(&mut rng, &edge_rows);
                edge_rows.push(r);
            }
            let edge_names: Vec<String> = (0..n_edges)
                .map(|_| rng.pick(&[0usize, 1, 2]))
                .map(|i| world.rel_names[i].clone())
                .collect();

            let nodes: Vec<GraphNode> = node_rows.iter().map(|r| world.node_of(r)).collect();
            let edges: Vec<GraphEdge> = edge_rows
                .iter()
                .zip(&edge_names)
                .map(|(r, n)| world.edge_of(r, n))
                .collect();
            upsert_nodes(&world.db, &nodes).await.expect("upsert nodes");
            upsert_edges(&world.db, &edges).await.expect("upsert edges");

            let scope = world.random_scope(&mut rng);
            let rs = scope_of(&scope);

            {
                let sel = reference_selection(&node_rows, &scope);
                let exc = reference_exclusive(&node_rows, &scope);
                if !sel.is_empty() {
                    cov_nonempty_selection += 1;
                }
                if sel.len() > exc.len() {
                    cov_exclusivity_bit += 1;
                    let blocked: Vec<&Row> = node_rows
                        .iter()
                        .filter(|r| sel.contains(&r.id) && !exc.contains(&r.id))
                        .collect();
                    for b in &blocked {
                        for other in &node_rows {
                            if !is_outside(other, &scope) || other.slug != b.slug {
                                continue;
                            }
                            if other.run.is_none() {
                                cov_null_run_claimed += 1;
                            }
                            if other.run == Some(scope.run) && other.dataset == scope.dataset {
                                cov_sibling_file_claimed += 1;
                            }
                            if other.dataset != scope.dataset {
                                cov_other_dataset_claimed += 1;
                            }
                        }
                    }
                }
                match &scope.data {
                    Some(v) if v.is_empty() => cov_empty_narrowing += 1,
                    Some(v) if v.len() >= world.data.len() => cov_full_narrowing += 1,
                    _ => {}
                }
            }
            let ctx = || {
                format!(
                    "case {case}\n  scope = {scope:?}\n  node rows = {node_rows:#?}\n  edge rows = {edge_rows:#?}"
                )
            };

            // --- selection ---------------------------------------------------
            let sql_nodes = ids_of_nodes(&get_nodes_for_run(&world.db, &rs).await.expect("nodes"));
            assert_eq!(
                sql_nodes,
                reference_selection(&node_rows, &scope),
                "node selection diverged\n{}",
                ctx()
            );
            let sql_edges = ids_of_edges(&get_edges_for_run(&world.db, &rs).await.expect("edges"));
            assert_eq!(
                sql_edges,
                reference_selection(&edge_rows, &scope),
                "edge selection diverged\n{}",
                ctx()
            );

            // --- exclusivity -------------------------------------------------
            let sql_unique_nodes = ids_of_nodes(
                &get_unique_nodes_for_run(&world.db, &rs)
                    .await
                    .expect("unique nodes"),
            );
            assert_eq!(
                sql_unique_nodes,
                reference_exclusive(&node_rows, &scope),
                "node exclusivity diverged\n{}",
                ctx()
            );
            let sql_unique_edges = ids_of_edges(
                &get_unique_edges_for_run(&world.db, &rs)
                    .await
                    .expect("unique edges"),
            );
            assert_eq!(
                sql_unique_edges,
                reference_exclusive(&edge_rows, &scope),
                "edge exclusivity diverged\n{}",
                ctx()
            );

            // --- the affected files ------------------------------------------
            let sql_data: BTreeSet<Uuid> = get_data_ids_for_run(&world.db, &rs)
                .await
                .expect("data ids")
                .into_iter()
                .collect();
            assert_eq!(
                sql_data,
                reference_data_ids(&node_rows, &edge_rows, &scope),
                "affected data ids diverged\n{}",
                ctx()
            );

            // --- the second identity: relationship names ----------------------
            let sql_names: BTreeSet<String> =
                get_relationship_names_claimed_outside_run(&world.db, &rs)
                    .await
                    .expect("names")
                    .into_iter()
                    .collect();
            assert_eq!(
                sql_names,
                reference_names_outside(&edge_rows, &edge_names, &scope),
                "relationship names claimed outside diverged\n{}",
                ctx()
            );

            // --- deletion ------------------------------------------------------
            // Deletion is the selection with no exclusivity check, so the same
            // reference answers it — as a count, and as what is left behind.
            let expected_del_nodes = reference_selection(&node_rows, &scope);
            let expected_del_edges = reference_selection(&edge_rows, &scope);
            let del_nodes = delete_nodes_for_run(&world.db, &rs).await.expect("del n");
            let del_edges = delete_edges_for_run(&world.db, &rs).await.expect("del e");
            assert_eq!(
                del_nodes as usize,
                expected_del_nodes.len(),
                "node delete count diverged\n{}",
                ctx()
            );
            assert_eq!(
                del_edges as usize,
                expected_del_edges.len(),
                "edge delete count diverged\n{}",
                ctx()
            );
            // Nothing selected may survive, and nothing unselected may go.
            let remaining_nodes = remaining_ids(&world.db, "nodes").await;
            let remaining_edges = remaining_ids(&world.db, "edges").await;
            let expect_left_n: BTreeSet<Uuid> = node_rows
                .iter()
                .filter(|r| !expected_del_nodes.contains(&r.id))
                .map(|r| r.id)
                .collect();
            let expect_left_e: BTreeSet<Uuid> = edge_rows
                .iter()
                .filter(|r| !expected_del_edges.contains(&r.id))
                .map(|r| r.id)
                .collect();
            assert_eq!(
                remaining_nodes,
                expect_left_n,
                "surviving node rows diverged\n{}",
                ctx()
            );
            assert_eq!(
                remaining_edges,
                expect_left_e,
                "surviving edge rows diverged\n{}",
                ctx()
            );
        }

        // The corpus has to have actually contained the situations the predicate
        // exists for; otherwise "they agree" is a statement about two empty sets.
        println!(
            "coverage: non-empty selections={cov_nonempty_selection}, \
             exclusivity blocked={cov_exclusivity_bit}, \
             blocked by NULL-run row={cov_null_run_claimed}, \
             blocked by sibling file in same run={cov_sibling_file_claimed}, \
             blocked by other dataset={cov_other_dataset_claimed}, \
             empty narrowing={cov_empty_narrowing}, full narrowing={cov_full_narrowing}"
        );
        assert!(
            cov_nonempty_selection > CASES / 10,
            "corpus barely selected anything: {cov_nonempty_selection}"
        );
        assert!(
            cov_exclusivity_bit > CASES / 20,
            "exclusivity almost never bit: {cov_exclusivity_bit}"
        );
        assert!(
            cov_null_run_claimed > 100,
            "NULL-run claimants too rare: {cov_null_run_claimed}"
        );
        assert!(
            cov_sibling_file_claimed > 100,
            "same-run sibling-file claimants too rare: {cov_sibling_file_claimed}"
        );
        assert!(
            cov_other_dataset_claimed > 100,
            "cross-dataset claimants too rare: {cov_other_dataset_claimed}"
        );
        assert!(
            cov_empty_narrowing > 100,
            "empty narrowing too rare: {cov_empty_narrowing}"
        );
        assert!(
            cov_full_narrowing > 100,
            "full narrowing too rare: {cov_full_narrowing}"
        );
    }

    /// Every id still in `table`, read with raw SQL so the check does not lean on
    /// the very predicate under test.
    async fn remaining_ids(db: &DatabaseConnection, table: &str) -> BTreeSet<Uuid> {
        let stmt =
            Statement::from_string(db.get_database_backend(), format!("SELECT id FROM {table}"));
        db.query_all(stmt)
            .await
            .expect("select ids")
            .into_iter()
            .map(|row| {
                let hex: String = row.try_get_by_index(0).expect("id column");
                Uuid::parse_str(&hex).expect("id is a uuid")
            })
            .collect()
    }
}
