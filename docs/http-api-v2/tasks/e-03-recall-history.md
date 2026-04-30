# E-03 — `GET /api/v1/recall`

| | |
|---|---|
| Wire path | `GET /api/v1/recall` |
| Status | **Done** (commit 0dafdee) |
| Depends on | none |
| Effort | ~0.5 day (verify + Decision 6 helper module + attribute on `created_at`). |
| Owner crate | `cognee-http-server` |

## 1. Goal

Confirm the recall-history endpoint matches Python's `RecallHistoryItem` wire shape, **and** introduce the project-wide `iso8601_offset` serde helper module that every later v2 task with a `DateTime<Utc>` field will reuse (Decision 6).

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `GET ""` handler | `cognee/api/v1/recall/routers/get_recall_router.py` | 58–76 |
| `RecallHistoryItem` DTO | same file | 52–56 |
| `get_history(user_id, limit=10)` | `cognee/modules/search/operations/get_history.py` | 12–~31 |

Verified against `/tmp/cognee-python` checkout — handler is at lines 58–76, the inline `class RecallHistoryItem(OutDTO)` at lines 52–56 has fields `id: UUID`, `text: str`, `user: str`, `created_at: datetime`. `get_history` orders by `created_at` **ASCENDING** (`order_by("created_at")` — default ASC, `cognee/modules/search/operations/get_history.py:23`), NOT desc. The handler always passes `limit=0` so no limit clause is applied (`get_recall_router.py:68`).

Response: `list[RecallHistoryItem]`, where each item is:

```json
{
  "id": "<uuid>",
  "text": "...",
  "user": "user|system",
  "createdAt": "<ISO8601 with +00:00>"
}
```

Field name on the wire is **`createdAt`** (camelCase) because `RecallHistoryItem` inherits `OutDTO`, which sets `alias_generator=to_camel` + `populate_by_name=True` (Decision 10).

Errors: `500 {error: "An error occurred while fetching recall history."}` for any exception caught.

## 3. Current Rust state

- Route: `crates/http-server/src/routers/recall.rs:31` — `.route("/", get(get_recall_history))` (already mounted; same router file also hosts `POST /recall` via line 32, which is E-04's territory).
- Handler: `crates/http-server/src/routers/recall.rs:53-87` — delegates to `state.components().search_orchestrator.get_history(Some(user.id), None)`. On missing orchestrator OR error, returns `500 {"error": "An error occurred while fetching recall history."}` via `RecallErrorBody::JustError` (matches Python).
- DTO: `crates/http-server/src/dto/recall.rs:19-21` — re-exports `crate::dto::search::SearchHistoryItemDTO as RecallHistoryItemDTO` (recall and search share the wire shape per Python parity). The struct itself lives at `crates/http-server/src/dto/search.rs:158-166`:
  ```rust
  #[derive(Debug, Clone, Serialize, ToSchema)]
  #[serde(rename_all = "camelCase")]
  pub struct SearchHistoryItemDTO {
      pub id: Uuid,
      pub text: String,
      pub user: String,
      pub created_at: DateTime<Utc>,   // ← no custom serde → emits "…Z" (chrono default), needs `iso8601_offset`.
  }
  ```
- `from_entry` mapping (`search.rs:168-182`): `Query` → `"user"`, `Result` → `"system"` (parity).
- Reads from `cognee_database::SearchHistoryDb::get_history(user_id, limit=0)`.
- Existing tests: `crates/http-server/tests/test_recall.rs:67-89` covers the no-orchestrator 500 path and the `{error}`-only envelope. `crates/http-server/tests/test_recall.rs:113-140` pins "search and recall histories must match" by running both endpoints and asserting equality. `crates/http-server/tests/test_search_history.rs` covers the 200-empty and 200-with-rows cases on the `/search` side, which `recall.rs:113-140` proves are equivalent for `/recall`.
- `crates/http-server/src/dto/util.rs` exists but currently only contains `DatasetIdRef`; it does **NOT** yet contain an `iso8601_offset` module (verified at `dto/util.rs:1-187`). Module is declared as `pub mod util;` at `dto/mod.rs:73`.

### 3.1 Wire-shape divergence vs Python (the reason this task lands code)

> **Resolved in commit 0dafdee** — the `iso8601_offset` helper landed at `crates/http-server/src/dto/util.rs` and is now applied to `SearchHistoryItemDTO::created_at`. Wire output matches Python byte-for-byte.

`SearchHistoryItemDTO::created_at` was serialized using chrono's default `Serialize` impl, which produces RFC 3339 with `Z` suffix and nanosecond precision (`"2026-04-29T14:32:01.123456789Z"`). Python's pydantic `OutDTO.model_dump()` calls `datetime.isoformat()` which produces `+00:00` offset and microsecond precision (`"2026-04-29T14:32:01.123456+00:00"`). Decision 6 settled this by introducing the `iso8601_offset` helper.

**This was the only Rust↔Python wire divergence on `GET /recall`.** All other fields (`id`, `text`, `user`) match byte-for-byte already.

The same divergence applies to every other `DateTime<Utc>` field across the http-server crate (`dto/search.rs:165` itself; `dto/notebooks.rs:22`; `dto/datasets.rs:16-17,27-28`). E-03's helper module fixes the `recall` history field; later tasks (E-09 `SessionRowDTO`, E-10 `SessionStatsDTO`, E-12 detail response) reuse the same helper. The other v1 DTOs that already use `DateTime<Utc>` default serde are out of scope here — they were not flagged in the v1 cleanup and remain on the chrono default (the §1.1 wire-conventions rule applies project-wide for v2 DTOs touched by this package; v1 DTOs are addressed via CLEAN-01-style audits, none of which currently mandate the iso8601 flip).

## 4. Verification & code steps

### 4.1 Helper module (Decision 6 — primary deliverable)

Add `iso8601_offset` submodule to `crates/http-server/src/dto/util.rs`:

- `pub fn serialize<S: Serializer>(dt: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error>` — formats with `format("%Y-%m-%dT%H:%M:%S%.6f%:z")` so the offset renders as `+00:00`. Truncate to microseconds (Python's default precision); the trailing-zero-strip on the fraction matches Python's `isoformat()` output for naive `datetime.fromtimestamp(...)` values.
- `pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error>` — accept any RFC 3339 string via `DateTime::parse_from_rfc3339` and convert to `Utc`. Both `Z` and `+00:00` parse cleanly without branching.

Unit tests (in the same file under `#[cfg(test)] mod tests`):

- `serializes_utc_with_plus_zero_zero` — `2026-04-29T14:32:01Z` round-trips as `2026-04-29T14:32:01+00:00`.
- `deserializes_z_suffix` — `"…Z"` parses cleanly to a `DateTime<Utc>`.
- `deserializes_plus_zero_zero` — `"…+00:00"` parses cleanly.
- `round_trip_microsecond_precision` — `2026-04-29T14:32:01.123456+00:00` survives a serialize → deserialize cycle.
- `truncates_nanoseconds_to_microseconds_on_serialize` — `2026-04-29T14:32:01.123456789Z` → `2026-04-29T14:32:01.123456+00:00`.

### 4.2 Apply to `RecallHistoryItemDTO` (= `SearchHistoryItemDTO`)

In `crates/http-server/src/dto/search.rs:165`:

```rust
#[serde(with = "crate::dto::util::iso8601_offset")]
pub created_at: DateTime<Utc>,
```

This single attribute change is the only edit to the recall handler chain; the DTO is shared with search, so both `GET /search` and `GET /recall` history endpoints flip in lockstep — that's the desired Python-parity outcome (search and recall share the wire shape per `recall.md`).

### 4.3 Cross-SDK parity test

Add `e2e-cross-sdk/harness/test_http_v2_recall_history.py` (new file — `test_http_recall.py` exists but only covers `POST /recall`, not `GET /recall`):

1. Authenticated `GET /api/v1/recall` against both backends with no prior history → assert `200 []` on both.
2. POST one search/recall (existing seeding from `seed.py`), then GET `/api/v1/recall` against both backends → structural-diff the rows.
3. After Decision 6 lands, **byte-equality** on the `createdAt` string field is the strict assertion (no normalizer needed).
4. The 500 error envelope path is non-deterministic across backends (Rust's "no orchestrator wired" vs Python's exception path); skip cross-SDK assertion for that case — the existing unit test in `test_recall.rs:67-89` covers Rust-side parity.

### 4.4 Existing test coverage already in place

- `crates/http-server/tests/test_recall.rs:67-89` — no-orchestrator → 500 with `{error}` only (passes today).
- `crates/http-server/tests/test_recall.rs:113-140` — GET `/search` and GET `/recall` return identical bodies (passes today, will continue to pass after the helper lands since both use the same DTO).
- `crates/http-server/tests/test_search_history.rs:13-31` — empty history returns `200 []` (passes today).
- `crates/http-server/tests/test_search_history.rs:33-73` — POST then GET returns query+result rows with `user`/`system` discriminator (passes today). After Decision 6, an additional assertion can confirm the `createdAt` string ends in `+00:00`.

The `401 unauthenticated` case is exercised by the auth middleware test suite generically; no recall-specific test is required.

## 5. Decision 6 work — `iso8601_offset` serde helper

> **Decision (2026-04-29) — Decision 6**: this task **owns** the `iso8601_offset` serde helper module (per [`../README.md §1.1 Wire conventions`](../README.md#11-wire-conventions-project-wide-set-by-decision-6)). E-03 is the first task in the §0 phase order to ship a wire-visible `DateTime<Utc>` field (`RecallHistoryItemDTO::created_at`), so the helper lands here and every later task (LIB-03's downstream DTOs in E-09/E-10/E-11/E-12, future v2 work) reuses it.
>
> **Investigation agent: do not re-litigate.**

This is **not** verify-only — the helper module + attribute application + cross-SDK test are real code work. The promotion from "verify only" was a planned consequence of Decision 6, not a verify-time discovery, so the investigation agent should NOT escalate it as a divergence.

### 5.1 Acceptance update

- [x] Helper module exists at `crates/http-server/src/dto/util.rs::iso8601_offset` with all 5 unit tests passing.
- [x] `SearchHistoryItemDTO::created_at` (re-exported as `RecallHistoryItemDTO::created_at`) uses `#[serde(with = "crate::dto::util::iso8601_offset")]`.
- [x] Cross-SDK parity test `test_http_v2_recall_history.py` asserts byte equality on `createdAt` (no normalizer needed).
- [x] Existing unit tests in `test_recall.rs` and `test_search_history.rs` continue to pass.

## 6. Acceptance criteria

- [x] `iso8601_offset` helper module landed with 5 unit tests.
- [x] `created_at` field on the shared search/recall history DTO uses the helper.
- [x] Cross-SDK parity test for `GET /recall` history landed and passes byte-equality on `createdAt`.
- [x] No regressions in `test_recall.rs` / `test_search_history.rs` / `test_search_post.rs`.
- [x] `cargo fmt` / `cargo check --all-targets` / `cargo test --workspace` / `scripts/check_all.sh` all green.

## 7. References

- [Python recall history handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L58)
- [Python `get_history` operation](https://github.com/topoteretes/cognee/blob/main/cognee/modules/search/operations/get_history.py)
- [Rust handler](../../../crates/http-server/src/routers/recall.rs)
- [Rust DTO (shared with search)](../../../crates/http-server/src/dto/search.rs)
- [Rust util module](../../../crates/http-server/src/dto/util.rs) (where `iso8601_offset` will land)
