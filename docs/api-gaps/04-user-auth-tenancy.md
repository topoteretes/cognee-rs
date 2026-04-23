# Gap 4: User / Authentication / Multi-Tenancy

This document details the user management, authentication, and multi-tenancy capabilities present in the Python SDK that are absent from the Rust implementation.

**Implementation plan:** [impl/04-user-auth-tenancy-plan.md](impl/04-user-auth-tenancy-plan.md)

---

## Python User Architecture

### Data Model

**Polymorphic Principal Hierarchy** (SQLAlchemy single-table inheritance via `type` discriminator):

```
Principal (base)         -- principals table
  |-- User               -- users table (FK -> principals.id)
  |-- Tenant             -- tenants table (FK -> principals.id)
  |-- Role               -- roles table (FK -> principals.id)
```

| Model | File | Key Fields |
|-------|------|------------|
| `Principal` | `cognee/modules/users/models/Principal.py` | `id: UUID PK`, `created_at`, `updated_at`, `type: str` (polymorphic discriminator) |
| `User` | `cognee/modules/users/models/User.py` | Inherits Principal + `email`, `hashed_password` (via `SQLAlchemyBaseUserTableUUID`), `is_active`, `is_verified`, `is_superuser`, `tenant_id: FK(tenants)`, `roles: M2M(user_roles)`, `tenants: M2M(user_tenants)`, `acls: O2M` |
| `Tenant` | `cognee/modules/users/models/Tenant.py` | Inherits Principal + `name: str UNIQUE`, `owner_id: UUID` (no FK), `users: M2M(user_tenants)`, `roles: O2M` |
| `Role` | `cognee/modules/users/models/Role.py` | Inherits Principal + `name: str`, `tenant_id: FK(tenants) NOT NULL`, `users: M2M(user_roles)`, UNIQUE(`tenant_id`, `name`) |

**Junction tables:**
- `user_tenants` (`user_id FK(users)`, `tenant_id FK(tenants)`, `created_at`) -- `cognee/modules/users/models/UserTenant.py`
- `user_roles` (`user_id FK(users)`, `role_id FK(roles)`, `created_at`) -- `cognee/modules/users/models/UserRole.py`

**ACL Model** (`cognee/modules/users/models/ACL.py`):

```
ACL: { id, principal_id FK(principals), permission_id FK(permissions), dataset_id FK(datasets) }
```

The `principal_id` can point to a User, Tenant, or Role (all share the `principals` base table).

**Permission Model** (`cognee/modules/users/models/Permission.py`):

```
Permission: { id, name (unique), created_at, updated_at }
```

**Permission Types** (`cognee/modules/users/permissions/permission_types.py`):

```python
PERMISSION_TYPES = ["read", "write", "delete", "share"]
```

**Default permission tables** (auto-grant when creating principals):
- `user_default_permissions` (`user_id`, `permission_id`)
- `tenant_default_permissions` (`tenant_id`, `permission_id`)
- `role_default_permissions` (`role_id`, `permission_id`)

**Additional models (not in scope for initial Rust port):**
- `UserApiKey` -- API key management for users
- `PrincipalConfiguration` -- Per-principal JSON configuration storage
- `DatasetDatabase` -- Per-dataset database routing metadata

### Default User Creation

**File:** `cognee/modules/users/methods/create_default_user.py`

```python
async def create_default_user():
    email = base_config.default_user_email or "default_user@example.com"
    password = base_config.default_user_password or "default_password"
    user = await create_user(
        email=email, password=password,
        is_superuser=True, is_active=True, is_verified=True, auto_login=True,
    )
    return user
```

**File:** `cognee/modules/users/methods/get_default_user.py`

- Queries DB for user by email; if not found, calls `create_default_user()`
- On first call, seeds the database with a default user
- Caches the result for the process lifetime

### Permission Resolution Flow

The Python permission chain is in `get_all_user_permission_datasets()`:

1. **Direct user ACL** -- `get_principal_datasets(user, permission_type)` queries ACL rows where `principal_id = user.id`
2. **Tenant-level ACL** -- For each tenant the user belongs to (via `user_tenants`), queries ACL rows where `principal_id = tenant.id`
3. **Role-level ACL** -- For each role the user holds (via `user_roles`), queries ACL rows where `principal_id = role.id`
4. **Deduplication** -- Merges results, deduplicates by dataset ID
5. **Tenant filtering** -- Only returns datasets whose `tenant_id` matches the user's current `tenant_id`

**Dataset creation** auto-grants owner "read", "write", "delete" permissions.

**Cross-user sharing** uses explicit `grant_permission(principal_id, dataset_id, permission_name)`.

### Tenant Isolation

- User has `tenant_id` field (current active tenant)
- `select_tenant(user_id, tenant_id)` validates membership in `user_tenants`, then updates `user.tenant_id`
- Passing `None` reverts to the default single-user tenant
- All dataset queries filter by `tenant_id` when access control is enabled

### User Management Permissions

`has_user_management_permission()` checks if a requester can manage users in a tenant:
- Tenant owner is always allowed
- Users with "admin" role in that tenant are allowed
- Extensible via `USER_MANAGEMENT_ALLOWED_ROLE_NAMES` frozenset

---

## Rust Current State

### No User Model

The Rust codebase has no `User`, `Tenant`, or `Role` structs. User identity is represented as a raw UUID string in configuration.

**`crates/lib/src/config.rs`** (line 10):
```rust
pub default_user_id: String,  // Default: "00000000-0000-0000-0000-000000000000"
```

Every CLI command parses this string into a UUID:
```rust
let owner_id = Uuid::parse_str(&cm.settings().default_user_id)?;
```

There is no `default_user_email` config field and no auto-creation of a user record.

### AclDb Trait

**File:** `crates/database/src/traits/acl_db.rs`

```rust
pub trait AclDb: Send + Sync {
    async fn has_permission(&self, principal_id: Uuid, dataset_id: Uuid, permission_name: &str) -> Result<bool, DatabaseError>;
    async fn authorized_dataset_ids(&self, principal_id: Uuid, permission_name: &str) -> Result<Vec<Uuid>, DatabaseError>;
    async fn grant_permission(&self, principal_id: Uuid, dataset_id: Uuid, permission_name: &str) -> Result<(), DatabaseError>;
    async fn revoke_permission(&self, principal_id: Uuid, dataset_id: Uuid, permission_name: &str) -> Result<(), DatabaseError>;
    async fn ensure_principal(&self, principal_id: Uuid, principal_type: &str) -> Result<(), DatabaseError>;
}
```

Implemented for `sea_orm::DatabaseConnection`. This provides flat principal-to-dataset permission checks with no role or tenant inheritance.

### ACL Database Tables (Already Exist)

The `m20250201_000001_acl_tables` migration creates:
- **`principals`** table (`id TEXT PK`, `type TEXT`, `created_at`, `updated_at`)
- **`permissions`** table (`id TEXT PK`, `name TEXT UNIQUE`, `created_at`, `updated_at`) -- seeded with read/write/delete/share
- **`acls`** table (`id TEXT PK`, `principal_id FK(principals)`, `permission_id FK(permissions)`, `dataset_id FK(datasets CASCADE)`, `created_at`, `updated_at`) with unique index on (`principal_id`, `permission_id`, `dataset_id`)

The migration also retroactively grants all four permissions to existing dataset owners.

### Permission Constants

`crates/database/src/ops/acl.rs` defines:
```rust
pub const PERMISSION_NAMES: &[&str] = &["read", "write", "delete", "share"];
```

These match Python's `PERMISSION_TYPES` but are defined only in the `ops` module, not as a public model constant.

### Default Permission Grants

`AddPipeline::with_acl_db()` enables auto-granting all four permissions to the owner when a new dataset is created. This is wired up in the ingestion pipeline and matches Python's `create_authorized_dataset()` behavior.

---

## Gap Analysis

| # | Feature | Python | Rust | Status |
|---|---------|--------|------|--------|
| 1 | **User model** | Full `User` class (email, password, roles, tenants) | No User struct; raw UUID from config | **Missing** |
| 2 | **Default user** | Auto-created in DB on first API call, cached | Static UUID string from config, no DB record | **Missing** |
| 3 | **User CRUD** | create, read, update, delete users | None | **Missing** |
| 4 | **Password hashing** | Via `fastapi-users` (`SQLAlchemyBaseUserTableUUID`) | None (out of scope -- HTTP concern) | **Out of scope** |
| 5 | **Tenant model** | `Tenant` class with name, owner, users M2M | No struct; `tenant_id` passed as `Option<Uuid>` parameter | **Missing** |
| 6 | **Tenant CRUD** | create, list, select, add/remove users | None | **Missing** |
| 7 | **Role model** | `Role` class with name, tenant FK, users M2M | None | **Missing** |
| 8 | **Role CRUD** | create role, assign user to role, list roles | None | **Missing** |
| 9 | **ACL model** | `ACL` class linking principal -> permission -> dataset | `AclDb` trait with `principals`, `permissions`, `acls` tables | **Partial** |
| 10 | **Permission constants** | `PERMISSION_TYPES = ["read", "write", "delete", "share"]` | `PERMISSION_NAMES` in `ops::acl` + seeded DB rows | **Partial** (not in models crate) |
| 11 | **Permission inheritance** | User -> Tenant -> Role chain via `get_all_user_permission_datasets()` | Flat principal -> dataset only (no role/tenant resolution) | **Missing** |
| 12 | **Default permissions on dataset creation** | Auto-grant read/write/delete to owner | Via `AddPipeline::with_acl_db()` grants all four | **Implemented** |
| 13 | **Cross-user sharing** | `grant_permission()` API | `AclDb::grant_permission()` trait method | **Implemented** |
| 14 | **Tenant switching** | `select_tenant(user_id, tenant_id)` with membership validation | Not supported | **Missing** |
| 15 | **Authentication** | JWT, API keys, cookies, OAuth2 via `fastapi-users` | None (out of scope -- HTTP concern) | **Out of scope** |
| 16 | **User management permissions** | Role-based (`has_user_management_permission`) | None | **Missing** |
| 17 | **Default permission tables** | `user_default_permissions`, `tenant_default_permissions`, `role_default_permissions` | None | **Missing** (deferred) |

---

## Note on Authentication

Authentication (JWT, API keys, OAuth2) is an HTTP-server concern. Since the Rust SDK targets library and CLI usage, authentication middleware is **out of scope** for this gap. The user model and permission system should be designed to be authentication-agnostic so that any future HTTP server can plug in its own auth strategy.

The library-level API should accept `User` or `user_id: Uuid` parameters instead of relying on authentication context. The CLI can continue using `default_user_id` from config for backward compatibility.
