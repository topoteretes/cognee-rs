# E-06 — `POST /api/v1/forget`

| | |
|---|---|
| Wire path | `POST /api/v1/forget` |
| Status | **Implemented** (verify only) |
| Depends on | none |
| Effort | ~0.25 day. |
| Owner crate | `cognee-http-server` |

## 1. Goal

Confirm forget endpoint matches Python parity. No changes expected.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `ForgetPayloadDTO` | `cognee/api/v1/forget/routers/get_forget_router.py` | 16–19 |
| `POST ""` handler | same | 25–~80 |

Body: `{data_id?, dataset?, everything: bool}`. Three modes (resolved by the handler):
1. `data_id + dataset` → delete a single item.
2. `dataset` alone → delete the whole dataset.
3. `everything: true` → delete all caller's data; `data_id`/`dataset` ignored.

## 3. Current Rust state

- DTO: `crates/http-server/src/dto/forget.rs` — full mode resolver (`ForgetMode::{DataItem,Dataset,Everything}`) and `DatasetRef::{Id,Name}`.
- Handler: `crates/http-server/src/routers/forget.rs:36` (`post_forget`).
- Delegates to `DeleteService::{delete_item, delete_dataset, delete_everything}`.

## 4. Verification steps

1. Existing tests in `crates/http-server/src/dto/forget.rs` (resolve_mode_*) — passing.
2. Confirm `crates/http-server/tests/` has a `test_forget.rs` exercising:
   - Mode 1 (item delete) → 200 `{data_id, dataset_id, status: "success"}`.
   - Mode 2 (dataset delete) → 200 `{dataset_id, status}`.
   - Mode 3 (everything) → 200 `{datasets_removed: N, status}`.
   - 422: `data_id` without `dataset`.
   - 422: empty body.
3. `e2e-cross-sdk/harness/test_http_v2_forget.py` — POST identical bodies to both servers; assert both DBs are empty post-call.
4. **Untagged `serde` enum**: `ForgetResponseDTO` is `#[serde(untagged)]` — confirm Python's plain dict response matches the variant fields exactly. Python returns plain dicts; the Rust `untagged` enum picks the right variant by field shape.

## 5. Acceptance criteria

- [ ] Cross-SDK parity test passes for all three modes.
- [ ] `data_id`-without-`dataset` returns 422 on both backends with identical error envelope.
- [ ] No code change required.

## 6. References

- [Python forget router](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/routers/get_forget_router.py)
- [Rust handler](../../../crates/http-server/src/routers/forget.rs)
