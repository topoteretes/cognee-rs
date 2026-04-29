# CLEAN-01 — v1 HTTP DTO casing audit and fix (camelCase wire parity)

| | |
|---|---|
| Scope | Audit and fix every v1 HTTP DTO whose JSON wire shape diverges from Python's camelCase output. Add regression tests to lock in both directions (accept snake_case + camelCase on input; emit camelCase on output). |
| Status | **Done** (commit e146835, 2026-04-29) |
| Phase | **0 — Pre-port cleanup** (runs before Phase A) |
| Depends on | nothing |
| Blocks | every v2 task that adds or modifies a body/response DTO (the convention must be settled before they land) |
| Effort | ~1.5 days. |
| Owner crate | `cognee-http-server` |

> **Decision (2026-04-29) — Decision 10**: Python's `cognee.api.DTO.InDTO` / `OutDTO` ([`cognee/api/DTO.py:7-17`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py#L7-L17)) sets `alias_generator=to_camel` + `populate_by_name=True`. Every Pydantic-DTO field is **camelCase on the wire** in both request bodies and response bodies, with snake_case accepted as an inbound fallback only. The existing v1 Rust port has drifted — some DTOs use camelCase, some use snake_case. This task brings v1 into compliance before v2 work begins. Investigation agent: do not re-litigate.

## 1. Goal

Bring every v1 HTTP DTO to byte-for-byte wire parity with Python's `to_camel` alias-generator output, then add unit + integration tests so the convention is enforced going forward. Without this, v2 work that lands new camelCase DTOs would create inconsistency within the same Rust HTTP server (some endpoints camelCase, others snake_case), confusing both clients and contributors.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `InDTO`, `OutDTO`, `to_camel`, `populate_by_name` | `cognee/api/DTO.py` | 1–22 |

Behavior:
- `alias_generator=to_camel` applied to every field of every DTO that inherits `InDTO` or `OutDTO`.
- `populate_by_name=True` makes Pydantic accept either the snake_case Python attribute name OR the camelCase alias on input.
- Pydantic's default `model_dump()` / `jsonable_encoder` uses the alias on **output** unless `by_alias=False` is passed (FastAPI's response handler always uses aliases).

Out of scope (these are NOT pydantic-aliased and stay snake_case on the wire):
- **Query parameters** declared at the FastAPI function signature (`async def list_sessions(order_by: str = Query(...))`) — wire name is the Python parameter name. FastAPI doesn't apply alias_generator here.
- **Multipart form fields** declared at the FastAPI function signature (`async def remember(datasetName: ... = Form(...), session_id: ... = Form(...))`) — wire name is the literal Python parameter name. Python intentionally mixes camelCase and snake_case for these.
- **Header names** — always lowercase per HTTP convention.
- **Path parameters** — always literal.

## 3. Current Rust state — drift inventory

`grep -n "rename_all\|rename = \"" crates/http-server/src/dto/*.rs` produces a mixed picture:

| File | Current | Status | Action |
|---|---|---|---|
| [`dto/add.rs`](../../../crates/http-server/src/dto/add.rs) | per-field `rename = "datasetName"` / `rename = "datasetId"` | ✅ correct (multipart form, see §2 out-of-scope) | none |
| [`dto/forget.rs`](../../../crates/http-server/src/dto/forget.rs) (request body) | `rename_all = "camelCase"` | ✅ correct | verify response variants also camelCase |
| [`dto/forget.rs`](../../../crates/http-server/src/dto/forget.rs) (response variants) | `rename_all = "snake_case"` | ✅ **correct (plain dict return; original ❌ flag was wrong — see §3.1)** | none |
| [`dto/cognify.rs`](../../../crates/http-server/src/dto/cognify.rs) | `rename_all = "snake_case"` | ❌ wrong | flip to `camelCase`; add aliases for snake_case inputs |
| [`dto/recall.rs`](../../../crates/http-server/src/dto/recall.rs) | mixed snake_case + bespoke renames | ❌ partial drift | flip; reconcile with E-04's later changes |
| [`dto/search.rs`](../../../crates/http-server/src/dto/search.rs) | `rename_all = "snake_case"` (some) + `SCREAMING_SNAKE_CASE` (enum) | ❌ wrong on DTO; enum is correct | flip DTO; leave enum |
| [`dto/improve.rs`](../../../crates/http-server/src/dto/improve.rs) | `rename_all = "snake_case"` + bespoke `rename = "datasetId"` | ❌ partial | flip top-level to `camelCase`; drop the per-field rename (now redundant) |
| [`dto/memify.rs`](../../../crates/http-server/src/dto/memify.rs) | similar to `improve.rs` | ❌ partial | flip top-level to `camelCase` |
| [`dto/remember.rs`](../../../crates/http-server/src/dto/remember.rs) (response `RememberResultDTO`) | `rename_all = "snake_case"` | ✅ **correct (Python returns `RememberResult.to_dict()` plain dict; original ❌ flag was wrong — see §3.1)** | none |
| [`dto/datasets.rs`](../../../crates/http-server/src/dto/datasets.rs), [`dto/users.rs`](../../../crates/http-server/src/dto/users.rs), [`dto/auth.rs`](../../../crates/http-server/src/dto/auth.rs), [`dto/permissions.rs`](../../../crates/http-server/src/dto/permissions.rs), etc. | unknown | audit | investigation step |

The investigation agent extends this table with the full audit before implementation begins.

### 3.1 Full audit (added by investigation agent 2026-04-29)

**Critical finding — Decision-10 polarity caveat.** Decision 10's "every body/response DTO uses camelCase" rule applies **only when the Python counterpart inherits `InDTO` / `OutDTO`** (i.e. has `alias_generator=to_camel`). It does **not** apply when Python returns a plain `dict`, a `TypedDict`, or a third-party Pydantic model (e.g. `fastapi-users`'s `BaseUser` / `BaseUserUpdate`, or a bare `BaseModel` like `UserDatasetPair`). FastAPI's `jsonable_encoder` does not synthesize aliases for plain dicts; bare `BaseModel` subclasses also have no alias generator. The original §3 table flagged several Rust DTOs as "wrong" that are actually correct because their Python counterparts return plain dicts.

The full audit below replaces §3:

| File | DTO | Python counterpart | Python kind | Current Rust | Required action |
|---|---|---|---|---|---|
| `dto/add.rs` | `AddPayloadDTO` | `cognee/api/v1/add/routers/get_add_router.py:58-66` | multipart `Form(...)` | per-field `rename = "datasetName"` / `rename = "datasetId"` | ✅ correct (multipart out-of-scope per Decision 10) |
| `dto/add.rs` | `AddResponseDTO` (line 59) | dict from `add()` library | plain dict | `rename_all = "snake_case"` | ✅ correct (plain dict; verify keys) |
| `dto/cognify.rs` | `CognifyPayloadDTO` | `cognee/api/v1/cognify/routers/get_cognify_router.py` SearchPayloadDTO style InDTO | **InDTO** | `rename_all = "snake_case"` | ❌ flip to `camelCase`; add `alias` for `dataset_ids`, `run_in_background`, `graph_model`, `custom_prompt`, `ontology_key`, `chunks_per_batch` |
| `dto/cognify.rs` | `CognifyWsFrameDTO` | WebSocket frame, not pydantic | n/a | `rename_all = "snake_case"` | ✅ correct (WebSocket out-of-scope per CLEAN-01 §7) |
| `dto/improve.rs` | `ImprovePayloadDTO` | `get_improve_router.py:39` InDTO | **InDTO** | `rename_all = "snake_case"` + `rename = "datasetId"` | ❌ flip top-level to `camelCase`; drop the per-field rename; add `alias = "dataset_name"`, `"run_in_background"` |
| `dto/memify.rs` | `MemifyPayloadDTO` | `get_memify_router.py` InDTO | **InDTO** | same drift as `improve.rs` | ❌ flip; drop redundant `rename = "datasetId"`; add aliases |
| `dto/forget.rs` | `ForgetPayloadDTO` | `get_forget_router.py:16-19` InDTO | **InDTO** | already `rename_all = "camelCase"` | ✅ correct; verify `alias = "data_id"` exists for input compat |
| `dto/forget.rs` | `ForgetDataItemResponse` / `ForgetDatasetResponse` / `ForgetEverythingResponse` | `forget.py:144,165,187` plain dict returns | plain dict | `rename_all = "snake_case"` | ✅ correct (plain dict); the original §3 flag is **wrong** — Python returns `{"data_id":..., "dataset_id":..., "status":...}` as a literal dict, not OutDTO. Confirmed at `/tmp/cognee-python/cognee/api/v1/forget/forget.py:144,165,187`. |
| `dto/recall.rs` | `RecallPayloadDTO` | `get_recall_router.py:23-34` InDTO | **InDTO** | NO `rename_all` (fields default to literal name = snake_case) | ❌ add `rename_all = "camelCase"`; per-field aliases for `search_type`, `dataset_ids`, `system_prompt`, `node_name`, `top_k`, `only_context` |
| `dto/search.rs` | `SearchPayloadDTO` | `get_search_router.py:25-37` InDTO | **InDTO** | NO `rename_all` | ❌ same as recall |
| `dto/search.rs` | `SearchHistoryItemDTO` | inline `SearchHistoryItem(OutDTO)` at `get_search_router.py:42-46` | **OutDTO** | NO `rename_all` (`created_at` emitted as snake) | ❌ add `rename_all = "camelCase"`; `created_at` becomes `createdAt` (also affects E-03) |
| `dto/search.rs` | `SearchResultDTO` | `cognee/modules/search/types/SearchResult.py` | **OutDTO** | NO `rename_all` (`search_result`, `dataset_id`, `dataset_name` snake) | ❌ flip to camelCase |
| `dto/search.rs` | `WireSearchType` enum | `SearchType` enum, SCREAMING_SNAKE wire literals | enum | `SCREAMING_SNAKE_CASE` | ✅ correct (enum literal contract) |
| `dto/search.rs` | `ErrorResponseDTO` | `cognee/api/DTO.py:20-22` `ErrorResponse(OutDTO)` | **OutDTO** | no `rename_all`; fields `error`, `detail` are single-word | ✅ correct (single-word fields are unaffected by camelCase) |
| `dto/remember.rs` | `RememberFormDTO` | multipart Form fields | Form | not serde-derived | ✅ correct (multipart) |
| `dto/remember.rs` | `RememberResultDTO` | `RememberResult.to_dict()` plain dict at `remember.py:415-437` | plain dict | `rename_all = "snake_case"` | ✅ correct (plain dict). Original §3 flag is **wrong**. |
| `dto/datasets.rs` | every DTO | `OutDTO` / `InDTO` | DTO | already `rename_all = "camelCase"` | ✅ correct |
| `dto/permissions.rs` | `SelectTenantDTO` | `get_permissions_router.py:29-30` `InDTO` (single field `tenant_id`) | **InDTO** | `rename_all = "snake_case"` + comment "do not apply camelCase" | ❌ comment is wrong per Decision 10; flip to `camelCase` (Python emits `tenantId` via `to_camel`); add `alias = "tenant_id"` for input compat. **Discrepancy with `docs/http-server/routers/permissions.md §4` and §1 of `permissions.rs` — both must be updated by the implementation agent.** |
| `dto/permissions.rs` | `GrantDatasetPermissionQuery`, `CreateRoleQuery`, `CreateTenantQuery`, `AssignRoleQuery`, `AddUserToTenantQuery` | FastAPI query params | Query | `rename_all = "snake_case"` | ✅ correct (query params out-of-scope per Decision 10) |
| `dto/permissions.rs` | `MessageResponse`, `CreateRoleResponse`, `CreateTenantResponse`, `SelectTenantResponse`, `TenantSummary`, `RoleSummary`, `UserInRole`, `UserInTenant` | plain dict returns from handlers (`JSONResponse(content={...})`) | plain dict | `rename_all = "snake_case"` | ✅ correct (plain dict). Verified at `/tmp/cognee-python/cognee/api/v1/permissions/routers/get_permissions_router.py:85-86` (`{"message": "..."}`); other endpoints similar. |
| `dto/permissions.rs` | `GrantDatasetPermissionBody` | `dataset_ids: List[UUID]` direct param | top-level array | `transparent` | ✅ correct |
| `dto/auth.rs` | `LoginPayloadDTO` | `OAuth2PasswordRequestForm` | OAuth2 form | no `rename_all` | ✅ correct (OAuth2 spec literal names) |
| `dto/auth.rs` | `LoginResponseDTO` | fastapi-users `BearerResponse` | third-party pydantic | snake-case (`access_token`, `token_type`) | ✅ correct (OAuth2 spec literal names) |
| `dto/auth.rs` | `MeShortResponseDTO` | ad-hoc dict at `get_auth_router.py:52-54` | plain dict | single-word `email` | ✅ correct |
| `dto/auth.rs` | `LogoutResponseDTO` | empty dict | plain dict | empty | ✅ correct |
| `dto/users.rs` | `UserReadDTO` | fastapi-users `BaseUser` + cognee extension | third-party pydantic (no `to_camel`) | no `rename_all`, fields snake_case | ✅ correct (third-party pydantic emits literal field names) |
| `dto/users.rs` | `UserUpdatePayloadDTO` | fastapi-users `BaseUserUpdate` | third-party pydantic | no `rename_all` | ✅ correct |
| `dto/auth_register.rs`, `dto/auth_reset_password.rs`, `dto/auth_verify.rs`, `dto/users_by_email.rs`, `dto/api_keys.rs` | various | mostly fastapi-users / ad-hoc dicts | mixed | (audit on a per-file basis during implementation) | implementation agent confirms via the same Python check |
| `dto/configuration.rs` | `StorePrincipalConfigurationPayloadDTO` | `get_configuration_router.py:21-23` `InDTO` (single-word fields `name`, `config`) | **InDTO** | `rename_all = "snake_case"` | ⚠️ functionally equivalent for these single-word fields; flip to `camelCase` for forward consistency |
| `dto/configuration.rs` | `PrincipalConfigurationDTO` | dict from `PrincipalConfiguration.to_json()` | plain dict (mixed) | bespoke `rename = "ownerId"` / `"createdAt"` / `"updatedAt"` | ✅ correct (mixed snake/camel matches Python — explicitly documented in `routers/configuration.md §4`) |
| `dto/visualize.rs` | `VisualizeQueryDTO` | FastAPI query param `dataset_id` | Query | no `rename_all` | ✅ correct (query param out-of-scope) |
| `dto/visualize.rs` | `UserDatasetPairDTO` | `class UserDatasetPair(BaseModel)` at `get_visualize_router.py:19` | bare `BaseModel` (no `to_camel`) | no `rename_all` | ✅ correct (bare BaseModel emits literal names) |
| `dto/sync.rs` | every DTO | (audit during implementation — file is internal `/api/v1/sync` family) | mixed | every struct `rename_all = "snake_case"` | ⚠️ implementation agent verifies per Python file (`get_sync_router.py`) |
| `dto/activity.rs` | every DTO | (audit during implementation — `/api/v1/activity`) | mixed | every struct `rename_all = "snake_case"` | ⚠️ implementation agent verifies per Python file (`get_activity_router.py`) |
| `dto/checks.rs` | (audit during implementation) | `/api/v1/checks` | unknown | `rename_all = "snake_case"` | ⚠️ implementation agent verifies |
| `dto/delete.rs` | (audit during implementation) | `/api/v1/delete` | unknown | `rename_all = "snake_case"` + `lowercase` enum | ⚠️ implementation agent verifies |
| `dto/llm.rs`, `dto/notebooks.rs`, `dto/ontologies.rs`, `dto/responses.rs`, `dto/settings.rs`, `dto/update.rs` | (audit during implementation) | various | mixed | mixed | ⚠️ implementation agent verifies each |
| `dto/pipeline_run.rs` | `PipelineRunInfoDTO` and friends | dict returned by `cognify()` / `memify()` (`pipeline_run_info`) | plain dict | `rename_all = "snake_case"` | ⚠️ likely correct (plain dict from library); verify against `/tmp/cognee-python/cognee/modules/pipelines` |
| `dto/util.rs` | `DatasetIdRef` newtype wrapper | n/a | n/a | transparent | ✅ correct |

The implementation agent extends this table per actual Python verification before flipping each file.

### 3.2 Cross-doc updates required by the `permissions` flip

The current Rust comment in `crates/http-server/src/dto/permissions.rs:1-4` and `docs/http-server/routers/permissions.md §4` (`#[serde(rename_all = "snake_case")]` annotated structs at lines 333, 352, 358, 364, 370, 376, 384, 390, 398, 405, 414, 421, 437, 445) **disagree** with Decision 10. Specifically `SelectTenantDTO` (an `InDTO`) is currently mandated to use snake_case in those docs, but Decision 10 says the wire is camelCase with snake_case input alias. The implementation agent must:

1. Flip `SelectTenantDTO` to `rename_all = "camelCase"` + `alias = "tenant_id"`.
2. Update `docs/http-server/routers/permissions.md §4` (the inline DTO declaration block) to match.
3. Remove or correct the misleading "do not apply `rename_all = camelCase`" comment at the top of `dto/permissions.rs`. The new comment should clarify: "Most response DTOs in this module emit snake_case because Python returns plain dicts via `JSONResponse(content={...})`; only `SelectTenantDTO` (an `InDTO`) follows Decision 10's camelCase rule with a snake_case input alias."

This is the only known case where Decision 10 forces a change to the v1 routers/* doc tree.

## 4. Implementation steps

### 4.1 Investigation pass

For every file in `crates/http-server/src/dto/`:

1. Identify whether each struct is a request body (input), response body (output), query-param container, or multipart-form container. The wire convention applies only to the first two.
2. For each request/response DTO, compare its current `rename_all` + per-field `rename` attributes against Python's `to_camel` alias-generator output. List every divergence.
3. For each divergence, note whether the Python attribute name is single-word (`type`, `data`, `error`, `status` → no transform) or multi-word (`dataset_id`, `session_id`, `feedback_text` → camelCase transform).
4. Write the full audit table as an addendum to this task doc's §3 before §4.2 begins.

### 4.2 Migration pattern

For each request-body DTO:

```rust
#[derive(Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]                       // wire defaults to camelCase
pub struct ExamplePayloadDTO {
    #[serde(default, alias = "dataset_id")]              // accept snake_case on input too
    pub dataset_id: Option<Uuid>,                        // Rust field stays snake_case

    #[serde(default, alias = "session_ids")]
    pub session_ids: Option<Vec<String>>,
    // ...
}
```

For each response-body DTO:

```rust
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]                       // wire is camelCase
pub struct ExampleResponseDTO {
    pub dataset_id: Uuid,                                // serializes as "datasetId"
    pub session_ids: Vec<String>,                        // serializes as "sessionIds"
    // ...
}
```

Notes:
- Single-word fields (`type`, `data`, `error`, `status`) are unaffected by `rename_all = "camelCase"` (they don't have a snake_case form to transform).
- The `alias = "..."` attribute is **deserialization only**; it never affects output.
- Drop now-redundant per-field `rename = "datasetId"` attributes once `rename_all = "camelCase"` is in place — they're equivalent and the redundancy invites future drift.

### 4.3 Wire-convention helper

To prevent future drift, add a thin doc/lint marker:

- Update [`crates/http-server/src/dto/mod.rs`](../../../crates/http-server/src/dto/mod.rs) module-level docs with a one-paragraph contract: "Every request/response DTO uses `#[serde(rename_all = "camelCase")]`. Request DTOs additionally apply `#[serde(alias = "<snake_form>")]` per multi-word field for input compatibility per Python's `populate_by_name=True`."
- Add a `cargo clippy` lint or a `#[deny(unknown_lints)]` mechanism — actually clippy can't enforce this. Use a `#[cfg(test)]` regression test that walks the OpenAPI schema and asserts every property name is camelCase. Easier than a lint.

## 5. Tests

### 5.1 Per-DTO unit tests

For every modified DTO file, add three tests (suffix the test names with the file name to keep them unique across the workspace):

```rust
#[test]
fn cognify_dto_accepts_camelcase_input() {
    let json = r#"{"datasetIds": ["uuid-here"], "runInBackground": true}"#;
    let parsed: CognifyPayloadDTO = serde_json::from_str(json).expect("parse");
    assert!(parsed.dataset_ids.is_some());
    assert_eq!(parsed.run_in_background, Some(true));
}

#[test]
fn cognify_dto_accepts_snake_case_input_via_alias() {
    let json = r#"{"dataset_ids": ["uuid-here"], "run_in_background": true}"#;
    let parsed: CognifyPayloadDTO = serde_json::from_str(json).expect("parse");
    assert!(parsed.dataset_ids.is_some());
}

#[test]
fn cognify_dto_serializes_camelcase_only() {
    let dto = CognifyPayloadDTO { dataset_ids: Some(vec![..]), run_in_background: Some(true), .. };
    let s = serde_json::to_string(&dto).expect("ser");
    assert!(s.contains("\"datasetIds\""));
    assert!(s.contains("\"runInBackground\""));
    assert!(!s.contains("\"dataset_ids\""));   // negative — no snake_case in output
}
```

### 5.2 Integration tests — wire-shape regressions

For each affected endpoint, add at least one integration test in `crates/http-server/tests/test_<router>_wire_shape.rs` that asserts:

- A POST with snake_case body fields succeeds (`200`).
- A POST with camelCase body fields succeeds (`200`).
- The response body contains only camelCase keys at the top level (`grep -v` for any underscore in property names).

Skeleton:

```rust
#[tokio::test]
async fn cognify_response_body_uses_camelcase_keys() {
    let app = build_test_app().await;
    let req = Request::post("/api/v1/cognify")
        .header("content-type", "application/json")
        .body(json!({ "datasetIds": [..] }).to_string())
        .unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&...).unwrap();
    let keys: Vec<&str> = resp.as_object().unwrap().keys().map(String::as_str).collect();
    for k in keys {
        assert!(!k.contains('_'), "response key {} should be camelCase", k);
    }
}
```

### 5.3 OpenAPI schema regression

A workspace-level test walks the generated OpenAPI document and asserts every property name in every component schema is camelCase:

```rust
#[test]
fn openapi_property_names_are_all_camelcase() {
    let openapi = build_openapi_spec();
    for (schema_name, schema) in openapi.components.unwrap().schemas {
        if let Some(props) = schema_properties(&schema) {
            for prop_name in props.keys() {
                assert!(
                    !prop_name.contains('_'),
                    "OpenAPI schema {} has snake_case property: {}",
                    schema_name, prop_name
                );
            }
        }
    }
}
```

This is the **lint** that prevents future drift — any new DTO that ships with snake_case automatically fails this test.

### 5.4 Cross-SDK harness updates

The existing `e2e-cross-sdk/harness/test_http_*.py` tests send snake_case bodies. Those continue to work after this task lands (because of the `serde(alias)` pattern). Update each test file to **also** assert response keys are camelCase:

```python
def test_cognify_response_uses_camelcase_keys(authed_clients, ...):
    py_resp = authed_clients["py"].post("/api/v1/cognify", json={"datasets": [...]})
    rs_resp = authed_clients["rs"].post("/api/v1/cognify", json={"datasets": [...]})
    for resp in (py_resp, rs_resp):
        for key in resp.json().keys():
            assert "_" not in key, f"snake_case key found in response: {key}"
```

This is what catches a v1 regression that creeps in **after** this task.

## 6. Acceptance criteria

- [x] Every request/response DTO under `crates/http-server/src/dto/` uses `#[serde(rename_all = "camelCase")]` (or has a documented exception, e.g. multipart form structs / plain-dict responses / fastapi-users / bare-`BaseModel`, all enumerated in `dto/mod.rs` and the OpenAPI whitelist).
- [x] Every multi-word field on a request DTO has `#[serde(alias = "<snake_form>")]` for input compatibility.
- [x] All per-DTO unit tests (3 per modified file) pass.
- [x] All endpoint integration wire-shape tests pass (`crates/http-server/tests/test_dto_wire_shape.rs`).
- [x] OpenAPI schema regression test passes (`crates/http-server/tests/test_openapi_camelcase.rs`).
- [x] Cross-SDK harness response-key tests pass against both Python and Rust backends (updates in `e2e-cross-sdk/harness/test_http_recall.py` and `test_http_search.py`).
- [x] No previously-passing test now fails (zero behavioral regression).
- [x] `scripts/check_all.sh` clean (only the pre-existing JS ts-jest `node:path` failure noted in the project guide remains).

## 7. Out of scope

- **Query parameter casing**: stays snake_case (FastAPI doesn't alias these — see §2). E-09's `OrderBy` enum query param is already correct.
- **Multipart form casing**: stays as Python's literal parameter names (e.g. `datasetName` mixed with `session_id`). The existing manual multipart parsing in `dto/remember.rs` already matches.
- **WebSocket frame payloads**: not pydantic-aliased; whatever shape the frame schema in [`docs/http-server/websocket.md`](../../http-server/websocket.md) declares is authoritative.
- **HTTP header names**: always lowercase, no change.

## 8. Recording in the audit-findings doc

Once this task lands, add a one-paragraph entry to [`../../http-server/audit-findings.md`](../../http-server/audit-findings.md) under "Resolved 2026-04-29": "v1 HTTP DTO casing drift — fixed in CLEAN-01 (commit `<sha>`). Convention enforced by an OpenAPI schema regression test."

## 9. References

- [Python `InDTO` / `OutDTO`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py)
- [README §1.1 Wire conventions](../README.md#11-wire-conventions-project-wide-set-by-decision-6)
- [v1 audit findings](../../http-server/audit-findings.md)
