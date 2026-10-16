use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sea_orm_migration::prelude::*;

/// Does `table` already have `column`?
///
/// `SchemaManager::has_column` is unusable here: it dispatches on
/// *sea-orm-migration*'s own `sqlx-*` features, which this crate does not
/// enable (it only turns on `sea-orm/sqlx-sqlite` / `sea-orm/sqlx-postgres`),
/// so every arm is compiled out and the call panics with "feature is off".
pub(super) async fn has_column(
    manager: &SchemaManager<'_>,
    table: &str,
    column: &str,
) -> Result<bool, DbErr> {
    let backend = manager.get_database_backend();
    let sql = match backend {
        DbBackend::Sqlite => {
            "SELECT COUNT(*) AS cnt FROM pragma_table_info(?) WHERE name = ?".to_string()
        }
        DbBackend::Postgres => "SELECT COUNT(*) AS cnt FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = $1 AND column_name = $2"
            .to_string(),
        other => {
            return Err(DbErr::Custom(format!(
                "column-existence probe does not support the {other:?} backend"
            )));
        }
    };
    let row = manager
        .get_connection()
        .query_one(Statement::from_sql_and_values(
            backend,
            sql,
            [table.into(), column.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("column-existence probe returned no row".to_owned()))?;
    Ok(row.try_get::<i64>("", "cnt")? > 0)
}

/// Add `column` to `table` unless it already exists.
pub(super) async fn add_column_if_missing(
    manager: &SchemaManager<'_>,
    table: &str,
    mut column: ColumnDef,
) -> Result<(), DbErr> {
    if has_column(manager, table, &column.get_column_name()).await? {
        return Ok(());
    }
    let alter = Table::alter()
        .table(Alias::new(table))
        .add_column(&mut column)
        .to_owned();
    manager.alter_table(alter).await
}

/// Drop `column` from `table` if it exists.
pub(super) async fn drop_column_if_present(
    manager: &SchemaManager<'_>,
    table: &str,
    column: &str,
) -> Result<(), DbErr> {
    if !has_column(manager, table, column).await? {
        return Ok(());
    }
    let alter = Table::alter()
        .table(Alias::new(table))
        .drop_column(Alias::new(column))
        .to_owned();
    manager.alter_table(alter).await
}

/// Rename `from` to `to` in `table` if `from` exists.
pub(super) async fn rename_column_if_present(
    manager: &SchemaManager<'_>,
    table: &str,
    from: &str,
    to: &str,
) -> Result<(), DbErr> {
    if !has_column(manager, table, from).await? {
        return Ok(());
    }
    let alter = Table::alter()
        .table(Alias::new(table))
        .rename_column(Alias::new(from), Alias::new(to))
        .to_owned();
    manager.alter_table(alter).await
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
    use sea_orm_migration::prelude::*;

    use super::{
        add_column_if_missing, drop_column_if_present, has_column, rename_column_if_present,
    };

    /// An in-memory database with one table, `t (id text)`.
    async fn db_with_table() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite always connects");
        let sql = "CREATE TABLE t (id TEXT)".to_string();
        db.execute(Statement::from_string(db.get_database_backend(), sql))
            .await
            .expect("creating the test table must succeed");
        db
    }

    #[tokio::test]
    async fn has_column_tells_present_from_missing() {
        let db = db_with_table().await;
        let manager = SchemaManager::new(&db);

        assert!(has_column(&manager, "t", "id").await.expect("probe runs"));
        assert!(!has_column(&manager, "t", "nope").await.expect("probe runs"));
    }

    #[tokio::test]
    async fn add_column_if_missing_adds_once() {
        let db = db_with_table().await;
        let manager = SchemaManager::new(&db);
        let column = || ColumnDef::new(Alias::new("extra")).text().null().to_owned();

        add_column_if_missing(&manager, "t", column())
            .await
            .expect("adds the column");
        add_column_if_missing(&manager, "t", column())
            .await
            .expect("skips an existing column");

        assert!(
            has_column(&manager, "t", "extra")
                .await
                .expect("probe runs")
        );
    }

    #[tokio::test]
    async fn drop_column_if_present_drops_once() {
        let db = db_with_table().await;
        let manager = SchemaManager::new(&db);
        let column = ColumnDef::new(Alias::new("extra")).text().null().to_owned();
        add_column_if_missing(&manager, "t", column)
            .await
            .expect("adds the column");

        drop_column_if_present(&manager, "t", "extra")
            .await
            .expect("drops the column");
        drop_column_if_present(&manager, "t", "extra")
            .await
            .expect("skips a missing column");

        assert!(
            !has_column(&manager, "t", "extra")
                .await
                .expect("probe runs")
        );
    }

    #[tokio::test]
    async fn rename_column_if_present_renames_once() {
        let db = db_with_table().await;
        let manager = SchemaManager::new(&db);

        rename_column_if_present(&manager, "t", "id", "key")
            .await
            .expect("renames the column");
        rename_column_if_present(&manager, "t", "id", "key")
            .await
            .expect("skips a missing column");

        assert!(!has_column(&manager, "t", "id").await.expect("probe runs"));
        assert!(has_column(&manager, "t", "key").await.expect("probe runs"));
    }
}
