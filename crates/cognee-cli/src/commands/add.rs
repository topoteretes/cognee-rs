use std::path::PathBuf;
use std::sync::Arc;

use cognee_lib::add::IngestPipeline;
use cognee_lib::database::{DatabaseTrait, SqliteDatabase};
use cognee_lib::models::DataInput;
use cognee_lib::storage::{LocalStorage, StorageTrait};
use uuid::Uuid;

use crate::cli::AddArgs;
use crate::config_store::load_config;
use crate::error::CliError;

pub fn run(args: AddArgs) -> Result<(), CliError> {
    let config = load_config()?;

    let storage_path = PathBuf::from(config.settings.data_root_directory);
    let database_url = config.settings.relational_db_url;
    let owner_id = Uuid::parse_str(&config.settings.default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            config.settings.default_user_id
        ))
    })?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::Runtime(format!("Failed to create async runtime: {error}")))?;

    runtime.block_on(async {
        let storage = Arc::new(LocalStorage::new(storage_path));
        storage.initialize().await.map_err(|error| {
            CliError::Runtime(format!("Storage initialization failed: {error}"))
        })?;

        let database = Arc::new(SqliteDatabase::new(&database_url).await.map_err(|error| {
            CliError::Runtime(format!("Database initialization failed: {error}"))
        })?);
        database.initialize().await.map_err(|error| {
            CliError::Runtime(format!("Database schema initialization failed: {error}"))
        })?;

        let pipeline = IngestPipeline::new(Arc::clone(&storage), Arc::clone(&database));

        let inputs = args
            .data
            .into_iter()
            .map(DataInput::from_string)
            .collect::<Vec<_>>();

        let results = pipeline
            .add(inputs, &args.dataset_name, owner_id)
            .await
            .map_err(|error| CliError::Runtime(format!("Add operation failed: {error}")))?;

        println!(
            "Success: Added {} item(s) to dataset '{}'.",
            results.len(),
            args.dataset_name
        );

        Ok(())
    })
}
