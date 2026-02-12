mod database_trait;
mod sqlite_database;

#[cfg(any(test, feature = "testing"))]
mod mock_database;

pub use database_trait::{DatabaseError, DatabaseTrait};
pub use sqlite_database::SqliteDatabase;

#[cfg(any(test, feature = "testing"))]
pub use mock_database::MockDatabase;
