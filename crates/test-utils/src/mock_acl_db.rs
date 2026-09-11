//! In-memory mock implementation of [`AclDb`] for testing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "mock infrastructure — panics are acceptable"
)]

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use cognee_database::{AclDb, DatabaseError};
use uuid::Uuid;

/// A `HashMap`-backed mock ACL database for unit and integration tests.
///
/// Thread-safe via `Mutex`. The inner store is a set of
/// `(principal_id, dataset_id, permission_name)` tuples, exactly like the
/// production `acls` table: a *principal* may be a user, a role, or a
/// tenant, and [`AclDb::grant_permission`] does not care which.
///
/// What makes the two halves of the [`AclDb`] trait observably different
/// here is **membership**. [`MockAclDb::add_user_to_tenant`] and
/// [`MockAclDb::add_user_to_role`] record which tenants a user belongs to
/// and which roles it holds. The plain `has_permission` /
/// `authorized_dataset_ids` consult only rows whose `principal_id` is the
/// caller itself; the `_with_roles` variants additionally consult rows
/// granted to any tenant or role the caller is a member of. A test that
/// grants `delete` to a *role* and then calls a code path as a *user*
/// therefore passes only if that path uses the roles-aware variant — which
/// is the property the mock exists to make testable.
///
/// Divergences from Python `get_all_user_permission_datasets` that this
/// mock does **not** model (it has no dataset table or user table):
/// - Python only walks roles inside the tenant loop, so a user with zero
///   tenants gets no role grants at all. Here role membership counts on
///   its own.
/// - Python drops every result whose `dataset.tenant_id != user.tenant_id`.
///   The mock cannot see dataset tenancy and applies no such filter.
pub struct MockAclDb {
    /// Set of granted permissions: (principal_id, dataset_id, permission_name).
    grants: Mutex<HashSet<(Uuid, Uuid, String)>>,
    /// Set of principals: (principal_id, principal_type).
    principals: Mutex<HashSet<(Uuid, String)>>,
    /// user_id → tenant ids the user belongs to.
    tenant_memberships: Mutex<HashMap<Uuid, HashSet<Uuid>>>,
    /// user_id → role ids the user holds.
    role_memberships: Mutex<HashMap<Uuid, HashSet<Uuid>>>,
    /// When set, [`AclDb::grant_permission`] returns this error instead of
    /// recording the grant. See [`MockAclDb::failing_grants`].
    grant_failure: Option<String>,
}

impl MockAclDb {
    pub fn new() -> Self {
        Self {
            grants: Mutex::new(HashSet::new()),
            principals: Mutex::new(HashSet::new()),
            tenant_memberships: Mutex::new(HashMap::new()),
            role_memberships: Mutex::new(HashMap::new()),
            grant_failure: None,
        }
    }

    /// A mock whose [`AclDb::grant_permission`] always fails with `reason`.
    ///
    /// Models the production case a best-effort grant used to hide: the
    /// dataset row is written, the ACL row is not, and the owner then cannot
    /// read their own dataset because `readable_dataset_ids` consults the ACL
    /// alone. Every create path must surface that as a failed create.
    pub fn failing_grants(reason: &str) -> Self {
        Self {
            grant_failure: Some(reason.to_string()),
            ..Self::new()
        }
    }

    /// Return the number of ACL grants currently stored.
    pub fn grant_count(&self) -> usize {
        let grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        grants.len()
    }

    /// Return the number of principals currently stored.
    pub fn principal_count(&self) -> usize {
        let principals = self.principals.lock().unwrap(); // lock poison is unrecoverable
        principals.len()
    }

    /// Check if a specific grant exists.
    pub fn has_grant(&self, principal_id: Uuid, dataset_id: Uuid, permission_name: &str) -> bool {
        let grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        grants.contains(&(principal_id, dataset_id, permission_name.to_string()))
    }

    /// Remove all grants for a specific dataset (simulates CASCADE DELETE).
    pub fn cascade_delete_dataset(&self, dataset_id: Uuid) {
        let mut grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        grants.retain(|(_, ds_id, _)| *ds_id != dataset_id);
    }

    /// Record that `user_id` belongs to `tenant_id`.
    ///
    /// Grants made to `tenant_id` (via [`AclDb::grant_permission`] with the
    /// tenant id as the principal) become visible to `user_id` through the
    /// `_with_roles` variants only. Also registers `tenant_id` as a
    /// `"tenant"` principal.
    pub fn add_user_to_tenant(&self, user_id: Uuid, tenant_id: Uuid) {
        {
            let mut principals = self.principals.lock().unwrap(); // lock poison is unrecoverable
            principals.insert((tenant_id, "tenant".to_string()));
        }
        let mut memberships = self.tenant_memberships.lock().unwrap(); // lock poison is unrecoverable
        memberships.entry(user_id).or_default().insert(tenant_id);
    }

    /// Record that `user_id` holds `role_id`.
    ///
    /// Grants made to `role_id` (via [`AclDb::grant_permission`] with the
    /// role id as the principal) become visible to `user_id` through the
    /// `_with_roles` variants only. Also registers `role_id` as a `"role"`
    /// principal.
    pub fn add_user_to_role(&self, user_id: Uuid, role_id: Uuid) {
        {
            let mut principals = self.principals.lock().unwrap(); // lock poison is unrecoverable
            principals.insert((role_id, "role".to_string()));
        }
        let mut memberships = self.role_memberships.lock().unwrap(); // lock poison is unrecoverable
        memberships.entry(user_id).or_default().insert(role_id);
    }

    /// Every principal id whose grants `user_id` inherits, in the trait's
    /// documented resolution order: the user itself, then its tenants, then
    /// the roles it holds *in those tenants*.
    ///
    /// Roles are gated on tenant membership on purpose. `AclDb`'s contract
    /// (`crates/database/src/traits/acl_db.rs`) says "Role-level ACL for each
    /// role the user holds **in those tenants**", and Python nests its role
    /// walk inside the tenant loop
    /// (`get_all_user_permission_datasets.py`), so a user belonging to no
    /// tenant inherits nothing from roles. A mock that granted role access
    /// without tenant membership would be *more permissive than the contract*,
    /// and any test relying on it would certify a scenario a conforming
    /// backend denies — passing here and failing in production.
    fn inherited_principals(&self, user_id: Uuid) -> Vec<Uuid> {
        let mut principals = vec![user_id];
        let tenants: Vec<Uuid> = {
            let memberships = self.tenant_memberships.lock().unwrap(); // lock poison is unrecoverable
            memberships
                .get(&user_id)
                .map(|ids| ids.iter().copied().collect())
                .unwrap_or_default()
        };
        principals.extend(tenants.iter().copied());
        // No tenants, no roles — see the note above.
        if !tenants.is_empty() {
            let roles = self.role_memberships.lock().unwrap(); // lock poison is unrecoverable
            if let Some(ids) = roles.get(&user_id) {
                principals.extend(ids.iter().copied());
            }
        }
        principals
    }
}

impl Default for MockAclDb {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AclDb for MockAclDb {
    async fn has_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<bool, DatabaseError> {
        let grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        Ok(grants.contains(&(principal_id, dataset_id, permission_name.to_string())))
    }

    async fn authorized_dataset_ids(
        &self,
        principal_id: Uuid,
        permission_name: &str,
    ) -> Result<Vec<Uuid>, DatabaseError> {
        let grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        let ids: Vec<Uuid> = grants
            .iter()
            .filter(|(pid, _, pname)| *pid == principal_id && pname == permission_name)
            .map(|(_, ds_id, _)| *ds_id)
            .collect();
        Ok(ids)
    }

    async fn grant_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<(), DatabaseError> {
        if let Some(reason) = &self.grant_failure {
            return Err(DatabaseError::QueryError(reason.clone()));
        }
        let mut grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        grants.insert((principal_id, dataset_id, permission_name.to_string()));
        Ok(())
    }

    async fn revoke_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<(), DatabaseError> {
        let mut grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        grants.remove(&(principal_id, dataset_id, permission_name.to_string()));
        Ok(())
    }

    async fn ensure_principal(
        &self,
        principal_id: Uuid,
        principal_type: &str,
    ) -> Result<(), DatabaseError> {
        let mut principals = self.principals.lock().unwrap(); // lock poison is unrecoverable
        principals.insert((principal_id, principal_type.to_string()));
        Ok(())
    }

    async fn has_permission_with_roles(
        &self,
        user_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<bool, DatabaseError> {
        let principals = self.inherited_principals(user_id);
        let grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        Ok(principals
            .iter()
            .any(|pid| grants.contains(&(*pid, dataset_id, permission_name.to_string()))))
    }

    async fn authorized_dataset_ids_with_roles(
        &self,
        user_id: Uuid,
        permission_name: &str,
    ) -> Result<Vec<Uuid>, DatabaseError> {
        let principals = self.inherited_principals(user_id);
        let grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        // Deduplicate: the same dataset may be granted to the user directly
        // and to one of its roles or tenants (the trait promises a deduped
        // result, as does Python's `unique.setdefault(dataset.id, ...)`).
        let mut seen = HashSet::new();
        let ids: Vec<Uuid> = grants
            .iter()
            .filter(|(pid, _, pname)| principals.contains(pid) && pname == permission_name)
            .map(|(_, ds_id, _)| *ds_id)
            .filter(|ds_id| seen.insert(*ds_id))
            .collect();
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property every consumer test relies on: a grant to a role or a
    /// tenant is visible to a member only through the `_with_roles`
    /// variants. If this ever regresses to delegation, tests that switch a
    /// call site from the plain variant become vacuous.
    #[tokio::test]
    async fn with_roles_variants_see_role_and_tenant_grants_plain_ones_do_not() {
        let acl = MockAclDb::new();
        let user = Uuid::new_v4();
        let role = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let via_role = Uuid::new_v4();
        let via_tenant = Uuid::new_v4();
        let direct = Uuid::new_v4();

        acl.add_user_to_role(user, role);
        acl.add_user_to_tenant(user, tenant);
        acl.grant_permission(role, via_role, "read").await.unwrap();
        acl.grant_permission(tenant, via_tenant, "read")
            .await
            .unwrap();
        acl.grant_permission(user, direct, "read").await.unwrap();

        // Plain: direct only.
        assert!(acl.has_permission(user, direct, "read").await.unwrap());
        assert!(!acl.has_permission(user, via_role, "read").await.unwrap());
        assert!(!acl.has_permission(user, via_tenant, "read").await.unwrap());
        assert_eq!(
            acl.authorized_dataset_ids(user, "read").await.unwrap(),
            vec![direct]
        );

        // Roles-aware: direct ∪ tenant ∪ role.
        assert!(
            acl.has_permission_with_roles(user, direct, "read")
                .await
                .unwrap()
        );
        assert!(
            acl.has_permission_with_roles(user, via_role, "read")
                .await
                .unwrap()
        );
        assert!(
            acl.has_permission_with_roles(user, via_tenant, "read")
                .await
                .unwrap()
        );
        let mut all = acl
            .authorized_dataset_ids_with_roles(user, "read")
            .await
            .unwrap();
        all.sort();
        let mut expected = vec![direct, via_role, via_tenant];
        expected.sort();
        assert_eq!(all, expected);
    }

    /// The tenant gate in `inherited_principals`. `AclDb`'s contract scopes
    /// role grants to "each role the user holds **in those tenants**", so a
    /// user who holds a role but belongs to no tenant must inherit nothing
    /// from it. Without this test the gate is invisible: every other case
    /// here grants tenant membership alongside the role, so removing the
    /// gate leaves them all green while the mock silently becomes more
    /// permissive than any conforming backend.
    #[tokio::test]
    async fn a_role_grant_confers_nothing_without_tenant_membership() {
        let acl = MockAclDb::new();
        let tenantless = Uuid::new_v4();
        let role = Uuid::new_v4();
        let via_role = Uuid::new_v4();

        // A role membership and a grant to that role — but no tenant.
        acl.add_user_to_role(tenantless, role);
        acl.grant_permission(role, via_role, "read").await.unwrap();

        assert!(
            !acl.has_permission_with_roles(tenantless, via_role, "read")
                .await
                .unwrap(),
            "a role grant must not reach a user who belongs to no tenant"
        );
        assert!(
            acl.authorized_dataset_ids_with_roles(tenantless, "read")
                .await
                .unwrap()
                .is_empty(),
            "enumeration must agree with the point check"
        );

        // Joining any tenant activates the role the user already held.
        acl.add_user_to_tenant(tenantless, Uuid::new_v4());
        assert!(
            acl.has_permission_with_roles(tenantless, via_role, "read")
                .await
                .unwrap(),
            "once the user is in a tenant, the role grant applies"
        );
    }

    #[tokio::test]
    async fn with_roles_respects_permission_name_and_membership() {
        let acl = MockAclDb::new();
        let member = Uuid::new_v4();
        let stranger = Uuid::new_v4();
        let role = Uuid::new_v4();
        let ds = Uuid::new_v4();

        // Both users belong to a tenant: role inheritance is gated on tenant
        // membership, so without this the role below is ignored and every
        // assertion here would hold even if role grants were broken outright.
        acl.add_user_to_tenant(member, Uuid::new_v4());
        acl.add_user_to_tenant(stranger, Uuid::new_v4());
        acl.add_user_to_role(member, role);
        acl.grant_permission(role, ds, "delete").await.unwrap();

        // The grant that does exist is visible — this is what makes the
        // negatives below meaningful rather than vacuous.
        assert!(
            acl.has_permission_with_roles(member, ds, "delete")
                .await
                .unwrap()
        );
        // Wrong permission name on the same role grant does not count.
        assert!(
            !acl.has_permission_with_roles(member, ds, "read")
                .await
                .unwrap()
        );
        // A user who does not hold the role gets nothing from it, even though
        // they too belong to a tenant.
        assert!(
            !acl.has_permission_with_roles(stranger, ds, "delete")
                .await
                .unwrap()
        );
        assert!(
            acl.authorized_dataset_ids_with_roles(stranger, "delete")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn with_roles_deduplicates_a_dataset_granted_both_directly_and_via_role() {
        let acl = MockAclDb::new();
        let user = Uuid::new_v4();
        let role = Uuid::new_v4();
        let shared = Uuid::new_v4();
        let role_only = Uuid::new_v4();

        // Tenant membership is what activates the role grant at all.
        acl.add_user_to_tenant(user, Uuid::new_v4());
        acl.add_user_to_role(user, role);
        // `shared` is granted twice over; `role_only` just once, via the role.
        acl.grant_permission(user, shared, "read").await.unwrap();
        acl.grant_permission(role, shared, "read").await.unwrap();
        acl.grant_permission(role, role_only, "read").await.unwrap();

        // `role_only` is what makes this test able to fail: asserting solely
        // on the doubly-granted id cannot tell deduplication apart from the
        // role being dropped, since both yield exactly one id.
        let mut got = acl
            .authorized_dataset_ids_with_roles(user, "read")
            .await
            .unwrap();
        got.sort();
        let mut expected = vec![shared, role_only];
        expected.sort();
        assert_eq!(got, expected, "each id exactly once, role grants included");
    }
}
