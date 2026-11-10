//! A process-wide registry of the embedded stores this process has open.
//!
//! An embedded store (a Ladybug graph file, a Qdrant Edge data directory) must
//! have exactly one live owner per process. The stores' own locks do not
//! enforce that within one process:
//!
//! - Ladybug takes POSIX `fcntl(F_SETLK)` record locks. Those are owned by the
//!   *process*, so a second `Database` on a path this process already has open
//!   is granted the lock and opens silently. The two instances then keep
//!   separate buffer managers over one data file and one `.wal`, and whichever
//!   checkpoints last overwrites the other's view and unlinks the shared log
//!   (`crates/graph/tests/ladybug_stale_instance.rs` pins that hazard).
//! - Qdrant Edge takes a `flock` on each collection's WAL. That one *is* per
//!   open file description, so the second open of a collection fails — but
//!   only when the collection is first touched, as a `WouldBlock` I/O error on
//!   every search and write, never at open.
//!
//! So an adapter for such a store takes a [`StoreClaim`] on the store's root
//! before it opens anything, and keeps it for exactly as long as it may still
//! hold the store's files or locks. A second claim on the same root while the
//! first is alive fails with [`StoreInUse`], naming the path, at open.
//!
//! The claim is released by dropping it, and only then. An adapter ties it to
//! the lifetime of whatever holds the files (for Ladybug the `Database`, for
//! Qdrant every loaded shard), so a store becomes claimable again exactly when
//! the previous owner has really let go — not when a handle was merely asked to
//! close while an in-flight query still holds the files open.
//!
//! The registry is per process and per path; it says nothing about other
//! processes, which the stores' own file locks (or nothing, for Ladybug) cover.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

/// The stable machine-readable code of a [`StoreInUse`] error, as the bindings
/// report it.
pub const STORE_IN_USE_CODE: &str = "STORE_IN_USE";

/// The canonical roots of every store currently claimed in this process.
static CLAIMED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

/// A second live open of an embedded store this process already has open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreInUse {
    path: PathBuf,
    store: &'static str,
}

impl StoreInUse {
    /// The canonical root of the store that is in use.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What kind of store it is (`"ladybug graph"`, `"qdrant vector"`, ...).
    pub fn store(&self) -> &'static str {
        self.store
    }
}

impl fmt::Display for StoreInUse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{STORE_IN_USE_CODE}: the {} store at {} is already open in this process; close \
             the engine that holds it (and let its in-flight operations finish) before \
             opening it again",
            self.store,
            self.path.display()
        )
    }
}

impl std::error::Error for StoreInUse {}

/// Proof that this process owns an embedded store; see the [module docs](self).
///
/// Dropping it releases the store.
#[derive(Debug)]
#[must_use = "the store is released as soon as the claim is dropped"]
pub struct StoreClaim {
    key: PathBuf,
}

impl StoreClaim {
    /// Claim the store rooted at `path` for this process.
    ///
    /// `path` need not exist yet (a store created on first open); it is keyed
    /// by its canonical form, so two spellings of one path (relative and
    /// absolute, through a symlinked parent) are the same store.
    ///
    /// # Errors
    /// [`StoreInUse`] while another claim on the same store is alive.
    pub fn acquire(path: &Path, store: &'static str) -> Result<Self, StoreInUse> {
        let key = store_key(path);
        let mut claimed = CLAIMED.lock().unwrap_or_else(PoisonError::into_inner);
        if !claimed.insert(key.clone()) {
            return Err(StoreInUse { path: key, store });
        }
        Ok(Self { key })
    }

    /// The canonical root this claim holds.
    pub fn path(&self) -> &Path {
        &self.key
    }
}

impl Drop for StoreClaim {
    fn drop(&mut self) {
        // Never panic here: a claim is often dropped while unwinding.
        CLAIMED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
    }
}

/// The registry key of a store root: the canonical form of its nearest
/// existing ancestor, joined with the components that do not exist yet.
///
/// Resolving the ancestor matters when the root is created after the claim:
/// a path under a symlinked directory (Android's `/data/user/0` ->
/// `/data/data`, macOS `/var` -> `/private/var`) must get the same key before
/// and after it exists, or a second claim through the resolved path is let in.
fn store_key(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut missing = Vec::new();
    let mut cursor = absolute.as_path();
    loop {
        if let Ok(canonical) = cursor.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(canonical, |key, name| key.join(name));
        }
        match (cursor.parent(), cursor.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_os_string());
                cursor = parent;
            }
            // Nothing on the path exists (or it ends in `..`): key it as given.
            _ => return absolute,
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn a_second_claim_on_one_store_is_refused_until_the_first_drops() {
        let dir = tempfile::tempdir().unwrap();
        let first = StoreClaim::acquire(dir.path(), "test").unwrap();
        let err = StoreClaim::acquire(dir.path(), "test").unwrap_err();
        assert_eq!(err.path(), first.path());
        assert_eq!(err.store(), "test");
        assert!(err.to_string().starts_with(STORE_IN_USE_CODE), "{err}");
        assert!(
            err.to_string()
                .contains(&first.path().display().to_string()),
            "{err}"
        );
        drop(first);
        let _again = StoreClaim::acquire(dir.path(), "test").unwrap();
    }

    #[test]
    fn different_stores_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let _a = StoreClaim::acquire(&dir.path().join("a"), "test").unwrap();
        let _b = StoreClaim::acquire(&dir.path().join("b"), "test").unwrap();
    }

    /// A root whose missing part sits under a symlinked ancestor (Android's
    /// `/data/user/0` -> `/data/data` on a fresh install, macOS `/var`): the
    /// key must resolve the symlink before the store exists, or a later claim
    /// through the resolved path gets a different key and is let in.
    #[cfg(unix)]
    #[test]
    fn a_missing_root_under_a_symlinked_ancestor_keys_like_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let _first = StoreClaim::acquire(&link.join("files").join("graph"), "test").unwrap();
        std::fs::create_dir_all(real.join("files")).unwrap();
        std::fs::write(real.join("files").join("graph"), b"").unwrap();
        assert!(
            StoreClaim::acquire(&real.join("files").join("graph"), "test").is_err(),
            "the same store through its resolved path must be refused"
        );
    }

    #[test]
    fn a_store_created_after_the_claim_keeps_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let _first = StoreClaim::acquire(&root, "test").unwrap();
        std::fs::create_dir(&root).unwrap();
        let spelled_differently = dir.path().join(".").join("store");
        assert!(StoreClaim::acquire(&spelled_differently, "test").is_err());
    }
}
