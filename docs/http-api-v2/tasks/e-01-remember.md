# E-01 — `POST /api/v1/remember`

| | |
|---|---|
| Wire path | `POST /api/v1/remember` |
| Status | **Implemented** (verify only) |
| Depends on | none |
| Effort | ~0.5 day (verification + parity test). |
| Owner crate | `cognee-http-server` |

## 1. Goal

Confirm the existing Rust handler still matches Python parity for the multipart "blob of text/files" path (the `cognee.remember(data, dataset_name, ...)` shape). No code changes expected.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `POST ""` handler | `cognee/api/v1/remember/routers/get_remember_router.py` | 28–113 |
| `cognee.remember(data, ...)` | `cognee/api/v1/remember/remember.py` | 1–230 |

Form fields (multipart):

```
data: List[UploadFile]
datasetName: Optional[str]
datasetId: UUID | "" | None
session_id: Optional[str]
node_set: Optional[List[str]]      (Python sends [""] → treat as None)
run_in_background: Optional[bool]  (default false)
custom_prompt: Optional[str]       (default "")
chunks_per_batch: Optional[int]    (default 10)
```

Response: `RememberResult.to_dict()`. `200` on success; `409` `{error}` envelope on exception; ValueError → `400 {error}`.

## 3. Current Rust state

- Router: `crates/http-server/src/routers/remember.rs:285` — `Router::new().route("/", post(post_remember))`.
- Multipart parsing: same file, `parse_remember_form` at line ~43.
- DTO: `crates/http-server/src/dto/remember.rs` — `RememberFormDTO`, `RememberResultDTO`.
- All Python form fields including the `[""]` → `None` translation are already covered.

## 4. Verification steps

1. `cargo test -p cognee-http-server --test test_remember -- --nocapture`. If no test file exists, create `crates/http-server/tests/test_remember.rs` exercising:
   - 200-path: multipart with one text file → `status: "started"` + UUID `pipeline_run_id`.
   - 200-path: `run_in_background=true` → returns immediately with `status: "running"`.
   - 422: missing `data` part.
   - 401: unauthenticated.
   - 409: `dataset_name` collides with another user's dataset (Python's catch-all `409 {error}`).
2. Add to `e2e-cross-sdk/harness/test_http_v2_remember.py` (per [e2e-parity.md §5](../e2e-parity.md)) — POST identical multipart against Python and Rust uvicorn-mirror, structural-diff the response.
3. Confirm OpenAPI snapshot matches Python — particularly the `node_set: [""]` example and the `chunks_per_batch: 10` default.

## 5. Polish (if tests reveal gaps)

- **`entry_type` / `entry_id` are NOT added in this task.** Per Decision 5 (2026-04-29), those fields are populated only by `/remember/entry`; the structural change to `RememberResultDTO` lands in **E-02** alongside the new route. On the `POST /remember` file-payload path Python omits both fields from the response, so the Rust DTO without them is already byte-correct. Do not pre-emptively add them here.
- Confirm 409 `{error}` envelope wording — Python returns `"An error occurred during remember."`; Rust must match verbatim.

## 6. Acceptance criteria

- [ ] Cross-SDK parity test for `POST /remember` passes against both backends.
- [ ] OpenAPI snapshot for the route matches Python (modulo `operationId` differences).
- [ ] No code change required; if any are required, capture them in a follow-up doc rather than expanding this task.

## 7. References

- [Python remember router](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py)
- [Rust handler](../../../crates/http-server/src/routers/remember.rs)
- [E-02 — `POST /remember/entry`](e-02-remember-entry.md) — sibling route, shared `RememberResult` type
