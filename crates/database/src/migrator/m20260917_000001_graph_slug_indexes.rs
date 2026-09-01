//! Adds a standalone `slug` index to `nodes` and to `edges` — the index the
//! run-scoped rollback sweep's exclusivity check needs.
//!
//! `get_unique_nodes_for_run` / `get_unique_edges_for_run` in
//! `ops::graph_storage` answer "is this artifact's slug claimed by any row
//! outside the scope being swept" with a correlated `NOT EXISTS` whose only
//! join key is `slug`:
//!
//! ```sql
//! NOT EXISTS (SELECT 1 FROM nodes AS n2
//!             WHERE n2.slug = nodes.slug AND <outside the scope>)
//! ```
//!
//! Before this migration neither table could serve that correlation:
//!
//! * `edges` had no `slug` index at all (baseline creates only
//!   `idx_edges_data_id` and `idx_edges_dataset_id`).
//! * `nodes` had `idx_nodes_dataset_slug` on `(dataset_id, slug)`, where
//!   `slug` is the *second* column — a B-tree on `(a, b)` is only usable for
//!   predicates that constrain `a`, and the subquery constrains nothing but
//!   `slug`, so the planner cannot use it as a prefix.
//!
//! So every candidate row re-scanned the whole table: SQLite planned the
//! subquery as `SCAN n2`, making the sweep O(n²) in the run's row count. A
//! 40 000-row run spent 52.8 s in a single exclusivity call with a user waiting
//! on a failed cognify's rollback. With these indexes the plan becomes
//! `SEARCH n2 USING INDEX ix_nodes_slug (slug=?)` and the same call takes
//! milliseconds.
//!
//! ## Postgres plans it differently, and still wants the index
//!
//! Postgres does not evaluate the subquery per row the way SQLite does: it
//! de-correlates the `NOT EXISTS` into a `Hash Right Anti Join`, one sequential
//! scan of the table hashed against the candidate set — already O(n), which is
//! why the pathology above is SQLite-only. But that plan is chosen on
//! cardinality, and it flips: measured on a 40 000-row `nodes` table, a *small*
//! run's rollback (the common case — a cognify that failed early, against a
//! large existing store) is planned as a `Nested Loop Anti Join` whose inner
//! side is `Index Scan using ix_nodes_slug (slug = nodes.slug)`. Without the
//! index that shape is unavailable and Postgres falls back to seq-scanning the
//! whole table to roll back five rows. So the index earns its keep on both
//! backends, for different plans.
//!
//! ## Why `nodes` gets a *second* index rather than a reordered one
//!
//! The obvious alternative — reorder `idx_nodes_dataset_slug` to
//! `(slug, dataset_id)`, which would serve both the correlation and any
//! `(dataset_id, slug)` lookup's slug half — is rejected on parity grounds.
//! Python declares that composite in `cognee/modules/graph/models/Node.py`
//! (`Index("index_node_dataset_slug", "dataset_id", "slug")`) and creates it in
//! alembic revision `84e5d08260d6`. Rust and Python share one database and
//! either may migrate it first, so a column order only Rust knows about would
//! be silently un-recreated — or contradicted — by the Python side. Dropping
//! the composite outright has the same problem *and* would remove the only
//! index serving a `dataset_id`-scoped slug lookup, which Python is free to
//! issue even though no Rust query currently does.
//!
//! Adding is reversible, dialect-neutral and costs one extra B-tree write per
//! node insert; reordering is neither reversible in a shared schema nor
//! verifiable from this side. So: leave the composite alone, add a standalone
//! `slug` index next to it.
//!
//! ## Index names
//!
//! Unlike `m20260916_000001`, there is no Python name to copy: Python indexes
//! `slug` on neither table (`Edge.py` has no `__table_args__` at all, and
//! `Node.py` has only the composite). We are therefore adding two indexes
//! Python lacks — purely additive, and harmless to a Python reader of the same
//! file. The names follow SQLAlchemy's own default for a single-column index,
//! `ix_<table>_<column>`, which is what Python would generate if `slug` ever
//! grows an `index=True`; the guards below then make whichever SDK runs second
//! skip instead of creating a duplicate. (`m20260916`'s
//! `ix_nodes_pipeline_run_id` is the same shape, for the same reason.)
use sea_orm::DbBackend;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const NODES_INDEX: &str = "ix_nodes_slug";
const EDGES_INDEX: &str = "ix_edges_slug";

/// Reject backends whose dialect cannot express the guards below.
///
/// The DDL here is otherwise dialect-neutral — a plain single-column B-tree,
/// which SQLite and Postgres spell identically. The one dialect-sensitive part
/// is the idempotency guard: both `CREATE INDEX IF NOT EXISTS` (SQLite; Postgres
/// 9.5+) and `DROP INDEX IF EXISTS` (both) are supported on the two backends the
/// migrator targets, and on neither by MySQL. Fail loudly rather than emit DDL
/// that would parse differently.
fn check_backend(manager: &SchemaManager) -> Result<(), DbErr> {
    match manager.get_database_backend() {
        DbBackend::Sqlite | DbBackend::Postgres => Ok(()),
        other => Err(DbErr::Custom(format!(
            "graph slug index migration does not support the {other:?} backend"
        ))),
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        check_backend(manager)?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(NODES_INDEX)
                    .table(Nodes::Table)
                    .col(Nodes::Slug)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(EDGES_INDEX)
                    .table(Edges::Table)
                    .col(Edges::Slug)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        check_backend(manager)?;

        // Reverse order of `up`. Only the indexes this migration created are
        // dropped — `idx_nodes_dataset_slug` is the baseline's and stays.
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(EDGES_INDEX)
                    .table(Edges::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(NODES_INDEX)
                    .table(Nodes::Table)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Nodes {
    Table,
    Slug,
}

#[derive(DeriveIden)]
enum Edges {
    Table,
    Slug,
}
