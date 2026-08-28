use std::path::PathBuf;
use std::sync::Arc;

use cognee::migration::{ARCHIVE_SUFFIX, ExportOptions, export_graph, pack_archive};
use cognee::{ComponentManager, PipelineContext};
use tracing::info;

use crate::cli::ExportArgs;
use crate::error::CliError;

/// Write the knowledge graph as a COGX archive.
pub fn run(args: ExportArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    if args.format != "cogx" {
        return Err(CliError::Validation(format!(
            "Unsupported export format '{}'. Only 'cogx' is supported — it is the \
             one format Python cognee can re-import.",
            args.format
        )));
    }

    let dataset = args.dataset.clone();
    let destination: PathBuf = match args.output.as_deref() {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(format!("{}_cogx", dataset.as_deref().unwrap_or("cognee"))),
    };
    let pack = args.pack;

    crate::teardown::run_command(Arc::clone(&cm), async move {
        // Scoped so the settings read guard is dropped before the awaits
        // below (clippy::await_holding_lock).
        let embedding_model = cm.settings().embedding_model_name.clone();

        let graph_db = cm
            .graph_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        let options = ExportOptions {
            embedding_model: Some(embedding_model),
            dataset_name: dataset,
        };

        let summary = export_graph(&*graph_db, &destination, &options)
            .await
            .map_err(|error| CliError::Runtime(format!("COGX export failed: {error}")))?;

        info!(
            nodes = summary.num_nodes,
            edges = summary.num_edges,
            "COGX archive written to {}",
            destination.display()
        );

        if pack {
            let tar_path = destination.with_file_name(format!(
                "{}{ARCHIVE_SUFFIX}",
                destination
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "cognee".to_string())
            ));
            let packed = pack_archive(&destination, tar_path)
                .map_err(|error| CliError::Runtime(format!("COGX packing failed: {error}")))?;
            println!("{}", packed.display());
        } else {
            println!("{}", destination.display());
        }

        Ok(())
    })
}
