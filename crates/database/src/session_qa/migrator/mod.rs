use sea_orm_migration::prelude::*;

mod m20260914_000001_baseline;
mod m20261031_000001_drop_session_graph_context;

pub struct SessionMigrator;

#[async_trait::async_trait]
impl MigratorTrait for SessionMigrator {
    fn migration_table_name() -> DynIden {
        Alias::new("seaql_session_migrations").into_iden()
    }

    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260914_000001_baseline::Migration),
            Box::new(m20261031_000001_drop_session_graph_context::Migration),
        ]
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable failures"
    )]
    use super::SessionMigrator;
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    use sea_orm_migration::MigratorTrait;

    async fn session_tables(db: &crate::DatabaseConnection) -> Vec<String> {
        db.query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
                .to_string(),
        ))
        .await
        .expect("table query")
        .iter()
        .map(|row| row.try_get::<String>("", "name").expect("name"))
        .collect()
    }

    /// A session database at the baseline revision, holding a graph snapshot
    /// written by the removed sync stage, loses the `session_graph_context`
    /// table on the next migration run; Q&A and trace tables stay. Rolling the
    /// drop back recreates the table the baseline expects.
    #[tokio::test]
    async fn drop_session_graph_context_migration_drops_the_snapshot_table() {
        let db = crate::connect("sqlite::memory:").await.expect("connect");
        SessionMigrator::up(&db, Some(1)).await.expect("baseline");
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "INSERT INTO session_graph_context (id, session_id, user_id, context, updated_at) \
             VALUES ('u:s', 's', 'u', 'old snapshot', '2026-01-01T00:00:00Z')"
                .to_string(),
        ))
        .await
        .expect("seed a legacy snapshot row");

        SessionMigrator::up(&db, None)
            .await
            .expect("drop migration");
        let tables = session_tables(&db).await;
        assert!(
            !tables.iter().any(|t| t == "session_graph_context"),
            "session_graph_context must be dropped: {tables:?}"
        );
        for t in ["session_qa_entries", "session_trace_steps"] {
            assert!(tables.iter().any(|x| x == t), "missing {t}: {tables:?}");
        }
        // Idempotent.
        SessionMigrator::up(&db, None).await.expect("second run");

        SessionMigrator::down(&db, Some(1))
            .await
            .expect("roll back");
        assert!(
            session_tables(&db)
                .await
                .iter()
                .any(|t| t == "session_graph_context"),
            "down must recreate session_graph_context"
        );
    }
}
