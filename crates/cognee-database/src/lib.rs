mod database_trait;
mod sqlite_database;

#[cfg(feature = "testing")]
mod mock_database;

pub use database_trait::{
    DatabaseError, DatabaseTrait, SearchHistoryEntry, SearchHistoryEntryType,
};
pub use sqlite_database::SqliteDatabase;

#[cfg(feature = "testing")]
pub use mock_database::MockDatabase;
