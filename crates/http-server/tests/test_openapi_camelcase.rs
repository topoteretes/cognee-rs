#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! CLEAN-01 §5.3 OpenAPI schema regression test.
//!
//! Walks every component schema in the generated OpenAPI document and asserts
//! that every property name is camelCase (no underscores). The whitelist below
//! enumerates every schema whose Python counterpart legitimately emits
//! snake_case on the wire — see the §3.1 audit table in
//! `docs/http-api-v2/tasks/clean-01-v1-dto-camelcase.md`.

mod support;

/// Schemas whose Python counterpart returns snake_case literal field names on
/// the wire — usually because the Python handler returns a plain `dict` (via
/// `JSONResponse(content={...})` or `return {...}`) rather than an `OutDTO`,
/// or because the Python class is a bare `BaseModel` / third-party Pydantic
/// model with no `alias_generator`.
///
/// Every entry includes a one-line justification.
const SNAKE_CASE_WHITELIST: &[(&str, &str)] = &[
    // ── Notebooks (SQLAlchemy model + bare BaseModel cells) ───────────────────
    (
        "NotebookDTO",
        "Python: SQLAlchemy `Notebook` ORM model — column names emitted literally.",
    ),
    (
        "NotebookCellDTO",
        "Python: bare `BaseModel` `NotebookCell` (no alias_generator).",
    ),
    (
        "NotebookDataDTO",
        "Python: `NotebookData(InDTO)` but every field is single-word.",
    ),
    (
        "RunCodeDataDTO",
        "Python: `RunCodeData(InDTO)` — single-word field `content`.",
    ),
    (
        "RunCodeOutcomeDTO",
        "Plain dict response: handler returns `JSONResponse(content={'result': ..., 'error': ...})`.",
    ),
    // ── Permissions (mostly plain-dict responses) ────────────────────────────
    (
        "MessageResponse",
        "Plain dict: handlers return `JSONResponse(content={'message': ...})`.",
    ),
    (
        "CreateRoleResponse",
        "Plain dict: handler returns `{'message': ..., 'role_id': ..., 'tenant_id': ...}`.",
    ),
    (
        "CreateTenantResponse",
        "Plain dict: handler returns `{'message': ..., 'tenant_id': ...}`.",
    ),
    (
        "SelectTenantResponse",
        "Plain dict: handler returns `{'message': ..., 'tenant_id': ...}`.",
    ),
    (
        "TenantSummary",
        "Plain dict: handler builds a list of `{'id': ..., 'name': ...}` rows.",
    ),
    (
        "RoleSummary",
        "Plain dict: built from raw rows including `description` and `user_count`.",
    ),
    (
        "UserInRole",
        "Plain dict: handler emits `{'id': ..., 'name': ...}`.",
    ),
    (
        "UserInTenant",
        "Plain dict: handler emits `{'id': ..., 'email': ..., 'roles': [...]}`.",
    ),
    // ── Visualize ─────────────────────────────────────────────────────────────
    (
        "UserDatasetPairDTO",
        "Python: `class UserDatasetPair(BaseModel)` — bare BaseModel emits literal field names.",
    ),
    // ── Configuration (mixed snake/camel — Python's `to_json()` is literal) ──
    (
        "PrincipalConfigurationDTO",
        "Plain-ish dict from `PrincipalConfiguration.to_json()` — keys are mixed (id/name snake; ownerId/createdAt/updatedAt camel) per Python parity.",
    ),
    // ── Responses helpers (bare BaseModel, no alias_generator) ───────────────
    (
        "ToolFunctionDTO",
        "Python: bare `BaseModel` `ToolFunction` — fields emitted literally.",
    ),
    (
        "FunctionDTO",
        "Python: bare `BaseModel` `Function` — fields emitted literally.",
    ),
    (
        "FunctionParametersDTO",
        "Python: bare `BaseModel` `FunctionParameters` — fields emitted literally.",
    ),
    (
        "ResponseToolCallDTO",
        "Python: bare `BaseModel` `ResponseToolCall` — fields emitted literally.",
    ),
    (
        "FunctionCallDTO",
        "Python: bare `BaseModel` `FunctionCall` — fields emitted literally.",
    ),
    (
        "ToolCallOutputDTO",
        "Python: bare `BaseModel` `ToolCallOutput` — fields emitted literally.",
    ),
    (
        "ChatUsageDTO",
        "Python: bare `BaseModel` `ChatUsage` — `prompt_tokens`/`completion_tokens`/`total_tokens` literal.",
    ),
    // ── Sessions (E-09 — Python returns plain dict, not OutDTO) ──────────────
    (
        "SessionListResponseDTO",
        "Plain dict: Python's `list_sessions` returns `JSONResponse(content={...})` (`get_sessions_router.py:99-107`) so wire keys are snake_case (E-09 carve-out).",
    ),
    (
        "SessionRowDTO",
        "Plain dict: Python's `SessionRecord.to_dict()` (`models.py:68-86`) returns a snake_case dict — E-09 carve-out.",
    ),
    (
        "SessionStatsDTO",
        "Plain dict: Python's `get_stats` returns `jsonable_encoder({...})` (`get_sessions_router.py:179-196`) — E-10 carve-out, same as the list response.",
    ),
    (
        "CostByModelDTO",
        "Plain list-of-dicts: Python's `cost_by_model` returns `jsonable_encoder([...])` (`get_sessions_router.py:241-251`) — E-11 carve-out, same as the list and stats endpoints.",
    ),
    (
        "SessionDetailDTO",
        "Plain dict: Python's `get_session_detail` returns `jsonable_encoder(record)` (`get_sessions_router.py:289-307`) — E-12 carve-out, same as the list/stats/cost-by-model endpoints.",
    ),
    // ── Remember (plain-dict response, Python `RememberResult.to_dict()`) ────
    (
        "RememberResultDTO",
        "Plain dict: Python's `RememberResult.to_dict()` returns a plain dict (not a pydantic BaseModel) so wire keys are snake_case (CLEAN-01 §3.1 carve-out).",
    ),
    (
        "RememberItemDTO",
        "Plain dict: per-item info embedded in `RememberResult.to_dict()` — same snake_case carve-out.",
    ),
];

fn is_camel_case(name: &str) -> bool {
    !name.contains('_')
}

#[tokio::test]
async fn openapi_property_names_are_all_camelcase() {
    let state = support::build_test_state().await;
    let app = support::test_router(state).await;
    let resp = support::oneshot_get(app, "/openapi.json").await;
    assert_eq!(resp.status(), 200, "openapi.json must return 200");

    let body = support::body_json(resp).await;
    let schemas = body["components"]["schemas"]
        .as_object()
        .expect("openapi document must have components.schemas object");

    let whitelist: std::collections::HashMap<&str, &str> =
        SNAKE_CASE_WHITELIST.iter().copied().collect();

    let mut violations: Vec<String> = Vec::new();

    for (schema_name, schema_value) in schemas {
        if whitelist.contains_key(schema_name.as_str()) {
            continue;
        }

        // Walk only the top-level `properties` object — additional indirection
        // (oneOf/allOf branches) is recursively reachable through their child
        // schemas which are themselves entries in `components.schemas` and get
        // checked on their own iteration.
        let Some(props) = schema_value.get("properties").and_then(|v| v.as_object()) else {
            continue;
        };

        for prop_name in props.keys() {
            if !is_camel_case(prop_name) {
                violations.push(format!("{schema_name}.{prop_name}"));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "OpenAPI components.schemas contain snake_case property names (Decision 10 violation): {violations:#?}.\n\
         If a schema legitimately needs snake_case keys, add it to SNAKE_CASE_WHITELIST in tests/test_openapi_camelcase.rs with a one-line justification."
    );
}
