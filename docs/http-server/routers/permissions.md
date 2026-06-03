# Router: permissions

The `/api/v1/permissions` router is the management API for Cognee's multi-tenant RBAC story:
tenants, roles, user-membership, per-dataset ACLs, and the user's currently selected tenant. It
is the largest router in the server (13 endpoints) and the only one that mutates the
`principals`/`tenants`/`roles`/`user_roles`/`user_tenants`/`acls` tables. The schema and
permission-resolution algorithm it relies on are specified in [`../tenants.md`](../tenants.md);
this doc only covers the HTTP surface and authorization checks per route.

Companion docs: [../architecture.md](../architecture.md),
[../auth.md](../auth.md), [../tenants.md](../tenants.md), [../observability.md](../observability.md).

## 1. Mount & file
- Mount prefix: `/api/v1/permissions` (Python: [`client.py` L228-L232](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L228-L232)).
- OpenAPI tag: `permissions`.
- Router file: `crates/http-server/src/routers/permissions.rs`.
- Repository module: `crates/database/src/permissions/` (the `PermissionsRepository` trait
  defined in [`../tenants.md` §9](../tenants.md#9-repository-surface)).
- Python source: [`cognee/api/v1/permissions/routers/get_permissions_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/permissions/routers/get_permissions_router.py)
  (378 lines, 13 endpoints).

## 2. Endpoints

Sorted by HTTP method (GET → POST → DELETE) then path. All endpoints require authentication
unless explicitly noted.

### 2.1 `GET /tenants/me` — list tenants the caller is a member of

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `200 OK`, `application/json`, `Vec<TenantSummary>`. Each item is `{"id": Uuid, "name": String}` — the tenant's UUID and display name.
  - Python source: [`get_user_tenants.py` L1-L29](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/get_user_tenants.py#L1-L29) returns `[{"id": str(tenant.id), "name": tenant.name}]`.
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 401 | `ApiError` (`InvalidCredentials`) | Missing or invalid auth credential. |
  | 500 | `ApiError` (`Internal`) | DB error reading `user_tenants`/`tenants`. |
- **Side effects**: read-only.
- **Delegation target**: `PermissionsRepository::list_my_tenants(user.id)` ([../tenants.md §9](../tenants.md#9-repository-surface)). Joins `user_tenants` ⨝ `tenants` ([../tenants.md §3.6](../tenants.md#36-user_tenants-m2m), [§3.3](../tenants.md#33-tenants)).
- **Validation rules**: none.
- **Authorization checks**: none beyond authentication. Every authenticated caller can list *their own* memberships; the WHERE clause on `user_tenants.user_id = user.id` is the isolation guarantee.
- **OpenAPI**: tag `permissions`, response schema `Vec<TenantSummary>`.
- **Telemetry**: span `cognee.api.permissions.list_my_tenants`. Attributes: `user.id`. No additional attrs (no path params).
- **Python parity notes**: response is a JSON array, not an envelope. UUIDs are stringified by Python; the Rust `Serialize` impl for `Uuid` produces the same hyphenated lowercase form by default.

### 2.2 `GET /tenants/{tenant_id}/roles` — list roles in a tenant

- **Auth**: `required`.
- **Path params**: `tenant_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `200 OK`, `application/json`, `Vec<RoleSummary>`. Each item: `{"id": Uuid, "name": String, "description": Option<String>, "user_count": usize}`.
  - Python source: [`get_tenant_roles.py` L26-L36](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/get_tenant_roles.py#L26-L36). `description` uses `getattr(role, "description", None)` — the column does not exist on the SQLAlchemy `Role` model today, so the field is always `null`. We replicate the field for wire-format parity but document the always-`null` value.
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 400 | `ApiError` (`Validation`) | Invalid UUID in `tenant_id`. |
  | 401 | `ApiError` (`InvalidCredentials`) | Unauthenticated. |
  | 403 | `ApiError` (`Forbidden`) | Caller is not the tenant owner and lacks an `USER_MANAGEMENT_ALLOWED_ROLE_NAMES` role in the tenant ([../tenants.md §3.4](../tenants.md#34-roles)). Maps to Python's `PermissionDeniedError`. |
  | 404 | `ApiError` (`NotFound`) | `tenant_id` does not exist. |
- **Side effects**: read-only.
- **Delegation target**: `PermissionsRepository::list_tenant_roles(tenant_id)` after `is_tenant_admin(user.id, tenant_id)` succeeds.
- **Validation rules**: `tenant_id` must parse as a UUID.
- **Authorization checks**: `has_user_management_permission(caller, tenant_id)` — i.e. caller is `tenants.owner_id` OR caller has any role in `USER_MANAGEMENT_ALLOWED_ROLE_NAMES = {"admin"}` within `tenant_id`. See [`has_user_management_permission.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/permissions/methods/has_user_management_permission.py).
- **OpenAPI**: tag `permissions`.
- **Telemetry**: span `cognee.api.permissions.list_tenant_roles`. Attrs: `user.id`, `tenant.id`.
- **Python parity notes**: Python uses `selectinload(Role.users)` and counts in Python; we should `LEFT JOIN user_roles` and `GROUP BY role.id` for an efficient count.

### 2.3 `GET /tenants/{tenant_id}/roles/{role_id}/users` — list users in a role

- **Auth**: `required`.
- **Path params**: `tenant_id: Uuid`, `role_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `200 OK`, `Vec<UserInRole>`. Each item: `{"id": Uuid, "name": String}` where `name` is the user's email (matching Python's [`get_users_in_role.py` L26-L32](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/get_users_in_role.py#L26-L32) — note Python populates the `name` key from `user.email`).
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 400 | `Validation` | Invalid UUID in either path param. |
  | 401 | `InvalidCredentials` | Unauthenticated. |
  | 403 | `Forbidden` | Caller fails `has_user_management_permission(tenant_id)`. |
  | 404 | `NotFound` | Tenant or role does not exist; or role does not belong to `tenant_id` (defensive). |
- **Side effects**: read-only.
- **Delegation target**: `PermissionsRepository::list_users_in_role(tenant_id, role_id)`.
- **Validation rules**: both UUIDs valid; the implementation should additionally assert `roles.tenant_id = path.tenant_id` for cross-tenant isolation (Python does not but the row's `tenant_id` makes a mismatch return zero users anyway).
- **Authorization checks**: `is_tenant_admin(user.id, tenant_id)` (= owner ∨ admin role).
- **OpenAPI**: tag `permissions`.
- **Telemetry**: span `cognee.api.permissions.list_users_in_role`. Attrs: `user.id`, `tenant.id`, `role.id`.
- **Python parity notes**: the wire-shape uses `name` for what is semantically the email — keep the field name to avoid breaking the existing Python clients.

### 2.4 `GET /tenants/{tenant_id}/roles/users/{user_id}` — list a user's roles in a tenant

- **Auth**: `required`.
- **Path params**: `tenant_id: Uuid`, `user_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `200 OK`, `Vec<RoleSummary>` ⊂ same shape as §2.2 but only `id` and `name` (Python's [`get_user_roles.py` L26-L31](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/get_user_roles.py#L26-L31) only emits `id` and `name`).
- **Error responses**: same matrix as §2.3.
- **Side effects**: read-only.
- **Delegation target**: `PermissionsRepository::list_user_roles(tenant_id, user_id)`.
- **Validation rules**: both UUIDs valid.
- **Authorization checks**: `is_tenant_admin(caller, tenant_id)`.
- **OpenAPI**: tag `permissions`.
- **Telemetry**: `cognee.api.permissions.list_user_roles`. Attrs: `user.id`, `tenant.id`, `target_user.id`.
- **Python parity notes**: Note the path nests `roles/users/{user_id}` (not `users/{user_id}/roles`); intentional — the resource is "roles" filtered by user.

### 2.5 `GET /tenants/{tenant_id}/users` — list users in a tenant

- **Auth**: `required`.
- **Path params**: `tenant_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `200 OK`, `Vec<UserInTenant>`. Each item: `{"id": Uuid, "email": String, "roles": Vec<RoleSummary>}` where `RoleSummary = {"id": Uuid, "name": String}` and `roles` is **scoped to `tenant_id`** via `selectinload(User.roles.and_(Role.tenant_id == tenant_id))` ([`get_users_in_tenant.py` L17-L37](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/get_users_in_tenant.py#L17-L37)).
- **Error responses**: same matrix as §2.3.
- **Side effects**: read-only.
- **Delegation target**: `PermissionsRepository::list_users_in_tenant(tenant_id)`.
- **Validation rules**: `tenant_id` is a UUID.
- **Authorization checks**: `is_tenant_admin(caller, tenant_id)`.
- **OpenAPI**: tag `permissions`.
- **Telemetry**: `cognee.api.permissions.list_users_in_tenant`. Attrs: `user.id`, `tenant.id`. Optional: `users.count` recorded after fetch.
- **Python parity notes**: roles are tenant-scoped — a user with roles in tenant A and tenant B sees only tenant A's roles when this is called with `tenant_id = A`.

### 2.6 `POST /datasets/{principal_id}` — grant permission on datasets to a principal

- **Auth**: `required`.
- **Path params**: `principal_id: Uuid` — the user, role, or tenant being granted.
- **Query params**: `permission_name: String` — one of `"read" | "write" | "delete" | "share"` ([`permission_types.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/permissions/permission_types.py#L3)).
- **Request body**: `application/json`, raw JSON array of UUIDs (Python: `dataset_ids: List[UUID]` is the body — FastAPI lifts `List[UUID]` to a JSON-array body when no `Body(...)` wrapper is given; we replicate as a top-level `Vec<Uuid>` body in axum). Single-UUID bodies are coerced to a one-element list to match Python's [`authorized_give_permission_on_datasets.py` L25-L26](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/permissions/methods/authorized_give_permission_on_datasets.py#L25-L26).
- **Response body**: `200 OK`, `{"message": "Permission assigned to principal"}`.
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 400 | `Validation` | Invalid UUIDs; unknown `permission_name` not in `PERMISSION_TYPES`. |
  | 401 | `InvalidCredentials` | Unauthenticated. |
  | 403 | `Forbidden` | Caller does not hold `share` on every dataset in `dataset_ids` (per `get_specific_user_permission_datasets`). |
  | 404 | `NotFound` | `principal_id` does not exist. |
  | 500 | `Internal` | DB error or `IntegrityError` after retries. |
- **Side effects**: insert one `acls` row per `(principal_id, permission_id, dataset_id)` triple ([../tenants.md §3.8](../tenants.md#38-acls--per-dataset-grants)). If the canonical `permissions` row for `permission_name` is absent, a row is upserted into `permissions` ([../tenants.md §3.7](../tenants.md#37-permissions-lookup)). Existing duplicate ACLs are skipped (Python's `give_permission_on_dataset` checks for an existing ACL before inserting).
- **Delegation target**: `cognee_lib::permissions::authorized_give_permission_on_datasets(principal_id, dataset_ids, permission_name, caller_id)` → calls `PermissionsRepository::grant_acl()` per dataset that the caller is allowed to share.
- **Validation rules**:
  - `permission_name ∈ PERMISSION_TYPES`.
  - `dataset_ids` is non-empty (Python crashes on an empty list because the for-loop is a no-op; we should reject up front with `400`).
- **Authorization checks**:
  1. Caller must hold the `share` permission on each dataset (filtered via `get_specific_user_permission_datasets(caller_id, "share", dataset_ids)`). Datasets the caller cannot share are silently dropped — Python does **not** error, it simply doesn't grant on those. We replicate this; the caller learns by absence, not error.
  2. Cross-tenant grants are not blocked at this layer (Python's TODO at [`authorized_give_permission_on_datasets.py` L33-L34](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/permissions/methods/authorized_give_permission_on_datasets.py#L33-L34) explicitly defers); we match the behavior.
- **OpenAPI**: tag `permissions`. The body is `Vec<Uuid>`; document that explicitly so SDK generators do not infer a wrapper object.
- **Telemetry**: `cognee.api.permissions.grant_dataset_permission`. Attrs: `user.id`, `principal.id`, `permission.name`, `datasets.count`.
- **Python parity notes**: the silent-skip-on-no-share behavior is not ideal but is the documented Python behavior. Open question §6.2 below.

### 2.7 `POST /roles?role_name=` — create role under caller's current tenant

- **Auth**: `required`.
- **Path params**: none.
- **Query params**: `role_name: String`.
- **Request body**: none.
- **Response body**: `200 OK`, `{"message": "Role created for tenant", "role_id": Uuid, "tenant_id": Uuid}` (Python returns string-encoded UUIDs; we serialize `Uuid` directly which produces the same hyphenated string).
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 400 | `Validation` | Empty `role_name`; or duplicate `(tenant_id, name)` (`EntityAlreadyExistsError`). |
  | 401 | `InvalidCredentials` | Unauthenticated. |
  | 403 | `Forbidden` | Caller is not `tenants.owner_id` of their current tenant ([`create_role.py` L34-L37](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/roles/methods/create_role.py#L34-L37)). Note: this is **owner-only**, *not* admin-role; admins cannot create roles. |
  | 500 | `Internal` | DB error. |
- **Side effects**: insert into `principals` (`type='role'`) + `roles` ([../tenants.md §3.4](../tenants.md#34-roles)) with `tenant_id = caller.tenant_id`.
- **Delegation target**: `PermissionsRepository::create_role(caller.tenant_id, role_name)`.
- **Validation rules**: `role_name` non-empty after trim. The `UNIQUE (tenant_id, name)` constraint is enforced at DB level.
- **Authorization checks**: `caller.id == tenants.owner_id` for `caller.tenant_id`.
- **OpenAPI**: tag `permissions`.
- **Telemetry**: `cognee.api.permissions.create_role`. Attrs: `user.id`, `role.name`, `tenant.id`.
- **Python parity notes**: the role is implicitly attached to the *caller's current* tenant (`user.tenant_id`); there is no way to create a role in a tenant the caller doesn't own. To create a role in a different tenant, the caller must first `POST /tenants/select` to switch.

### 2.8 `POST /tenants?tenant_name=` — create a new tenant owned by caller

- **Auth**: `required`.
- **Path params**: none.
- **Query params**: `tenant_name: String`.
- **Request body**: none.
- **Response body**: `200 OK`, `{"message": "Tenant created.", "tenant_id": Uuid}`.
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 400 | `Validation` | Empty `tenant_name`; or duplicate name (`EntityAlreadyExistsError`). |
  | 401 | `InvalidCredentials` | Unauthenticated. |
  | 500 | `Internal` | DB error. |
- **Side effects** ([`create_tenant.py` L27-L51](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/create_tenant.py#L27-L51)):
  1. Insert into `principals` (`type='tenant'`) + `tenants` ([../tenants.md §3.3](../tenants.md#33-tenants)) with `owner_id = caller.id`.
  2. Set `users.tenant_id = new_tenant.id` for the caller (Python's `set_as_active_tenant=True` default).
  3. Insert into `user_tenants` ([../tenants.md §3.6](../tenants.md#36-user_tenants-m2m)) for the `(caller, new_tenant)` membership.
- **Delegation target**: `PermissionsRepository::create_tenant(tenant_name, caller.id)`. Note: the trait signature in [../tenants.md §9](../tenants.md#9-repository-surface) currently takes `owner_id`; the implementation also performs the side effects 2 and 3 above to match Python.
- **Validation rules**: `tenant_name` is non-empty (the `UNIQUE` constraint on `tenants.name` enforces uniqueness). No regex restriction in Python.
- **Authorization checks**: none beyond authentication. *Any authenticated user can create a tenant*; they become its owner.
- **OpenAPI**: tag `permissions`.
- **Telemetry**: `cognee.api.permissions.create_tenant`. Attrs: `user.id`, `tenant.name` (post-redaction), `tenant.id`.
- **Python parity notes**: this is the only endpoint that combines `principals/tenants/users.tenant_id/user_tenants` writes in one call. Wrap the three statements in a single transaction (Python uses one async session but commits between steps; we should improve atomicity but emit the same final state).

### 2.9 `POST /tenants/select` — set caller's current tenant (PUT-replace semantics)

- **Auth**: `required`.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `SelectTenantDTO { tenant_id: Option<Uuid> }`. A null body field selects the user's "default single-user tenant" — Python interprets this as `users.tenant_id = NULL` ([`select_tenant.py` L31-L36](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/select_tenant.py#L31-L36)).
- **Response body**: `200 OK`, `{"message": "Tenant selected.", "tenant_id": Option<Uuid>}` (note: Python passes `payload.tenant_id` straight through, so a `null` request becomes a `null` response field).
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 400 | `Validation` | Body is not valid JSON / `tenant_id` not a UUID nor null. |
  | 401 | `InvalidCredentials` | Unauthenticated. |
  | 404 | `NotFound` | `tenant_id` is non-null and does not exist; or caller is not a member of that tenant (Python's `TenantNotFoundError("User is not part of the tenant.")` at [`select_tenant.py` L52-L55](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/select_tenant.py#L52-L55)). |
  | 500 | `Internal` | DB error. |
- **Side effects**: **`UPDATE users SET tenant_id = :tenant_id WHERE id = :caller_id`** ([../tenants.md §3.2](../tenants.md#32-users)). This is a PUT-replace on the single-valued `users.tenant_id` column — it does **not** add a `user_tenants` row; the user must already be a member via the M2M table. PUT-replace semantics: passing `null` zeroes the column.
- **Delegation target**: `PermissionsRepository::select_current_tenant(caller.id, tenant_id)`.
- **Validation rules**: when `tenant_id` is non-null, the caller must have a `user_tenants` row for it. `select_tenant` rejects with 404 otherwise — this is the cross-tenant isolation guarantee for tenant switching.
- **Authorization checks**: implicit — the membership check in `user_tenants` is itself the authorization gate. There is no admin-role bypass.
- **OpenAPI**: tag `permissions`. Body is `SelectTenantDTO`. Document explicitly that `null` is meaningful (it is *not* the absence of the field; it is an explicit "use my single-user tenant" signal).
- **Telemetry**: `cognee.api.permissions.select_tenant`. Attrs: `user.id`, `tenant.id` (or the literal `"null"` when null).
- **Python parity notes**: when `tenant_id` is null, Python returns the literal string `"None"` (Python's `str(None) == "None"`). Rust matches verbatim — emit the JSON string `"None"` in the response, **not** JSON `null`. The Rust handler must explicitly stringify the `Option<Uuid>` to preserve this exact wire shape; using `serde_json`'s default `None`-to-`null` would diverge.

### 2.10 `POST /users/{user_id}/roles?role_id=` — assign role to user

- **Auth**: `required`.
- **Path params**: `user_id: Uuid` — the user being added to the role.
- **Query params**: `role_id: Uuid`.
- **Request body**: none.
- **Response body**: `200 OK`, `{"message": "User added to role"}`.
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 400 | `Validation` / `EntityAlreadyExists` | Invalid UUIDs; or `(user_id, role_id)` already in `user_roles`. |
  | 401 | `InvalidCredentials` | Unauthenticated. |
  | 403 | `Forbidden` | Caller is not the role's tenant owner ([`add_user_to_role.py` L55-L58](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/roles/methods/add_user_to_role.py#L55-L58)). Owner-only; admins cannot. |
  | 404 | `NotFound` | User or role does not exist; or user is not in the role's tenant (`TenantNotFoundError`, [`add_user_to_role.py` L51-L54](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/roles/methods/add_user_to_role.py#L51-L54)). |
  | 500 | `Internal` | DB error. |
- **Side effects**: insert into `user_roles` ([../tenants.md §3.5](../tenants.md#35-user_roles-m2m)) — composite PK `(user_id, role_id)`.
- **Delegation target**: `PermissionsRepository::assign_role(user_id, role_id)` (the repo internally validates user-tenant membership and checks tenant ownership).
- **Validation rules**: both UUIDs valid; user must be in `user_tenants` for `roles.tenant_id`; caller must be `tenants.owner_id` of `roles.tenant_id`.
- **Authorization checks**: tenant-owner only. This is **stricter** than `has_user_management_permission` — admins cannot assign roles, only the tenant owner can.
- **OpenAPI**: tag `permissions`.
- **Telemetry**: `cognee.api.permissions.assign_role`. Attrs: `user.id`, `target_user.id`, `role.id`.
- **Python parity notes**: this is one of two endpoints (along with §2.7) where authorization is **owner-only** even though `has_user_management_permission` would be the "natural" gate. Match Python exactly.

### 2.11 `POST /users/{user_id}/tenants?tenant_id=` — add user to tenant

- **Auth**: `required`.
- **Path params**: `user_id: Uuid`.
- **Query params**: `tenant_id: Uuid`.
- **Request body**: none.
- **Response body**: `200 OK`, `{"message": "User added to tenant"}`.
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 400 | `Validation` / `EntityAlreadyExists` | Invalid UUIDs; or `(user_id, tenant_id)` already in `user_tenants`. |
  | 401 | `InvalidCredentials` | Unauthenticated. |
  | 403 | `Forbidden` | Caller is not `tenants.owner_id` of `tenant_id` ([`add_user_to_tenant.py` L46-L49](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/add_user_to_tenant.py#L46-L49)). |
  | 404 | `NotFound` | User or tenant not found. |
  | 500 | `Internal` | DB error. |
- **Side effects**: insert into `user_tenants` ([../tenants.md §3.6](../tenants.md#36-user_tenants-m2m)). Does **not** modify `users.tenant_id` (Python's optional `set_as_active_tenant=True` is not exposed on this endpoint).
- **Delegation target**: `PermissionsRepository::add_user_to_tenant(user_id, tenant_id)`.
- **Validation rules**: both UUIDs valid.
- **Authorization checks**: tenant-owner only. (Again stricter than admin-role.)
- **OpenAPI**: tag `permissions`.
- **Telemetry**: `cognee.api.permissions.add_user_to_tenant`. Attrs: `user.id`, `target_user.id`, `tenant.id`.
- **Python parity notes**: the tenant owner cannot be added a second time to their own tenant — the `UNIQUE` PK on `user_tenants` rejects duplicates. The target user is **not** automatically given a role.

### 2.12 `DELETE /tenants/{tenant_id}/users/{user_id}` — remove user from tenant

- **Auth**: `required`.
- **Path params**: `tenant_id: Uuid`, `user_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `200 OK`, `{"message": "User removed from tenant"}`.
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 400 | `Validation` (`CogneeValidationError`) | Caller is trying to remove the tenant owner ([`remove_user_from_tenant.py` L46-L51](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/remove_user_from_tenant.py#L46-L51)). |
  | 401 | `InvalidCredentials` | Unauthenticated. |
  | 403 | `Forbidden` | Caller is not tenant owner *and* lacks `USER_MANAGEMENT_ALLOWED_ROLE_NAMES` role. **This is the broader admin-role-allowed gate.** |
  | 404 | `NotFound` | Tenant does not exist; user does not exist; user is not in this tenant. |
  | 500 | `Internal` | DB error. |
- **Side effects** ([`remove_user_from_tenant.py` L67-L93](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/remove_user_from_tenant.py#L67-L93)):
  1. `DELETE FROM user_roles WHERE user_id = :u AND role_id IN (SELECT id FROM roles WHERE tenant_id = :t)` ([../tenants.md §3.5](../tenants.md#35-user_roles-m2m)).
  2. `DELETE FROM acls WHERE principal_id = :u AND dataset_id IN (SELECT id FROM datasets WHERE tenant_id = :t)` ([../tenants.md §3.8](../tenants.md#38-acls--per-dataset-grants)).
  3. `DELETE FROM user_tenants WHERE user_id = :u AND tenant_id = :t` ([../tenants.md §3.6](../tenants.md#36-user_tenants-m2m)).
  4. **Not** modified: `users.tenant_id` (the user's *current* tenant column), data they own (datasets stay), default-permission tables.
- **Delegation target**: `PermissionsRepository::remove_user_from_tenant(user_id, tenant_id)`.
- **Validation rules**: both UUIDs valid; target is not the tenant owner.
- **Authorization checks**: `is_tenant_admin(caller, tenant_id)` (= owner ∨ admin role). This is the **only** mutation endpoint in this router that admin-role users can invoke; sections §2.7, §2.10, §2.11 are owner-only.
- **OpenAPI**: tag `permissions`.
- **Telemetry**: `cognee.api.permissions.remove_user_from_tenant`. Attrs: `user.id`, `target_user.id`, `tenant.id`, `roles_removed`, `acls_revoked`.
- **Python parity notes**: the "datasets they created stay in the tenant" behavior is intentional (Python comment at [`remove_user_from_tenant.py` L25-L29](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/remove_user_from_tenant.py#L25-L29)). Removing a user does *not* re-home their data. Document this clearly in our OpenAPI summary.

### 2.13 Authorization summary table

Quick-reference for the asymmetric authorization in this router. "Owner" = `tenants.owner_id`. "Admin" = caller has any role in `USER_MANAGEMENT_ALLOWED_ROLE_NAMES = {"admin"}` for the affected tenant ([../tenants.md §3.4](../tenants.md#34-roles)).

| Endpoint | Owner can? | Admin can? | Notes |
|---|---|---|---|
| `GET /tenants/me` | (n/a) | (n/a) | Self-scoped only. |
| `GET /tenants/{t}/roles` | yes | yes | `has_user_management_permission`. |
| `GET /tenants/{t}/roles/{r}/users` | yes | yes | same gate. |
| `GET /tenants/{t}/roles/users/{u}` | yes | yes | same gate. |
| `GET /tenants/{t}/users` | yes | yes | same gate. |
| `POST /datasets/{p}` | per-dataset `share` | per-dataset `share` | not owner-bound; depends on the caller's `share` ACL. |
| `POST /roles` | yes | **no** | owner-only. |
| `POST /tenants` | (n/a) | (n/a) | any authenticated user. |
| `POST /tenants/select` | (n/a — self) | (n/a — self) | membership in `user_tenants` is the gate. |
| `POST /users/{u}/roles` | yes | **no** | owner-only. |
| `POST /users/{u}/tenants` | yes | **no** | owner-only. |
| `DELETE /tenants/{t}/users/{u}` | yes | yes | `has_user_management_permission`; cannot remove owner. |

## 3. Cross-cutting behavior

- **Tenant-scoping**: all writes are scoped to the caller's reachable tenants. The `is_tenant_admin` / `has_user_management_permission` helpers in `crates/database/src/permissions/tenant_admin.rs` ([../tenants.md §8](../tenants.md#8-endpoint-surface)) are the single source of truth and must be reused — do not inline ownership checks at the handler level.
- **Error mapping**: Python's `PermissionDeniedError` → `403`; `EntityNotFoundError` / `UserNotFoundError` / `TenantNotFoundError` / `RoleNotFoundError` / `PermissionNotFoundError` → `404`; `EntityAlreadyExistsError` → `400`; `CogneeValidationError` → `400` with the carried `status_code`. Mapping table in [../architecture.md §9](../architecture.md#9-error-handling).
- **No request-body schema** for query-param-only endpoints (§2.6 is the exception — body is the dataset list).
- **Telemetry**: every handler is `#[tracing::instrument(skip(state))]`, span name `cognee.api.permissions.<verb>`, plus the `endpoint` attribute that Python's `send_telemetry` records ([Python source examples in `get_permissions_router.py` L65-L74](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/permissions/routers/get_permissions_router.py#L65-L74)). See [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions).
- **Idempotency**: `POST /users/{u}/roles`, `POST /users/{u}/tenants`, `POST /datasets/{p}` are not idempotent — duplicates return `400 EntityAlreadyExistsError` (or are silently skipped for `acls` because `give_permission_on_dataset` checks for existing rows). `POST /tenants/select` is idempotent (re-selecting the same tenant is a no-op `UPDATE`).
- **Bulk deletes are out of scope** — the router does not expose tenant or role deletion. Match Python; see [../tenants.md §10](../tenants.md#10-multi-tenant-isolation-guarantees).

## 4. DTO definitions

```rust
// crates/http-server/src/dto/permissions.rs
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// ── Request DTOs ────────────────────────────────────────────────────────────

/// Body for `POST /tenants/select`.
///
/// `SelectTenantDTO` inherits `InDTO` in Python, so the wire is camelCase
/// per Decision 10 (`tenantId`). Snake_case `tenant_id` is accepted as an
/// inbound alias for compatibility with `populate_by_name=True`. This is
/// the only DTO in this module that follows the camelCase rule — every
/// response struct below stays snake_case because Python returns plain
/// `JSONResponse(content={...})` dicts whose keys are not aliased.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SelectTenantDTO {
    /// Target tenant. `None` (JSON `null`) selects the user's default
    /// single-user tenant by setting `users.tenant_id = NULL`. Mirrors Python's
    /// `SelectTenantDTO.tenant_id: UUID | None = None`.
    #[serde(default, alias = "tenant_id")]
    pub tenant_id: Option<Uuid>,
}

/// Body for `POST /datasets/{principal_id}` — a JSON array of dataset UUIDs.
/// Python lifts `dataset_ids: List[UUID]` into a top-level body. We model it
/// as a thin newtype rather than a struct wrapper to match the wire format.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(transparent)]
pub struct GrantDatasetPermissionBody(pub Vec<Uuid>);

// ── Query-param DTOs (axum extracts these via Query<...>) ───────────────────

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct GrantDatasetPermissionQuery {
    pub permission_name: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CreateRoleQuery {
    pub role_name: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CreateTenantQuery {
    pub tenant_name: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct AssignRoleQuery {
    pub role_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct AddUserToTenantQuery {
    pub tenant_id: Uuid,
}

// ── Response DTOs ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct MessageResponse {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CreateRoleResponse {
    pub message: String,
    pub role_id: Uuid,
    pub tenant_id: Uuid,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CreateTenantResponse {
    pub message: String,
    pub tenant_id: Uuid,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SelectTenantResponse {
    pub message: String,
    /// Echoes the request value, including `null`. Python returns `"None"` here
    /// as a stringified Python None — see open question §6.4.
    pub tenant_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct TenantSummary {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct RoleSummary {
    pub id: Uuid,
    pub name: String,
    /// Python emits `description` via `getattr(role, "description", None)`;
    /// the column does not exist, so this is always `null`. Keep the field for
    /// wire-format parity with Python.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Number of users currently assigned to this role. Only emitted by
    /// `GET /tenants/{t}/roles`; the `roles/users/{u}` endpoint omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct UserInRole {
    pub id: Uuid,
    /// Python sets this to `user.email` and labels the field `name`. Match.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct UserInTenant {
    pub id: Uuid,
    pub email: String,
    /// Roles scoped to the requested tenant only.
    pub roles: Vec<RoleSummary>,
}
```

## 5. Implementation tasks

1. Add DTO structs in `crates/http-server/src/dto/permissions.rs` per §4.
2. Add handler functions in `crates/http-server/src/routers/permissions.rs`. Each handler is a thin
   adapter that calls one method on `Arc<dyn PermissionsRepository>` (see [../tenants.md §9](../tenants.md#9-repository-surface)).
3. Implement `PermissionsRepository` in `crates/database/src/permissions/sea_orm_impl.rs`.
   Cover: `list_my_tenants`, `list_tenant_roles`, `list_users_in_role`, `list_user_roles`,
   `list_users_in_tenant`, `grant_acl` (with `permission` upsert + duplicate-ACL skip),
   `create_role`, `create_tenant` (with the three-step transaction in §2.8), `select_current_tenant`,
   `assign_role`, `add_user_to_tenant`, `remove_user_from_tenant`.
4. Implement `is_tenant_admin` and `has_user_management_permission` in
   `crates/database/src/permissions/tenant_admin.rs` and reuse from every authorization gate.
5. Add OpenAPI annotations (`#[utoipa::path(...)]`) for all 13 endpoints; tag `permissions`.
6. Add unit tests in the router file: error-mapping (every Python exception → expected status).
7. Add integration tests in `crates/http-server/tests/test_permissions.rs`:
   - Round-trip: create tenant → create role → assign role → list users in role.
   - Cross-tenant isolation: tenant A admin cannot list/modify tenant B.
   - Owner-only enforcement on §2.7 / §2.10 / §2.11 (admin role denied).
   - Owner-or-admin on §2.12.
   - `POST /tenants/select` with `null` body and with cross-tenant target.
   - `POST /datasets/{p}` with mixed allow/deny dataset list — assert silent skip.
   - Cannot remove tenant owner (§2.12).
8. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_permissions.py`: Python seeds the
   tenant graph; Rust reads via the same endpoints; assert response shapes are byte-for-byte equal,
   including the `select_tenant` `"None"` literal-string for null tenants.

## 6. Open questions

1. **`POST /datasets/{p}` empty list** — Python does nothing for an empty `dataset_ids` (returns success). Rust matches: empty list → 200 with the Python-shaped success body, no `400`.
2. **`POST /datasets/{p}` partial-grant signal** — Python silently skips datasets the caller cannot `share` and returns the same generic success body. Rust matches: same message, same opacity. No structured `granted`/`denied` body in v1.
3. **`POST /roles`, `POST /users/{u}/roles`, `POST /users/{u}/tenants` are owner-only** — Rust matches verbatim: owner-only gates on these three endpoints, even though §2.12 uses the broader `has_user_management_permission` gate. The asymmetry is Python's behavior.
4. **`SelectTenantResponse.tenant_id` for null body** — Python returns the literal string `"None"` via `str(None)`. Rust matches: emit the JSON string `"None"`, not JSON `null`. This is the wire contract.
5. **Default single-user tenant lookup** — *Resolved during P5 (commit 2652aea)*: `bootstrap_default_principals` always sets `users.tenant_id = default_tenant.id` for the default user (and the existing migration declares `tenants.owner_id NOT NULL`, so the default tenant always has a placeholder owner). The "`users.tenant_id IS NULL` fallback" branch is therefore unreachable in the Rust port; we still match Python's wire output because Python ends up at the same row via its on-first-request bootstrap.
6. **Atomicity of `POST /tenants`** — Python uses three sequential commits without a transaction. Rust matches: three sequential commits, same observable behavior on partial failure. No application-level transaction wrap.
7. **`acls` cleanup on role deletion** — there is no role-deletion endpoint, but if added later,
   should ACLs granted to the role be cascaded? Schema FK `acls.principal_id → principals.id` is
   not declared `ON DELETE CASCADE`. Out of scope for this router; track in [../tenants.md §13](../tenants.md#13-open-questions).

## 7. References

- Python router: [`get_permissions_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/permissions/routers/get_permissions_router.py).
- Python methods (one per endpoint):
  - [`get_user_tenants.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/get_user_tenants.py),
  - [`get_tenant_roles.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/get_tenant_roles.py),
  - [`get_users_in_role.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/get_users_in_role.py),
  - [`get_user_roles.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/get_user_roles.py),
  - [`get_users_in_tenant.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/get_users_in_tenant.py),
  - [`authorized_give_permission_on_datasets.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/permissions/methods/authorized_give_permission_on_datasets.py),
  - [`give_permission_on_dataset.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/permissions/methods/give_permission_on_dataset.py),
  - [`create_role.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/roles/methods/create_role.py),
  - [`create_tenant.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/create_tenant.py),
  - [`select_tenant.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/select_tenant.py),
  - [`add_user_to_role.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/roles/methods/add_user_to_role.py),
  - [`add_user_to_tenant.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/add_user_to_tenant.py),
  - [`remove_user_from_tenant.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/remove_user_from_tenant.py).
- Authorization helper: [`has_user_management_permission.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/permissions/methods/has_user_management_permission.py).
- Permission constants: [`permission_types.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/permissions/permission_types.py).
- Schema: [../tenants.md](../tenants.md).
- Error mapping: [../architecture.md §9](../architecture.md#9-error-handling).
- Auth extractors: [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).
- Telemetry conventions: [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions), [§3.4](../observability.md#34-span-name-conventions).
