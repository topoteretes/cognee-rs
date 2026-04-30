# E-08 — `POST /api/v1/visualize/multi`

| | |
|---|---|
| Wire path | `POST /api/v1/visualize/multi` |
| Status | **Done (commit afa048f, Decision 16 — Option A)** — Rust converged to Python's dedupe + email-label semantics. |
| Depends on | none |
| Effort | ~0.5 day if both divergences are accepted as-is; ~1 day if Rust must converge to Python's dedupe + email-label semantics. |
| Owner crate | `cognee-http-server` (router) + `cognee-visualization` (aggregation) |

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
  - Uses the `SuperuserOnly` extractor (403 envelope; see [`auth/superuser.rs`](../../../crates/http-server/src/auth/) — confirmed to emit `{"error": "Superuser privileges required for multi-user visualization"}` per the existing test [`tests/test_visualize_multi.rs:41-98`](../../../crates/http-server/tests/test_visualize_multi.rs#L41-L98)).
  - Iterates pairs, resolves each `dataset_id`, checks `AclDb::has_permission(pair.user_id, dataset.id, "read")` — i.e. the target user's grant, matching Python.
  - Calls `cognee_visualization::render_multi_user(&user_pairs)` with `(pair.user_id.to_string(), graph_db)` tuples.
  - 409 catch-all on every error path (`ApiError::VisualizeError(StatusCode::CONFLICT, ..)`).
- DTO: [`crates/http-server/src/dto/visualize.rs:17-21`](../../../crates/http-server/src/dto/visualize.rs#L17-L21) — `UserDatasetPairDTO { user_id, dataset_id }`. Note: serde defaults to snake_case here, but **Python's pydantic `BaseModel` also emits snake_case** (the camelCase alias generator only applies to `cognee.api.DTO.InDTO`/`OutDTO` subclasses, not raw `BaseModel`). Verified at [`get_visualize_router.py:19-21`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L19-L21) — `class UserDatasetPair(BaseModel)`. So the snake_case wire shape is parity-correct.
- Existing Rust integration tests at [`crates/http-server/tests/test_visualize_multi.rs`](../../../crates/http-server/tests/test_visualize_multi.rs) cover: empty array → 200, non-superuser → 403 with `{error}` envelope, unknown dataset → 409. All pass.
- Existing visualization-crate aggregation tests at [`crates/visualization/tests/test_render_multi_user.rs`](../../../crates/visualization/tests/test_render_multi_user.rs) cover: two-user three-node aggregation, empty input, single-user tagging.

### 3.1 Structural divergences from Python (found 2026-04-29 investigation)

**The two findings below are NOT in the existing acknowledged divergences list (D-1 in README §1.2). They need a user decision before E-08 can complete.**

1. **No node/edge deduplication across users.** Python's [`aggregate_multi_user_graphs` at `cognee_network_visualization.py:115-157`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/visualization/cognee_network_visualization.py#L115-L157) keeps `all_nodes: dict` keyed by `str(node_id)` (first-write-wins) and `seen_edges: set` keyed by `(str(source), str(target), relation)`. Rust's [`render_multi_user` at `crates/visualization/src/lib.rs:97-123`](../../../crates/visualization/src/lib.rs#L97-L123) does straight `Vec::push` / `Vec::extend` with no dedupe. **Effect on the seven extracted payloads** (Decision 11): when two `(user, dataset)` pairs share entities (common in real cognify graphs — e.g. shared schema nodes, shared `EntityType` rows), Python emits one node entry per unique id; Rust emits one per `(pair, id)` combination. The `nodes` payload diverges; `links` likely diverges too.

2. **`source_user` label semantics differ.** Python labels each node with `user.email or str(user.id)` — i.e. a human-readable email when available. Rust's HTTP handler stringifies `pair.user_id` (a `Uuid`) at [`routers/visualize.rs:160`](../../../crates/http-server/src/routers/visualize.rs#L160) and passes it as the `user_id` string. The visualization crate then writes that to both `user_id` and `source_user` ([`lib.rs:107-116`](../../../crates/visualization/src/lib.rs#L107-L116)). **Effect on the seven extracted payloads**: every node's `source_user` field carries different values (Python: `"alice@example.com"`; Rust: `"00000000-0000-0000-0000-000000000001"`); the `userColors` color map is keyed by those values, so its keys also diverge.

Both divergences fall outside Decision 11's "out of scope" list (bundle hash / CDN / theme / layout); the seven `__*_DATA__` payloads are the in-scope surface. So the strict-parity rule applies: either the divergences must be resolved in Rust, or a new entry **D-2** must be added to README §1.2 documenting them as an accepted v2 divergence.

> **Resolved (2026-04-29)**: both divergences converged in commit `afa048f` per Decision 16 (Option A). `render_multi_user` now deduplicates nodes by `str(node_id)` first-write-wins and edges by `(source, target, relation)`; the HTTP handler resolves `pair.user_id` to a `User` row and passes `user.email` (or stringified id fallback) as the `source_user` label. **No new wire divergence (no D-2)**.

Recommendation for the user: convergence (option A) is feasible with ~30 lines of code in `render_multi_user`. The handler already has access to the target user's record (it can resolve the email via the user repo before calling `render_multi_user`). Since the goal of the v2 port is byte-for-byte parity with Python, this is the right path. If the user decides it's too much scope, document divergence D-2 instead.

## 4. Implementation steps (Option A — converge Rust to Python)

> **Decision 16 (2026-04-29)** records the user's choice of Option A. Investigation agent: do not re-litigate.

The 403 envelope, permission-against-target-user, 409 catch-all, and snake_case DTO are already at parity (see §3). The only remaining work is making `render_multi_user` match Python's dedupe + email-label semantics, then locking it in with a cross-SDK parity test.

1. **Library: dedupe nodes & edges in `cognee_visualization::render_multi_user`** at [`crates/visualization/src/lib.rs:97-123`](../../../crates/visualization/src/lib.rs#L97-L123):
   - Replace the node `Vec` accumulator with a `HashMap<String, NodeWithSourceUser>` keyed by `node.id.to_string()` — first-write-wins (mirror Python `cognee_network_visualization.py:142`).
   - Replace the edge `Vec` accumulator with a `Vec` filtered by a `HashSet<(String, String, String)>` keyed by `(source.to_string(), target.to_string(), relation)` (mirror Python L150-155).
   - Preserve the existing iteration order across `(user, dataset)` pairs so first-write-wins behavior is deterministic and matches Python.

2. **Library: change `render_multi_user` signature** to accept a human-readable label per pair instead of a stringified UUID:
   - From `&[(String /* user_id */, Arc<dyn GraphDBTrait>)]` → `&[(String /* user_label */, Arc<dyn GraphDBTrait>)]`. Internal field names that previously held `user_id` should be renamed to `source_user` (matching Python's wire-output key); the `user_id` field on each emitted node is dropped (Python doesn't emit `user_id` separately — only `source_user`).
   - Confirm the `userColors` color map is keyed by the new `source_user` label (not the UUID), matching Python.

3. **Handler: resolve `pair.user_id` to a User row** in `post_visualize_multi` at [`crates/http-server/src/routers/visualize.rs:103-167`](../../../crates/http-server/src/routers/visualize.rs#L103-L167):
   - For each pair, after the existing permission check, look up the target user record via the existing user repository accessor (read the surrounding handler to find the right `state.lib`/`state.user_db` accessor — likely `state.lib.user_repository().get_user(pair.user_id).await?` or similar).
   - Build the label as `user.email.clone().unwrap_or_else(|| user.id.to_string())` to match Python's `getattr(user, "email", None) or str(user.id)` at `cognee_network_visualization.py:135`.
   - Pass the label (not the stringified UUID) into `render_multi_user`.
   - If user lookup fails, surface as the existing 409 catch-all (matches Python's catch-all behavior).

4. **Library tests: extend `crates/visualization/tests/test_render_multi_user.rs`** with two new cases:
   - `dedupe_overlapping_nodes_first_write_wins` — two pairs that share at least one node id by content; assert the rendered output contains exactly one node entry per unique id, with the `source_user` value from the first pair.
   - `dedupe_edges_by_source_target_relation` — two pairs that share an edge `(source, target, relation)`; assert exactly one edge entry in the output.

5. **Handler tests: extend `crates/http-server/tests/test_visualize_multi.rs`** if needed:
   - Existing tests cover 200/403/409. Add `multi_uses_email_label_when_user_has_email` if straightforward to set up the test user with an email; otherwise rely on the cross-SDK harness for that assertion.

6. **Cross-SDK parity test: extend `e2e-cross-sdk/harness/test_http_visualize.py`** (the file rewritten by E-07 at commit 35d6b3c) — add multi-pair test functions that reuse the existing `_extract_payload` and `_normalize_payload` helpers:
   - `test_visualize_multi_smoke` — POST with two-pair array against both backends; assert status 200, `Content-Type: text/html`, all seven markers present.
   - `test_visualize_multi_payload_equality_disjoint` — two `(user, dataset)` pairs with disjoint graphs; structural diff with stable sort; equality required.
   - `test_visualize_multi_payload_equality_overlapping` — two pairs with at least one shared node id; structural diff equality required (proves dedupe parity).
   - `test_visualize_multi_user_colors_keys_match` — assert `userColors` keys match across both backends (proves email-label parity).

7. **Run gates**: `cargo fmt`, `cargo check --all-targets`, `cargo test --workspace`, `scripts/check_all.sh`. Pre-existing JS jest + CLI E2E without `OPENAI_TOKEN` failures are safe to ignore per IMPLEMENTATION-PROMPT.md §0.

## 5. Polish (only if verification reveals gaps)

- The Rust 403 envelope: confirm the `SuperuserOnly` extractor produces `{"error": "Superuser privileges required for multi-user visualization"}` byte-for-byte (or whatever Python emits). If it emits `{"detail": ...}` or a different message, that is a divergence to fix.
- Confirm the iteration's permission check uses the **target user's** ACL row, not the caller's. The current code uses `pair.user_id` — that's correct, just double-check no merge has flipped it to `user.id`.

## 6. Acceptance criteria (Option A)

- [x] `render_multi_user` deduplicates nodes by `str(node_id)` first-write-wins (mirror Python `cognee_network_visualization.py:142`).
- [x] `render_multi_user` deduplicates edges by `(source, target, relation)` (mirror Python L150-155).
- [x] `render_multi_user` accepts a `user_label: String` per pair; emits `source_user` (not `user_id`) on each node.
- [x] `post_visualize_multi` resolves `pair.user_id` to a User row and passes `user.email.unwrap_or_else(|| user.id.to_string())` as the label.
- [x] Two new library tests cover node dedupe and edge dedupe.
- [x] Cross-SDK harness adds `_multi_*` tests; structural-HTML diff passes for disjoint AND overlapping graph pairs.
- [x] `userColors` keys match across backends in the `_multi_user_colors_keys_match` test.
- [x] No regressions in existing tests (`test_visualize_multi.rs`, `test_render_multi_user.rs`).
- [x] All gates green: `cargo fmt`, `cargo check --all-targets`, `cargo test --workspace`, `scripts/check_all.sh`.

## 7. References

- [Python `POST /multi` handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L77)
- [Python mount prefix](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L241)
- [Rust handler](../../../crates/http-server/src/routers/visualize.rs#L103)
- [E-07 — sibling `GET /visualize` task](e-07-visualize.md) (shares the bundle-hash strategy)
