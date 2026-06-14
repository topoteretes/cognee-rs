# 10 — Wire permission revoke endpoints (HTTP)

> Wave 2 · Priority P0 · Track A · Release-blocking: yes · Effort: 0.5d ·
> Depends on: — · Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) B8.1, [release-readiness-plan.md](../release-readiness-plan.md) T8.3

[← back to index](00-INDEX.md)

## Goal

The Python permissions router exposes three DELETE endpoints that let an admin/owner
**undo** grants. The Rust HTTP server exposes the grant/assign side but **not** the
revoke side, even though the underlying repository methods already exist
(`revoke_acl`, `revoke_role`). Wire them up:

| Route | Effect | Repo method | Status |
|---|---|---|---|
| `DELETE /api/v1/permissions/datasets/{principal_id}` | revoke an ACL grant | `revoke_acl` ✅ exists | wire |
| `DELETE /api/v1/permissions/users/{user_id}/roles` | remove a user from a role | `revoke_role` ✅ exists | wire |
| `DELETE /api/v1/permissions/roles/{role_id}` | delete a role + its memberships/ACLs | **no repo method yet** | see step 4 |

Also fix the **false** doc claim in `docs/http-server/routers/permissions.md:319` that
says revoke endpoints are omitted "to match Python" — Python *has* them.

## Background & why

Without revoke, a Rust deployment can grant dataset permissions and assign roles but can
**never take them back** over HTTP — a real RBAC hole and a parity gap. The audit
classifies this as "mostly wiring" because the destructive repository operations
(`revoke_acl`, `revoke_role`) are implemented and tested at the repo layer; only the
router handlers and routes are missing. The one exception is full role deletion
(`DELETE /roles/{role_id}`), which has no repo method yet — handle per step 4.

### Python vs Rust

| | Python | Rust (now) |
|---|---|---|
| grant ACL | `POST /datasets/{principal_id}` | ✅ `grant_dataset_permission` (`permissions.rs:330`) |
| **revoke ACL** | `DELETE /datasets/{principal_id}` (`get_permissions_router.py:89`) | ❌ missing |
| assign role | `POST /users/{user_id}/roles` | ✅ `assign_role` (`permissions.rs:522`) |
| **remove from role** | `DELETE /users/{user_id}/roles` (`get_permissions_router.py:246`) | ❌ missing |
| create role | `POST /roles` | ✅ `create_role` (`permissions.rs:395`) |
| **delete role** | `DELETE /roles/{role_id}` (`get_permissions_router.py:175`) | ❌ missing + no repo method |

## Prerequisites

```bash
git checkout main && git pull
git checkout -b task/10-permission-revoke-endpoints
```

Read first:
- Rust router: [crates/http-server/src/routers/permissions.rs](../../../crates/http-server/src/routers/permissions.rs) — study `grant_dataset_permission` (line 330), `assign_role` (522), `remove_user_from_tenant` (611, the existing DELETE handler to mirror), and the `router()` builder (631).
- Rust DTOs: [crates/http-server/src/dto/permissions.rs](../../../crates/http-server/src/dto/permissions.rs).
- Rust repo trait: [crates/database/src/permissions/mod.rs](../../../crates/database/src/permissions/mod.rs) — `revoke_acl` (110), `revoke_role` (128), `user_can` (87), `role_tenant_id` (184), `tenant_owner` (178).
- Rust repo impl: [crates/database/src/permissions/sea_orm_impl.rs](../../../crates/database/src/permissions/sea_orm_impl.rs) — `revoke_acl` (358), `revoke_role` (561).
- Doc to fix: [docs/http-server/routers/permissions.md](../../../docs/http-server/routers/permissions.md) line ~319.
- Existing HTTP test for the harness pattern: [crates/http-server/tests/test_permissions_acl.rs](../../../crates/http-server/tests/test_permissions_acl.rs) and its `mod support`.
- Python: `/tmp/cognee-python/cognee/api/v1/permissions/routers/get_permissions_router.py` lines 89-130 (revoke ACL), 175-201 (delete role), 246-278 (remove from role).

## Python reference

`/tmp/cognee-python/cognee/api/v1/permissions/routers/get_permissions_router.py`

### DELETE /datasets/{principal_id} — revoke ACL (line 89)
```python
@permissions_router.delete("/datasets/{principal_id}")
async def revoke_datasets_permission_from_principal(
    permission_name: str,           # query param
    dataset_ids: List[UUID],        # request body (JSON array)
    principal_id: UUID,             # path
    user: User = Depends(get_authenticated_user),
):
    await authorized_revoke_permission_on_datasets(
        principal_id, [...dataset_ids], permission_name, user.id,
    )
    return JSONResponse(status_code=200, content={"message": "Permission revoked from principal"})
```
Mirror image of the grant endpoint: same path/query/body shape; success body is
`{"message": "Permission revoked from principal"}`.

### DELETE /users/{user_id}/roles — remove user from role (line 246)
```python
@permissions_router.delete("/users/{user_id}/roles")
async def remove_user_from_role_endpoint(
    user_id: UUID,                  # path
    role_id: UUID,                  # query param
    user: User = Depends(get_authenticated_user),
):
    await remove_user_from_role_method(user_id=user_id, role_id=role_id, owner_id=user.id)
    return JSONResponse(status_code=200, content={"message": "User removed from role"})
```
Mirror image of `POST /users/{user_id}/roles` (`assign_role`); success body
`{"message": "User removed from role"}`.

### DELETE /roles/{role_id} — delete role (line 175)
```python
@permissions_router.delete("/roles/{role_id}")
async def delete_role_endpoint(role_id: UUID, user=...):
    await delete_role_method(role_id=role_id, owner_id=user.id)
    return JSONResponse(status_code=200, content={"message": "Role deleted"})
```
Removes all user-role memberships and ACL entries for the role, then deletes the role
itself. **Note:** there is **no** Rust repo method for this yet (`revoke_role` only
removes one user from one role). See step 4.

**Auth to match:** Python uses `has_user_management_permission`/owner-level checks. Use
the existing Rust helpers `require_tenant_owner` / `require_tenant_admin` for symmetry
with the corresponding POST handlers:
- revoke ACL → same per-dataset `share` gate as grant (silent-skip pattern).
- remove from role → owner-only on the role's tenant (mirror `assign_role`, which is
  owner-only via `require_tenant_owner` on `role_tenant_id`).
- delete role → owner-only on the role's tenant (mirror `create_role`).

## Files to change

| Path | Change |
|---|---|
| `crates/http-server/src/routers/permissions.rs` | Add 2 (or 3) DELETE handlers + register routes. |
| `crates/http-server/src/dto/permissions.rs` | Add query DTOs (`RevokeDatasetPermissionQuery`, `RemoveUserFromRoleQuery`) reusing existing body/response DTOs. |
| `crates/database/src/permissions/{mod.rs,sea_orm_impl.rs}` | (step 4 only, if implementing `DELETE /roles/{role_id}`) add a `delete_role` repo method. |
| `docs/http-server/routers/permissions.md` | Fix the false "omitted to match Python" claim (~line 319); document the 2–3 new endpoints + auth table. |
| `crates/http-server/tests/test_permissions_revoke.rs` | New HTTP test file. |

## Implementation steps

1. **Add DTOs** in `crates/http-server/src/dto/permissions.rs` (mirror the existing
   `GrantDatasetPermissionQuery` / `AssignRoleQuery`):
   ```rust
   /// Query for `DELETE /datasets/{principal_id}` (revoke ACL).
   #[derive(Debug, Clone, Deserialize, ToSchema, IntoParams)]
   pub struct RevokeDatasetPermissionQuery {
       /// one of read|write|delete|share
       pub permission_name: String,
   }

   /// Query for `DELETE /users/{user_id}/roles` (remove user from role).
   #[derive(Debug, Clone, Deserialize, ToSchema, IntoParams)]
   pub struct RemoveUserFromRoleQuery {
       pub role_id: Uuid,
   }
   ```
   Reuse `GrantDatasetPermissionBody(Vec<Uuid>)` for the revoke-ACL body and
   `MessageResponse` for every response. (Match the derive macros actually used by the
   existing DTOs in this file — copy them verbatim.)

2. **Add `revoke_dataset_permission`** handler in `permissions.rs` — clone
   `grant_dataset_permission` (line 330), swap `grant_acl` → `revoke_acl`, and change the
   message. Keep the empty-list → 200 success and the per-dataset `share` silent-skip:
   ```rust
   #[utoipa::path(
       delete,
       path = "/api/v1/permissions/datasets/{principal_id}",
       tag = "permissions",
       params(
           ("principal_id" = Uuid, Path, description = "principal whose grant is revoked"),
           ("permission_name" = String, Query, description = "one of read|write|delete|share"),
       ),
       request_body = GrantDatasetPermissionBody,
       responses(
           (status = 200, description = "permission revoked", body = MessageResponse),
           (status = 400, description = "invalid permission name"),
           (status = 401, description = "unauthorized"),
       )
   )]
   #[tracing::instrument(
       skip(state, body),
       name = "cognee.api.permissions.revoke_dataset_permission",
       fields(principal_id = %principal_id)
   )]
   pub async fn revoke_dataset_permission(
       user: AuthenticatedUser,
       State(state): State<AppState>,
       Path(principal_id): Path<Uuid>,
       Query(query): Query<RevokeDatasetPermissionQuery>,
       Json(body): Json<GrantDatasetPermissionBody>,
   ) -> Result<Json<MessageResponse>, ApiError> {
       let GrantDatasetPermissionBody(dataset_ids) = body;
       let permission_name = query.permission_name.trim().to_lowercase();
       if !PERMISSION_NAMES.contains(&permission_name.as_str()) {
           return Err(ApiError::BadRequest(format!(
               "Unknown permission '{permission_name}'; must be one of read|write|delete|share"
           )));
       }
       if dataset_ids.is_empty() {
           return Ok(Json(MessageResponse {
               message: "Permission revoked from principal".into(),
           }));
       }
       let handles = components(&state)?;
       let repo = permissions_repo(handles)?;
       for ds_id in &dataset_ids {
           let allowed = repo.user_can(user.id, *ds_id, "share")
               .await.map_err(map_permissions_error)?;
           if !allowed {
               tracing::debug!("Caller {} lacks share on dataset {}; skipping revoke", user.id, ds_id);
               continue;
           }
           repo.revoke_acl(principal_id, *ds_id, &permission_name)
               .await.map_err(map_permissions_error)?;
       }
       Ok(Json(MessageResponse { message: "Permission revoked from principal".into() }))
   }
   ```

3. **Add `remove_user_from_role`** handler — clone `assign_role` (line 522), swap
   `assign_role` → `revoke_role`, change the message:
   ```rust
   #[utoipa::path(
       delete,
       path = "/api/v1/permissions/users/{user_id}/roles",
       tag = "permissions",
       params(
           ("user_id" = Uuid, Path, description = "target user"),
           ("role_id" = Uuid, Query, description = "role to remove"),
       ),
       responses(
           (status = 200, description = "user removed from role", body = MessageResponse),
           (status = 401, description = "unauthorized"),
           (status = 403, description = "not the tenant owner"),
           (status = 404, description = "role not found"),
       )
   )]
   #[tracing::instrument(
       skip(state),
       name = "cognee.api.permissions.remove_user_from_role",
       fields(target_user_id = %target_user)
   )]
   pub async fn remove_user_from_role(
       user: AuthenticatedUser,
       State(state): State<AppState>,
       Path(target_user): Path<Uuid>,
       Query(query): Query<RemoveUserFromRoleQuery>,
   ) -> Result<Json<MessageResponse>, ApiError> {
       let handles = components(&state)?;
       let repo = permissions_repo(handles)?;
       let role_tenant_id = repo.role_tenant_id(query.role_id)
           .await.map_err(map_permissions_error)?
           .ok_or_else(|| ApiError::NotFound(format!("Role '{}' not found", query.role_id)))?;
       require_tenant_owner(repo, user.id, role_tenant_id).await?;
       repo.revoke_role(target_user, query.role_id)
           .await.map_err(map_permissions_error)?;
       Ok(Json(MessageResponse { message: "User removed from role".into() }))
   }
   ```

4. **`DELETE /roles/{role_id}` (delete role)** — there is **no** `delete_role` repo
   method. Choose one:
   - **(Recommended for this P0 task) defer** the role-delete endpoint and scope the task
     to the two wiring-only endpoints. Update the doc (step 6) to state that role
     deletion is a tracked follow-up (it needs a new repo method, not just wiring) rather
     than the current false "to match Python" claim.
   - **(If implementing now)** add `delete_role(&self, role_id, owner_id)` to
     `PermissionsRepository` and `SeaOrmPermissionsRepository`: verify the caller is the
     role's tenant owner (`role_tenant_id` + `tenant_owner`), then in one transaction
     delete `user_roles` rows for the role, delete `acls` rows whose `principal_id ==
     role_id` (roles are principals), and delete the `role` row. Mirror the cascade in
     `remove_user_from_tenant`'s impl. Then add the handler (clone `create_role`'s
     owner-only gate) and route. Add a repo-level test in
     `crates/database/tests/` for the cascade.

5. **Register the routes** in `router()` (`permissions.rs:631`). Axum allows the same
   path with different methods via `.on`/method-router chaining — add `.delete(...)` to
   the existing routes that already have a POST:
   ```rust
   // POST + DELETE on the same path:
   .route("/datasets/{principal_id}",
       post(grant_dataset_permission).delete(revoke_dataset_permission))
   .route("/users/{user_id}/roles",
       post(assign_role).delete(remove_user_from_role))
   // (step 4, if implemented) DELETE /roles/{role_id}
   // .route("/roles/{role_id}", delete(delete_role_handler))
   ```
   > If the current builder registers POST routes with `post(handler)` on their own
   > `.route(...)` line, merge the DELETE into the same `.route` call (one path → one
   > MethodRouter) — do not add a second `.route` for the same path or axum will panic
   > at startup with an overlapping-route error.

6. **Fix the docs.** In `docs/http-server/routers/permissions.md`:
   - Replace the false line ~319 ("Bulk deletes are out of scope — the router does not
     expose tenant or role deletion. Match Python …") with an accurate statement: revoke
     ACL and remove-from-role are now exposed (Python parity); role *deletion*
     (`DELETE /roles/{role_id}`) is [implemented | a tracked follow-up needing a new
     repo method].
   - Add the new endpoints to the endpoint surface table (§2) and the auth table (§2.x)
     with their gates: revoke ACL → per-dataset `share`; remove-from-role → owner-only;
     delete-role → owner-only.

## Verification

```bash
# 1. Confirm the repo methods exist and signatures match what the handlers call.
grep -n "fn revoke_acl\|fn revoke_role" crates/database/src/permissions/mod.rs

# 2. Confirm Python routes (source of truth).
grep -n 'delete("/datasets\|delete("/users/{user_id}/roles\|delete("/roles/{role_id}' \
  /tmp/cognee-python/cognee/api/v1/permissions/routers/get_permissions_router.py

# 3. Build + run the new HTTP tests.
cargo test -p cognee-http-server --test test_permissions_revoke

# 4. No overlapping-route panic at router build (covered by any test that calls test_router()).
cargo test -p cognee-http-server permissions

# 5. Gate.
scripts/check_all.sh
```

### Tests to add — `crates/http-server/tests/test_permissions_revoke.rs`

Mirror `test_permissions_acl.rs` (`mod support`; helpers `build_permissions_state`,
`seed_perm_user`, `seed_dataset`, `bearer_header`, `test_router`, `oneshot_request`,
`body_json`, `permissions_repo`):

- `revoke_acl_round_trip`: grant `read` to a grantee, then
  `DELETE /datasets/{grantee}?permission_name=read` with the dataset in the body; assert
  200, body `{"message":"Permission revoked from principal"}`, and that
  `repo.user_can(grantee, ds, "read")` is now `false`.
- `revoke_acl_unknown_permission_400`: `permission_name=bogus` → 400.
- `revoke_acl_empty_body_200`: empty array → 200 (parity with grant §6.1).
- `revoke_acl_silently_skips_when_caller_lacks_share`: caller without `share` → 200 but
  ACL unchanged.
- `remove_user_from_role_round_trip`: owner creates a role, assigns a user, then
  `DELETE /users/{user}/roles?role_id=...` → 200, body `{"message":"User removed from role"}`,
  and `list_users_in_role` no longer contains the user.
- `remove_user_from_role_non_owner_403`: a non-owner caller → 403.
- `remove_user_from_role_unknown_role_404`: random `role_id` → 404.
- (step 4, if implemented) `delete_role_cascade`: create role + assign user + grant
  role-ACL, delete role, assert role gone, memberships gone, role-principal ACLs gone.

## Acceptance criteria

- [ ] `DELETE /api/v1/permissions/datasets/{principal_id}` revokes an ACL via `revoke_acl`,
      returns `{"message":"Permission revoked from principal"}`, mirrors grant's
      validation/silent-skip/empty-body behavior.
- [ ] `DELETE /api/v1/permissions/users/{user_id}/roles` removes a user from a role via
      `revoke_role`, owner-only, returns `{"message":"User removed from role"}`.
- [ ] `DELETE /api/v1/permissions/roles/{role_id}` either implemented (with a new
      `delete_role` repo method + cascade) **or** explicitly documented as a tracked
      follow-up (not silently dropped).
- [ ] `docs/http-server/routers/permissions.md` no longer claims revoke is omitted "to
      match Python"; the new endpoints + auth gates are documented.
- [ ] New HTTP tests pass; `scripts/check_all.sh` passes; the router builds without an
      overlapping-route panic.

## Gotchas / do-not

- **Route registration:** axum requires one `MethodRouter` per path. Add `.delete(...)`
  to the *existing* `.route("/datasets/{principal_id}", post(...))` and
  `.route("/users/{user_id}/roles", post(...))` lines — a duplicate `.route` for the same
  path panics on startup. Verify with a test that calls `test_router()`.
- **`revoke_acl` is idempotent** (it's a `delete_many` with filters — see
  `sea_orm_impl.rs:367`). Revoking a non-existent grant is a no-op success; do not add a
  spurious 404. Match Python (no existence check before delete).
- **Auth parity:** keep the per-dataset `share` silent-skip on revoke-ACL (same as
  grant) — do **not** hard-403 the whole request if one dataset is unauthorized.
- **OpenAPI:** if the crate aggregates `#[utoipa::path]` into an `ApiDoc`, register the
  new handlers there too, or the OpenAPI parity test (`test_http_openapi`) may flag a
  missing path. Check how the existing handlers are collected.
- **No cross-SDK on-disk impact:** these are HTTP-surface + ACL-row deletes; they do not
  touch DB schema, IDs, hashes, or collection names. Safe.
- **Telemetry:** Python emits a `send_telemetry("Permissions API Endpoint Invoked", ...)`
  per call; the Rust handlers already carry `#[tracing::instrument]` spans named
  `cognee.api.permissions.<verb>` — keep that convention for the new handlers.

## Rollback

```bash
git checkout main -- \
  crates/http-server/src/routers/permissions.rs \
  crates/http-server/src/dto/permissions.rs \
  docs/http-server/routers/permissions.md
git rm crates/http-server/tests/test_permissions_revoke.rs   # if created
```
If step 4 added a repo method, also revert
`crates/database/src/permissions/{mod.rs,sea_orm_impl.rs}`.
