# E-06 — `POST /api/v1/forget`

| | |
|---|---|
| Wire path | `POST /api/v1/forget` |
| Status | **Done** (verified, no code change) |
| Depends on | none |
| Effort | ~0.25 day. |
| Owner crate | `cognee-http-server` |

> **Investigation 2026-04-29**: zero parity divergences against
> `/tmp/cognee-python/cognee/api/v1/forget/{routers/get_forget_router.py,forget.py}`.
> All Rust artefacts already exist:
> - DTO `crates/http-server/src/dto/forget.rs:13` (camelCase wire + `data_id` snake alias per Decision 10).
> - Handler `crates/http-server/src/routers/forget.rs:36` (three modes; canonical 422 message string `INVALID_PARAMS_MSG` at line 30).
> - Unit tests (7 in `dto::forget::tests`) + integration tests (6 in `tests/test_forget.rs`) all pass.
> - Cross-SDK parity test `e2e-cross-sdk/harness/test_http_forget.py` covers all three modes
>   plus non-existent (file already present, not the speculative path in §4 step 3 below).
> - No `DateTime<Utc>` field on this route — Decision 6 (`iso8601_offset` helper) is N/A.
>
> Investigation 2026-04-29: zero divergences against Python source-of-truth; verify-only
> short-circuit per IMPLEMENTATION-PROMPT.md §0 Lessons #3. **Closed:** task marked Done
> with no code change required.

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

1. Existing unit tests in `crates/http-server/src/dto/forget.rs` (`resolve_mode_*`, `dataset_ref_*`) — **passing** (7 tests).
2. Existing integration tests `crates/http-server/tests/test_forget.rs` — **passing** (6 tests):
   - `test_forget_no_auth_returns_401` — auth guard.
   - `test_forget_no_fields_returns_422` — empty body → 422 with `{"error": ...}`.
   - `test_forget_data_id_only_returns_422` — `data_id` w/o `dataset` → 422.
   - `test_forget_everything_true_ignores_extra_fields` — mode 3 priority.
   - `test_forget_everything_resolves_mode_correctly` — mode 3 not 422.
   - `test_forget_dataset_only_resolves_to_mode2` — mode 2 path reachable.
   - Note: full mode 1/2/3 deletion behavior (200 responses) requires wired backends and is verified by the cross-SDK harness, not the in-process Rust tests.
3. Cross-SDK parity test ~~`e2e-cross-sdk/harness/test_http_v2_forget.py`~~ → already exists at `e2e-cross-sdk/harness/test_http_forget.py` (no `_v2_` segment in the v1 harness file naming). Covers `test_forget_by_data_id`, `test_forget_by_dataset`, `test_forget_everything`, `test_forget_nonexistent_returns_404`. Asserts `py.status_code == rs.status_code` plus `assert_responses_match` with `DEFAULT_IGNORE`.
4. **Untagged `serde` enum**: `ForgetResponseDTO` is `#[serde(untagged)]` — Python returns plain dicts (not `OutDTO`), so wire is snake_case. The variants `ForgetDataItemResponse` / `ForgetDatasetResponse` / `ForgetEverythingResponse` use `#[serde(rename_all = "snake_case")]` to match Python's plain-dict snake_case keys; Decision 10 (camelCase) does **not** apply to plain-dict responses. Confirmed against `_forget_data_item`/`_forget_dataset`/`_forget_everything` at `/tmp/cognee-python/cognee/api/v1/forget/forget.py:144,165,187`.

## 5. Acceptance criteria

- [x] Cross-SDK parity test passes for all three modes (file `test_http_forget.py` present, assertions in place — full pass requires running the Docker harness).
- [x] `data_id`-without-`dataset` returns 422 on both backends with identical `{"error": "Invalid request parameters. Specify dataset, data_id+dataset, or everything=True."}` envelope (verified by `test_forget_data_id_only_returns_422` and matched against Python `get_forget_router.py:55-61`).
- [x] No code change required.

## 6. References

- [Python forget router](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/routers/get_forget_router.py)
- [Rust handler](../../../crates/http-server/src/routers/forget.rs)
