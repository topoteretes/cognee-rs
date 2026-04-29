# E-07 — `GET /api/v1/visualize`

| | |
|---|---|
| Wire path | `GET /api/v1/visualize?dataset_id=<uuid>` |
| Status | **Implemented** (verify only) |
| Depends on | none |
| Effort | ~0.25 day. |
| Owner crate | `cognee-http-server` |

## 1. Goal

Confirm the single-dataset HTML rendering matches Python's output character-for-character (or as close as the d3.js template allows). No changes expected.

## 2. Python source-of-truth

`cognee/api/v1/visualize/routers/get_visualize_router.py` — single `GET` handler returning `text/html`. On any failure (`PermissionDenied`, `DatasetNotFound`, render error) collapses into a 409 `{error}` envelope. **Strict parity quirk**: the Python code uses a broad `except Exception` that swallows 403/404/500 into 409. Rust must replicate this.

## 3. Current Rust state

- Router: `crates/http-server/src/routers/visualize.rs:35` — `.route("/", get(get_visualize))`.
- DTO: `crates/http-server/src/dto/visualize.rs` — `VisualizeQueryDTO`.
- Renders via the `cognee-visualization` crate (commit `a0daab3`), which ports Python's `cognee_network_visualization` byte-for-byte.

## 4. Verification steps

1. Existing test: `crates/http-server/tests/test_visualize.rs` (or under `routers/visualize.rs#[cfg(test)]`):
   - 200 `text/html` for own dataset.
   - 401 unauthenticated.
   - 409 for permission-denied dataset (NOT 403 — the parity quirk).
   - 409 for nonexistent dataset.
2. `e2e-cross-sdk/harness/test_http_v2_visualize.py` — **template-extracted JSON equality strategy** (Decision 11):

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
   - **Smoke check**: response status is `200`, `Content-Type: text/html`, body contains all seven literal markers (`var nodes = `, `var links = `, `const schemaData = ` and the four color-map markers — verify the exact JS variable names by reading the Rust template at first-run; they must match Python).
   - **JSON-equality check**: write a `_extract_payload(body: str) -> dict` helper that regexes out each substituted JSON literal, reverses Python's `</` escape (`replace("<\\/", "</")`), and returns `{"nodes": ..., "links": ..., "schema": ..., "task_colors": ..., "pipeline_colors": ..., "nodeset_colors": ..., "user_colors": ...}`. `json.loads()` each value. Structural-diff the two extracted dicts.
   - **Sort before diff**: node arrays by `node["id"]`, link arrays by `(link["source"], link["target"], link.get("label"))`, color maps by their key (they're already objects so no sort). This handles deterministic-but-unordered emissions.
   - **Out of scope** for the diff: d3.js bundle hash, CDN URLs, theme/CSS, layout-coordinate randomness — none of these affect the seven extracted payloads.
   - **Negative test**: seed the two backends with intentionally different graph contents and assert `_extract_payload(rs) != _extract_payload(py)` so the harness is sanity-verified to be doing real work.

## 5. Acceptance criteria

- [ ] 200 / 401 / 409 status mapping confirmed by tests.
- [ ] Cross-SDK JSON-equality strategy implemented per Decision 11 — extracts all seven JS-variable JSON payloads from the HTML and structurally diffs them with stable sort.
- [ ] `_extract_payload` helper handles Python's `</` → `<\/` escape (reverse it before `json.loads`).
- [ ] Smoke check verifies `Content-Type: text/html` and presence of all seven JS-variable markers in both bodies.
- [ ] Negative test confirms the harness detects real graph differences.
- [ ] No code change in `crates/`.

## 6. References

- [Python visualize router](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/visualize/routers/get_visualize_router.py)
- [Rust handler](../../../crates/http-server/src/routers/visualize.rs)
- [`cognee-visualization` crate](../../../crates/visualization/)
