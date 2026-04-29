# E-08 — `POST /api/v1/visualize/multi`

| | |
|---|---|
| Wire path | `POST /api/v1/visualize/multi` |
| Status | **Implemented** (verify only) |
| Depends on | none |
| Effort | ~0.5 day. |
| Owner crate | `cognee-http-server` |

> **Doc-correction note (2026-04-29)**: this task was previously labelled "Rust-only divergence — decision required" based on an incomplete grep of the Python tree. It is **not** a divergence. Python's `POST /api/v1/visualize/multi` lives in [`cognee/api/v1/users/routers/get_visualize_router.py:77`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L77) — the file is filed under `users/routers/` for historical reasons but the router is mounted at the `/api/v1/visualize` prefix (see [`cognee/api/client.py:241`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L241)), so both `GET ""` and `POST "/multi"` share that namespace. The Rust handler at [`crates/http-server/src/routers/visualize.rs:103`](../../../crates/http-server/src/routers/visualize.rs#L103) is a parity port.

## 1. Goal

Confirm the existing Rust `POST /visualize/multi` handler matches Python's superuser-only multi-dataset visualization byte-for-byte. No code changes expected.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `UserDatasetPair` model | `cognee/api/v1/users/routers/get_visualize_router.py` | 19–21 |
| `POST "/multi"` handler | same | 77–~140 |
| Mount prefix | `cognee/api/client.py` | 241 |

### Request body

JSON array (no envelope):

```json
[
  {"user_id": "<uuid>", "dataset_id": "<uuid>"},
  {"user_id": "<uuid>", "dataset_id": "<uuid>"}
]
```

### Behavior — parity-critical points

1. **Superuser-only**. Non-superusers get `403 {"error": "Superuser privileges required for multi-user visualization"}`.
2. **Permission is resolved against the target user**, not the caller. For each pair, Python does:
   ```python
   target_user = await get_user(pair.user_id)
   datasets = await get_authorized_existing_datasets([pair.dataset_id], "read", target_user)
   ```
   So a superuser still cannot include a dataset whose **owner** lacks read permission on it (rare but real edge case — e.g. ACL-revoked datasets).
3. **Catch-all 409**: any exception from the iteration or the multi-user render collapses to `409 {"error": str(exc)}`.
4. **Response**: `text/html` body from `visualize_multi_user_graph(user_dataset_pairs)`. Color-by-user tagging in the d3.js template.

## 3. Current Rust state

- Route registered at [`crates/http-server/src/routers/visualize.rs:36`](../../../crates/http-server/src/routers/visualize.rs#L36): `.route("/multi", post(post_visualize_multi))`.
- Handler at [`crates/http-server/src/routers/visualize.rs:103-167`](../../../crates/http-server/src/routers/visualize.rs#L103-L167):
  - Uses the `SuperuserOnly` extractor (403 envelope).
  - Iterates pairs, resolves each `dataset_id`, checks `AclDb::has_permission(pair.user_id, dataset.id, "read")` — i.e. the target user's grant, matching Python.
  - Calls `cognee_visualization::render_multi_user(&user_pairs)`.
  - 409 catch-all on every error path.
- DTO: `crates/http-server/src/dto/visualize.rs` — `UserDatasetPairDTO { user_id, dataset_id }`.

## 4. Verification steps

1. **Cross-SDK parity test** in `e2e-cross-sdk/harness/test_http_v2_visualize.py` (extends E-07's test file):
   - Seed both servers with two users + two datasets. Add data to each.
   - Call `POST /visualize/multi` with both pairs against both backends.
   - Reuse the `_extract_payload` helper from E-07's test file (Decision 11) to extract the seven JS-variable JSON payloads (`nodes`, `links`, `schemaData`, the four color maps) and structurally diff them with stable sort. Same recipe as the single-dataset test; only difference is the request shape (POST body of pair array instead of GET query).
   - **`user_colors` parity is critical here**: the multi-user variant adds per-user color tagging via `__USER_COLORS__`. Confirm both backends emit identical user→color maps for the same input pairs.

2. **Rust unit tests** in `crates/http-server/tests/test_visualize.rs` (extend the existing file):
   - `multi_returns_403_for_non_superuser`.
   - `multi_returns_403_envelope_uses_error_key_not_detail` — Python parity quirk: the `403` body is `{"error": ...}`, not FastAPI's default `{"detail": ...}`.
   - `multi_resolves_permission_against_target_user_not_caller` — superuser caller, target user has been ACL-revoked → 409 (NOT 200).
   - `multi_empty_array_returns_409_or_200` — confirm what Python does with `[]` and replicate. (Likely 200 with an empty visualization.)
   - `multi_dataset_not_found_returns_409`.

3. **OpenAPI snapshot** — confirm `POST /visualize/multi` is advertised with the request body `array<UserDatasetPair>` (no wrapping object). Compare against Python's `openapi.json`.

## 5. Polish (only if verification reveals gaps)

- The Rust 403 envelope: confirm the `SuperuserOnly` extractor produces `{"error": "Superuser privileges required for multi-user visualization"}` byte-for-byte (or whatever Python emits). If it emits `{"detail": ...}` or a different message, that is a divergence to fix.
- Confirm the iteration's permission check uses the **target user's** ACL row, not the caller's. The current code uses `pair.user_id` — that's correct, just double-check no merge has flipped it to `user.id`.

## 6. Acceptance criteria

- [ ] Cross-SDK structural-HTML diff passes for the two-pair case.
- [ ] 403 envelope uses `{"error": ...}` (not `{"detail": ...}`).
- [ ] Permission check is against `pair.user_id`, not the caller.
- [ ] 409 catch-all matches Python's wording.
- [ ] OpenAPI snapshot matches Python.

## 7. References

- [Python `POST /multi` handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L77)
- [Python mount prefix](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L241)
- [Rust handler](../../../crates/http-server/src/routers/visualize.rs#L103)
- [E-07 — sibling `GET /visualize` task](e-07-visualize.md) (shares the bundle-hash strategy)
