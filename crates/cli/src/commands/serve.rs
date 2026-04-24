//! `cognee-cli serve` — connect the SDK to a Cognee instance.
//!
//! Dispatches to [`cognee_lib::serve`] with a [`ServeConfig`] built from the
//! CLI arguments. Direct mode is selected when `--url` is present; cloud
//! mode runs the full Auth0 device-code flow otherwise.

use std::sync::Arc;

use cognee_lib::{ComponentManager, ServeConfig, serve};
use tracing::info;

use crate::cli::ServeArgs;
use crate::error::CliError;

/// Run the `serve` subcommand.
///
/// Returns `Ok(())` on success. Direct mode is advisory about the `/health`
/// probe — a failing probe logs a warning but still succeeds, mirroring
/// Python's `_serve_direct`.
pub fn run(args: ServeArgs, _cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::Runtime(format!("Failed to create async runtime: {error}")))?;

    runtime.block_on(async move {
        let config = build_config(args);
        let client = serve(config)
            .await
            .map_err(|error| CliError::Runtime(format!("Failed to connect to Cognee: {error}")))?;
        info!(
            target: "cognee_cli::serve",
            service_url = %client.service_url,
            "serve: connected"
        );
        println!("Connected to Cognee at {}", client.service_url);
        Ok(())
    })
}

fn build_config(args: ServeArgs) -> ServeConfig {
    match args.url {
        Some(url) => {
            let mut cfg = ServeConfig::direct(url);
            if let Some(key) = args.api_key {
                cfg = cfg.api_key(key);
            }
            // Auth0 / cloud_url overrides are ignored in direct mode (Python parity).
            cfg
        }
        None => {
            let mut cfg = ServeConfig::cloud();
            if let Some(key) = args.api_key {
                cfg = cfg.api_key(key);
            }
            if let Some(domain) = args.auth0_domain {
                cfg = cfg.auth0_domain(domain);
            }
            if let Some(client_id) = args.auth0_client_id {
                cfg = cfg.auth0_client_id(client_id);
            }
            if let Some(audience) = args.auth0_audience {
                cfg = cfg.auth0_audience(audience);
            }
            if let Some(url) = args.cloud_url {
                cfg = cfg.cloud_url(url);
            }
            cfg
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_config_direct_when_url_set() {
        let args = ServeArgs {
            url: Some("https://example.com".into()),
            api_key: Some("k".into()),
            auth0_domain: None,
            auth0_client_id: None,
            auth0_audience: None,
            cloud_url: None,
        };
        let cfg = build_config(args);
        assert_eq!(cfg.url.as_deref(), Some("https://example.com"));
        assert_eq!(cfg.api_key.as_deref(), Some("k"));
    }

    #[test]
    fn build_config_cloud_when_url_absent() {
        let args = ServeArgs {
            url: None,
            api_key: None,
            auth0_domain: Some("d".into()),
            auth0_client_id: Some("c".into()),
            auth0_audience: Some("a".into()),
            cloud_url: Some("https://mgmt.example".into()),
        };
        let cfg = build_config(args);
        assert!(cfg.url.is_none());
        assert_eq!(cfg.auth0_domain.as_deref(), Some("d"));
        assert_eq!(cfg.auth0_client_id.as_deref(), Some("c"));
        assert_eq!(cfg.auth0_audience.as_deref(), Some("a"));
        assert_eq!(cfg.cloud_url.as_deref(), Some("https://mgmt.example"));
    }
}
