//! SDK-1701: a delete removes a code repository's `.enola-<dataset_id>`
//! snapshot for each detached `(row, dataset)` pair, and a cognee clone
//! (stamp with `repo_url`) once no row of any dataset names its path. A
//! user's own repository (no `repo_url`) is never removed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cognee_database::{self as database, DatabaseConnection, DeleteDb, ops};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_models::{Data, Dataset};
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::FailingDeleteDb;
use uuid::Uuid;

async fn setup() -> (Arc<DatabaseConnection>, DeleteService) {
    let db = database::connect("sqlite::memory:").await.unwrap();
    database::initialize(&db).await.unwrap();
    let db = Arc::new(db);
    let svc = DeleteService::new(
        Arc::new(MockStorage::new()) as Arc<dyn StorageTrait>,
        db.clone() as Arc<dyn DeleteDb>,
    );
    (db, svc)
}

async fn dataset(db: &DatabaseConnection, owner: Uuid, name: &str) -> Uuid {
    let ds = Dataset::new(name.to_string(), owner, None, Uuid::new_v4());
    let id = ds.id;
    ops::datasets::create_dataset(db, ds).await.unwrap();
    id
}

/// A code-repository row for `repo` in `dataset_id`; `repo_url` makes it a
/// cognee clone.
async fn repo_row(
    db: &DatabaseConnection,
    owner: Uuid,
    dataset_id: Uuid,
    repo: &Path,
    repo_url: Option<&str>,
) -> Uuid {
    let mut stamp = serde_json::json!({
        "source": "code_repo",
        "repo_path": repo.to_string_lossy(),
        "file_count": 1,
    });
    if let Some(url) = repo_url {
        stamp["repo_url"] = serde_json::Value::from(url);
    }
    let data_id = Uuid::new_v4();
    let data = Data::builder(
        data_id,
        "repo",
        format!("file://{}/manifest.json", repo.display()),
        format!("file://{}", repo.display()),
        "json",
        "application/json",
        format!("hash-{data_id}"),
        owner,
    )
    .system_metadata(stamp.to_string())
    .dataset_id(dataset_id)
    .build();
    ops::data::create_data(db, data).await.unwrap();
    ops::datasets::attach_data_to_dataset(db, dataset_id, data_id)
        .await
        .unwrap();
    data_id
}

/// A repository directory with `.git`, `.enola` and one `.enola-<id>` per
/// dataset.
fn make_repo(root: &Path, slug: &str, datasets: &[Uuid]) -> PathBuf {
    let repo = root.join(slug);
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join(".enola")).unwrap();
    std::fs::write(repo.join("main.rs"), "fn main() {}").unwrap();
    for id in datasets {
        let dir = repo.join(format!(".enola-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("snapshot.json"), "{}").unwrap();
    }
    repo
}

fn enola(repo: &Path, dataset_id: Uuid) -> PathBuf {
    repo.join(format!(".enola-{dataset_id}"))
}

fn forget_item(dataset_id: Uuid, data_id: Uuid) -> DeleteRequest {
    DeleteRequest {
        scope: DeleteScope::DataInDataset {
            dataset_id,
            data_id,
            delete_dataset_if_empty: false,
        },
        mode: DeleteMode::Soft,
        memory_only: false,
    }
}

const URL: &str = "https://github.com/acme/widget";

#[tokio::test]
async fn forgetting_the_last_row_of_a_clone_removes_the_clone() {
    let (db, svc) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let owner = Uuid::new_v4();
    let ds = dataset(&db, owner, "code").await;
    let clone = make_repo(root.path(), "acme_widget", &[ds]);
    let data_id = repo_row(&db, owner, ds, &clone, Some(URL)).await;

    svc.execute(&forget_item(ds, data_id)).await.unwrap();

    assert!(!enola(&clone, ds).exists());
    assert!(!clone.exists(), "an unreferenced clone is removed");
}

/// Shared by the `DatabaseConnection` query and the trait's default scan.
async fn assert_shared_clone_survives(svc: &DeleteService, db: &DatabaseConnection) {
    let root = tempfile::tempdir().unwrap();
    let owner = Uuid::new_v4();
    let ds_a = dataset(db, owner, "a").await;
    let ds_b = dataset(db, owner, "b").await;
    let clone = make_repo(root.path(), "acme_widget", &[ds_a, ds_b]);
    let row_a = repo_row(db, owner, ds_a, &clone, Some(URL)).await;
    let row_b = repo_row(db, owner, ds_b, &clone, Some(URL)).await;

    svc.execute(&forget_item(ds_a, row_a)).await.unwrap();

    assert!(
        clone.join(".git").exists(),
        "dataset b still uses the clone"
    );
    assert!(!enola(&clone, ds_a).exists());
    assert!(enola(&clone, ds_b).exists());

    svc.execute(&forget_item(ds_b, row_b)).await.unwrap();
    assert!(!clone.exists(), "the last reference is gone");
}

#[tokio::test]
async fn forgetting_one_of_two_rows_keeps_the_shared_clone() {
    let (db, svc) = setup().await;
    assert_shared_clone_survives(&svc, &db).await;
}

#[tokio::test]
async fn the_default_reference_scan_keeps_the_shared_clone() {
    let (db, _) = setup().await;
    // `FailingDeleteDb` does not override `list_code_repo_path_references`,
    // so this drives the trait's default implementation.
    let wrapped = Arc::new(FailingDeleteDb::new(db.clone() as Arc<dyn DeleteDb>, None));
    let svc = DeleteService::new(
        Arc::new(MockStorage::new()) as Arc<dyn StorageTrait>,
        wrapped as Arc<dyn DeleteDb>,
    );
    assert_shared_clone_survives(&svc, &db).await;
}

/// SDK-1709: the row moved here from the `dev` branch clone, which no row
/// names any more. Forgetting it removes that clone too, keeps a branch
/// clone another row names, and leaves other repositories' clones alone —
/// a longer slug, a slug that only shares a prefix, a directory without
/// `.git`.
#[tokio::test]
async fn forgetting_a_clone_removes_its_unreferenced_branch_clones() {
    let (db, svc) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let owner = Uuid::new_v4();
    let ds = dataset(&db, owner, "code").await;
    let other_ds = dataset(&db, owner, "other").await;
    let default = make_repo(root.path(), "acme_widget", &[ds]);
    let moved_off = make_repo(root.path(), "acme_widget@dev-1a2b3c4d", &[ds]);
    let named = make_repo(root.path(), "acme_widget@qa-5e6f7a8b", &[]);
    repo_row(&db, owner, other_ds, &named, Some(URL)).await;
    let longer = make_repo(root.path(), "acme_widget-extra", &[]);
    let prefix_only = make_repo(root.path(), "acme_widgetx@dev-1a2b3c4d", &[]);
    let not_a_clone = root.path().join("acme_widget@old-00000000");
    std::fs::create_dir_all(&not_a_clone).unwrap();
    let data_id = repo_row(&db, owner, ds, &default, Some(URL)).await;

    svc.execute(&forget_item(ds, data_id)).await.unwrap();

    assert!(!default.exists(), "the clone the row named");
    assert!(!moved_off.exists(), "the branch clone the row moved off");
    assert!(named.join(".git").exists(), "another row names it");
    assert!(longer.join(".git").exists(), "another repository");
    assert!(prefix_only.join(".git").exists(), "another repository");
    assert!(not_a_clone.exists(), "no .git inside");
}

/// SDK-1709: forgetting a row that names a user's own directory never
/// looks at the directories beside it.
#[tokio::test]
async fn a_local_repository_has_no_branch_clones() {
    let (db, svc) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let owner = Uuid::new_v4();
    let ds = dataset(&db, owner, "local").await;
    let repo = make_repo(root.path(), "my-project", &[ds]);
    let beside = make_repo(root.path(), "my-project@dev-1a2b3c4d", &[]);
    let data_id = repo_row(&db, owner, ds, &repo, None).await;

    svc.execute(&forget_item(ds, data_id)).await.unwrap();

    assert!(repo.join(".git").exists());
    assert!(beside.join(".git").exists());
}

#[tokio::test]
async fn a_local_repository_is_never_removed() {
    let (db, svc) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let owner = Uuid::new_v4();
    let ds = dataset(&db, owner, "local").await;
    let other = Uuid::new_v4();
    let repo = make_repo(root.path(), "my-project", &[ds, other]);
    let data_id = repo_row(&db, owner, ds, &repo, None).await;

    svc.execute(&forget_item(ds, data_id)).await.unwrap();

    assert!(!enola(&repo, ds).exists(), "its own snapshot is removed");
    assert!(enola(&repo, other).exists(), "another dataset's is not");
    assert!(repo.join(".enola").exists(), "a single-repo `.enola` stays");
    assert!(repo.join(".git").exists() && repo.join("main.rs").exists());
}

#[tokio::test]
async fn deleting_a_dataset_removes_its_snapshot_from_every_member() {
    let (db, svc) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let owner = Uuid::new_v4();
    let ds = dataset(&db, owner, "cluster").await;
    let keep = dataset(&db, owner, "keep").await;
    let local = make_repo(root.path(), "local-repo", &[ds]);
    let clone = make_repo(root.path(), "acme_widget", &[ds, keep]);
    repo_row(&db, owner, ds, &local, None).await;
    repo_row(&db, owner, ds, &clone, Some(URL)).await;
    repo_row(&db, owner, keep, &clone, Some(URL)).await;

    svc.execute(&DeleteRequest {
        scope: DeleteScope::DatasetById { dataset_id: ds },
        mode: DeleteMode::Soft,
        memory_only: false,
    })
    .await
    .unwrap();

    assert!(!enola(&local, ds).exists());
    assert!(!enola(&clone, ds).exists());
    assert!(local.join(".git").exists(), "a local repository stays");
    assert!(
        enola(&clone, keep).exists(),
        "the clone and the other dataset's snapshot stay"
    );
}

#[tokio::test]
async fn deleting_the_only_dataset_of_a_clone_removes_the_clone() {
    let (db, svc) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let owner = Uuid::new_v4();
    let ds = dataset(&db, owner, "only").await;
    let clone = make_repo(root.path(), "acme_widget", &[ds]);
    repo_row(&db, owner, ds, &clone, Some(URL)).await;

    svc.execute(&DeleteRequest {
        scope: DeleteScope::DatasetById { dataset_id: ds },
        mode: DeleteMode::Soft,
        memory_only: false,
    })
    .await
    .unwrap();

    assert!(!clone.exists(), "no dataset references the clone any more");
}

/// A memory-only forget keeps the rows, so it keeps their folders too.
#[tokio::test]
async fn a_memory_only_forget_keeps_the_folders() {
    let (db, svc) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let owner = Uuid::new_v4();
    let ds = dataset(&db, owner, "code").await;
    let clone = make_repo(root.path(), "acme_widget", &[ds]);
    let data_id = repo_row(&db, owner, ds, &clone, Some(URL)).await;

    let mut request = forget_item(ds, data_id);
    request.memory_only = true;
    svc.execute(&request).await.unwrap();

    assert!(enola(&clone, ds).exists());
    assert!(clone.join(".git").exists());
}

/// A row that names the clone but belongs to no dataset (detached) still
/// keeps it: the `DatabaseConnection` query sees every `Data` row, so a
/// clone is removed only when nothing at all names it.
#[tokio::test]
async fn a_detached_row_keeps_the_clone() {
    let (db, svc) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let owner = Uuid::new_v4();
    let ds = dataset(&db, owner, "code").await;
    let clone = make_repo(root.path(), "acme_widget", &[ds]);
    let data_id = repo_row(&db, owner, ds, &clone, Some(URL)).await;
    // A second row naming the same clone, with no dataset.
    let orphan = Uuid::new_v4();
    let stamp = serde_json::json!({
        "source": "code_repo",
        "repo_path": clone.to_string_lossy(),
        "file_count": 1,
        "repo_url": URL,
    });
    let data = Data::builder(
        orphan,
        "repo",
        format!("file://{}/manifest.json", clone.display()),
        format!("file://{}", clone.display()),
        "json",
        "application/json",
        format!("hash-{orphan}"),
        owner,
    )
    .system_metadata(stamp.to_string())
    .build();
    ops::data::create_data(&db, data).await.unwrap();

    svc.execute(&forget_item(ds, data_id)).await.unwrap();

    assert!(!enola(&clone, ds).exists());
    assert!(
        clone.join(".git").exists(),
        "the detached row still names it"
    );
}

/// Removing a folder is best effort: a failure is logged and the delete
/// still succeeds.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_removal_does_not_fail_the_delete() {
    use std::os::unix::fs::PermissionsExt;
    let (db, svc) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let owner = Uuid::new_v4();
    let ds = dataset(&db, owner, "code").await;
    let parent = root.path().join("clones");
    std::fs::create_dir_all(&parent).unwrap();
    let clone = make_repo(&parent, "acme_widget", &[ds]);
    let data_id = repo_row(&db, owner, ds, &clone, Some(URL)).await;
    // The clone's parent and the clone itself are read-only: neither the
    // snapshot nor the clone can be removed.
    let read_only = std::fs::Permissions::from_mode(0o555);
    std::fs::set_permissions(&clone, read_only.clone()).unwrap();
    std::fs::set_permissions(&parent, read_only).unwrap();

    let result = svc.execute(&forget_item(ds, data_id)).await;

    let writable = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&parent, writable.clone()).unwrap();
    std::fs::set_permissions(&clone, writable).unwrap();
    result.unwrap();
    assert!(
        clone.join(".git").exists(),
        "the clone could not be removed"
    );
    // The row itself is gone: the relational delete committed.
    assert!(ops::data::get_data(&db, data_id).await.unwrap().is_none());
}
