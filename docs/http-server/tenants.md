# HTTP Server — Multi-Tenant Schema & Permission Model

Specification for the relational schema and permission-resolution logic that backs the Rust HTTP server's multi-tenant story. Mirrors Python's [`cognee/modules/users/models/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/users/models) and [`permissions/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/users/permissions) layout so a Python-seeded database is readable by Rust and vice versa.

Companion docs: [architecture.md](architecture.md), [auth.md](auth.md). Per-route contracts for `/api/v1/permissions/*` will live in `routers/permissions.md` (separate doc).

## 1. Goals & non-goals

### Goals

- **Schema parity with Python**: same table names, same column names, same data types, same constraints. A Rust server connecting to a Python-populated DB sees the same rows it would see from Python.
- **Polymorphic principal model**: users, tenants, and roles all inherit from a single `principals` table, matching SQLAlchemy's [`polymorphic_identity`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/Principal.py) pattern.
- **Per-dataset ACLs** with optional default permissions at user/role/tenant scope. Resolves a `(user, dataset, permission)` query in a small bounded number of joins.
- **Tenant ownership semantics**: every tenant has an `owner_id`; a user can be a member of multiple tenants but has exactly one *current* tenant (`users.tenant_id`).
- **Idempotent migration**: if any of these tables already exist (Python schema is on disk), the SeaORM migration is a no-op for them; if they don't, it creates them with identical shape.

### Non-goals

- **Per-row dataset ACLs beyond what Python implements**: e.g. column-level or attribute-based access control. Out of scope.
- **External identity providers** (LDAP, SAML, OIDC group sync). Out of scope.
- **Permission UI / admin tooling beyond the existing endpoints**. We expose the same routes Python exposes — front-end work happens in `cognee-frontend` separately.

## 2. Conceptual model

```
                        ┌──────────────┐
                        │  principals  │   polymorphic super-table
                        │  type ∈ {    │
                        │   user,      │
                        │   tenant,    │
                        │   role       │
                        │  }           │
                        └──────┬───────┘
                               │ id (PK)
                ┌──────────────┼─────────────────┐
                │              │                 │
        ┌───────▼──────┐ ┌─────▼─────┐ ┌─────────▼────────┐
        │    users     │ │  tenants  │ │      roles       │
        │ email        │ │ name      │ │ name             │
        │ tenant_id ◄──┼─┤ owner_id  │ │ tenant_id ───────┼─┐
        │ hashed_pw    │ │           │ │ UNIQUE (tenant,  │ │
        │ is_*         │ │           │ │         name)    │ │
        └─┬───────┬────┘ └─┬─────────┘ └─────────┬────────┘ │
          │       │        │                     │          │
          │       │        │                     │          │
          │       │        │                     │          │
   user_roles  user_tenants│              role_default_permissions
   (user,role) (user,tnt)  │              (role, permission)
                           │                     │
                           │                     │
                  tenant_default_permissions     │
                  (tenant, permission)           │
                                                 │
                  user_default_permissions       │
                  (user, permission)             │
                                                 │
                                                 ▼
                                        ┌──────────────┐
                                        │ permissions  │
                                        │ name UNIQUE  │
                                        │ (read/write/ │
                                        │  delete/share│
                                        │  )           │
                                        └──────┬───────┘
                                               │
                                               │
                                       ┌───────▼──────┐
                                       │     acls     │  per-dataset grant
                                       │ principal_id │  (any principal type)
                                       │ permission_id│
                                       │ dataset_id   │
                                       └──────────────┘
```

`acls` is the per-dataset table: principal × permission × dataset. The three `*_default_permissions` tables are scope-level grants that apply *across all datasets in the relevant scope* (the user's, the role's, or the tenant's). Both layers participate in resolution (§5).

## 3. Tables

All UUID columns are SQL `UUID`. `created_at` / `updated_at` are `TIMESTAMPTZ`. The schema lives in the single baseline migration `crates/database/src/migrator/m20260914_000001_baseline.rs` and is idempotent against a Python-seeded DB.

### 3.1 `principals` (super-table)

| Column | Type | Constraints | Source |
|---|---|---|---|
| `id` | UUID | PK, INDEX, default `uuid4()` | [`Principal.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/Principal.py) |
| `created_at` | TIMESTAMPTZ | NOT NULL, DEFAULT NOW() | — |
| `updated_at` | TIMESTAMPTZ | NULL, ON UPDATE NOW() | — |
| `type` | TEXT | NOT NULL | `'user' \| 'tenant' \| 'role'` |

`users.id`, `tenants.id`, `roles.id` are all `FK → principals.id`. The `type` discriminator is what SQLAlchemy uses for [`polymorphic_identity`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/Principal.py#L17-L19); SeaORM doesn't have polymorphic loading natively, so we treat the three child tables as independent and select-by-type when needed.

### 3.2 `users`

| Column | Type | Constraints | Source |
|---|---|---|---|
| `id` | UUID | PK, FK → `principals.id` ON DELETE CASCADE | [`User.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/User.py) |
| `email` | VARCHAR(320) | UNIQUE, NOT NULL | fastapi-users |
| `hashed_password` | VARCHAR(1024) | NOT NULL | fastapi-users |
| `is_active` | BOOLEAN | NOT NULL DEFAULT TRUE | — |
| `is_superuser` | BOOLEAN | NOT NULL DEFAULT FALSE | — |
| `is_verified` | BOOLEAN | NOT NULL DEFAULT TRUE | Python's [`UserCreate`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/User.py#L48-L50) defaults to `True` |
| `tenant_id` | UUID | NULL, FK → `tenants.id` | the user's *current* tenant |

Indexes: `email` UNIQUE, `tenant_id`. See [auth.md §11](auth.md#11-database-schema-seaorm-migration) for the wider auth context.

### 3.3 `tenants`

| Column | Type | Constraints | Source |
|---|---|---|---|
| `id` | UUID | PK, FK → `principals.id` | [`Tenant.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/Tenant.py) |
| `name` | VARCHAR | UNIQUE, NOT NULL, INDEX | — |
| `owner_id` | UUID | NULL, INDEX | the user who created the tenant |

`owner_id` is *not* a hard FK — Python's column has no `ForeignKey(...)` constraint. Match this so cascades behave identically. Application logic enforces the integrity.

> **Implementation divergence (landed in P5, commits aefb105 + 2652aea):** the
> user/tenant/role tables (since consolidated into the single baseline migration
> `crates/database/src/migrator/m20260914_000001_baseline.rs`) declared
> `owner_id` as `TEXT NOT NULL` rather than the `NULL` shape this section
> specifies. We did **not** re-issue the column at P5 (would have required a
> destructive `ALTER` or a churn migration). Instead, the `bootstrap_default_principals`
> entrypoint inserts the `default_tenant` row with `owner_id` set to the
> default-user id as a placeholder. Application code never depends on
> `owner_id` being meaningful for the default tenant, and the shape stays
> compatible with Python writers (Python always populates `owner_id` because
> every tenant it creates has a creator). Re-aligning the column with Python's
> nullability is tracked as a future migration. Cross-reference:
> [implementation/p5-admin.md §1.1](implementation/p5-admin.md#11-implementation-divergences-recorded-post-landing).

### 3.4 `roles`

| Column | Type | Constraints | Source |
|---|---|---|---|
| `id` | UUID | PK, FK → `principals.id` ON DELETE CASCADE | [`Role.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/Role.py) |
| `name` | VARCHAR | NOT NULL, INDEX | role name (e.g. `"admin"`) |
| `tenant_id` | UUID | NOT NULL, FK → `tenants.id` | scoping tenant |

Unique constraint: `UNIQUE (tenant_id, name)` — `uq_roles_tenant_id_name`. A role name is unique *within* a tenant; two tenants can both have an `"admin"` role.

Special-case role names with extra privileges live in [`permission_types.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/permissions/permission_types.py): `USER_MANAGEMENT_ALLOWED_ROLE_NAMES = {"admin"}`. Members of these roles are allowed to manage tenant membership beyond the tenant owner. Encoded as a constant in `cognee-database` so changes track Python.

### 3.5 `user_roles` (M2M)

| Column | Type | Constraints |
|---|---|---|
| `user_id` | UUID | PK, FK → `users.id` |
| `role_id` | UUID | PK, FK → `roles.id` |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT NOW() |

Composite PK `(user_id, role_id)`. Source: [`UserRole.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/UserRole.py).

### 3.6 `user_tenants` (M2M)

| Column | Type | Constraints |
|---|---|---|
| `user_id` | UUID | PK, FK → `users.id` |
| `tenant_id` | UUID | PK, FK → `tenants.id` |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT NOW() |

Composite PK. Models a user's *membership* in tenants (multiple). Distinct from `users.tenant_id`, which is the user's *current* tenant. Switching tenants updates `users.tenant_id`; it does not modify `user_tenants`.

### 3.7 `permissions` (lookup)

| Column | Type | Constraints |
|---|---|---|
| `id` | UUID | PK, default `uuid4()` |
| `name` | VARCHAR | UNIQUE, NOT NULL, INDEX |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT NOW() |
| `updated_at` | TIMESTAMPTZ | NULL ON UPDATE NOW() |

Seeded with one row per name in [`PERMISSION_TYPES = ["read", "write", "delete", "share"]`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/permissions/permission_types.py). The migration's "down" path does not delete these rows because dropping `permissions` would invalidate `acls`; "down" is a no-op.

### 3.8 `acls` — per-dataset grants

| Column | Type | Constraints |
|---|---|---|
| `id` | UUID | PK, default `uuid4()` |
| `principal_id` | UUID | FK → `principals.id` |
| `permission_id` | UUID | FK → `permissions.id` |
| `dataset_id` | UUID | FK → `datasets.id` ON DELETE CASCADE |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT NOW() |
| `updated_at` | TIMESTAMPTZ | NULL ON UPDATE NOW() |

Source: [`ACL.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/ACL.py).

### 3.9 `role_default_permissions`

| Column | Type | Constraints |
|---|---|---|
| `role_id` | UUID | PK, FK → `roles.id` ON DELETE CASCADE |
| `permission_id` | UUID | PK, FK → `permissions.id` ON DELETE CASCADE |
| `created_at` | TIMESTAMPTZ | NOT NULL DEFAULT NOW() |

Source: [`RoleDefaultPermissions.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/RoleDefaultPermissions.py). Grants a permission to anyone holding the role, **across all datasets the role's tenant has access to**.

### 3.10 `user_default_permissions`

Same shape as `role_default_permissions`, keyed on `(user_id, permission_id)`. Source: [`UserDefaultPermissions.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/UserDefaultPermissions.py).

### 3.11 `tenant_default_permissions`

Same shape, keyed on `(tenant_id, permission_id)`. Source: [`TenantDefaultPermissions.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/TenantDefaultPermissions.py). Grants a permission to all tenant members.

## 4. Indexes

The Python schema has only the indexes implied by `index=True` on columns. We add a small set of *additional* covering indexes to make permission resolution cheap:

```sql
CREATE INDEX IF NOT EXISTS ix_acls_principal_dataset
    ON acls (principal_id, dataset_id);
CREATE INDEX IF NOT EXISTS ix_acls_dataset
    ON acls (dataset_id);
CREATE INDEX IF NOT EXISTS ix_user_roles_user
    ON user_roles (user_id);
CREATE INDEX IF NOT EXISTS ix_user_tenants_user
    ON user_tenants (user_id);
CREATE INDEX IF NOT EXISTS ix_roles_tenant
    ON roles (tenant_id);
```

These are *additive* and don't conflict with Python's schema.

## 5. Permission resolution

The canonical question: **"Does user `U` have permission `P` on dataset `D`?"**

### 5.1 Resolution order (short-circuit on first hit)

```
1. U.is_superuser == TRUE                                 → ALLOW
2. ACL exists where principal_id = U.id
                AND permission.name = P
                AND dataset_id = D                        → ALLOW
3. ACL exists where principal_id IN (U's roles in U.tenant_id)
                AND permission.name = P
                AND dataset_id = D                        → ALLOW
4. ACL exists where principal_id = U.tenant_id
                AND permission.name = P
                AND dataset_id = D                        → ALLOW
5. user_default_permissions exists for (U.id, P)
                AND U has visibility into D's tenant      → ALLOW
6. role_default_permissions exists for any role R held by U
                AND (R.tenant_id covers D's tenant)
                AND permission name = P                   → ALLOW
7. tenant_default_permissions exists for (U.tenant_id, P)
                AND D belongs to U.tenant_id              → ALLOW
8. otherwise                                              → DENY
```

"Visibility into D's tenant" means the user is a member of the tenant that owns the dataset (via `datasets.owner_id` → user → tenant, or directly via dataset-level tenant association if added later).

### 5.2 SQL implementation

A single UNION-ALL query covers steps 2–7; we short-circuit step 1 (`is_superuser`) in the application layer.

```sql
SELECT 1
FROM (
    -- step 2: direct user ACL
    SELECT 1 FROM acls a
      JOIN permissions p ON p.id = a.permission_id
     WHERE a.principal_id = :user_id AND a.dataset_id = :dataset_id AND p.name = :perm
  UNION ALL
    -- step 3: role ACL
    SELECT 1 FROM acls a
      JOIN permissions p ON p.id = a.permission_id
      JOIN user_roles ur ON ur.role_id = a.principal_id
     WHERE ur.user_id = :user_id AND a.dataset_id = :dataset_id AND p.name = :perm
  UNION ALL
    -- step 4: tenant ACL
    SELECT 1 FROM acls a
      JOIN permissions p ON p.id = a.permission_id
     WHERE a.principal_id = :tenant_id AND a.dataset_id = :dataset_id AND p.name = :perm
  UNION ALL
    -- step 5: user default
    SELECT 1 FROM user_default_permissions udp
      JOIN permissions p ON p.id = udp.permission_id
     WHERE udp.user_id = :user_id AND p.name = :perm
       AND :dataset_belongs_to_user_tenant
  -- … steps 6–7 analogous
) hits LIMIT 1;
```

Returns one row → ALLOW; zero rows → DENY. Indexes on `acls(principal_id, dataset_id)` and `user_roles(user_id)` make this O(log n) per branch. Total: 4 index-only seeks plus three single-row lookups in the worst case.

### 5.3 Bulk variant

For endpoints that need to filter "datasets visible to me" (e.g. `GET /api/v1/datasets`), we run a similar query but without the `dataset_id` constraint, returning the set of dataset IDs. Implemented as the `visible_datasets` method on the `PermissionsRepository` trait (`crates/database/src/permissions/mod.rs`), with the SeaORM implementation in `crates/database/src/permissions/sea_orm_impl.rs`. Bounded by `LIMIT` to avoid full-table scans.

### 5.4 Caching

No request-level caching in phase 1 — permission resolution is fast enough at this scale. Add a short-lived (10s) in-memory `LruCache<(user_id, dataset_id, perm), bool>` later if profiling shows hot paths.

## 6. Bootstrap (default user / default tenant)

Python bootstrap on first request:

1. Create the singleton tenant `"default_tenant"` if it doesn't exist.
2. Create the default user `"default_user@example.com"` (matching [`get_default_user`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/)).
3. Insert the four canonical permissions into `permissions` if missing.
4. Optionally create an `"admin"` role in the default tenant.

Rust bootstrap runs at server startup ([architecture.md §14](architecture.md#14-startup-lifecycle)) and is **idempotent**: every step uses an upsert keyed on `name` so re-running the migration on an already-bootstrapped DB is a no-op.

```rust
async fn bootstrap_default_principals(db: &Db) -> Result<(), DbError> {
    upsert_permission_names(db, &["read", "write", "delete", "share"]).await?;
    let tenant = upsert_tenant(db, "default_tenant", /*owner=*/ None).await?;
    let user = upsert_user(db, "default_user@example.com", /*pw=*/ "", tenant.id).await?;
    add_user_to_tenant(db, user.id, tenant.id).await?;
    Ok(())
}
```

The default user's password is empty (it's never used for login — `REQUIRE_AUTHENTICATION=false` is the only path that surfaces them). Same as Python.

> **As-landed (P5, commits aefb105 + 2652aea):** the actual bootstrap order
> creates the default user **first**, then the default tenant, because
> `tenants.owner_id` is `NOT NULL` in the existing migration (see §3.3
> divergence note). Bootstrap inserts `tenants(owner_id = default_user.id)`
> as a placeholder, then back-fills `users.tenant_id = default_tenant.id`,
> then upserts the `(default_user, default_tenant)` `user_tenants` row. The
> end state is identical to the snippet above; only the insert order differs
> to satisfy the column's NOT NULL constraint.

## 7. `tenant_id` on related tables

The Python schema sprinkles `tenant_id` (without `FK`) on a few non-RBAC tables (e.g. `pipeline_runs`, `data`, `datasets`) to support tenant filtering. Existing Rust migrations already mirror this — see the project guide's note about *"tenant_id indexes"*. The tenants migration in this doc does **not** add or alter those columns; it only owns `principals` / `users` / `tenants` / `roles` / the M2M tables / `permissions` / `acls` / `*_default_permissions`.

## 8. Endpoint surface

The `/api/v1/permissions` router exposes the management API. Full per-route contracts in `routers/permissions.md`. Quick map:

| Endpoint | Action |
|---|---|
| `POST /datasets/{principal_id}?permission_name=&dataset_ids=…` | Insert into `acls` for each (principal, permission, dataset). |
| `POST /roles?role_name=` | Insert into `principals` + `roles` (under the caller's tenant). |
| `POST /users/{user_id}/roles?role_id=` | Insert into `user_roles`. |
| `POST /users/{user_id}/tenants?tenant_id=` | Insert into `user_tenants` (and update `users.tenant_id` if first). |
| `DELETE /tenants/{tenant_id}/users/{user_id}` | Delete from `user_tenants`; leave `user_roles` untouched (Python does the same). |
| `POST /tenants?tenant_name=` | Insert into `principals` + `tenants` with `owner_id = caller`. |
| `POST /tenants/select` | Update `users.tenant_id` for the caller. |
| `GET /tenants/{tenant_id}/roles` | List roles. |
| `GET /tenants/{tenant_id}/roles/{role_id}/users` | List users in a role. |
| `GET /tenants/{tenant_id}/roles/users/{user_id}` | List a user's roles in this tenant. |
| `GET /tenants/{tenant_id}/users` | List users in a tenant. |
| `GET /tenants/me` | List tenants the caller is a member of. |

Authorization for each endpoint is described in `routers/permissions.md`. Two patterns recur:

- **Tenant ownership**: the operation is allowed if the caller is `tenants.owner_id` for the affected tenant.
- **User-management role**: the operation is allowed if the caller has any role in `USER_MANAGEMENT_ALLOWED_ROLE_NAMES` within the affected tenant.

Both checks are implemented in `crates/database/src/permissions/tenant_admin.rs`.

## 9. Repository surface

```rust
// crates/database/src/permissions/mod.rs
#[async_trait]
pub trait PermissionsRepository: Send + Sync {
    async fn user_can(&self, user_id: Uuid, dataset_id: Uuid, perm: &str) -> Result<bool, PermissionsError>;
    async fn visible_datasets(&self, user_id: Uuid, perm: &str) -> Result<Vec<Uuid>, PermissionsError>;

    async fn grant_acl(&self, principal_id: Uuid, dataset_id: Uuid, perm: &str) -> Result<(), PermissionsError>;
    async fn revoke_acl(&self, principal_id: Uuid, dataset_id: Uuid, perm: &str) -> Result<(), PermissionsError>;

    async fn create_role(&self, tenant_id: Uuid, name: &str) -> Result<Uuid, PermissionsError>;
    async fn assign_role(&self, user_id: Uuid, role_id: Uuid) -> Result<(), PermissionsError>;
    async fn revoke_role(&self, user_id: Uuid, role_id: Uuid) -> Result<(), PermissionsError>;

    async fn create_tenant(&self, name: &str, owner_id: Uuid) -> Result<Uuid, PermissionsError>;
    async fn add_user_to_tenant(&self, user_id: Uuid, tenant_id: Uuid) -> Result<(), PermissionsError>;
    async fn remove_user_from_tenant(&self, user_id: Uuid, tenant_id: Uuid) -> Result<(), PermissionsError>;
    async fn select_current_tenant(&self, user_id: Uuid, tenant_id: Option<Uuid>) -> Result<(), PermissionsError>;

    async fn list_tenant_roles(&self, tenant_id: Uuid) -> Result<Vec<Role>, PermissionsError>;
    async fn list_users_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<User>, PermissionsError>;
    async fn list_users_in_role(&self, tenant_id: Uuid, role_id: Uuid) -> Result<Vec<User>, PermissionsError>;
    async fn list_user_roles(&self, tenant_id: Uuid, user_id: Uuid) -> Result<Vec<Role>, PermissionsError>;
    async fn list_my_tenants(&self, user_id: Uuid) -> Result<Vec<Tenant>, PermissionsError>;

    async fn is_tenant_admin(&self, user_id: Uuid, tenant_id: Uuid) -> Result<bool, PermissionsError>;
}
```

This trait lives in `cognee-database`. The HTTP server holds an `Arc<dyn PermissionsRepository>` inside `AppState::lib`. SeaORM-backed implementation in the same crate.

## 10. Multi-tenant isolation guarantees

| Guarantee | Implementation |
|---|---|
| A user can only mutate principals within tenants they own or admin. | Every mutating endpoint runs `is_tenant_admin(caller, target_tenant)` first. |
| `user_can()` denies by default; missing data is *not* an allow signal. | All resolution branches return `Some(true)` or `None`; the absence of any branch is `false`. |
| Cross-tenant ACL grants are rejected at the API layer. | `POST /datasets/{principal_id}` validates the caller is allowed to grant on each dataset and that the principal is in scope. |
| Soft cascade on tenant deletion is **not** implemented. | We never expose `DELETE /tenants/{id}` — Python doesn't either. Tenant deletion happens at DB level only. |
| Default-user / default-tenant cannot be deleted. | Repository guard: `delete_user` and `delete_tenant` reject the well-known IDs. |

## 11. Migrations

### 11.1 Up

Single SeaORM migration creating all tables in dependency order:

```
principals
  ├── users
  ├── tenants
  └── roles
permissions
acls (depends on principals + permissions + datasets)
user_roles, user_tenants
role_default_permissions, user_default_permissions, tenant_default_permissions
```

The migration uses `IF NOT EXISTS` semantics — running against a Python-seeded DB is a no-op for the tables and indexes that exist, and creates only what's missing. Cross-checked against [`SQLAlchemyBaseUserTableUUID`](https://github.com/fastapi-users/fastapi-users-db-sqlalchemy/blob/main/fastapi_users_db_sqlalchemy/__init__.py) (the source of `users` schema fastapi-users would have created in Python) before final landing.

### 11.2 Down

A no-op migration that *does not* drop any RBAC tables. Dropping is destructive and not safe in mixed-deployment scenarios. Operators who genuinely want to wipe RBAC state do so manually.

## 12. Testing strategy

| Layer | Tests |
|---|---|
| Migration | Up against an empty DB creates all tables; up against a Python-seeded DB is a no-op (idempotent); down is a no-op. |
| Repository | Round-trip every CRUD operation; exercise tenant/role/user M2M edges; exercise ACL grant/revoke. |
| Permissions algorithm | Test matrix for resolution: superuser path, direct user ACL, role ACL, tenant ACL, user default, role default, tenant default, deny. Each as a separate test fixture. |
| Multi-tenant isolation | A user in tenant A cannot grant ACL on a dataset in tenant B; assert `403 Forbidden`. |
| Bootstrap | Run `bootstrap_default_principals` twice; assert no duplicates; assert all four permissions exist. |
| Cross-SDK | Seed a DB via Python (`add` + grant a permission), then read with Rust and assert the same `user_can` resolution. |

Test fixtures: `crates/http-server/tests/fixtures/permissions/`. Helpers in `cognee-test-utils` for building a "tenant + user + role + ACL" graph quickly.

## 13. Open questions

1. **Cross-tenant role inheritance**: a role in tenant A cannot grant permissions in tenant B by design. But is there a use case for "global" roles (e.g. a billing admin across tenants)? Defer; would require schema changes.
2. **`updated_at` on M2M tables**: Python only has `created_at` on `user_roles`, `user_tenants`, `role_default_permissions`, etc. We match this. Some operations (revoking and re-granting) lose the update-time signal.
3. **Soft-delete vs hard-delete**: Python hard-deletes ACL/role rows. We match this. Some compliance regimes prefer soft-delete; out of scope for phase 1.
4. **Tenant slug vs name**: `tenants.name` is unique. Frontends often want a URL-safe slug separate from the display name. Not in Python; defer.
5. **Permission-set caching**: §5.4 punts on caching. If profiling shows `user_can` dominates request latency, a 10s LRU is the simplest mitigation; a `tracing` span covering the resolution branch counts will inform.
6. **`acls.principal_id` ambiguity**: a single column references all three principal types (user / tenant / role). Resolution queries don't need to disambiguate, but list-style endpoints (e.g. "show all ACLs on a dataset") have to JOIN against three tables to label the principal. Acceptable; document.

## 14. References

- Polymorphic principal model: [`cognee/modules/users/models/Principal.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/Principal.py).
- User table: [`User.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/User.py).
- Tenant table: [`Tenant.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/Tenant.py).
- Role table: [`Role.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/Role.py).
- Membership tables: [`UserRole.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/UserRole.py), [`UserTenant.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/UserTenant.py).
- Permission lookup: [`Permission.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/Permission.py).
- ACL: [`ACL.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/ACL.py).
- Default-permission tables: [`RoleDefaultPermissions.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/RoleDefaultPermissions.py), [`UserDefaultPermissions.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/UserDefaultPermissions.py), [`TenantDefaultPermissions.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/TenantDefaultPermissions.py).
- Permission constants: [`permission_types.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/permissions/permission_types.py).
- Bootstrap reference (`get_default_user`): [`cognee/modules/users/methods/get_default_user.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_default_user.py).
- Architectural context: [architecture.md §14 — Startup lifecycle](architecture.md#14-startup-lifecycle).
