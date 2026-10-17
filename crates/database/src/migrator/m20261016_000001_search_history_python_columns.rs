//! Renames `queries.query_text` to `text` and `results.serialized_result` to
//! `value`, the names Python uses, so both SDKs can share search history.
//!
//! A database Python created already has the new names, so each rename is
//! skipped when the old column is absent.
use sea_orm_migration::prelude::*;

use super::helpers::rename_column_if_present;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// `(table, old name, new name)` for each renamed column.
const RENAMES: [(&str, &str, &str); 2] = [
    ("queries", "query_text", "text"),
    ("results", "serialized_result", "value"),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (table, old, new) in RENAMES {
            rename_column_if_present(manager, table, old, new).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (table, old, new) in RENAMES {
            rename_column_if_present(manager, table, new, old).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use sea_orm::Database;
    use sea_orm_migration::prelude::*;

    use super::RENAMES;
    use crate::migrator::{Migrator, helpers::has_column};

    #[tokio::test]
    async fn a_migrated_database_uses_python_column_names() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite always connects");
        Migrator::up(&db, None).await.expect("migrates cleanly");
        let manager = SchemaManager::new(&db);

        for (table, old, new) in RENAMES {
            assert!(has_column(&manager, table, new).await.expect("probe runs"));
            assert!(!has_column(&manager, table, old).await.expect("probe runs"));
        }
    }
}
