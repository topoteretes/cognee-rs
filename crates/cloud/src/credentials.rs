//! On-disk storage of cloud credentials at `~/.cognee/cloud_credentials.json`.
//!
//! Byte-for-byte compatible with the Python reference at
//! `cognee/api/v1/serve/credentials.py` — both SDKs can read and write the
//! same file. The struct uses `expires_at: f64` (Unix seconds) to match the
//! Python dataclass which stores a float timestamp.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::error::{CloudError, CloudResult};

/// Filename under `~/.cognee/` — matches Python's `credentials.py:15`.
const CREDS_FILENAME: &str = "cloud_credentials.json";

/// Seconds of clock-skew margin used by [`is_token_expired`] — matches
/// the `60` constant in `credentials.py:67`.
const EXPIRY_BUFFER_SECS: f64 = 60.0;

/// On-disk credential record shared with the Python SDK.
///
/// Field names and types mirror the Python `CloudCredentials` dataclass
/// (`credentials.py:19–28`) so the JSON file round-trips between the two
/// SDKs without translation. Unknown fields are ignored on load, matching
/// the Python loader (`credentials.py:42–43`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloudCredentials {
    /// OAuth2 access token (Auth0-issued).
    pub access_token: String,
    /// OAuth2 refresh token — present when `offline_access` scope is granted.
    pub refresh_token: Option<String>,
    /// Unix-seconds timestamp at which `access_token` expires.
    pub expires_at: f64,
    /// URL of the tenant's Cognee service endpoint.
    pub service_url: String,
    /// Long-lived API key issued by the management API for this tenant.
    pub api_key: String,
    /// Management API base URL used to provision / refresh this credential.
    pub management_url: String,
    /// Tenant UUID returned by the management API.
    pub tenant_id: String,
    /// Human-readable tenant name.
    pub tenant_name: String,
    /// Authenticated user's email (extracted from id_token).
    pub email: String,
}

/// Absolute path to the credential file: `~/.cognee/cloud_credentials.json`.
///
/// Mirrors `get_credentials_path()` in `credentials.py:31–32`.
///
/// # Panics
///
/// Panics only if the platform cannot resolve a home directory, which is
/// not expected on any supported target (Linux / macOS / Windows). This
/// matches the Python behaviour where `Path.home()` raises `RuntimeError`
/// in the same cases.
#[allow(
    clippy::expect_used,
    reason = "dirs::home_dir() returning None on a supported platform (Linux/macOS/Windows) is an unrecoverable misconfiguration; matches Python's RuntimeError behaviour"
)]
pub fn credentials_path() -> PathBuf {
    let home = dirs::home_dir()
        .expect("dirs::home_dir() returns Some on all supported platforms (Linux/macOS/Windows)");
    home.join(".cognee").join(CREDS_FILENAME)
}

/// Write credentials to `~/.cognee/cloud_credentials.json` with pretty
/// (2-space indented) JSON. Creates the parent directory if missing, then
/// applies mode `0o600` on Unix so that only the owner can read the file.
///
/// Mirrors `save_credentials()` in `credentials.py:49–54`.
///
/// # Errors
///
/// Returns any IO or serialization error encountered while writing.
pub async fn save(creds: &CloudCredentials) -> CloudResult<()> {
    let path = credentials_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    // `serde_json::to_vec_pretty` writes 2-space indent — matches
    // Python's `json.dumps(..., indent=2)`.
    let body = serde_json::to_vec_pretty(creds)?;
    fs::write(&path, body).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path).await?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&path, perms).await?;
    }

    Ok(())
}

/// Read credentials from disk.
///
/// Returns `Ok(None)` if the file does not exist (first run / after
/// `disconnect(wipe=true)`). Returns an error only for IO failures other
/// than "not found" or for malformed JSON.
///
/// The Python reference swallows malformed-JSON errors too
/// (`credentials.py:44–46` logs and returns `None`); the Rust version is
/// stricter and surfaces them as [`CloudError::InvalidCredentials`] so
/// callers can decide whether to delete + re-authenticate.
pub async fn load() -> CloudResult<Option<CloudCredentials>> {
    let path = credentials_path();
    let bytes = match fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    match serde_json::from_slice::<CloudCredentials>(&bytes) {
        Ok(creds) => Ok(Some(creds)),
        Err(e) => Err(CloudError::InvalidCredentials(e.to_string())),
    }
}

/// Remove the credential file. Idempotent — a missing file is treated as
/// success. Mirrors `clear_credentials()` in `credentials.py:57–61`.
pub async fn delete() -> CloudResult<()> {
    let path = credentials_path();
    match fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// `true` if the credentials are unset or within
/// [`EXPIRY_BUFFER_SECS`] of expiring.
///
/// Mirrors `is_token_expired()` in `credentials.py:64–67`.
pub fn is_token_expired(creds: &CloudCredentials) -> bool {
    if creds.expires_at == 0.0 {
        return true;
    }
    let now = chrono::Utc::now().timestamp() as f64;
    now > (creds.expires_at - EXPIRY_BUFFER_SECS)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `dirs::home_dir()` reads `$HOME`, so tests that override it must not
    // run in parallel. Locking also protects the credential-file singleton.
    // A plain `std::sync::Mutex` is used here (not tokio's) so it works
    // from synchronous test bodies without holding an async context.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    fn sample_creds() -> CloudCredentials {
        CloudCredentials {
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at: 1_714_060_000.5,
            service_url: "https://svc.example".into(),
            api_key: "ck_example".into(),
            management_url: "https://mgmt.example".into(),
            tenant_id: "tenant-uuid".into(),
            tenant_name: "tenant-name".into(),
            email: "user@example.com".into(),
        }
    }

    /// Run `fut_builder` in a fresh tokio runtime with `$HOME` pointed at
    /// `tmp`. The test holds [`HOME_LOCK`] for the duration and restores
    /// the original `$HOME` value afterwards. We build the runtime under
    /// the lock (no nested runtimes: the outer `#[test]` is synchronous).
    ///
    /// `poisoning()` would only fire if an earlier test panicked while
    /// holding the lock; in that case the env-var table is already
    /// potentially corrupt, so we `expect` rather than try to recover.
    fn with_home_runtime<R, F>(tmp: &std::path::Path, fut_builder: F) -> R
    where
        F: for<'a> FnOnce(&'a tokio::runtime::Runtime) -> R,
    {
        let guard = HOME_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let prev = std::env::var("HOME").ok();
        // SAFETY: tests are serialized via HOME_LOCK, so no concurrent
        // thread is observing/mutating the environment while we hold it.
        unsafe { std::env::set_var("HOME", tmp) };

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime for credentials tests");

        let out = fut_builder(&rt);

        match prev {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        drop(guard);
        out
    }

    #[test]
    fn credentials_path_uses_home_and_fixed_name() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = with_home_runtime(tmp.path(), |_rt| credentials_path());
        assert_eq!(path, tmp.path().join(".cognee").join(CREDS_FILENAME));
    }

    #[test]
    fn round_trip_save_and_load() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let expected = sample_creds();
        let loaded = with_home_runtime(tmp.path(), |rt| {
            rt.block_on(async {
                save(&expected).await.expect("save sample creds");
                load().await
            })
        });
        let loaded = loaded
            .expect("load returned io/serde error")
            .expect("file should exist after save");
        assert_eq!(loaded, expected);
    }

    #[test]
    fn load_missing_file_returns_none() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let out = with_home_runtime(tmp.path(), |rt| rt.block_on(async { load().await }));
        assert!(matches!(out, Ok(None)));
    }

    #[test]
    fn delete_is_idempotent_when_missing() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let out = with_home_runtime(tmp.path(), |rt| rt.block_on(async { delete().await }));
        assert!(out.is_ok(), "deleting a missing file must succeed");
    }

    #[test]
    fn delete_removes_existing_file() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let creds = sample_creds();
        let (path_existed, path_after_delete) = with_home_runtime(tmp.path(), |rt| {
            rt.block_on(async { save(&creds).await.expect("save sample creds") });
            let existed = credentials_path().exists();
            rt.block_on(async { delete().await.expect("delete existing creds") });
            (existed, credentials_path())
        });
        assert!(path_existed, "file should exist after save");
        assert!(
            !path_after_delete.exists(),
            "credential file should be gone after delete()"
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_sets_0o600_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("create tempdir");
        let creds = sample_creds();
        let mode = with_home_runtime(tmp.path(), |rt| {
            rt.block_on(async { save(&creds).await.expect("save sample creds") });
            let meta = std::fs::metadata(credentials_path()).expect("stat credentials file");
            meta.permissions().mode() & 0o777
        });
        assert_eq!(mode, 0o600, "credentials file must be owner-only readable");
    }

    #[test]
    fn load_surfaces_malformed_json() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let out = with_home_runtime(tmp.path(), |rt| {
            rt.block_on(async {
                let path = credentials_path();
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).await.expect("create parent dir");
                }
                fs::write(&path, b"not json at all")
                    .await
                    .expect("write bad file");
                load().await
            })
        });

        match out {
            Err(CloudError::InvalidCredentials(_)) => {}
            other => panic!("expected InvalidCredentials error, got {:?}", other),
        }
    }

    #[test]
    fn load_ignores_unknown_fields_for_python_forward_compat() {
        // Python may add new credential fields; the Rust SDK should
        // tolerate them by silently ignoring unknown keys.
        let tmp = tempfile::tempdir().expect("create tempdir");
        let out = with_home_runtime(tmp.path(), |rt| {
            rt.block_on(async {
                let path = credentials_path();
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).await.expect("create parent dir");
                }
                fs::write(
                    &path,
                    br#"{
  "access_token": "a",
  "expires_at": 0.0,
  "future_field": "ignored"
}"#,
                )
                .await
                .expect("write creds with unknown field");
                load().await
            })
        });
        let creds = out.expect("load ok").expect("file present");
        assert_eq!(creds.access_token, "a");
    }

    #[test]
    fn is_token_expired_true_for_default() {
        let creds = CloudCredentials::default();
        assert!(is_token_expired(&creds));
    }

    #[test]
    fn is_token_expired_true_for_past_timestamp() {
        let creds = CloudCredentials {
            expires_at: 1.0,
            ..CloudCredentials::default()
        };
        assert!(is_token_expired(&creds));
    }

    #[test]
    fn is_token_expired_false_for_future_timestamp() {
        let future = chrono::Utc::now().timestamp() as f64 + 3600.0;
        let creds = CloudCredentials {
            expires_at: future,
            ..CloudCredentials::default()
        };
        assert!(!is_token_expired(&creds));
    }
}
