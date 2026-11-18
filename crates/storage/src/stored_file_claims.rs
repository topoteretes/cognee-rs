//! Keep a stored file from being removed while an add is about to use it
//! (Python `cognee/infrastructure/files/stored_file_claims.py`).
//!
//! Stored files are content-addressed and shared: identical bytes land in one
//! file for every dataset. Delete removes a file once no row points at it, but
//! an add stores its file before its row is saved. Without a claim, a delete
//! in another dataset could see "no row" in between and remove the file the
//! add's row is about to point at.
//!
//! So an add holds a [`ClaimScope`] until its rows are saved and claims each
//! file in it *before* writing the file; a removal holds a [`RemovalGuard`]
//! from its reference check to its last delete and skips claimed files. Unlike
//! Python, which claims after writing and so has to detect a removal that
//! slipped in between, claiming first leaves no such gap.
//!
//! Process-local, as in Python: two processes sharing one data root are not
//! covered.
#![allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Taken by a claim and by a removal for its whole check-then-delete, so the
/// two never interleave.
static REMOVAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Path → how many open scopes claim it. A plain mutex, so a scope can
/// release its claims on drop without waiting for [`REMOVAL_LOCK`].
static CLAIMS: LazyLock<Mutex<HashMap<PathBuf, usize>>> = LazyLock::new(Default::default);

/// The files an operation stored for rows it has yet to save. Each claim
/// lasts until the scope is dropped; share it with `Arc` across tasks.
#[derive(Debug, Default)]
pub struct ClaimScope {
    claimed: Mutex<Vec<PathBuf>>,
}

impl ClaimScope {
    /// Claim `full_path` until this scope ends. Call it before writing the
    /// file: a removal that runs after it skips the file, and one that ran
    /// before it has finished, so the write that follows lands.
    pub async fn claim(&self, full_path: &Path) {
        let key = claim_key(full_path);
        let _removals = REMOVAL_LOCK.lock().await;
        // lock poison is unrecoverable
        *CLAIMS.lock().unwrap().entry(key.clone()).or_default() += 1;
        self.claimed.lock().unwrap().push(key); // lock poison is unrecoverable
    }
}

impl Drop for ClaimScope {
    fn drop(&mut self) {
        let claimed = std::mem::take(self.claimed.get_mut().unwrap()); // lock poison is unrecoverable
        let mut claims = CLAIMS.lock().unwrap(); // lock poison is unrecoverable
        for key in claimed {
            if let Some(count) = claims.get_mut(&key) {
                *count -= 1;
                if *count == 0 {
                    claims.remove(&key);
                }
            }
        }
    }
}

/// Held by a removal from its reference check to its last delete, so no
/// claim lands in between.
pub struct RemovalGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

/// Start removing stored files; see [`RemovalGuard`].
pub async fn removing_stored_files() -> RemovalGuard {
    RemovalGuard {
        _lock: REMOVAL_LOCK.lock().await,
    }
}

impl RemovalGuard {
    /// Whether an open scope claims `full_path`, which must then be kept.
    pub fn is_claimed(&self, full_path: &Path) -> bool {
        // lock poison is unrecoverable
        CLAIMS.lock().unwrap().contains_key(&claim_key(full_path))
    }
}

/// `path` made absolute against the working directory, so an add's
/// root-relative path and a delete's `file://` path name one key.
fn claim_key(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The claim table is process-wide, so each test claims its own path; a
    /// claim does not need the file to exist.
    fn unique_path() -> PathBuf {
        std::env::temp_dir().join(uuid::Uuid::new_v4().to_string())
    }

    #[tokio::test]
    async fn a_claim_lasts_until_its_last_scope_ends() {
        let path = unique_path();
        let (first, second) = (ClaimScope::default(), ClaimScope::default());
        first.claim(&path).await;
        second.claim(&path).await;

        drop(first);
        assert!(removing_stored_files().await.is_claimed(&path));
        drop(second);
        assert!(!removing_stored_files().await.is_claimed(&path));
    }

    /// An add claims a root-relative path; delete checks the absolute one.
    #[tokio::test]
    async fn a_relative_path_and_its_absolute_form_are_one_claim() {
        let relative = PathBuf::from(uuid::Uuid::new_v4().to_string()).join("text_x.txt");
        let scope = ClaimScope::default();
        scope.claim(&relative).await;

        let absolute = std::env::current_dir().unwrap().join(&relative);
        assert!(removing_stored_files().await.is_claimed(&absolute));
    }

    /// A claim cannot land between a removal's check and its delete.
    #[tokio::test]
    async fn a_claim_waits_for_a_running_removal() {
        let path = unique_path();
        let scope = ClaimScope::default();
        let removal = removing_stored_files().await;

        let blocked = tokio::time::timeout(Duration::from_millis(50), scope.claim(&path)).await;
        assert!(blocked.is_err(), "the claim waits while the removal runs");
        assert!(!removal.is_claimed(&path));

        drop(removal);
        scope.claim(&path).await;
        assert!(removing_stored_files().await.is_claimed(&path));
    }
}
