//! `disconnect()` — tear down the cloud-routed mode and optionally
//! wipe the on-disk credential cache.
//!
//! Line-by-line port of `cognee/api/v1/serve/disconnect.py`.

use crate::credentials;
use crate::error::CloudResult;
use crate::state::{clear_client, get_client};

/// Disconnect from Cognee Cloud and revert to local-execution mode.
///
/// After this returns:
/// - [`crate::state::is_connected`] reports `false`.
/// - The V2 operations (`remember` / `recall` / `improve` / `forget`)
///   execute locally again.
/// - If `wipe_credentials` is `true`, the on-disk credential file is
///   removed too, so the next [`crate::serve::serve`] call has to
///   re-authenticate.
///
/// Mirrors Python's `disconnect()` in `disconnect.py:8–35`. The
/// `wipe_credentials` flag corresponds to Python's `clear_saved`.
///
/// # Errors
///
/// Returns any IO error bubbled up from
/// [`crate::credentials::delete`]. Clearing the in-memory client is
/// infallible.
pub async fn disconnect(wipe_credentials: bool) -> CloudResult<()> {
    if let Some(client) = get_client().await {
        client.close().await;
        clear_client().await;
        tracing::info!(target: "cognee_cloud::disconnect", "Disconnected from Cognee Cloud");
        println!("  Disconnected from Cognee Cloud. Operations now run locally.");
    } else {
        println!("  Not connected to Cognee Cloud.");
    }

    if wipe_credentials {
        credentials::delete().await?;
        println!("  Saved credentials cleared.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_client::CloudClient;
    use crate::credentials::{self as creds_mod, CloudCredentials};
    use crate::state::{is_connected, set_client};
    use std::sync::Mutex;

    // Same rationale as in `serve::tests` — the process-wide singleton
    // and `$HOME` env var are shared across all tests in this crate, so
    // we serialise them via an explicit mutex.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_isolated_env<F, Fut>(tmp: &std::path::Path, body: F)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let prev = std::env::var("HOME").ok();
        // SAFETY: ENV_LOCK serializes env-table writes across this module.
        unsafe { std::env::set_var("HOME", tmp) };

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime for disconnect tests");
        rt.block_on(async {
            clear_client().await;
            body().await;
            clear_client().await;
        });

        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn disconnect_when_not_connected_is_noop_without_wipe() {
        let tmp = tempfile::tempdir().expect("tempdir");
        with_isolated_env(tmp.path(), || async {
            assert!(!is_connected().await);
            disconnect(false).await.expect("noop disconnect ok");
            assert!(!is_connected().await);
        });
    }

    #[test]
    fn disconnect_clears_installed_client() {
        let tmp = tempfile::tempdir().expect("tempdir");
        with_isolated_env(tmp.path(), || async {
            let client =
                CloudClient::new("http://example.com", "k").expect("construct dummy client");
            set_client(client).await;
            assert!(is_connected().await);

            disconnect(false).await.expect("disconnect ok");
            assert!(!is_connected().await);
        });
    }

    #[test]
    fn disconnect_with_wipe_removes_credential_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        with_isolated_env(tmp.path(), || async {
            // Pre-seed a credential file and verify it exists.
            let sample = CloudCredentials {
                service_url: "http://example.com".into(),
                api_key: "k".into(),
                email: "local".into(),
                ..CloudCredentials::default()
            };
            creds_mod::save(&sample).await.expect("seed creds");
            assert!(creds_mod::credentials_path().exists());

            disconnect(true).await.expect("disconnect wipes");
            assert!(
                !creds_mod::credentials_path().exists(),
                "wipe must delete credential file"
            );
        });
    }

    #[test]
    fn disconnect_with_wipe_when_file_missing_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        with_isolated_env(tmp.path(), || async {
            // No creds file exists, but wipe=true should still succeed.
            assert!(!creds_mod::credentials_path().exists());
            disconnect(true).await.expect("idempotent wipe");
        });
    }

    #[test]
    fn disconnect_wipe_without_client_still_clears_credentials() {
        let tmp = tempfile::tempdir().expect("tempdir");
        with_isolated_env(tmp.path(), || async {
            // Seed creds, don't install a client.
            let sample = CloudCredentials {
                service_url: "http://example.com".into(),
                api_key: "k".into(),
                ..CloudCredentials::default()
            };
            creds_mod::save(&sample).await.expect("seed creds");
            assert!(!is_connected().await);
            assert!(creds_mod::credentials_path().exists());

            disconnect(true).await.expect("disconnect ok");
            assert!(!is_connected().await);
            assert!(!creds_mod::credentials_path().exists());
        });
    }
}
