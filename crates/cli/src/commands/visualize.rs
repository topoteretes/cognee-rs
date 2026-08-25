use std::path::PathBuf;
use std::sync::Arc;

use cognee::{ComponentManager, PipelineContext, visualize};
use tracing::info;

use crate::cli::VisualizeArgs;
use crate::error::CliError;

pub fn run(args: VisualizeArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    crate::teardown::run_command(Arc::clone(&cm), async {
        let graph_db = cm
            .graph_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        let output: Option<PathBuf> = args.output.as_deref().map(PathBuf::from);
        let path = visualize(&*graph_db, output.as_deref())
            .await
            .map_err(|error| CliError::Runtime(format!("Visualization failed: {error}")))?;

        info!("Graph visualization saved to {}", path.display());
        println!("{}", path.display());
        Ok(())
    })
}
