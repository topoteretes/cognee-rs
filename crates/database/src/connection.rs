use sea_orm::{Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

use crate::migrator::Migrator;
use crate::types::DatabaseError;

/// Open a connection to the relational database.
pub async fn connect(url: &str) -> Result<DatabaseConnection, DatabaseError> {
    Database::connect(url)
        .await
        .map_err(|e| DatabaseError::ConnectionError(e.to_string()))
}

/// Run all pending migrations on an existing connection.
pub async fn initialize(db: &DatabaseConnection) -> Result<(), DatabaseError> {
    Migrator::up(db, None)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))
}
