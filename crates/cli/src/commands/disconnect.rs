//! `cognee-cli disconnect` — tear down the remote cloud client.
//!
//! Thin wrapper around [`cognee_lib::disconnect`]. If `--wipe-credentials`
//! is passed, the cached credentials file at
//! `~/.cognee/cloud_credentials.json` is deleted as well.

use std::sync::Arc;

use cognee_lib::{ComponentManager, disconnect};
use tracing::info;

use crate::cli::DisconnectArgs;
use crate::error::CliError;

/// Run the `disconnect` subcommand.
///
/// Succeeds even when there is no active client or no on-disk credentials —
/// matches Python's idempotent `disconnect()`.
pub fn run(args: DisconnectArgs, _cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::Runtime(format!("Failed to create async runtime: {error}")))?;

    runtime.block_on(async move {
        disconnect(args.wipe_credentials).await.map_err(|error| {
            CliError::Runtime(format!("Failed to disconnect from Cognee: {error}"))
        })?;
        info!(
            target: "cognee_cli::disconnect",
            wipe_credentials = args.wipe_credentials,
            "disconnect: complete"
        );
        Ok(())
    })
}
