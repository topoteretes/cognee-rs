# Implementation Plan: User / Authentication / Multi-Tenancy

> **Status (2026-06): COMPLETED.** The models, traits, ops, migrations, and
> default-user bootstrap described in this plan have been implemented. See the
> resolved status note at the top of [04-user-auth-tenancy.md](../04-user-auth-tenancy.md)
> for the as-built locations. File names and exact signatures may differ from
> the proposals below (e.g. the migration is
> `m20250422_000001_user_tenant_role_tables` with follow-ups
> `m20260428_000001_tenants_rbac` and `m20260512_000001_add_parent_user_id`).
> Retained as a historical record.

This plan covers closing the user management, role-based access control, and multi-tenancy gaps between the Python and Rust SDKs (see [04-user-auth-tenancy.md](../04-user-auth-tenancy.md)).

---

## Phase 1: Core Data Models

### Step 1.1 -- User, Tenant, Role structs in `cognee-models`

Create three new files.

**`crates/models/src/user.rs`**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A registered user. Corresponds to Python `cognee.modules.users.models.User`.
///
/// Fields intentionally omit `hashed_password` -- the Rust SDK does not
/// implement authentication (see non-goal note in the gap doc). Password
/// handling is delegated to whatever HTTP/auth layer sits on top.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub is_active: bool,
    pub is_superuser: bool,
    /// The user's currently-selected tenant (can be `None` for the
    /// single-user default tenant).
    pub tenant_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}
```

**`crates/models/src/tenant.rs`**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// An organizational tenant. Corresponds to Python
/// `cognee.modules.users.models.Tenant`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    /// The user who created/owns this tenant.
    pub owner_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}
```

**`crates/models/src/role.rs`**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A named role scoped to a tenant. Corresponds to Python
/// `cognee.modules.users.models.Role`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Role {
    pub id: Uuid,
    pub name: String,
    pub tenant_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}
```

**`crates/models/src/permission.rs`**

```rust
/// Canonical permission names matching Python's `PERMISSION_TYPES`.
pub mod permissions {
    pub const READ: &str = "read";
    pub const WRITE: &str = "write";
    pub const DELETE: &str = "delete";
    pub const SHARE: &str = "share";

    pub const ALL: &[&str] = &[READ, WRITE, DELETE, SHARE];
}
```

**Modify `crates/models/src/lib.rs`** -- add `pub mod user; pub mod tenant; pub mod role; pub mod permission;` and re-export the four types and the `permissions` module.

---

## Phase 2: Database Traits

### Step 2.1 -- `UserDb` trait

**`crates/database/src/traits/user_db.rs`**

```rust
use async_trait::async_trait;
use cognee_models::User;
use uuid::Uuid;

use crate::types::DatabaseError;

/// CRUD operations for `User` rows.
#[async_trait]
pub trait UserDb: Send + Sync {
    async fn get_user(&self, id: Uuid) -> Result<Option<User>, DatabaseError>;
    async fn get_user_by_email(&self, email: &str) -> Result<Option<User>, DatabaseError>;
    async fn create_user(&self, user: &User) -> Result<User, DatabaseError>;
    async fn update_user(&self, user: &User) -> Result<User, DatabaseError>;
    async fn delete_user(&self, id: Uuid) -> Result<(), DatabaseError>;
    async fn list_users(&self, tenant_id: Option<Uuid>) -> Result<Vec<User>, DatabaseError>;
}
```

### Step 2.2 -- `TenantDb` trait

**`crates/database/src/traits/tenant_db.rs`**

```rust
use async_trait::async_trait;
use cognee_models::{Tenant, User};
use uuid::Uuid;

use crate::types::DatabaseError;

/// CRUD and membership operations for tenants.
#[async_trait]
pub trait TenantDb: Send + Sync {
    async fn create_tenant(&self, tenant: &Tenant) -> Result<Tenant, DatabaseError>;
    async fn get_tenant(&self, id: Uuid) -> Result<Option<Tenant>, DatabaseError>;
    async fn list_tenants_for_user(&self, user_id: Uuid) -> Result<Vec<Tenant>, DatabaseError>;
    async fn add_user_to_tenant(&self, user_id: Uuid, tenant_id: Uuid) -> Result<(), DatabaseError>;
    async fn remove_user_from_tenant(&self, user_id: Uuid, tenant_id: Uuid) -> Result<(), DatabaseError>;

    /// Switch the user's active tenant. Validates the user is a member.
    /// Passing `None` reverts to the default single-user tenant.
    async fn select_tenant(&self, user_id: Uuid, tenant_id: Option<Uuid>) -> Result<User, DatabaseError>;
}
```

### Step 2.3 -- `RoleDb` trait

**`crates/database/src/traits/role_db.rs`**

```rust
use async_trait::async_trait;
use cognee_models::Role;
use uuid::Uuid;

use crate::types::DatabaseError;

/// CRUD and membership operations for roles (scoped to a tenant).
#[async_trait]
pub trait RoleDb: Send + Sync {
    async fn create_role(&self, role: &Role) -> Result<Role, DatabaseError>;
    async fn get_role(&self, id: Uuid) -> Result<Option<Role>, DatabaseError>;
    async fn list_roles_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<Role>, DatabaseError>;
    async fn assign_user_to_role(&self, user_id: Uuid, role_id: Uuid) -> Result<(), DatabaseError>;
    async fn remove_user_from_role(&self, user_id: Uuid, role_id: Uuid) -> Result<(), DatabaseError>;
    async fn get_user_roles(&self, user_id: Uuid, tenant_id: Uuid) -> Result<Vec<Role>, DatabaseError>;
}
```

### Step 2.4 -- Extend `AclDb` for role-based permission inheritance

Add two new methods to the existing `AclDb` trait. These resolve the
user -> tenant -> role chain that Python implements in
`get_all_user_permission_datasets()`.

```rust
// Added to the existing AclDb trait in crates/database/src/traits/acl_db.rs

    /// Check permission considering role and tenant inheritance.
    ///
    /// Resolution order (mirrors Python `get_all_user_permission_datasets`):
    /// 1. Direct user ACL
    /// 2. Tenant-level ACL for each tenant the user belongs to
    /// 3. Role-level ACL for each role the user holds in those tenants
    ///
    /// Requires `UserDb` / `TenantDb` / `RoleDb` to be available -- the
    /// `DatabaseConnection` impl will have all of these.
    async fn has_permission_with_roles(
        &self,
        user_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<bool, DatabaseError>;

    /// Return all dataset IDs the user can access via direct, tenant, or
    /// role grants. Filters to datasets matching the user's current
    /// `tenant_id` (like Python's `dataset.tenant_id == user.tenant_id`).
    async fn authorized_dataset_ids_with_roles(
        &self,
        user_id: Uuid,
        permission_name: &str,
    ) -> Result<Vec<Uuid>, DatabaseError>;
```

**Modify `crates/database/src/traits/mod.rs`** -- add the three new trait
modules and re-export them. Update `crates/database/src/lib.rs` to
re-export the new traits.

---

## Phase 3: Database Migrations

Add a new migration file:
`crates/database/src/migrator/m20250301_000001_user_tenant_role_tables.rs`

### Tables to create

1. **`users`** -- `id TEXT PK FK->principals.id`, `email TEXT NOT NULL UNIQUE`, `is_active BOOL NOT NULL DEFAULT true`, `is_superuser BOOL NOT NULL DEFAULT false`, `tenant_id TEXT FK->tenants.id NULL`, `created_at TIMESTAMPTZ NOT NULL`, `updated_at TIMESTAMPTZ NULL`

2. **`tenants`** -- `id TEXT PK FK->principals.id`, `name TEXT NOT NULL UNIQUE`, `owner_id TEXT NOT NULL` (no FK to users to avoid circular), `created_at TIMESTAMPTZ NOT NULL`, `updated_at TIMESTAMPTZ NULL`

3. **`roles`** -- `id TEXT PK FK->principals.id`, `name TEXT NOT NULL`, `tenant_id TEXT NOT NULL FK->tenants.id`, `created_at TIMESTAMPTZ NOT NULL`, `updated_at TIMESTAMPTZ NULL`, UNIQUE(`tenant_id`, `name`)

4. **`user_tenants`** -- `user_id TEXT FK->users.id`, `tenant_id TEXT FK->tenants.id`, `created_at TIMESTAMPTZ NOT NULL`, PK(`user_id`, `tenant_id`)

5. **`user_roles`** -- `user_id TEXT FK->users.id`, `role_id TEXT FK->roles.id`, `created_at TIMESTAMPTZ NOT NULL`, PK(`user_id`, `role_id`)

### Seed data

After table creation, seed a default user to match the existing
`default_user_id = "00000000-0000-0000-0000-000000000000"`:

```sql
-- Ensure the default principal exists (may already be there from ACL migration).
INSERT INTO principals (id, type, created_at)
VALUES ('00000000000000000000000000000000', 'user', datetime('now'))
ON CONFLICT (id) DO NOTHING;

INSERT INTO users (id, email, is_active, is_superuser, tenant_id, created_at)
VALUES ('00000000000000000000000000000000', 'default_user@example.com', true, true, NULL, datetime('now'))
ON CONFLICT (id) DO NOTHING;
```

### Register migration

Add the new migration to `crates/database/src/migrator/mod.rs` in the `Migrator::migrations()` vec.

---

## Phase 4: SeaORM Entities

### New entity files

- `crates/database/src/entities/user.rs` -- SeaORM model for `users` table
- `crates/database/src/entities/tenant.rs` -- SeaORM model for `tenants` table
- `crates/database/src/entities/role.rs` -- SeaORM model for `roles` table
- `crates/database/src/entities/user_tenant.rs` -- SeaORM model for `user_tenants` junction table
- `crates/database/src/entities/user_role.rs` -- SeaORM model for `user_roles` junction table

All entities follow the existing pattern in `crates/database/src/entities/` (String IDs stored as hex UUIDs, `uuid_hex` conversion utilities).

### Modify `crates/database/src/entities/mod.rs`

Add the five new modules.

---

## Phase 5: Trait Implementations

### Step 5.1 -- `UserDb` impl for `DatabaseConnection`

New file: `crates/database/src/ops/user.rs`

Standard SeaORM CRUD using the `user` entity. `get_user_by_email` queries
by email column. `list_users` optionally filters by `tenant_id`.

### Step 5.2 -- `TenantDb` impl for `DatabaseConnection`

New file: `crates/database/src/ops/tenant.rs`

`select_tenant`:
1. Look up user.
2. If `tenant_id` is `None`, set user.tenant_id to NULL and return.
3. Verify membership in `user_tenants`.
4. Update user.tenant_id and return.

### Step 5.3 -- `RoleDb` impl for `DatabaseConnection`

New file: `crates/database/src/ops/role.rs`

`create_role` also inserts a corresponding row in `principals` (type = "role").
`assign_user_to_role` inserts into `user_roles`.

### Step 5.4 -- `AclDb` role-aware methods impl

In `crates/database/src/ops/acl.rs`, add:

`has_permission_with_roles`:
1. Check direct user ACL (existing `has_permission`).
2. If not found, query `user_tenants` for user's tenants; check tenant-level ACL.
3. If not found, query `user_roles` for user's roles; check role-level ACL.
4. Return `true` if any match.

`authorized_dataset_ids_with_roles`:
1. Collect dataset IDs from user direct ACL.
2. Collect dataset IDs from each tenant ACL.
3. Collect dataset IDs from each role ACL.
4. Deduplicate.
5. Filter to datasets whose `tenant_id` matches the user's current `tenant_id`
   (read from `users` table). This mirrors Python's
   `dataset.tenant_id == user.tenant_id` filter.

---

## Phase 6: Default User Management

### Step 6.1 -- `get_or_create_default_user()`

New file: `crates/lib/src/api/user.rs`

```rust
use cognee_database::UserDb;
use cognee_models::User;
use uuid::Uuid;
use chrono::Utc;

/// Retrieve the default user, creating it if it doesn't exist.
///
/// Mirrors Python's `get_default_user()` / `create_default_user()`.
/// Uses deterministic UUID5 from the email so re-runs are idempotent.
pub async fn get_or_create_default_user(
    db: &dyn UserDb,
    email: &str,
) -> Result<User, Box<dyn std::error::Error>> {
    if let Some(user) = db.get_user_by_email(email).await? {
        return Ok(user);
    }
    let user = User {
        id: Uuid::new_v5(&Uuid::NAMESPACE_OID, email.as_bytes()),
        email: email.to_string(),
        is_active: true,
        is_superuser: true,
        tenant_id: None,
        created_at: Utc::now(),
        updated_at: None,
    };
    db.create_user(&user).await.map_err(Into::into)
}
```

### Step 6.2 -- Config update

In `crates/lib/src/config.rs`, add `default_user_email: String` field to
`Settings` with default value `"default_user@example.com"`. Read from
`COGNEE_DEFAULT_USER_EMAIL` env var in `overlay_from_env()`.

Also update `crates/cli/src/config_store.rs` to support the new setting.

### Step 6.3 -- CLI integration

In each CLI command (`add`, `cognify`, `search`, `delete`, `memify`,
`add-and-cognify`), replace the raw `Uuid::parse_str(&settings.default_user_id)`
pattern with:
1. Call `get_or_create_default_user(db, &settings.default_user_email)`.
2. Use `user.id` as `owner_id`.
3. Maintain backward compatibility: if `default_user_id` is set to a
   non-zero UUID, prefer it (the migration seeds the default user with
   the all-zeros UUID, so existing configs continue working).

---

## Phase 7: Pipeline Integration

### Step 7.1 -- Ingestion pipeline

`AddPipeline` currently accepts `owner_id: Uuid`. Two options:
- **Option A (minimal):** Keep `owner_id` parameter; callers resolve user
  before calling. Add `with_user_db()` builder method for auto-resolving.
- **Option B (breaking):** Change to accept `&User`. This is cleaner but
  breaks all call sites.

Recommended: **Option A** for backward compatibility. Add an optional
`user_db: Option<Arc<dyn UserDb>>` field. When set, the pipeline can
resolve the user and validate permissions.

### Step 7.2 -- Search pipeline

`SearchBuilder` currently takes `owner_id: Uuid`. Similarly, add an
optional `with_user_db()` method. When both `user_db` and `acl_db` are
set, use `authorized_dataset_ids_with_roles()` instead of
`authorized_dataset_ids()`.

### Step 7.3 -- Delete pipeline

`DeleteService` takes `owner_id`. Same pattern -- optional `UserDb`
for user-aware deletion with role-based permission checks.

---

## Phase 8: Mock Implementations

### `cognee-test-utils`

Add mock implementations:

- `MockUserDb` -- HashMap-based, keyed by UUID and email
- `MockTenantDb` -- HashMap-based
- `MockRoleDb` -- HashMap-based

These enable unit testing without a real database.

---

## File Summary

### Files to create

| File | Purpose |
|------|---------|
| `crates/models/src/user.rs` | `User` struct |
| `crates/models/src/tenant.rs` | `Tenant` struct |
| `crates/models/src/role.rs` | `Role` struct |
| `crates/models/src/permission.rs` | Permission constants |
| `crates/database/src/traits/user_db.rs` | `UserDb` trait |
| `crates/database/src/traits/tenant_db.rs` | `TenantDb` trait |
| `crates/database/src/traits/role_db.rs` | `RoleDb` trait |
| `crates/database/src/entities/user.rs` | SeaORM entity |
| `crates/database/src/entities/tenant.rs` | SeaORM entity |
| `crates/database/src/entities/role.rs` | SeaORM entity |
| `crates/database/src/entities/user_tenant.rs` | SeaORM junction entity |
| `crates/database/src/entities/user_role.rs` | SeaORM junction entity |
| `crates/database/src/migrator/m20250301_000001_user_tenant_role_tables.rs` | Migration |
| `crates/database/src/ops/user.rs` | `UserDb` impl |
| `crates/database/src/ops/tenant.rs` | `TenantDb` impl |
| `crates/database/src/ops/role.rs` | `RoleDb` impl |
| `crates/lib/src/api/user.rs` | `get_or_create_default_user()` |
| `crates/test-utils/src/mock_user_db.rs` | Mock `UserDb` |
| `crates/test-utils/src/mock_tenant_db.rs` | Mock `TenantDb` |
| `crates/test-utils/src/mock_role_db.rs` | Mock `RoleDb` |

### Files to modify

| File | Change |
|------|--------|
| `crates/models/src/lib.rs` | Re-export new modules |
| `crates/database/src/traits/mod.rs` | Register new trait modules |
| `crates/database/src/traits/acl_db.rs` | Add `has_permission_with_roles`, `authorized_dataset_ids_with_roles` |
| `crates/database/src/entities/mod.rs` | Register new entity modules |
| `crates/database/src/ops/mod.rs` | Register new ops modules |
| `crates/database/src/ops/acl.rs` | Implement role-aware permission methods |
| `crates/database/src/migrator/mod.rs` | Register new migration |
| `crates/database/src/lib.rs` | Re-export new traits and types |
| `crates/lib/src/config.rs` | Add `default_user_email` field |
| `crates/lib/src/api/mod.rs` | Register user module (create if needed) |
| `crates/lib/src/lib.rs` | Re-export user API |
| `crates/cli/src/config_store.rs` | Support `default_user_email` setting |
| `crates/cli/src/commands/add.rs` | Use `get_or_create_default_user()` |
| `crates/cli/src/commands/cognify.rs` | Use `get_or_create_default_user()` |
| `crates/cli/src/commands/search.rs` | Use `get_or_create_default_user()` |
| `crates/cli/src/commands/delete.rs` | Use `get_or_create_default_user()` |
| `crates/cli/src/commands/memify.rs` | Use `get_or_create_default_user()` |
| `crates/cli/src/commands/add_and_cognify.rs` | Use `get_or_create_default_user()` |
| `crates/ingestion/src/pipeline.rs` | Optional `with_user_db()` builder |
| `crates/search/src/...` | Optional role-aware permission resolution |
| `crates/test-utils/src/lib.rs` | Re-export new mocks |

---

## Sequencing and Dependencies

```
Phase 1 (models)
    |
    v
Phase 2 (traits)  +  Phase 3 (migrations)
    |                      |
    v                      v
Phase 4 (entities) --------+
    |
    v
Phase 5 (trait impls)
    |
    v
Phase 6 (default user) + Phase 8 (mocks)
    |
    v
Phase 7 (pipeline integration)
```

Phases 1-5 must be sequential. Phase 6 and 8 can proceed in parallel
once Phase 5 is done. Phase 7 comes last since it depends on everything.

---

## Non-Goals (Explicitly Out of Scope)

- **Authentication middleware** (JWT, API keys, OAuth2, cookies) -- HTTP-server concern. The library-level API accepts `User` or `user_id: Uuid` parameters; any future HTTP server plugs in its own auth.
- **Password hashing** -- The `User` struct intentionally omits `hashed_password`. Authentication is deferred to the HTTP layer.
- **`UserApiKey` model** -- API key management is an HTTP-server concern.
- **`PrincipalConfiguration` model** -- Per-principal config storage. Can be added later when needed.
- **`DatasetDatabase` model** -- Per-dataset database routing. Not yet relevant (Rust uses a single-database model).
- **Default permission tables** (`UserDefaultPermissions`, `TenantDefaultPermissions`, `RoleDefaultPermissions`) -- Python uses these for auto-granting permissions when users/tenants/roles are created. Can be added as a follow-up if needed; for now, explicit grant calls suffice.
