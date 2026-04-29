# E-03 — `GET /api/v1/recall`

| | |
|---|---|
| Wire path | `GET /api/v1/recall` |
| Status | **Implemented + Decision 6 polish** (was previously verify-only; promoted 2026-04-29) |
| Depends on | none |
| Effort | ~0.5 day (verify + Decision 6 helper module + attribute on `created_at`). |
| Owner crate | `cognee-http-server` |

## 1. Goal

Confirm the recall-history endpoint matches Python's `RecallHistoryItem` wire shape. No code changes expected.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `GET ""` handler | `cognee/api/v1/recall/routers/get_recall_router.py` | 58–76 |
| `RecallHistoryItem` DTO | same file | 50–55 |
| `get_history(user_id, limit=0)` | `cognee/modules/search/operations/get_history.py` | 12–~50 |

Response: `list[RecallHistoryItem]`, where each item is:

```json
{
  "id": "<uuid>",
  "text": "...",
  "user": "user|system|...",
  "created_at": "<ISO8601>"
}
```

Errors: `500 {error: "An error occurred while fetching recall history."}` for any exception.

## 3. Current Rust state

- Route: `crates/http-server/src/routers/recall.rs:31` — `.route("/", get(get_recall_history))`.
- DTO: `crates/http-server/src/dto/recall.rs` — `RecallHistoryItemDTO`.
- Reads from `SearchHistoryDb` (in `cognee-database`) which already mirrors Python's `search_history` table.

## 4. Verification steps

1. Existing test under `crates/http-server/tests/test_recall.rs` (or similar) — confirm coverage of:
   - 200 with empty history (new user).
   - 200 with mixed `user`/`system` rows ordered by `created_at` desc (Python's `get_history` sorts that way).
   - 401 unauthenticated.
   - 500 on DB error (Python catches `Exception` and returns the canned message).
2. Add to `e2e-cross-sdk/harness/test_http_v2_recall.py` — call `POST /recall` once, then `GET /recall` against both backends, structural-diff the history rows.
3. Verify the timestamp serialization on `created_at` matches the project-wide convention from [`../README.md §1.1 Wire conventions`](../README.md#11-wire-conventions-project-wide-set-by-decision-6) — emit `+00:00`, accept either `+00:00` or `Z` on deserialization.

## 5. Decision 6 work — `iso8601_offset` serde helper

> **Decision (2026-04-29) — Decision 6**: this task **owns** the `iso8601_offset` serde helper module (per [`../README.md §1.1 Wire conventions`](../README.md#11-wire-conventions-project-wide-set-by-decision-6)). E-03 is the first task in the §0 phase order to ship a wire-visible `DateTime<Utc>` field (`RecallHistoryItemDTO::created_at`), so the helper lands here and every later task (LIB-03's downstream DTOs in E-09/E-10/E-11/E-12, future v2 work) reuses it.

This is **not** verify-only — the helper module + attribute application are real code work. The promotion from "verify only" was a planned consequence of Decision 6, not a verify-time discovery, so the investigation agent should NOT escalate it as a divergence.

### 5.1 Helper module

New module at `crates/http-server/src/dto/util.rs::iso8601_offset` (or extend the existing `util.rs` if present):

- `pub fn serialize<S: Serializer>(dt: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error>` — formats with `format("%Y-%m-%dT%H:%M:%S%.6f%:z")` so the offset renders as `+00:00`. Truncate to microseconds (Python's default precision); trailing-zero-strip on the fraction to match Python's `isoformat()`.
- `pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error>` — accept any RFC 3339 string via `DateTime::parse_from_rfc3339` and convert to `Utc`. Both `Z` and `+00:00` parse cleanly without branching.

Unit tests:
- `serializes_utc_with_plus_zero_zero`.
- `deserializes_z_suffix`.
- `deserializes_plus_zero_zero`.
- `round_trip_microsecond_precision`.
- `truncates_nanoseconds_to_microseconds_on_serialize` — `2026-04-29T14:32:01.123456789Z` → `2026-04-29T14:32:01.123456+00:00`.

### 5.2 Apply to `RecallHistoryItemDTO`

```rust
#[serde(with = "crate::dto::util::iso8601_offset")]
pub created_at: DateTime<Utc>,
```

The cross-SDK parity test in §4.2 must pass byte-equality on the `created_at` string after this lands.

### 5.3 Acceptance update

- [ ] Helper module exists at `crates/http-server/src/dto/util.rs::iso8601_offset` with all 5 unit tests passing.
- [ ] `RecallHistoryItemDTO::created_at` uses the helper.
- [ ] Cross-SDK parity test asserts byte equality on `created_at` (no normalizer needed).

## 6. Acceptance criteria

- [ ] Cross-SDK parity test passes (with documented timestamp normalizer if needed).
- [ ] No code change required; any change captured here as a follow-up.

## 7. References

- [Python recall history handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/recall/routers/get_recall_router.py#L58)
- [Rust handler](../../../crates/http-server/src/routers/recall.rs)
