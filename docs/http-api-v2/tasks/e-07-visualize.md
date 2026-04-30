# E-07 — `GET /api/v1/visualize`

| | |
|---|---|
| Wire path | `GET /api/v1/visualize?dataset_id=<uuid>` |
| Status | **Done (commit 35d6b3c)** |
| Depends on | none |
| Effort | ~0.25 day. |
| Owner crate | `cognee-http-server` |

> **Investigation 2026-04-29 (verify-only short-circuit) — final/closed (landed 2026-04-29 in commit 35d6b3c)**: handler at `crates/http-server/src/routers/visualize.rs:59-101` matches Python `get_visualize_router.py:27-74` byte-for-byte: 409 catch-all (`AclDb::has_permission` / `IngestDb::get_dataset` / `cognee_visualization::render` errors all collapse), `{error}` envelope via `ApiError::VisualizeError`, no 403/404. The `cognee-visualization` crate (`crates/visualization/src/html.rs:32-51`) ports `_build_html()` byte-for-byte — same seven `__*_DATA__` placeholders (`__NODES_DATA__`, `__LINKS_DATA__`, `__TASK_COLORS__`, `__PIPELINE_COLORS__`, `__NODESET_COLORS__`, `__USER_COLORS__`, `__SCHEMA_DATA__`), same `</` → `<\/` escape (`safe_json_embed` at `html.rs:23-26`), same `null`-when-`None` for `__SCHEMA_DATA__`. The Rust template's JS variable names match Python: `var nodes`, `var links`, `var taskColors`, `var pipelineColors`, `var nodesetColors`, `var userColors`, `const schemaData` (`crates/visualization/assets/graph_template.html:404-409,227`). Handler unit tests already cover 200 / 409 / 422 (`crates/http-server/tests/test_visualize_single.rs:29,51,106,155`). **No code change needed in `crates/`**. The cross-SDK harness `e2e-cross-sdk/harness/test_http_visualize.py` was **stale** — it greped for `<!--JSON_ISLAND_START/END-->` markers that exist neither in Python nor Rust templates, so both tests effectively `pytest.skip`-ed. Resolution: rewrote the harness in commit 35d6b3c to apply Decision 11's seven-`__*_DATA__` extraction strategy (regex out each substituted JSON literal, reverse the `</` escape, structural diff with stable sort, plus a negative test).

## 1. Goal

Confirm the single-dataset HTML rendering matches Python's output character-for-character (or as close as the d3.js template allows). No changes expected.

## 2. Python source-of-truth

`cognee/api/v1/users/routers/get_visualize_router.py:27-74` — single `GET` handler returning `text/html` (the file lives under `users/routers/` upstream but is mounted at `/api/v1/visualize` per `cognee/api/client.py:241`). On any failure (`PermissionDenied`, `DatasetNotFound`, render error) collapses into a 409 `{error}` envelope. **Strict parity quirk**: the Python code uses a broad `except Exception` that swallows 403/404/500 into 409. Rust must replicate this — confirmed at `crates/http-server/src/routers/visualize.rs:71-101`.

## 3. Current Rust state

- Router: `crates/http-server/src/routers/visualize.rs:35` — `.route("/", get(get_visualize))`.
- DTO: `crates/http-server/src/dto/visualize.rs:11-13` — `VisualizeQueryDTO { dataset_id: Uuid }`.
- Handler: `crates/http-server/src/routers/visualize.rs:59-101` — fetches dataset, gates `AclDb::has_permission(... "read")`, calls `cognee_visualization::render(graph_db)`. All errors collapse to `ApiError::VisualizeError(StatusCode::CONFLICT, ...)`.
- Renders via the `cognee-visualization` crate (`crates/visualization/`), which ports Python's `_build_html()` byte-for-byte. Template: `crates/visualization/assets/graph_template.html` (seven `__*_DATA__` placeholders confirmed at lines 404–409 + 227). Substitution: `crates/visualization/src/html.rs:32-51` with the matching `</` → `<\/` escape at lines 23–26.
- Handler unit tests: `crates/http-server/tests/test_visualize_single.rs` covers 200 (line 106), 409 for permission denied (line 155), 409 for unknown dataset (line 51), 400/422 for missing query param (line 30).

## 4. Verification steps

1. ~~Existing test: `crates/http-server/tests/test_visualize.rs` (or under `routers/visualize.rs#[cfg(test)]`):~~
   The handler unit tests live at `crates/http-server/tests/test_visualize_single.rs` and already pass:
   - 200 `text/html` for own dataset (line 106).
   - 409 for permission-denied dataset (NOT 403 — the parity quirk; line 155).
   - 409 for nonexistent dataset (line 51).
   - 400/422 for missing query param (line 30). 401-unauthenticated is enforced by the `AuthenticatedUser` axum extractor; no direct test needed for verify-only.
2. `e2e-cross-sdk/harness/test_http_visualize.py` — **template-extracted JSON equality strategy** (Decision 11). The existing file at this path is **stale** (greps for `<!--JSON_ISLAND_START/END-->` markers that exist nowhere) and must be rewritten:

   Python's HTML template ([`cognee_network_visualization.py:170-185`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/visualization/cognee_network_visualization.py#L170-L185)) substitutes seven JSON-shaped placeholders into JS variable assignments:

   | Placeholder | Resulting JS line | Parity-relevant? |
   |---|---|---|
   | `__NODES_DATA__` | `var nodes = [...];` | **Yes** |
   | `__LINKS_DATA__` | `var links = [...];` | **Yes** |
   | `__SCHEMA_DATA__` | `const schemaData = {...};` (or `null`) | Yes |
   | `__TASK_COLORS__` | `var ... = {...};` | Yes |
   | `__PIPELINE_COLORS__` | `var ... = {...};` | Yes |
   | `__NODESET_COLORS__` | `var ... = {...};` | Yes |
   | `__USER_COLORS__` | `var ... = {...};` | Yes |

   Test recipe:

   - Add identical fixtures to both servers (same nodes, edges, edge weights, node types).
   - Hit `GET /visualize?dataset_id=X` against both backends.
   - **Smoke check**: response status is `200`, `Content-Type: text/html`, body contains all seven literal markers. The exact JS variable names (verified against `crates/visualization/assets/graph_template.html`) are: `var nodes = `, `var links = `, `var taskColors = `, `var pipelineColors = `, `var nodesetColors = `, `var userColors = `, `const schemaData = ` (these match Python's template).
   - **JSON-equality check**: write a `_extract_payload(body: str) -> dict` helper that regexes out each substituted JSON literal, reverses Python's `</` escape (`replace("<\\/", "</")`), and returns `{"nodes": ..., "links": ..., "schema": ..., "task_colors": ..., "pipeline_colors": ..., "nodeset_colors": ..., "user_colors": ...}`. `json.loads()` each value. Structural-diff the two extracted dicts.
   - **Sort before diff**: node arrays by `node["id"]`, link arrays by `(link["source"], link["target"], link.get("label"))`, color maps by their key (they're already objects so no sort). This handles deterministic-but-unordered emissions.
   - **Out of scope** for the diff: d3.js bundle hash, CDN URLs, theme/CSS, layout-coordinate randomness — none of these affect the seven extracted payloads.
   - **Negative test**: seed the two backends with intentionally different graph contents and assert `_extract_payload(rs) != _extract_payload(py)` so the harness is sanity-verified to be doing real work.

## 5. Acceptance criteria

- [x] 200 / 401 / 409 status mapping confirmed by tests.
- [x] Cross-SDK JSON-equality strategy implemented per Decision 11 — extracts all seven JS-variable JSON payloads from the HTML and structurally diffs them with stable sort.
- [x] `_extract_payload` helper handles Python's `</` → `<\/` escape (reverse it before `json.loads`).
- [x] Smoke check verifies `Content-Type: text/html` and presence of all seven JS-variable markers in both bodies.
- [x] Negative test confirms the harness detects real graph differences.
- [x] No code change in `crates/`.

## 6. References

- [Python visualize router](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/visualize/routers/get_visualize_router.py)
- [Rust handler](../../../crates/http-server/src/routers/visualize.rs)
- [`cognee-visualization` crate](../../../crates/visualization/)
