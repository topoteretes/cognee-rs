#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Delete-parity matrix (SDK-936), `cognee-delete` half: every delete
//! entry point × {grant (G), no grant (N), grant only in another tenant (X),
//! no ACL (A)}. "ACL" is [`AuthorizedDeleteService`], "no ACL" the plain
//! [`DeleteService`].
//!
//! Rules (Python @37f788bd2):
//! - under an ACL a `delete` grant counts only in the caller's tenant
//!   (`get_all_user_permission_datasets.py:49-53`), so X answers like N;
//! - names stay owner(+tenant)-scoped (`get_dataset_ids.py:29-34`), grant
//!   reach goes only through ids (`get_unique_dataset_id.py:15-24`);
//! - a dataset-less item delete does not exist in Python
//!   (`datasets.py:218-224`, `forget.py:161-162`) and is refused under an ACL;
//! - delete-all / `User` are tenant-scoped, by id, and stop at the first error
//!   (`datasets.py:316-323`, `forget.py:191-205`);
//! - a missing item inside an empty dataset deletes the dataset when
//!   `delete_dataset_if_empty` is set (`datasets.py:259-273`).
//!
//! Fixture: owner O has three datasets all named `"shared"`, seeded `o_t2`
//! (T2), `o_t1` (T1), `o_none` (tenant-less) in that order, so a tenant-loose
//! lookup would hit the wrong row first; each holds one data item. O holds
//! all four permissions on its rows; grantee G holds `delete` on `o_t1` and
//! `o_t2`; outsider N holds nothing. The caller acts in T1 unless a test says
//! otherwise. Every negative cell asserts the target row still exists.

use std::sync::Arc;

use cognee_database::{self as database, AclDb, DatabaseConnection, DeleteDb};
use cognee_delete::{
    AuthorizedDeleteService, DeleteError, DeleteMode, DeleteRequest, DeleteScope, DeleteService,
};
use cognee_models::{Data, Dataset};
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{FailingDeleteDb, MockAclDb};
use uuid::Uuid;

const NAME: &str = "shared";

struct Fx {
    db: Arc<DatabaseConnection>,
    storage: Arc<MockStorage>,
    acl: Arc<MockAclDb>,
    owner: Uuid,
    grantee: Uuid,
    outsider: Uuid,
    t1: Uuid,
    t2: Uuid,
    /// `(dataset_id, data_id)`
    o_t2: (Uuid, Uuid),
    o_t1: (Uuid, Uuid),
    o_none: (Uuid, Uuid),
}

async fn seed_dataset(
    db: &DatabaseConnection,
    acl: &MockAclDb,
    owner: Uuid,
    name: &str,
    tenant: Option<Uuid>,
) -> Uuid {
    let dataset = Dataset::new(name.to_string(), owner, tenant, Uuid::new_v4());
    let id = dataset.id;
    database::ops::datasets::create_dataset(db, dataset)
        .await
        .unwrap();
    if let Some(t) = tenant {
        acl.set_dataset_tenant(id, t);
    }
    id
}

async fn seed_item(
    db: &DatabaseConnection,
    storage: &MockStorage,
    owner: Uuid,
    dataset: Uuid,
) -> Uuid {
    let location = storage.store(b"content", "item.txt").await.unwrap();
    let data_id = Uuid::new_v4();
    let data = Data::builder(
        data_id,
        "item.txt",
        &location,
        "file://item.txt",
        "txt",
        "text/plain",
        format!("hash-{data_id}"),
        owner,
    )
    .build();
    database::ops::data::create_data(db, data).await.unwrap();
    database::ops::datasets::attach_data_to_dataset(db, dataset, data_id)
        .await
        .unwrap();
    data_id
}

async fn grant(acl: &MockAclDb, principal: Uuid, dataset: Uuid, permission: &str) {
    let acl: &dyn AclDb = acl;
    acl.ensure_principal(principal, "user").await.unwrap();
    acl.grant_permission(principal, dataset, permission)
        .await
        .unwrap();
}

async fn fixture() -> Fx {
    let db = database::connect("sqlite::memory:").await.unwrap();
    database::initialize(&db).await.unwrap();
    let db = Arc::new(db);
    let storage = Arc::new(MockStorage::new());
    let acl = Arc::new(MockAclDb::new());
    let (owner, grantee, outsider) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let (t1, t2) = (Uuid::new_v4(), Uuid::new_v4());
    let mut rows = Vec::new();
    for tenant in [Some(t2), Some(t1), None] {
        let ds = seed_dataset(&db, &acl, owner, NAME, tenant).await;
        let item = seed_item(&db, &storage, owner, ds).await;
        for p in ["read", "write", "delete", "share"] {
            grant(&acl, owner, ds, p).await;
        }
        rows.push((ds, item));
    }
    grant(&acl, grantee, rows[1].0, "delete").await;
    grant(&acl, grantee, rows[0].0, "delete").await;
    Fx {
        db,
        storage,
        acl,
        owner,
        grantee,
        outsider,
        t1,
        t2,
        o_t2: rows[0],
        o_t1: rows[1],
        o_none: rows[2],
    }
}

impl Fx {
    fn plain_over(&self, db: Arc<dyn DeleteDb>) -> DeleteService {
        DeleteService::new(self.storage.clone() as Arc<dyn StorageTrait>, db)
    }

    fn plain(&self) -> DeleteService {
        self.plain_over(self.db.clone() as Arc<dyn DeleteDb>)
    }

    fn acl_over(&self, db: Arc<dyn DeleteDb>) -> AuthorizedDeleteService {
        AuthorizedDeleteService::new(
            self.plain_over(Arc::clone(&db)),
            self.acl.clone() as Arc<dyn AclDb>,
            db,
        )
    }

    fn acl(&self) -> AuthorizedDeleteService {
        self.acl_over(self.db.clone() as Arc<dyn DeleteDb>)
    }

    async fn dataset_exists(&self, id: Uuid) -> bool {
        database::ops::datasets::get_dataset(&self.db, id)
            .await
            .unwrap()
            .is_some()
    }

    async fn data_exists(&self, id: Uuid) -> bool {
        database::ops::data::get_data(&self.db, id)
            .await
            .unwrap()
            .is_some()
    }

    async fn linked(&self, dataset: Uuid, data: Uuid) -> bool {
        database::ops::datasets::get_dataset_data(&self.db, dataset)
            .await
            .unwrap()
            .iter()
            .any(|d| d.id == data)
    }

    /// All three rows (and their items) still exist.
    async fn assert_untouched(&self, what: &str) {
        for (ds, item) in [self.o_t2, self.o_t1, self.o_none] {
            assert!(self.dataset_exists(ds).await, "{what}: dataset {ds} gone");
            assert!(self.data_exists(item).await, "{what}: item {item} gone");
        }
    }

    async fn history(&self, user: Uuid) -> u64 {
        database::ops::search_history::count_queries_by_user(&self.db, user)
            .await
            .unwrap()
    }

    async fn log_history(&self, user: Uuid) {
        database::ops::search_history::log_query(&self.db, "q", "CHUNKS", Some(user), None)
            .await
            .unwrap();
    }
}

fn req(scope: DeleteScope) -> DeleteRequest {
    DeleteRequest {
        scope,
        mode: DeleteMode::Soft,
        memory_only: false,
    }
}

fn assert_denied<T: std::fmt::Debug>(result: Result<T, DeleteError>, what: &str) {
    match result {
        Err(DeleteError::PermissionDenied(_)) => {}
        other => panic!("{what}: expected PermissionDenied, got {other:?}"),
    }
}

fn assert_validation<T: std::fmt::Debug>(result: Result<T, DeleteError>, what: &str) {
    match result {
        Err(DeleteError::Validation(_)) => {}
        other => panic!("{what}: expected Validation, got {other:?}"),
    }
}

fn dataset_by_name(owner: Uuid, tenant: Option<Uuid>) -> DeleteScope {
    DeleteScope::Dataset {
        owner_id: owner,
        dataset_name: NAME.to_string(),
        tenant_id: tenant,
    }
}

fn data_by_name(owner: Uuid, data: Uuid, tenant: Option<Uuid>) -> DeleteScope {
    DeleteScope::Data {
        owner_id: owner,
        data_id: data,
        dataset_name: Some(NAME.to_string()),
        delete_dataset_if_empty: false,
        tenant_id: tenant,
    }
}

fn data_without_dataset(owner: Uuid, data: Uuid, tenant: Option<Uuid>) -> DeleteScope {
    DeleteScope::Data {
        owner_id: owner,
        data_id: data,
        dataset_name: None,
        delete_dataset_if_empty: false,
        tenant_id: tenant,
    }
}

fn data_in(dataset: Uuid, data: Uuid) -> DeleteScope {
    DeleteScope::DataInDataset {
        dataset_id: dataset,
        data_id: data,
        delete_dataset_if_empty: false,
    }
}

// ---------------------------------------------------------------------------
// by-id dataset (`DatasetById`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn by_id_dataset_grant_deletes() {
    let f = fixture().await;
    let scope = DeleteScope::DatasetById {
        dataset_id: f.o_t1.0,
    };
    f.acl()
        .execute(&req(scope), f.grantee, Some(f.t1))
        .await
        .expect("G holds delete on o_t1 in T1");
    assert!(!f.dataset_exists(f.o_t1.0).await);
    assert!(f.dataset_exists(f.o_t2.0).await);
    assert!(f.dataset_exists(f.o_none.0).await);
}

#[tokio::test]
async fn by_id_dataset_no_grant_denied() {
    let f = fixture().await;
    let scope = DeleteScope::DatasetById {
        dataset_id: f.o_t1.0,
    };
    assert_denied(
        f.acl().execute(&req(scope), f.outsider, Some(f.t1)).await,
        "N",
    );
    f.assert_untouched("N").await;
}

#[tokio::test]
async fn by_id_dataset_other_tenant_grant_denied() {
    let f = fixture().await;
    let scope = DeleteScope::DatasetById {
        dataset_id: f.o_t2.0,
    };
    assert_denied(
        f.acl().execute(&req(scope), f.grantee, Some(f.t1)).await,
        "X: G's grant on o_t2 is in T2, G acts in T1",
    );
    f.assert_untouched("X").await;
}

// ---------------------------------------------------------------------------
// by-name dataset (`Dataset{name, tenant}`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn by_name_dataset_owner_grant_deletes_only_the_tenant_row() {
    let f = fixture().await;
    f.acl()
        .execute(
            &req(dataset_by_name(f.owner, Some(f.t1))),
            f.owner,
            Some(f.t1),
        )
        .await
        .unwrap();
    assert!(!f.dataset_exists(f.o_t1.0).await);
    assert!(f.dataset_exists(f.o_t2.0).await);
    assert!(f.dataset_exists(f.o_none.0).await);
}

#[tokio::test]
async fn by_name_dataset_without_grant_denied() {
    let f = fixture().await;
    let acl: &dyn AclDb = f.acl.as_ref();
    acl.revoke_permission(f.owner, f.o_t1.0, "delete")
        .await
        .unwrap();
    assert_denied(
        f.acl()
            .execute(
                &req(dataset_by_name(f.owner, Some(f.t1))),
                f.owner,
                Some(f.t1),
            )
            .await,
        "N: owner without its delete grant",
    );
    // An outsider naming O's dataset holds nothing either.
    assert_denied(
        f.acl()
            .execute(
                &req(dataset_by_name(f.owner, Some(f.t1))),
                f.outsider,
                Some(f.t1),
            )
            .await,
        "N: outsider",
    );
    f.assert_untouched("N").await;
}

/// Names never reach grants: a by-name scope must name the **caller's** own
/// dataset. G holds `delete` on `o_t1`, yet `Dataset{owner: O, "shared", T1}`
/// sent by G is denied — and so is a name O does not have, with the same
/// error, so the scope is no oracle on O's dataset names
/// (`get_dataset_ids.py:29-34`, `get_unique_dataset_id.py:15-24` @37f788bd2).
#[tokio::test]
async fn by_name_scope_of_another_owner_denied_even_with_grant() {
    let f = fixture().await;
    let missing = DeleteScope::Dataset {
        owner_id: f.owner,
        dataset_name: "no_such_name".to_string(),
        tenant_id: Some(f.t1),
    };
    for (scope, what) in [
        (dataset_by_name(f.owner, Some(f.t1)), "Dataset by O's name"),
        (
            data_by_name(f.owner, f.o_t1.1, Some(f.t1)),
            "Data by O's name",
        ),
        (missing, "a name O does not have"),
    ] {
        assert_denied(
            f.acl()
                .execute(&req(scope.clone()), f.grantee, Some(f.t1))
                .await,
            what,
        );
        assert_denied(
            f.acl().preview(&req(scope), f.grantee, Some(f.t1)).await,
            what,
        );
    }
    f.assert_untouched("names never reach grants").await;
    // By id the same grant does reach it.
    f.acl()
        .execute(
            &req(DeleteScope::DatasetById {
                dataset_id: f.o_t1.0,
            }),
            f.grantee,
            Some(f.t1),
        )
        .await
        .unwrap();
    assert!(!f.dataset_exists(f.o_t1.0).await);
}

#[tokio::test]
async fn by_name_dataset_scope_tenant_other_than_caller_denied() {
    let f = fixture().await;
    assert_denied(
        f.acl()
            .execute(
                &req(dataset_by_name(f.owner, Some(f.t2))),
                f.owner,
                Some(f.t1),
            )
            .await,
        "X: scope in T2, caller in T1",
    );
    assert_denied(
        f.acl()
            .preview(
                &req(dataset_by_name(f.owner, Some(f.t2))),
                f.owner,
                Some(f.t1),
            )
            .await,
        "X preview",
    );
    f.assert_untouched("X").await;
}

#[tokio::test]
async fn plain_by_name_dataset_resolves_only_in_scope_tenant() {
    let f = fixture().await;
    assert_validation(
        f.plain()
            .execute(&req(dataset_by_name(f.owner, Some(Uuid::new_v4()))))
            .await,
        "A: unknown tenant",
    );
    f.assert_untouched("A: unknown tenant").await;

    f.plain()
        .execute(&req(dataset_by_name(f.owner, Some(f.t1))))
        .await
        .unwrap();
    assert!(!f.dataset_exists(f.o_t1.0).await);
    assert!(f.dataset_exists(f.o_t2.0).await);
    assert!(f.dataset_exists(f.o_none.0).await);

    f.plain()
        .execute(&req(dataset_by_name(f.owner, None)))
        .await
        .unwrap();
    assert!(!f.dataset_exists(f.o_none.0).await);
    assert!(f.dataset_exists(f.o_t2.0).await, "T2 never touched");

    // Another owner never reaches O's rows by name.
    assert_validation(
        f.plain()
            .execute(&req(dataset_by_name(f.grantee, Some(f.t2))))
            .await,
        "A: foreign owner",
    );
    assert!(f.dataset_exists(f.o_t2.0).await);
}

// ---------------------------------------------------------------------------
// item by id (`DataInDataset`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn data_in_dataset_grant_deletes_item() {
    let f = fixture().await;
    f.acl()
        .execute(&req(data_in(f.o_t1.0, f.o_t1.1)), f.grantee, Some(f.t1))
        .await
        .unwrap();
    assert!(!f.data_exists(f.o_t1.1).await);
    assert!(f.dataset_exists(f.o_t1.0).await, "the dataset stays");
    assert!(f.data_exists(f.o_t2.1).await);
}

#[tokio::test]
async fn data_in_dataset_no_grant_denied() {
    let f = fixture().await;
    assert_denied(
        f.acl()
            .execute(&req(data_in(f.o_t1.0, f.o_t1.1)), f.outsider, Some(f.t1))
            .await,
        "N",
    );
    f.assert_untouched("N").await;
}

#[tokio::test]
async fn data_in_dataset_other_tenant_grant_denied() {
    let f = fixture().await;
    assert_denied(
        f.acl()
            .execute(&req(data_in(f.o_t2.0, f.o_t2.1)), f.grantee, Some(f.t1))
            .await,
        "X",
    );
    f.assert_untouched("X").await;
}

// ---------------------------------------------------------------------------
// item by dataset name (`Data{Some(name)}`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn data_by_name_owner_grant_deletes_item_in_tenant_row() {
    let f = fixture().await;
    f.acl()
        .execute(
            &req(data_by_name(f.owner, f.o_t1.1, Some(f.t1))),
            f.owner,
            Some(f.t1),
        )
        .await
        .unwrap();
    assert!(!f.data_exists(f.o_t1.1).await);
    assert!(f.data_exists(f.o_t2.1).await);
    assert!(f.data_exists(f.o_none.1).await);
}

#[tokio::test]
async fn data_by_name_without_grant_denied() {
    let f = fixture().await;
    let acl: &dyn AclDb = f.acl.as_ref();
    acl.revoke_permission(f.owner, f.o_t1.0, "delete")
        .await
        .unwrap();
    assert_denied(
        f.acl()
            .execute(
                &req(data_by_name(f.owner, f.o_t1.1, Some(f.t1))),
                f.owner,
                Some(f.t1),
            )
            .await,
        "N",
    );
    f.assert_untouched("N").await;
}

#[tokio::test]
async fn data_by_name_scope_tenant_other_than_caller_denied() {
    let f = fixture().await;
    assert_denied(
        f.acl()
            .execute(
                &req(data_by_name(f.owner, f.o_t2.1, Some(f.t2))),
                f.owner,
                Some(f.t1),
            )
            .await,
        "X",
    );
    f.assert_untouched("X").await;
}

#[tokio::test]
async fn plain_data_by_name_resolves_only_in_scope_tenant() {
    let f = fixture().await;
    // The tenant-less "shared" does not hold o_t1's item: refused, nothing
    // detached (a loose lookup could have found o_t1).
    assert_validation(
        f.plain()
            .execute(&req(data_by_name(f.owner, f.o_t1.1, None)))
            .await,
        "A: item of another tenant's same-named dataset",
    );
    f.assert_untouched("A").await;

    f.plain()
        .execute(&req(data_by_name(f.owner, f.o_t1.1, Some(f.t1))))
        .await
        .unwrap();
    assert!(!f.data_exists(f.o_t1.1).await);
    assert!(f.data_exists(f.o_t2.1).await);
    assert!(f.data_exists(f.o_none.1).await);
}

// ---------------------------------------------------------------------------
// item without dataset (`Data{None}`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dataset_less_item_refused_under_acl() {
    let f = fixture().await;
    let svc = f.acl();
    for (principal, tenant, what) in [
        (f.owner, Some(f.t1), "G: owner with every grant"),
        (f.outsider, Some(f.t1), "N"),
        (f.grantee, Some(f.t1), "X: grant on o_t2 only counts in T2"),
    ] {
        let scope = data_without_dataset(f.owner, f.o_t1.1, tenant);
        assert_validation(
            svc.execute(&req(scope.clone()), principal, tenant).await,
            what,
        );
        assert_validation(svc.preview(&req(scope), principal, tenant).await, what);
    }
    f.assert_untouched("dataset-less item").await;
}

/// Rows are dataset-scoped, so a dataset-less item delete finds the one
/// dataset that holds the item, and only when that dataset is the owner's in
/// the scope tenant: another tenant finds no link and is refused.
#[tokio::test]
async fn plain_dataset_less_item_detaches_only_owner_links_in_tenant() {
    let f = fixture().await;
    assert_validation(
        f.plain()
            .execute(&req(data_without_dataset(f.owner, f.o_t1.1, Some(f.t2))))
            .await,
        "o_t1's item is not in a T2 dataset",
    );
    f.assert_untouched("dataset-less item in the wrong tenant")
        .await;

    f.plain()
        .execute(&req(data_without_dataset(f.owner, f.o_t1.1, Some(f.t1))))
        .await
        .unwrap();
    assert!(
        !f.linked(f.o_t1.0, f.o_t1.1).await,
        "the T1 link is detached"
    );
    assert!(
        !f.data_exists(f.o_t1.1).await,
        "with its only dataset gone, the row is deleted"
    );
    assert!(f.data_exists(f.o_t2.1).await, "o_t2's item is kept");
    assert!(f.data_exists(f.o_none.1).await, "o_none's item is kept");
}

// ---------------------------------------------------------------------------
// `All` (ACL: the caller's delete-authorized datasets in its tenant)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn all_deletes_callers_delete_authorized_in_tenant() {
    let f = fixture().await;
    let result = f
        .acl()
        .execute(&req(DeleteScope::All), f.grantee, Some(f.t1))
        .await
        .unwrap();
    assert_eq!(result.deleted_datasets, 1);
    assert!(!f.dataset_exists(f.o_t1.0).await);
    assert!(f.dataset_exists(f.o_t2.0).await);
    assert!(f.dataset_exists(f.o_none.0).await);
}

#[tokio::test]
async fn all_with_no_grants_deletes_nothing() {
    let f = fixture().await;
    let result = f
        .acl()
        .execute(&req(DeleteScope::All), f.outsider, Some(f.t1))
        .await
        .unwrap();
    assert_eq!(result.deleted_datasets, 0);
    f.assert_untouched("N").await;
}

#[tokio::test]
async fn all_ignores_grants_in_other_tenant() {
    let f = fixture().await;
    f.acl()
        .execute(&req(DeleteScope::All), f.grantee, Some(f.t2))
        .await
        .unwrap();
    assert!(!f.dataset_exists(f.o_t2.0).await);
    assert!(
        f.dataset_exists(f.o_t1.0).await,
        "the T1 grant is not used from T2"
    );
    assert!(f.dataset_exists(f.o_none.0).await);
}

// ---------------------------------------------------------------------------
// `User`
// ---------------------------------------------------------------------------

fn user(owner: Uuid, tenant: Option<Uuid>) -> DeleteScope {
    DeleteScope::User {
        owner_id: owner,
        tenant_id: tenant,
    }
}

/// `User` never deletes search history, like Python's `delete_all` /
/// `_forget_everything` (`datasets.py:316-323`, `forget.py:191-221`
/// @37f788bd2). The history table has no tenant column, so O's history from
/// its work in T1 and T2 must survive a `User{O, T1}` (and a `User{O, T2}`).
#[tokio::test]
async fn user_scope_owner_deletes_own_tenant_datasets_and_keeps_history() {
    let f = fixture().await;
    f.log_history(f.owner).await; // a T1 session
    f.log_history(f.owner).await; // a T2 session
    let preview = f
        .acl()
        .preview(&req(user(f.owner, Some(f.t1))), f.owner, Some(f.t1))
        .await
        .unwrap();
    assert_eq!(preview.datasets_to_delete, 1);
    assert_eq!(preview.search_queries_to_delete, 0);
    let result = f
        .acl()
        .execute(&req(user(f.owner, Some(f.t1))), f.owner, Some(f.t1))
        .await
        .unwrap();
    assert_eq!(result.deleted_datasets, 1);
    assert_eq!(result.deleted_search_queries, 0);
    assert_eq!(f.history(f.owner).await, 2, "history spans tenants: kept");
    assert!(!f.dataset_exists(f.o_t1.0).await);
    assert!(f.dataset_exists(f.o_t2.0).await);
    assert!(f.dataset_exists(f.o_none.0).await);

    // The same from T2: its dataset goes, the history still stays.
    f.acl()
        .execute(&req(user(f.owner, Some(f.t2))), f.owner, Some(f.t2))
        .await
        .unwrap();
    assert!(!f.dataset_exists(f.o_t2.0).await);
    assert_eq!(f.history(f.owner).await, 2);
}

#[tokio::test]
async fn plain_user_scope_keeps_history_across_tenants() {
    let f = fixture().await;
    f.log_history(f.owner).await;
    f.log_history(f.owner).await;
    let preview = f
        .plain()
        .preview(&req(user(f.owner, Some(f.t1))))
        .await
        .unwrap();
    assert_eq!(preview.search_queries_to_delete, 0);
    let result = f
        .plain()
        .execute(&req(user(f.owner, Some(f.t1))))
        .await
        .unwrap();
    assert_eq!(result.deleted_search_queries, 0);
    assert_eq!(f.history(f.owner).await, 2);
    assert!(f.dataset_exists(f.o_t2.0).await);
}

#[tokio::test]
async fn user_scope_grantee_cover_deletes_without_touching_owner_history() {
    let f = fixture().await;
    f.log_history(f.owner).await;
    let preview = f
        .acl()
        .preview(&req(user(f.owner, Some(f.t1))), f.grantee, Some(f.t1))
        .await
        .unwrap();
    assert_eq!(preview.search_queries_to_delete, 0);
    let result = f
        .acl()
        .execute(&req(user(f.owner, Some(f.t1))), f.grantee, Some(f.t1))
        .await
        .expect("G covers O's only T1 dataset");
    assert_eq!(result.deleted_datasets, 1);
    assert_eq!(result.deleted_search_queries, 0);
    assert_eq!(f.history(f.owner).await, 1, "no grant reaches O's history");
    assert!(!f.dataset_exists(f.o_t1.0).await);
    assert!(f.dataset_exists(f.o_t2.0).await);
}

#[tokio::test]
async fn user_scope_uncovered_denied() {
    let f = fixture().await;
    f.log_history(f.owner).await;
    assert_denied(
        f.acl()
            .execute(&req(user(f.owner, Some(f.t1))), f.outsider, Some(f.t1))
            .await,
        "N",
    );
    f.assert_untouched("N").await;
    assert_eq!(f.history(f.owner).await, 1);
}

#[tokio::test]
async fn user_scope_tenant_other_than_caller_denied() {
    let f = fixture().await;
    f.log_history(f.owner).await;
    for principal in [f.owner, f.grantee] {
        assert_denied(
            f.acl()
                .execute(&req(user(f.owner, Some(f.t2))), principal, Some(f.t1))
                .await,
            "X: User in T2 from T1",
        );
    }
    // The vacuous-cover hole: a victim with no datasets in the caller's
    // tenant is not a way to wipe its history either.
    let victim = Uuid::new_v4();
    f.log_history(victim).await;
    assert_denied(
        f.acl()
            .execute(&req(user(victim, None)), f.owner, Some(f.t1))
            .await,
        "X: victim scope in another tenant",
    );
    f.acl()
        .execute(&req(user(victim, Some(f.t1))), f.owner, Some(f.t1))
        .await
        .expect("an empty cover in the caller's tenant is admitted");
    assert_eq!(
        f.history(victim).await,
        1,
        "another user's history survives"
    );
    f.assert_untouched("X").await;
    assert_eq!(f.history(f.owner).await, 1);
}

#[tokio::test]
async fn plain_user_scope_is_tenant_scoped() {
    let f = fixture().await;
    let result = f
        .plain()
        .execute(&req(user(f.owner, Some(f.t1))))
        .await
        .unwrap();
    assert_eq!(result.deleted_datasets, 1);
    assert!(!f.dataset_exists(f.o_t1.0).await);
    assert!(f.dataset_exists(f.o_t2.0).await);
    assert!(f.dataset_exists(f.o_none.0).await);

    f.plain().execute(&req(user(f.owner, None))).await.unwrap();
    assert!(!f.dataset_exists(f.o_none.0).await);
    assert!(f.dataset_exists(f.o_t2.0).await, "T2 never touched");
}

// ---------------------------------------------------------------------------
// stop at the first error
// ---------------------------------------------------------------------------

/// A second owner P with two datasets in T1, granted to itself; returns P
/// and its dataset ids in the order `User` walks them.
async fn two_owner_datasets(f: &Fx) -> (Uuid, Vec<Uuid>) {
    let p = Uuid::new_v4();
    for name in ["p1", "p2"] {
        let ds = seed_dataset(&f.db, &f.acl, p, name, Some(f.t1)).await;
        seed_item(&f.db, &f.storage, p, ds).await;
        grant(&f.acl, p, ds, "delete").await;
    }
    let order = DeleteDb::list_datasets_by_owner(f.db.as_ref(), p)
        .await
        .unwrap()
        .into_iter()
        .map(|ds| ds.id)
        .collect();
    f.log_history(p).await;
    (p, order)
}

fn failing(f: &Fx, fail: Uuid) -> Arc<dyn DeleteDb> {
    Arc::new(FailingDeleteDb::new(
        f.db.clone() as Arc<dyn DeleteDb>,
        Some(fail),
    ))
}

fn assert_partial(
    result: Result<cognee_delete::DeleteResult, DeleteError>,
    failed: Uuid,
    what: &str,
) {
    match result {
        Err(DeleteError::Partial {
            completed,
            failed_dataset_id,
            ..
        }) => {
            assert_eq!(completed.deleted_datasets, 1, "{what}");
            assert_eq!(failed_dataset_id, failed, "{what}");
        }
        other => panic!("{what}: expected Partial, got {other:?}"),
    }
}

#[tokio::test]
async fn user_scope_stops_at_first_failure_with_partial() {
    let f = fixture().await;
    let (p, order) = two_owner_datasets(&f).await;

    // The second fails: the first stays deleted, reported in `Partial`.
    let result = f
        .acl_over(failing(&f, order[1]))
        .execute(&req(user(p, Some(f.t1))), p, Some(f.t1))
        .await;
    assert_partial(result, order[1], "ACL User, second fails");
    assert!(!f.dataset_exists(order[0]).await);
    assert!(f.dataset_exists(order[1]).await);
    assert_eq!(f.history(p).await, 1, "history kept after a partial run");

    // The first fails: that error as is, nothing deleted.
    let f = fixture().await;
    let (p, order) = two_owner_datasets(&f).await;
    let result = f
        .acl_over(failing(&f, order[0]))
        .execute(&req(user(p, Some(f.t1))), p, Some(f.t1))
        .await;
    assert!(
        matches!(result, Err(ref e) if !matches!(e, DeleteError::Partial { .. })),
        "first failure is returned as is, got {result:?}"
    );
    assert!(f.dataset_exists(order[1]).await);
}

#[tokio::test]
async fn plain_user_scope_stops_at_first_failure_with_partial() {
    let f = fixture().await;
    let (p, order) = two_owner_datasets(&f).await;
    let result = f
        .plain_over(failing(&f, order[1]))
        .execute(&req(user(p, Some(f.t1))))
        .await;
    assert_partial(result, order[1], "plain User, second fails");
    assert!(!f.dataset_exists(order[0]).await);
    assert!(f.dataset_exists(order[1]).await);
    assert_eq!(f.history(p).await, 1);

    // `execute_datasets_by_id` is the shared implementation.
    let result = f
        .plain_over(failing(&f, order[1]))
        .execute_datasets_by_id(&[order[1]], DeleteMode::Soft, false)
        .await;
    assert!(
        matches!(result, Err(ref e) if !matches!(e, DeleteError::Partial { .. })),
        "got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// A missing item inside a dataset honours `delete_dataset_if_empty`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ghost_item_in_dataset_deletes_empty_dataset_when_flagged() {
    let f = fixture().await;
    let empty = seed_dataset(&f.db, &f.acl, f.owner, "empty", Some(f.t1)).await;
    let result = f
        .plain()
        .execute(&req(DeleteScope::DataInDataset {
            dataset_id: empty,
            data_id: Uuid::new_v4(),
            delete_dataset_if_empty: true,
        }))
        .await
        .unwrap();
    assert_eq!(result.deleted_datasets, 1);
    assert!(!f.dataset_exists(empty).await);
    f.assert_untouched("ghost item").await;
}

#[tokio::test]
async fn ghost_item_keeps_dataset_without_flag() {
    let f = fixture().await;
    let empty = seed_dataset(&f.db, &f.acl, f.owner, "empty", Some(f.t1)).await;
    let result = f
        .plain()
        .execute(&req(DeleteScope::DataInDataset {
            dataset_id: empty,
            data_id: Uuid::new_v4(),
            delete_dataset_if_empty: false,
        }))
        .await
        .unwrap();
    assert_eq!(result.deleted_datasets, 0);
    assert!(f.dataset_exists(empty).await);
}

#[tokio::test]
async fn ghost_item_keeps_non_empty_dataset() {
    let f = fixture().await;
    let result = f
        .plain()
        .execute(&req(DeleteScope::DataInDataset {
            dataset_id: f.o_t1.0,
            data_id: Uuid::new_v4(),
            delete_dataset_if_empty: true,
        }))
        .await
        .unwrap();
    assert_eq!(result.deleted_datasets, 0);
    f.assert_untouched("ghost item, non-empty dataset").await;
}

#[tokio::test]
async fn ghost_item_by_dataset_name_honours_flag() {
    let f = fixture().await;
    let empty = seed_dataset(&f.db, &f.acl, f.owner, "empty", Some(f.t1)).await;
    let scope = |flag| DeleteScope::Data {
        owner_id: f.owner,
        data_id: Uuid::new_v4(),
        dataset_name: Some("empty".to_string()),
        delete_dataset_if_empty: flag,
        tenant_id: Some(f.t1),
    };
    f.plain().execute(&req(scope(false))).await.unwrap();
    assert!(f.dataset_exists(empty).await, "kept without the flag");
    let result = f.plain().execute(&req(scope(true))).await.unwrap();
    assert_eq!(result.deleted_datasets, 1);
    assert!(!f.dataset_exists(empty).await);
    // The name resolves in the scope tenant: a tenant-less scope misses it.
    assert_validation(
        f.plain()
            .execute(&req(DeleteScope::Data {
                owner_id: f.owner,
                data_id: Uuid::new_v4(),
                dataset_name: Some("empty".to_string()),
                delete_dataset_if_empty: true,
                tenant_id: None,
            }))
            .await,
        "ghost item, name in another tenant",
    );
}
