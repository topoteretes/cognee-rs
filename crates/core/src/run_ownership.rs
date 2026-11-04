//! OS-backed ownership of a pipeline run, for safe startup recovery.
//!
//! Port of Python's `cognee/modules/pipelines/run_ownership.py`. A run that
//! writes a `STARTED` row also holds an exclusive file lock on
//! `<root>/<token>/owner.lock` and records `{"recovery_lock": "<token>"}` in
//! the row. The OS drops the lock when the owning process exits, however it
//! exits. Startup recovery can therefore prove a `STARTED` row is abandoned by
//! taking that same lock without blocking: if it gets it, the writer is gone;
//! if it does not, the writer is alive (or the marker is not on this
//! filesystem) and the row is left alone. A row without a marker is
//! unverifiable and is never recovered.
//!
//! Age alone is not proof of death: a legitimate run can last hours.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use uuid::Uuid;

const LOCK_FILE: &str = "owner.lock";

/// A held run-ownership lock. Dropping it releases the lock; if the run was
/// [closed](Self::mark_closed) the marker directory is removed too, otherwise
/// it stays for startup recovery to find.
#[derive(Debug)]
pub struct RunOwnership {
    token: Option<Uuid>,
    dir: Option<PathBuf>,
    // Held for its OS lock; released when the handle drops.
    file: Option<File>,
    closed: bool,
}

impl RunOwnership {
    /// Take ownership of a new run under `root`.
    ///
    /// Never fails: when the marker cannot be created the run proceeds without
    /// one ([`Self::token`] is `None`) and is simply not recoverable
    /// automatically, as in Python.
    pub fn acquire(root: &Path) -> Self {
        let token = Uuid::new_v4();
        let dir = root.join(token.to_string());
        let locked = std::fs::create_dir_all(&dir)
            .and_then(|()| {
                OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(dir.join(LOCK_FILE))
            })
            .and_then(|file| {
                file.try_lock().map_err(std::io::Error::other)?;
                Ok(file)
            });
        match locked {
            Ok(file) => Self {
                token: Some(token),
                dir: Some(dir),
                file: Some(file),
                closed: false,
            },
            Err(e) => {
                tracing::warn!(
                    "run ownership unavailable; automatic recovery is disabled for this run: {e}"
                );
                let _ = std::fs::remove_dir_all(&dir);
                Self::none()
            }
        }
    }

    /// A recovery claim on an existing marker. `None` when the marker is
    /// missing (a legacy row, or another filesystem) or its lock is still held
    /// (a live writer): neither can be proven abandoned.
    pub fn claim(root: &Path, token: Uuid) -> Option<Self> {
        let dir = root.join(token.to_string());
        if !dir.is_dir() {
            return None;
        }
        let file = OpenOptions::new()
            .write(true)
            .open(dir.join(LOCK_FILE))
            .ok()?;
        file.try_lock().ok()?;
        Some(Self {
            token: Some(token),
            dir: Some(dir),
            file: Some(file),
            closed: false,
        })
    }

    /// The recovery token to record in the `STARTED` row, when there is one.
    pub fn token(&self) -> Option<Uuid> {
        self.token
    }

    /// The run reached a terminal status: remove the marker on drop.
    pub fn mark_closed(&mut self) {
        self.closed = true;
    }
}

impl RunOwnership {
    fn none() -> Self {
        Self {
            token: None,
            dir: None,
            file: None,
            closed: false,
        }
    }
}

impl Drop for RunOwnership {
    fn drop(&mut self) {
        // Release before removing: Windows cannot unlink an open file.
        drop(self.file.take());
        if self.closed
            && let Some(dir) = self.dir.take()
        {
            let _ = std::fs::remove_file(dir.join(LOCK_FILE));
            let _ = std::fs::remove_dir(&dir);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    /// A live owner cannot be claimed; once it drops (the process died) it can.
    #[test]
    fn a_live_owner_blocks_a_claim_and_a_dropped_one_does_not() {
        let root = tempfile_root();
        let owner = RunOwnership::acquire(&root);
        let token = owner.token().expect("marker created");
        assert!(RunOwnership::claim(&root, token).is_none());
        drop(owner);
        let mut claim = RunOwnership::claim(&root, token).expect("abandoned marker claimable");
        claim.mark_closed();
        drop(claim);
        assert!(
            RunOwnership::claim(&root, token).is_none(),
            "marker removed"
        );
    }

    /// No marker on this filesystem means unverifiable, never claimable.
    #[test]
    fn a_missing_marker_is_not_claimable() {
        let root = tempfile_root();
        assert!(RunOwnership::claim(&root, Uuid::new_v4()).is_none());
    }

    /// An uncreatable root degrades to "no token" instead of failing the run.
    #[test]
    fn an_unusable_root_yields_no_token() {
        let root = tempfile_root();
        let blocker = root.join("file");
        std::fs::write(&blocker, b"x").unwrap();
        assert!(
            RunOwnership::acquire(&blocker.join("sub"))
                .token()
                .is_none()
        );
    }

    fn tempfile_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cognee-ownership-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
