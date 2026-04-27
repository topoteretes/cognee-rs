# Implementation: P5 — Admin + RBAC

## 1. Goal

Land the admin layer of the Rust HTTP server: the `/api/v1/permissions` router (13 endpoints
covering tenants, roles, user membership, and per-dataset ACLs), the `/api/v1/settings` router
(global LLM and vector-DB config with API-key redaction), and the `/api/v1/configuration` router
(per-user named JSON blobs). This phase also lands the **real** SeaORM migration for the RBAC
schema that earlier phases stubbed out (`principals`, `users` extensions, `tenants`, `roles`,
`user_roles`, `user_tenants`, `permissions`, `acls`, and the three default-permission tables) and
the `PermissionsRepository` SeaORM-backed implementation that P2 stitched a stub against. By the
end of this phase, the multi-tenant story works end-to-end on a live DB; every `// TODO(P5)`
marker dropped in earlier phases is removed; and `bootstrap_default_principals` creates the full
default-tenant + default-user + canonical-permissions graph on first boot.

## 2. References (read these before starting)

- Phase template + invariants: [implementation/README.md](README.md).
- Phase scope summary: [plan.md §4 P5 / §7](../plan.md).
- Error model + canonical envelope: [architecture.md §9](../architecture.md#9-error-handling).
- Auth extractor: [auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution),
  [§4 `AuthenticatedUser`](../auth.md#4-authentication-flow).
- **Multi-tenant schema, indexes, resolution, bootstrap**: [tenants.md](../tenants.md) — the entire
  doc is the spec for the migration and the `PermissionsRepository`. In particular: §3 (tables),
  §4 (indexes), §5 (resolution), §6 (bootstrap), §9 (repository surface), §11 (migrations).
- Per-router specs:
  - [routers/permissions.md](../routers/permissions.md) — 13 endpoints; each has its own §2.x with
    auth, validation, side effects, and Python-parity notes.
  - [routers/settings.md](../routers/settings.md).
  - [routers/configuration.md](../routers/configuration.md).
- Cross-router conventions (envelope exceptions, telemetry, permission-gate placement):
  [routers/README.md §3](../routers/README.md#3-cross-router-conventions).

## 3. Prerequisites

- **P0** done: `cognee-http-server` crate, `AppState`, `ApiError`, `Json` extractor, OpenAPI
  bootstrap, `lifecycle::on_startup`. The lifecycle slot for `bootstrap_default_principals`
  exists as a no-op call site that this phase fills in (see [tenants.md §6](../tenants.md#6-bootstrap-default-user--default-tenant)).
- **P1** done: `users` SeaORM migration, `AuthenticatedUser` extractor, login / me / logout. We
  need `AuthenticatedUser` in every handler in this phase except where called out otherwise.
- **P2** done: write-path routers landed against a **stub** `PermissionsRepository`. The stub left
  one or more `// TODO(P5): wire real PermissionsRepository` markers in `crates/http-server/src/`
  (typically inside `state.rs` and at the call-sites that today return `unimplemented!()` or
  `Ok(true)` for permission checks). This phase replaces the stub with the SeaORM-backed
  implementation and removes those markers.

## 4. Step-by-step

> Each step is one commit. If a step's diff would exceed ~300 lines, split it. Every step
> has a `Verify:` clause; do not move on until it passes.

### Step 1: Create the RBAC SeaORM migration (schema)

- **File(s)**: `crates/database/src/migrator/m_<timestamp>_tenants_rbac.rs` (timestamp the file
  per the existing convention — see the sibling `m20250422_000001_user_tenant_role_tables.rs`
  naming style — and pick a strictly-greater stamp than any existing migration). Add the new
  module to `crates/database/src/migrator/mod.rs` and append it to the `MigratorTrait::migrations`
  vec.
- **Action**: Implement `Migration::up` to create every table listed in [tenants.md §3](../tenants.md#3-tables)
  in dependency order: `principals` → `users` (extension only — `tenant_id` column added if not
  present), `tenants`, `roles` (with the `UNIQUE (tenant_id, name)` constraint), `permissions`,
  `acls`, `user_roles`, `user_tenants`, `role_default_permissions`, `user_default_permissions`,
  `tenant_default_permissions`. Use SeaORM's `TableCreateStatement::if_not_exists()` (and
  `AlterTableStatement` for the `users.tenant_id` column extension, also gated on `IF NOT EXISTS`).
  The migration **must be idempotent** against:
  1. an empty DB (creates everything);
  2. a DB seeded by P1's `users` migration (extends `users` with `tenant_id`, creates the rest);
  3. a Python-seeded DB that already has every table (no-ops cleanly).
  Implement `Migration::down` as a literal no-op per [tenants.md §11.2](../tenants.md#112-down).
- **Spec reference**: [tenants.md §3](../tenants.md#3-tables) (per-table column lists),
  [§11](../tenants.md#11-migrations) (idempotency contract).
- **Verify**:
  - `cargo check -p cognee-database`.
  - Manual: run `cargo run -p cognee-cli -- run-sequence` (or your existing migration entrypoint)
    against a fresh `sqlite::memory:` DB, then run it a **second** time and assert no error and no
    duplicate-table errors.

### Step 2: RBAC migration — indexes and seed rows

- **File(s)**: same file as step 1.
- **Action**: Append the additive indexes from [tenants.md §4](../tenants.md#4-indexes) using
  `IndexCreateStatement::if_not_exists()`: `ix_acls_principal_dataset`, `ix_acls_dataset`,
  `ix_user_roles_user`, `ix_user_tenants_user`, `ix_roles_tenant`. Then seed the four canonical
  permission rows (`read`, `write`, `delete`, `share` per
  [tenants.md §3.7](../tenants.md#37-permissions-lookup)) idempotently — use an `INSERT … WHERE NOT
  EXISTS (SELECT 1 FROM permissions WHERE name = ?)` pattern, or a portable upsert through
  SeaORM's query builder. **Do not** issue the seeds inside `Migration::down` — leave the rows in
  place even on rollback per [tenants.md §3.7](../tenants.md#37-permissions-lookup).
- **Spec reference**: [tenants.md §4](../tenants.md#4-indexes), [§3.7](../tenants.md#37-permissions-lookup).
- **Verify**: re-run the migration against the seeded DB from step 1; assert exactly four rows in
  `permissions` (one per name). `cargo test -p cognee-database --test rbac_migration_idempotent`
  (added in step 18).

### Step 3: Add or extend SeaORM entities for RBAC

- **File(s)**: under `crates/database/src/entities/`. Files for `principal.rs`, `tenant.rs`,
  `role.rs`, `user_role.rs`, `user_tenant.rs`, `permission.rs`, `acl.rs`, and `user.rs` (extension)
  may already exist as stubs from prior work — audit each and bring its column list up to spec.
  Add new files for `role_default_permission.rs`, `user_default_permission.rs`,
  `tenant_default_permission.rs`. Re-export everything from `entities/mod.rs`.
- **Action**: Each entity is a `#[derive(DeriveEntityModel)]` struct mirroring the table schema in
  [tenants.md §3](../tenants.md#3-tables) one-for-one. Wire `Relation::User`, `Relation::Tenant`,
  `Relation::Role`, etc. for the join paths the repository will use (see
  [tenants.md §5.2](../tenants.md#52-sql-implementation) for the join graph). Keep the entity files
  pure — no business logic.
- **Spec reference**: [tenants.md §3](../tenants.md#3-tables).
- **Verify**: `cargo check -p cognee-database --all-targets`.

### Step 4: Define the `PermissionsRepository` trait

- **File(s)**: `crates/database/src/permissions/mod.rs` (new module; add `pub mod permissions;` to
  `crates/database/src/lib.rs`).
- **Action**: Define `#[async_trait] pub trait PermissionsRepository: Send + Sync` with the full
  method list from [tenants.md §9](../tenants.md#9-repository-surface): `user_can`,
  `visible_datasets`, `grant_acl`, `revoke_acl`, `create_role`, `assign_role`, `revoke_role`,
  `create_tenant`, `add_user_to_tenant`, `remove_user_from_tenant`, `select_current_tenant`, plus
  the listing methods (`list_my_tenants`, `list_tenant_roles`, `list_users_in_tenant`,
  `list_users_in_role`, `list_user_roles`), plus `is_tenant_admin`. Define lightweight value types
  (`Role`, `Tenant`, `User`) inside the same module — these are projection structs the listing
  methods return; do **not** leak SeaORM `Model` types across the trait boundary. Define a small
  `PermissionsError` enum (thiserror) wrapping `DbErr`, `EntityNotFound`, `EntityAlreadyExists`,
  `PermissionDenied`, `Validation` so the HTTP layer can map cleanly to `ApiError` per
  [architecture.md §9](../architecture.md#9-error-handling).
- **Spec reference**: [tenants.md §9](../tenants.md#9-repository-surface).
- **Verify**: `cargo check -p cognee-database`.

### Step 5: Implement `is_tenant_admin` / `has_user_management_permission`

- **File(s)**: `crates/database/src/permissions/tenant_admin.rs` (referenced in
  [tenants.md §8](../tenants.md#8-endpoint-surface) and reused in §10 isolation guarantees).
- **Action**: Implement the two helpers as standalone async functions on the SeaORM connection.
  `is_tenant_admin(user_id, tenant_id)` returns `true` when caller is `tenants.owner_id` **OR**
  caller has any role in the constant `USER_MANAGEMENT_ALLOWED_ROLE_NAMES = ["admin"]` for that
  tenant ([tenants.md §3.4](../tenants.md#34-roles)). `has_user_management_permission` is an alias
  for the same function — keep both names so reviewers can grep against the Python source. Encode
  the role-name allow-list as a `pub const &[&str]` so changes track Python's `permission_types.py`.
- **Spec reference**: [tenants.md §3.4](../tenants.md#34-roles),
  [§10](../tenants.md#10-multi-tenant-isolation-guarantees).
- **Verify**: inline unit tests cover (a) owner returns true; (b) non-owner without admin role
  returns false; (c) non-owner with admin role returns true; (d) non-owner with non-admin role
  returns false. Use an in-memory SQLite DB seeded with the migration. `cargo test -p
  cognee-database --lib permissions::tenant_admin::tests`.

### Step 6: Implement `user_can` (8-step resolution)

- **File(s)**: `crates/database/src/permissions/sea_orm_impl.rs` (new file; will hold the full
  `impl PermissionsRepository for SeaOrmPermissionsRepository`).
- **Action**: Implement `user_can(user_id, dataset_id, perm)`. The algorithm has eight branches
  enumerated in [tenants.md §5.1](../tenants.md#51-resolution-order-short-circuit-on-first-hit);
  short-circuit step 1 (`is_superuser`) in application code by reading the user row first, then
  emit the `UNION ALL` query covering branches 2–7 ([tenants.md §5.2](../tenants.md#52-sql-implementation)).
  Use SeaORM's `Statement::from_sql_and_values` so the SQL is a single round-trip; SeaORM's
  type-safe builder cannot express UNION ALL across heterogeneous joins cleanly. Branch 8
  (`DENY`) is the implicit fall-through (zero rows returned). Return `Ok(true)` if the union has
  at least one row, `Ok(false)` otherwise.
- **Spec reference**: [tenants.md §5](../tenants.md#5-permission-resolution).
- **Verify**: see step 18 — the `permissions_repository.rs` integration test exercises the full
  truth table for all eight branches.

### Step 7: Implement the rest of `PermissionsRepository`

- **File(s)**: `crates/database/src/permissions/sea_orm_impl.rs` (continued).
- **Action**: Fill in the remaining methods. Notable details:
  - `grant_acl`: upsert the canonical `permissions` row by name first (in case the migration's
    seed was rolled back), then insert into `acls` only if no row already exists for
    `(principal_id, permission_id, dataset_id)`. Match Python's silent-skip-on-duplicate behavior
    per [routers/permissions.md §2.6](../routers/permissions.md#26-post-datasetsprincipal_id--grant-permission-on-datasets-to-a-principal).
  - `create_tenant`: three writes per
    [routers/permissions.md §2.8](../routers/permissions.md#28-post-tenantstenant_name--create-a-new-tenant-owned-by-caller)
    side-effects 1–3. Insert into `principals` (`type='tenant'`) **and** `tenants`; set
    `users.tenant_id = new_tenant.id` for the caller; insert `(caller, new_tenant)` into
    `user_tenants`. Python issues these as three sequential commits without a transaction — match
    verbatim. **Do not** wrap them in a single transaction (this is open question §6 in the
    permissions spec; we replicate Python).
  - `select_current_tenant`: a single `UPDATE users SET tenant_id = ? WHERE id = ?`. When the
    target is non-null, first verify the caller is in `user_tenants` for that tenant and return
    `EntityNotFound` matching Python's `TenantNotFoundError("User is not part of the tenant.")`.
  - `remove_user_from_tenant`: three deletes ordered per
    [routers/permissions.md §2.12](../routers/permissions.md#212-delete-tenantstenant_idusersuser_id--remove-user-from-tenant)
    side-effects 1–3. Reject removing the tenant owner with `Validation` (Python's
    `CogneeValidationError`).
  - `list_*`: straightforward joins; cap at `LIMIT 50` to match Python's defaults
    ([routers/README.md §3.4](../routers/README.md#34-pagination)).
  - `is_tenant_admin`: delegate to step 5's helper.
- **Spec reference**: [tenants.md §9](../tenants.md#9-repository-surface),
  [routers/permissions.md §2.x](../routers/permissions.md#2-endpoints) for per-method side effects.
- **Verify**: `cargo check -p cognee-database`. Full integration test in step 18.

### Step 8: Replace P2's stub repository with the SeaORM impl

- **File(s)**: `crates/lib/src/lib.rs` (re-export `permissions::PermissionsRepository` and the
  SeaORM impl); `crates/http-server/src/state.rs` (replace the placeholder slot); each call-site
  flagged with `// TODO(P5): wire real PermissionsRepository` (grep the workspace).
- **Action**: Add `pub use cognee_database::permissions::{PermissionsRepository,
  SeaOrmPermissionsRepository}` to `cognee-lib`. In `AppState`, change the
  `permissions: Option<…>` placeholder slot from P2 into a non-optional
  `pub permissions: Arc<dyn PermissionsRepository>` field. Wire the constructor in
  `AppState::build` to instantiate `SeaOrmPermissionsRepository::new(db.clone())`. Walk every P2
  call-site that today says `unimplemented!()` or returns a hard-coded `true` for permission
  checks and rewrite it to call `state.permissions.user_can(...)`. Remove every `// TODO(P5):`
  marker as you fix the corresponding call. After this step, the workspace must contain zero
  `TODO(P5)` markers.
- **Spec reference**: [tenants.md §9](../tenants.md#9-repository-surface),
  [routers/README.md §3.8](../routers/README.md#38-permission-gates).
- **Verify**: `rg "TODO\(P5\)" crates/ docs/` returns nothing. `cargo check --all-targets`.
  `cargo test -p cognee-http-server` (P2's permission-gated tests now actually exercise the real
  resolver).

### Step 9: Permissions DTOs

- **File(s)**: `crates/http-server/src/dto/permissions.rs` (new); register the module in
  `crates/http-server/src/dto/mod.rs`.
- **Action**: Add the DTO structs verbatim from [routers/permissions.md §4](../routers/permissions.md#4-dto-definitions):
  request DTOs (`SelectTenantDTO`, `GrantDatasetPermissionBody` newtype, the five
  `*Query` structs), response DTOs (`MessageResponse`, `CreateRoleResponse`,
  `CreateTenantResponse`, `SelectTenantResponse`, `TenantSummary`, `RoleSummary`, `UserInRole`,
  `UserInTenant`). All structs derive `Serialize`/`Deserialize`/`ToSchema` as appropriate and use
  `#[serde(rename_all = "snake_case")]`. **Do not** apply camelCase rename anywhere in this
  module — the permissions router is uniformly snake_case.
- **Spec reference**: [routers/permissions.md §4](../routers/permissions.md#4-dto-definitions).
- **Verify**: `cargo check -p cognee-http-server`. Inline unit test confirming
  `serde_json::from_str::<SelectTenantDTO>(r#"{"tenant_id": null}"#)` deserializes to `Some(None)`
  on the `Option<Uuid>` field (Python parity for the explicit-null body).

### Step 10: Permissions router — read endpoints (GETs)

- **File(s)**: `crates/http-server/src/routers/permissions.rs` (new; register in
  `crates/http-server/src/routers/mod.rs`).
- **Action**: Implement five GET handlers, one per
  [routers/permissions.md §2.1–§2.5](../routers/permissions.md#21-get-tenantsme--list-tenants-the-caller-is-a-member-of):
  - `GET /tenants/me` ([§2.1](../routers/permissions.md#21-get-tenantsme--list-tenants-the-caller-is-a-member-of)) — auth-only; calls `list_my_tenants(user.id)`.
  - `GET /tenants/{tenant_id}/roles` ([§2.2](../routers/permissions.md#22-get-tenantstenant_idroles--list-roles-in-a-tenant)) — gated by
    `is_tenant_admin(user.id, tenant_id)`; calls `list_tenant_roles(tenant_id)`. The
    `description` field on `RoleSummary` is **always `null`** (Python's `getattr` defaults to
    None) — keep the field on the wire for parity.
  - `GET /tenants/{tenant_id}/roles/{role_id}/users` ([§2.3](../routers/permissions.md#23-get-tenantstenant_idrolesrole_idusers--list-users-in-a-role)) — `is_tenant_admin` gate; the response items use `name` for what is semantically the email — match Python's wire field name.
  - `GET /tenants/{tenant_id}/roles/users/{user_id}` ([§2.4](../routers/permissions.md#24-get-tenantstenant_idrolesusersuser_id--list-a-users-roles-in-a-tenant)) — `is_tenant_admin` gate.
  - `GET /tenants/{tenant_id}/users` ([§2.5](../routers/permissions.md#25-get-tenantstenant_idusers--list-users-in-a-tenant)) — `is_tenant_admin` gate;
    each `UserInTenant` has `roles` scoped to `tenant_id` only.
  Each handler is `#[tracing::instrument(skip(state))]` with the span name from the corresponding
  spec section. Each emits the canonical `ApiError` envelope on failure.
- **Spec reference**: [routers/permissions.md §2.1–§2.5](../routers/permissions.md#2-endpoints).
- **Verify**: `cargo check -p cognee-http-server`. Functional checks land in step 19.

### Step 11: Permissions router — `POST /datasets/{principal_id}` (ACL grant)

- **File(s)**: `crates/http-server/src/routers/permissions.rs` (continued).
- **Action**: Implement the ACL-grant endpoint per
  [routers/permissions.md §2.6](../routers/permissions.md#26-post-datasetsprincipal_id--grant-permission-on-datasets-to-a-principal).
  The body is a top-level `Vec<Uuid>` (model as `GrantDatasetPermissionBody` newtype with
  `#[serde(transparent)]`). Validate `permission_name ∈ {"read","write","delete","share"}` →
  `400` if not; reject empty `dataset_ids` with `400` (Python silently no-ops, but the spec calls
  for an explicit reject — see [routers/permissions.md §6.1](../routers/permissions.md#6-open-questions)
  for the open-question note; **match the spec's resolution: empty list returns 200 with the
  Python success body, no 400**). Filter `dataset_ids` to the subset the caller can `share`
  (call `user_can(caller, dataset_id, "share")` for each — silently drop the rest, matching Python
  per [routers/permissions.md §6.2](../routers/permissions.md#6-open-questions)). For each
  surviving dataset, call `permissions.grant_acl(principal_id, dataset_id, permission_name)`.
  Response is `{"message": "Permission assigned to principal"}`.
- **Spec reference**: [routers/permissions.md §2.6](../routers/permissions.md#26-post-datasetsprincipal_id--grant-permission-on-datasets-to-a-principal).
- **Verify**: `cargo check -p cognee-http-server`. Functional checks in step 19.

### Step 12: Permissions router — `POST /roles`, `POST /tenants`, `POST /tenants/select`

- **File(s)**: `crates/http-server/src/routers/permissions.rs` (continued).
- **Action**: Three create-style handlers:
  - `POST /roles?role_name=` ([§2.7](../routers/permissions.md#27-post-rolesrole_name--create-role-under-callers-current-tenant)) — **owner-only** gate (`caller.id == tenants.owner_id` for caller's *current* tenant; admin role does **not** suffice). Trim and reject empty `role_name` with `400`. Calls `create_role(caller.tenant_id, role_name)`.
  - `POST /tenants?tenant_name=` ([§2.8](../routers/permissions.md#28-post-tenantstenant_name--create-a-new-tenant-owned-by-caller)) — auth-only (any user can create a tenant; they become its owner). Calls `create_tenant(name, caller.id)` which also sets the user's current tenant and inserts the M2M membership row (per the repository contract from step 7).
  - `POST /tenants/select` ([§2.9](../routers/permissions.md#29-post-tenantsselect--set-callers-current-tenant-put-replace-semantics)) — auth-only with implicit membership gate inside the repository call. Body is `SelectTenantDTO { tenant_id: Option<Uuid> }`. **Critical Python parity**: when the request `tenant_id` is `null`, the response field `tenant_id` must serialize as the literal **JSON string `"None"`**, not JSON `null`. The default `Option<Uuid>` serialization emits `null`, so this requires a custom serializer (`serialize_with` that emits `String("None")` when `None`, otherwise the UUID's hyphenated string form). See [routers/permissions.md §2.9 (Python parity notes)](../routers/permissions.md#29-post-tenantsselect--set-callers-current-tenant-put-replace-semantics) and [§6.4](../routers/permissions.md#6-open-questions).
- **Spec reference**: same three sub-sections in `routers/permissions.md`.
- **Verify**: `cargo check -p cognee-http-server`. The `null → "None"` behavior is exercised in
  step 19's `test_permissions_select_null.rs`.

### Step 13: Permissions router — user/tenant membership endpoints

- **File(s)**: `crates/http-server/src/routers/permissions.rs` (continued).
- **Action**: Three handlers covering user-membership writes:
  - `POST /users/{user_id}/roles?role_id=` ([§2.10](../routers/permissions.md#210-post-usersuser_idrolesrole_id--assign-role-to-user)) — **owner-only** gate (admins cannot). Reject duplicate `(user_id, role_id)` with `400 EntityAlreadyExists`.
  - `POST /users/{user_id}/tenants?tenant_id=` ([§2.11](../routers/permissions.md#211-post-usersuser_idtenantstenant_id--add-user-to-tenant)) — **owner-only** gate.
  - `DELETE /tenants/{tenant_id}/users/{user_id}` ([§2.12](../routers/permissions.md#212-delete-tenantstenant_idusersuser_id--remove-user-from-tenant)) — **broader admin-allowed** gate (`is_tenant_admin`). This is the **only** mutation endpoint in this router that admin role members can invoke; the others are owner-only. Reject removing the tenant owner with `400` (Python's `CogneeValidationError`).
  Cross-reference [routers/permissions.md §2.13 (authorization summary table)](../routers/permissions.md#213-authorization-summary-table) when wiring the gates — the asymmetry is intentional.
- **Spec reference**: [routers/permissions.md §2.10–§2.12](../routers/permissions.md#2-endpoints), [§2.13](../routers/permissions.md#213-authorization-summary-table).
- **Verify**: `cargo check -p cognee-http-server`. Functional verification in step 19.

### Step 14: Settings DTOs and helpers

- **File(s)**: `crates/http-server/src/dto/settings.rs` (new).
- **Action**: Add the DTO structs from [routers/settings.md §4](../routers/settings.md#4-dto-definitions):
  `ConfigChoice`, `LLMConfigOutputDTO`, `VectorDBConfigOutputDTO`, `SettingsDTO`,
  `LLMConfigInputDTO`, `LlmProvider` enum, `VectorDBConfigInputDTO`, `VectorDbProvider` enum,
  `SettingsPayloadDTO`. Add the two helpers — `redact_api_key(Option<&str>) -> Option<String>`
  (mirrors Python's `key[0:10] + "*" * (len(key) - 10)`) and `should_persist_api_key(&str) -> bool`
  (mirrors the `'*****' not in key and len(key.strip()) > 0` substring guard). The substring check
  is **non-equality** — any submitted key containing the literal `"*****"` is dropped, matching
  Python's footgun behavior verbatim per [routers/settings.md §2.2 Python parity notes](../routers/settings.md#22-post---save-partial-update-settings).
- **Spec reference**: [routers/settings.md §4](../routers/settings.md#4-dto-definitions).
- **Verify**: inline unit tests for the helpers covering `""`, `"   "`, `"sk-real-key"`,
  `"sk-prefix*****abc"`, `"AAAAAAAAAA*****"`, and the no-key / short-key / long-key cases for
  `redact_api_key`. `cargo test -p cognee-http-server --lib dto::settings::tests`.

### Step 15: `cognee_lib::settings` façade

- **File(s)**: `crates/lib/src/settings.rs` (new); add `pub mod settings;` in `crates/lib/src/lib.rs`.
- **Action**: Expose `get_settings() -> SettingsSnapshot`, `save_llm_config(input)`, and
  `save_vector_db_config(input)` as thin wrappers over the existing `LlmConfig` and
  `VectorDbConfig` process-singletons. The provider/model lists rendered into the GET response are
  static constants — declare them inside the **router** file (`routers/settings.rs`) as `static
  LLM_PROVIDERS: &[ConfigChoice]` etc., and copy verbatim from Python's
  [`get_settings.py` L60-L179](https://github.com/topoteretes/cognee/blob/main/cognee/modules/settings/get_settings.py#L60-L179).
  The cross-SDK parity test (step 21) compares these arrays as JSON; literal-equality with Python
  is non-negotiable. **Do not** persist settings to a relational table — Python keeps state in
  process memory and we replicate that exactly per [routers/settings.md §3](../routers/settings.md#3-cross-cutting-behavior).
- **Spec reference**: [routers/settings.md §3 / §5 task 3](../routers/settings.md#5-implementation-tasks).
- **Verify**: `cargo check -p cognee-lib --features server`.

### Step 16: Settings router

- **File(s)**: `crates/http-server/src/routers/settings.rs` (new; register in `routers/mod.rs`).
- **Action**: Two handlers:
  - `GET /` ([§2.1](../routers/settings.md#21-get---read-current-settings)) — auth-only; reads
    `LlmConfig` + `VectorDbConfig` snapshots, applies `redact_api_key` to both `api_key` fields,
    populates the static provider/model lists. **Critical**: never log the raw key
    ([routers/settings.md §3](../routers/settings.md#3-cross-cutting-behavior)). For the empty
    vector-DB key edge case where Python would compute `"*" * -10`, return the empty string
    rather than `null` — see [routers/settings.md §6.1](../routers/settings.md#6-open-questions).
  - `POST /` ([§2.2](../routers/settings.md#22-post---save-partial-update-settings)) — auth-only.
    Body is `SettingsPayloadDTO` (both sub-objects optional). For each provided sub-object, drop
    the `api_key` field via `should_persist_api_key`; only forward kept values to the
    `cognee_lib::settings::save_*_config` helpers. **There is no admin-role gate** — any
    authenticated user can rewrite global settings; this matches Python and is documented as
    open question §6.5 in the settings spec. Response is `200 OK` with body `null` (Python parity
    — handler has no `return`).
  Both handlers carry `#[tracing::instrument(skip(state))]` spans named `cognee.api.settings.get`
  / `cognee.api.settings.save`.
- **Spec reference**: [routers/settings.md §2](../routers/settings.md#2-endpoints).
- **Verify**: `cargo check -p cognee-http-server`. Functional verification in step 20.

### Step 17: Configuration DTOs, façade, and router

- **File(s)**: `crates/http-server/src/dto/configuration.rs` (new);
  `crates/database/src/entities/principal_configuration.rs` (new — only if missing today; check
  the `entities/` listing); `crates/lib/src/users.rs` (extend with three façades);
  `crates/http-server/src/routers/configuration.rs` (new; register in `routers/mod.rs`).
- **Action**:
  - DTOs: `StorePrincipalConfigurationPayloadDTO` (snake_case JSON body — note the `Form(...)` in
    Python is a Pydantic typing artifact and the body is JSON, **not** multipart;
    [routers/configuration.md §2.3 Python parity notes](../routers/configuration.md#23-post-store_user_configuration--upsert-a-named-configuration-for-the-caller))
    and `PrincipalConfigurationDTO` with the **mixed snake/camel keys** from
    [routers/configuration.md §4](../routers/configuration.md#4-dto-definitions): `id`, `name`,
    `configuration` are snake_case but `ownerId`, `createdAt`, `updatedAt` are camelCase via
    field-level `#[serde(rename = "...")]`. Do **not** apply `rename_all` globally.
  - Façades: `cognee_lib::users::{store_principal_configuration, get_principal_configuration,
    get_principal_all_configuration}`. Owner-id-keyed lookups for the list variant; **no
    owner-id check** on the by-id fetch (Python parity bug, replicated verbatim — see
    [routers/configuration.md §2.2 Authorization checks](../routers/configuration.md#22-get-get_user_configurationconfig_id--fetch-one-configuration-by-id)
    and document the security gap in this phase's §6 acceptance criteria).
  - Router: three endpoints per [routers/configuration.md §2](../routers/configuration.md#2-endpoints):
    - `GET /get_user_configuration/` ([§2.1](../routers/configuration.md#21-get-get_user_configuration--list-all-of-callers-named-configurations)) — **trailing slash matters**; configure axum's strict-slash matching.
    - `GET /get_user_configuration/{config_id}` ([§2.2](../routers/configuration.md#22-get-get_user_configurationconfig_id--fetch-one-configuration-by-id)) — returns `200 {}` on miss (no 404). Cross-user reads are permitted (Python parity).
    - `POST /store_user_configuration` ([§2.3](../routers/configuration.md#23-post-store_user_configuration--upsert-a-named-configuration-for-the-caller)) — returns **`200 OK` with body `null`**, **NOT `204 No Content`**. Strict wire parity.
- **Spec reference**: [routers/configuration.md](../routers/configuration.md) (entire doc).
- **Verify**: `cargo check -p cognee-http-server`. Functional verification in step 20.

### Step 18: Wire all three routers into `build_router` and update bootstrap

- **File(s)**: `crates/http-server/src/lib.rs` (mount the routers); `crates/http-server/src/lifecycle.rs`
  (replace the P0 stub `bootstrap_default_principals` with the real implementation);
  `crates/http-server/src/openapi.rs` (register the new endpoints in `paths(...)`).
- **Action**:
  - In `build_router`, nest the three routers with their canonical prefixes:
    `/api/v1/permissions`, `/api/v1/settings`, `/api/v1/configuration`. Each layered under the
    `AuthenticatedUser` middleware where the spec calls for it (every endpoint in this phase is
    authenticated).
  - In `lifecycle::on_startup`, replace the no-op `bootstrap_default_principals` from P0 with the
    real implementation per [tenants.md §6](../tenants.md#6-bootstrap-default-user--default-tenant):
    1. Upsert the four canonical permissions (idempotent — the migration already seeded them, but
       the upsert covers DBs where the seed was rolled back).
    2. Upsert the `"default_tenant"` row (with `owner_id = NULL` initially).
    3. Upsert the `"default_user@example.com"` user with `tenant_id = default_tenant.id` and an
       empty password (per [tenants.md §6](../tenants.md#6-bootstrap-default-user--default-tenant) — the password
       is never used for login when `REQUIRE_AUTHENTICATION=false`).
    4. Insert the `(default_user, default_tenant)` row in `user_tenants`.
    Every step uses an upsert keyed on the natural identifier (email, name) so re-running on a
    bootstrapped DB is a no-op.
  - In `openapi.rs`, register all 18 endpoints (13 + 2 + 3) into the `paths(...)` list of
    `ApiDoc`, plus add `permissions`, `settings`, `configuration` to the OpenAPI tag list with
    descriptions matching Python's tags in `client.py`.
- **Spec reference**: [tenants.md §6](../tenants.md#6-bootstrap-default-user--default-tenant),
  [architecture.md §13](../architecture.md#13-openapi-generation--utoipa),
  [architecture.md §14](../architecture.md#14-startup-lifecycle).
- **Verify**: `cargo check -p cognee-http-server --all-targets`. Manual smoke: boot the server
  twice against the same SQLite file; assert no duplicate-key errors and exactly one
  `default_user@example.com` row.

## 5. Tests

Each test file lives at `crates/http-server/tests/<name>.rs` (or
`crates/database/tests/<name>.rs` for the repository-only test) and uses the integration-test
support module from P0 (`tests/support/mod.rs`), extended as needed for tenant/role/ACL fixtures.
Tests run on in-memory SQLite seeded with the migration from steps 1–2.

| File | Coverage |
|---|---|
| `crates/database/tests/permissions_repository.rs` | Round-trip every method on `SeaOrmPermissionsRepository`. The headline test is the `user_can` truth table: build a fixture with one user × one dataset × every combination of (superuser, direct ACL, role ACL, tenant ACL, user-default, role-default, tenant-default, none) and assert the resolution against the 8-step matrix from [tenants.md §5.1](../tenants.md#51-resolution-order-short-circuit-on-first-hit). Also exercise the migration idempotency case (run migrations twice, assert no error and exactly four `permissions` rows). |
| `crates/http-server/tests/test_permissions_acl.rs` | `POST /api/v1/permissions/datasets/{principal_id}` round-trip: caller with `share` on a dataset grants `read` to a second user → second user's `user_can(read, dataset)` returns true. Mixed allow/deny dataset list — assert silent skip on datasets the caller cannot share (Python parity per [routers/permissions.md §2.6](../routers/permissions.md#26-post-datasetsprincipal_id--grant-permission-on-datasets-to-a-principal)). |
| `crates/http-server/tests/test_permissions_roles.rs` | `POST /roles` (owner-only — admin-role caller gets `403`); `POST /users/{u}/roles` (owner-only); `GET /tenants/{t}/roles/{r}/users` shows the assigned user. Cross-tenant isolation: tenant-A admin attempting these calls against tenant B → `403`. |
| `crates/http-server/tests/test_permissions_tenants.rs` | Full lifecycle: create tenant via `POST /tenants` → caller is set as owner and current tenant; add user via `POST /users/{u}/tenants` → user appears in `GET /tenants/{t}/users`; `POST /tenants/select` to switch caller's current tenant; `GET /tenants/me` reflects the membership. `DELETE /tenants/{t}/users/{u}` against tenant owner → `400 Validation`. |
| `crates/http-server/tests/test_permissions_select_null.rs` | `POST /api/v1/permissions/tenants/select` with body `{"tenant_id": null}` → `200`; assert response body is exactly `{"message": "Tenant selected.", "tenant_id": "None"}` — note the literal **JSON string `"None"`**, not `null`. Strict Python parity per [routers/permissions.md §2.9](../routers/permissions.md#29-post-tenantsselect--set-callers-current-tenant-put-replace-semantics). |
| `crates/http-server/tests/test_permissions_resolution.rs` | End-to-end resolution table over HTTP: build the eight fixture cases above, then for each case make a real authenticated request that goes through a permission-gated endpoint (e.g. `GET /api/v1/datasets/{id}` from P2). Assert allow/deny per [tenants.md §5.1](../tenants.md#51-resolution-order-short-circuit-on-first-hit). |
| `crates/http-server/tests/test_settings.rs` | `GET /api/v1/settings` redacts the API key per `redact_api_key`; `POST /api/v1/settings` round-trips (set then get the redacted form); echo-guard test (resubmit a key containing `"*****"` → real key not overwritten); `provider: "bedrock"` on save → `400`; partial save (only `llm` provided → `vector_db` unchanged). |
| `crates/http-server/tests/test_configuration.rs` | Store → list returns one row; store same `name` again → list still has one row with bumped `updatedAt`; two users storing `"default"` → each sees only their own row; `GET /…/{nonexistent}` → `200 {}`; `POST /store_user_configuration` returns `200` with body `null` (assert via `len(body) == 4` for `"null"` — strict wire parity); **cross-user fetch test (Python-parity bug replication)**: user A's GET on user B's `config_id` returns user B's data — comment loudly that this is a known confidentiality bug we replicate for parity per [routers/configuration.md §6.1](../routers/configuration.md#6-open-questions). |

Inline unit tests in the source files cover smaller invariants (DTO serialization shapes, the
`redact_api_key` / `should_persist_api_key` matrix, `is_tenant_admin` truth table — see step 5).

## 6. Acceptance criteria

- [ ] `cargo check --all-targets` passes for the whole workspace.
- [ ] All P5 tests in §5 pass: `cargo test -p cognee-http-server` and `cargo test -p
      cognee-database --test permissions_repository`.
- [ ] The RBAC migration runs cleanly **twice in a row** against an empty DB (no duplicate-table
      errors, `permissions` table contains exactly four rows).
- [ ] The RBAC migration runs cleanly against a Python-seeded DB — verified by a fixture in
      `e2e-cross-sdk/` that snapshots the Python schema, then runs the Rust migration on top, and
      asserts no rows mutated and no errors. (Defer the actual cross-SDK harness wiring to P8 if
      needed; this acceptance gate becomes "manual run on a checked-in Python schema dump"
      instead.)
- [ ] `rg "TODO\(P5\)" crates/ docs/` returns no matches. The placeholder
      `PermissionsRepository` from P2 is removed; every call-site uses the real SeaORM impl.
- [ ] `bootstrap_default_principals` in `lifecycle.rs` is the real implementation per
      [tenants.md §6](../tenants.md#6-bootstrap-default-user--default-tenant); booting a fresh
      server creates the default tenant + default user + four permissions + M2M membership in
      one go, and a second boot is a no-op.
- [ ] Manual smoke: hit `GET /api/v1/permissions/tenants/me` after first boot and confirm the
      default user's `user_tenants` row resolves to `[{"id": "<default_tenant_id>", "name":
      "default_tenant"}]`.
- [ ] Manual smoke: `POST /api/v1/permissions/tenants/select {"tenant_id": null}` returns the
      JSON string `"None"` in the response body's `tenant_id` field — verify with `curl … |
      jq '.tenant_id'` returning the string `"None"` (with quotes), not `null`.
- [ ] Manual smoke: `POST /api/v1/configuration/store_user_configuration` returns
      `Content-Length: 4` and a body of literally `null` (Python parity, not `204`).
- [ ] `scripts/check_all.sh` passes (fmt, `cargo check --all-targets`, `cargo clippy -- -D
      warnings`, capi/python/js wrapper checks unchanged).
- [ ] Status row for **P5** in [implementation/README.md](README.md) flips
      **Draft → In Progress → Done**.
- [ ] Status rows for `permissions`, `settings`, `configuration` in
      [routers/README.md](../routers/README.md) flip **Draft → In Progress → Done**.

## 7. Files touched

New (under `crates/database/`):

- `src/migrator/m_<timestamp>_tenants_rbac.rs`
- `src/permissions/mod.rs`
- `src/permissions/sea_orm_impl.rs`
- `src/permissions/tenant_admin.rs`
- `src/entities/role_default_permission.rs`
- `src/entities/user_default_permission.rs`
- `src/entities/tenant_default_permission.rs`
- `src/entities/principal_configuration.rs` *(only if not already present from earlier work)*
- `tests/permissions_repository.rs`

New (under `crates/http-server/`):

- `src/dto/permissions.rs`
- `src/dto/settings.rs`
- `src/dto/configuration.rs`
- `src/routers/permissions.rs`
- `src/routers/settings.rs`
- `src/routers/configuration.rs`
- `tests/test_permissions_acl.rs`
- `tests/test_permissions_roles.rs`
- `tests/test_permissions_tenants.rs`
- `tests/test_permissions_select_null.rs`
- `tests/test_permissions_resolution.rs`
- `tests/test_settings.rs`
- `tests/test_configuration.rs`

New (under `crates/lib/`):

- `src/settings.rs` *(façade over `LlmConfig` / `VectorDbConfig`)*

Modified:

- `crates/database/src/migrator/mod.rs` — register the new migration.
- `crates/database/src/entities/mod.rs` — re-export the new entities; bring existing
  `principal.rs`/`tenant.rs`/`role.rs`/`user_role.rs`/`user_tenant.rs`/`permission.rs`/`acl.rs`
  files up to spec (auditing column lists against [tenants.md §3](../tenants.md#3-tables)).
- `crates/database/src/entities/user.rs` — add `tenant_id` column to the `Model` if missing.
- `crates/database/src/lib.rs` — `pub mod permissions;`.
- `crates/lib/src/lib.rs` — `pub mod settings;`; re-export
  `cognee_database::permissions::{PermissionsRepository, SeaOrmPermissionsRepository}`.
- `crates/lib/src/users.rs` — add the three principal-configuration façades.
- `crates/http-server/src/state.rs` — replace the P2 placeholder permissions slot with
  `Arc<dyn PermissionsRepository>`; update `AppState::build`.
- `crates/http-server/src/lifecycle.rs` — replace stub `bootstrap_default_principals` with the
  real implementation per [tenants.md §6](../tenants.md#6-bootstrap-default-user--default-tenant).
- `crates/http-server/src/lib.rs` — `build_router` mounts the three new routers.
- `crates/http-server/src/openapi.rs` — register the 18 new endpoints in `paths(...)`; add
  `permissions`, `settings`, `configuration` tag descriptions.
- `crates/http-server/src/dto/mod.rs` — `pub mod {permissions, settings, configuration};`.
- `crates/http-server/src/routers/mod.rs` — `pub mod {permissions, settings, configuration};`.
- Every P2 call-site flagged with `// TODO(P5): wire real PermissionsRepository` — the marker is
  removed and the real `state.permissions.user_can(...)` is wired in.
- `docs/http-server/implementation/README.md` — flip P5 status row.
- `docs/http-server/routers/README.md` — flip the `permissions`, `settings`, `configuration`
  status rows.

Out of scope (do NOT touch in this phase):

- The cross-SDK HTTP parity harness (`e2e-cross-sdk/harness/test_http_*.py`) — lands as part of
  P8.
- Any new `/api/v1/*` router beyond the three above — `/activity`, `/sync`, `/checks`,
  `/notebooks`, `/responses` belong to P6/P7.
- Permission-set caching (the 10-second LRU mentioned in [tenants.md §5.4](../tenants.md#54-caching))
  — defer until profiling shows a hot path.
- Any change to the polymorphic-principal model beyond what
  [tenants.md §3](../tenants.md#3-tables) specifies (e.g. global cross-tenant roles, soft-delete
  semantics) — those are open questions tracked in [tenants.md §13](../tenants.md#13-open-questions).
- An admin-role gate on `/api/v1/settings` — Python lets any authenticated user save; we match
  verbatim per [routers/settings.md §6.5](../routers/settings.md#6-open-questions).
- Adding a unique index on `principal_configuration (owner_id, name)` — Python relies on
  SELECT-then-UPSERT without one; we match per [routers/configuration.md §6.2](../routers/configuration.md#6-open-questions).
