use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use cognee_mcp::config::EnvSource;
use cognee_mcp::reference::{
    REFERENCE_DATASET, ReferenceConfig, ReferenceError, ReferenceLayout, ReferenceLimits,
};

#[derive(Default)]
struct FakeEnv {
    values: BTreeMap<String, String>,
    reads: RefCell<Vec<String>>,
}

impl EnvSource for FakeEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.reads.borrow_mut().push(key.to_owned());
        self.values.get(key).cloned()
    }
}

fn fake_env<const N: usize>(values: [(&str, &str); N]) -> FakeEnv {
    FakeEnv {
        values: values
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
        reads: RefCell::default(),
    }
}

#[test]
fn absent_reference_root_disables_only_reference_memory() {
    let env = fake_env([("HOME", "/home/alice")]);

    let config = ReferenceConfig::from_env(&env).expect("reference config");

    assert!(config.is_none());
    assert_eq!(env.reads.into_inner(), vec!["APEX_COGNEE_REFERENCE_ROOT"]);
}

#[test]
fn configured_reference_root_uses_fixed_dataset_and_limits() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let canonical_root = temporary.path().canonicalize().expect("canonical root");
    let env = fake_env([(
        "APEX_COGNEE_REFERENCE_ROOT",
        canonical_root.to_str().expect("UTF-8 root"),
    )]);

    let config = ReferenceConfig::from_env(&env)
        .expect("reference config")
        .expect("enabled reference config");

    assert_eq!(config.layout.root, canonical_root);
    assert_eq!(config.dataset, REFERENCE_DATASET);
    assert_eq!(
        config.limits,
        ReferenceLimits {
            max_input_bytes: 2 * 1024 * 1024,
            max_batch_bytes: 8 * 1024 * 1024,
            max_batch_files: 32,
            max_pending_events: 128,
            max_pending_bytes: 64 * 1024 * 1024,
            max_item_bytes: 2 * 1024,
            max_payload_bytes: 8 * 1024,
        }
    );
}

#[test]
fn configured_reference_root_rejects_relative_and_parent_components_without_leaking_them() {
    for configured in ["relative/operator-secret", "/tmp/operator-secret/../escape"] {
        let error =
            ReferenceConfig::from_env(&fake_env([("APEX_COGNEE_REFERENCE_ROOT", configured)]))
                .expect_err("unsafe root must fail");

        assert!(matches!(error, ReferenceError::InvalidRoot));
        assert!(!format!("{error:?}").contains("operator-secret"));
        assert!(!error.to_string().contains("operator-secret"));
    }
}

#[test]
#[cfg(unix)]
fn configured_reference_root_resolves_a_symlinked_existing_ancestor() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().expect("temporary directory");
    let outside = tempfile::tempdir().expect("outside directory");
    let link = temporary
        .path()
        .canonicalize()
        .expect("canonical temporary directory")
        .join("operator-secret-link");
    symlink(outside.path(), &link).expect("create directory symlink");
    let configured = link.join("reference");

    let config = ReferenceConfig::from_env(&fake_env([(
        "APEX_COGNEE_REFERENCE_ROOT",
        configured.to_str().expect("UTF-8 root"),
    )]))
    .expect("canonical reference config")
    .expect("enabled reference config");

    assert_eq!(
        config.layout.root,
        outside
            .path()
            .canonicalize()
            .expect("canonical outside directory")
            .join("reference")
    );
    assert!(!format!("{config:?}").contains("operator-secret-link"));
}

#[test]
fn layout_names_every_reader_and_administrator_path() {
    let root = PathBuf::from("/srv/apex/cognee-reference");
    let layout = ReferenceLayout::under(root.clone());

    assert_eq!(layout.root, root);
    assert_eq!(layout.schema, root.join("schema.json"));
    assert_eq!(layout.current, root.join("current.json"));
    assert_eq!(layout.delta, root.join("delta"));
    assert_eq!(layout.delta_head, root.join("delta/head.json"));
    assert_eq!(layout.delta_events, root.join("delta/events"));
    assert_eq!(layout.generations, root.join("generations"));
    assert_eq!(layout.admin, root.join("admin"));
    assert_eq!(layout.delta_lock, root.join("admin/lock/delta.lock"));
    assert_eq!(layout.publish_lock, root.join("admin/lock/publish.lock"));
    assert_eq!(layout.builder, root.join("admin/builder"));
    assert_eq!(layout.staging, root.join("admin/staging"));
    assert_eq!(layout.status, root.join("admin/status"));
}

#[test]
fn reader_validation_never_creates_a_missing_root() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("missing-reference-root");
    let layout = ReferenceLayout::under(root.clone());

    let error = layout
        .validate_reader_root()
        .expect_err("missing root must be unavailable");

    assert!(matches!(error, ReferenceError::Unavailable));
    assert!(!root.exists());
}

#[test]
fn reader_validation_accepts_a_initialized_delta_root_without_mutating_it() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("reference");
    let layout = ReferenceLayout::under(root);
    layout.ensure_admin_tree().expect("administrator tree");
    std::fs::write(
        &layout.schema,
        br#"{"schema_version":1,"dataset":"fleet_reference"}"#,
    )
    .expect("schema");
    std::fs::write(
        &layout.delta_head,
        br#"{"schema_version":1,"highest_committed_sequence":0}"#,
    )
    .expect("delta head");
    let before = tree_snapshot(&layout.root);

    layout.validate_reader_root().expect("valid reader root");

    assert_eq!(tree_snapshot(&layout.root), before);
}

#[test]
#[cfg(unix)]
fn administrator_tree_has_public_read_roots_and_private_working_paths() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("reference");
    let layout = ReferenceLayout::under(root.clone());

    layout.ensure_admin_tree().expect("administrator tree");

    for directory in [
        &root,
        &layout.delta,
        &layout.delta_events,
        &layout.generations,
    ] {
        let mode = std::fs::metadata(directory)
            .expect("public directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755, "{}", directory.display());
    }
    for directory in [
        &layout.admin,
        layout.delta_lock.parent().expect("lock directory"),
        &layout.builder,
        &layout.staging,
        &layout.status,
    ] {
        let mode = std::fs::metadata(directory)
            .expect("private directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{}", directory.display());
    }
    assert!(!layout.delta_lock.exists());
    assert!(!layout.publish_lock.exists());
}

#[test]
fn reference_errors_have_stable_classes_and_retryability() {
    assert_eq!(ReferenceError::Unavailable.class(), "REFERENCE_UNAVAILABLE");
    assert!(ReferenceError::Unavailable.retryable());
    assert_eq!(
        ReferenceError::ModelMismatch.class(),
        "REFERENCE_MODEL_MISMATCH"
    );
    assert!(!ReferenceError::ModelMismatch.retryable());
    assert_eq!(ReferenceError::ReadOnly.class(), "REFERENCE_READ_ONLY");
    assert!(!ReferenceError::ReadOnly.retryable());
    assert_eq!(
        ReferenceError::CorruptRecord.class(),
        "REFERENCE_CORRUPT_RECORD"
    );
    assert!(!ReferenceError::CorruptRecord.retryable());
    assert_eq!(
        ReferenceError::BacklogLimit.class(),
        "REFERENCE_BACKLOG_LIMIT"
    );
    assert!(ReferenceError::BacklogLimit.retryable());
}

fn tree_snapshot(root: &std::path::Path) -> Vec<(PathBuf, bool, u64)> {
    let mut snapshot = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries = std::fs::read_dir(&directory)
            .expect("read snapshot directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("snapshot entries");
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let entry_path = entry.path();
            let metadata = entry.metadata().expect("snapshot metadata");
            snapshot.push((
                entry_path
                    .strip_prefix(root)
                    .expect("relative path")
                    .to_owned(),
                metadata.is_dir(),
                metadata.len(),
            ));
            if metadata.is_dir() {
                pending.push(entry_path);
            }
        }
    }
    snapshot.sort();
    snapshot
}
