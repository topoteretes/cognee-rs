# E-10 — `GET /api/v1/sessions/stats`

| | |
|---|---|
| Wire path | `GET /api/v1/sessions/stats?range=` |
| Status | **Missing** |
| Depends on | LIB-05 (`SessionLifecycleDb::aggregate_stats`), which transitively depends on LIB-03. |
| Effort | ~1 day. |
| Owner crate | `cognee-http-server` |

## 1. Goal

Aggregate counters for the dashboard stat cards + status bar. Reads `session_records` (and computed `started_at`/`ended_at` durations) — does NOT touch `session_model_usage` (that's E-11).

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `GET /stats` handler | `cognee/api/v1/sessions/routers/get_sessions_router.py` | 112–197 |

### Response shape

```json
{
  "range":                    "30d",
  "sessions":                 <int>,
  "total_spend_usd":          <float>,
  "avg_spend_per_session_usd":<float>,
  "tokens_in":                <int>,
  "tokens_out":               <int>,
  "tokens_total":             <int>,
  "agent_time_s":             <float>,    // sum(end - started) over all rows
  "avg_session_s":            <float>,
  "success_rate":             <float>,    // completed / (completed+failed+abandoned)
  "completed":                <int>,
  "failed":                   <int>,
  "abandoned":                <int>,
  "running":                  <int>
}
```

Notes:
- `end = ended_at OR last_activity_at`. Skip rows where `started_at` or `end` is null.
- Visibility: caller's own rows OR rows whose `dataset_id` is in `permitted_dataset_ids_for(user)`.
- Uses `get_effective_status_sql()` so `running`-past-threshold rows count toward `abandoned`.
- `success_rate = 1.0` when `decided == 0` (Python explicit fallback).

## 3. Current Rust state

No route. No `SessionLifecycleDb::aggregate_stats` method (LIB-05 will add it; LIB-03 lands the underlying entities first).

## 4. Implementation steps

1. **Handler `get_stats`** in `crates/http-server/src/routers/sessions.rs` (added in E-09):
   ```rust
   pub async fn get_stats(
       user: AuthenticatedUser,
       State(state): State<AppState>,
       Query(q): Query<StatsQuery>,
   ) -> Result<Json<SessionStatsDTO>, ApiError> { ... }
   ```
   - `StatsQuery` = `{ range: RangeWindow }` (default `"30d"`).
   - Resolve permitted datasets (same as E-09).
   - Translate range to `Option<DateTime<Utc>>`.
   - Call `state.components.session_lifecycle_db.aggregate_stats(user.id, permitted, since)`.
   - Map `SessionStats` → `SessionStatsDTO`.

2. **DTO** in `crates/http-server/src/dto/sessions.rs`:
   ```rust
   /// Response DTO — wire is camelCase per Decision 10.
   #[derive(Debug, Serialize, ToSchema)]
   #[serde(rename_all = "camelCase")]
   pub struct SessionStatsDTO {
       pub range: String,                          // single-word, unaffected
       pub sessions: i64,                          // single-word, unaffected
       pub total_spend_usd: f64,                   // → "totalSpendUsd"
       pub avg_spend_per_session_usd: f64,         // → "avgSpendPerSessionUsd"
       pub tokens_in: i64,                         // → "tokensIn"
       pub tokens_out: i64,                        // → "tokensOut"
       pub tokens_total: i64,                      // → "tokensTotal"
       pub agent_time_s: f64,                      // → "agentTimeS"
       pub avg_session_s: f64,                     // → "avgSessionS"
       pub success_rate: f64,                      // → "successRate"
       pub completed: i64,
       pub failed: i64,
       pub abandoned: i64,
       pub running: i64,
   }
   ```

3. **Repository** at `crates/database/src/...` (lives in LIB-05):
   ```rust
   async fn aggregate_stats(...) -> Result<SessionStats, DbError>;
   ```
   Implementation does three queries — totals (count + sum tokens/cost), durations (per-row to compute `agent_time_s` server-side; mirrors Python's loop), and status buckets (using effective-status expression). Avoid loading all rows; durations can be done in a single SQL `SUM(EXTRACT(epoch FROM coalesce(ended_at, last_activity_at) - started_at))`.

4. **Wire** at `/api/v1/sessions/stats` (router built in E-09).

## 5. Tests

- `crates/http-server/tests/test_sessions_stats.rs`:
  - `stats_empty_returns_success_rate_one` — fresh user, no sessions.
  - `stats_buckets_reflect_effective_status` — running row past threshold counts as abandoned.
  - `stats_durations_skip_null_started_at`.
  - `stats_visibility_includes_permitted_datasets`.
  - `stats_range_24h_filters_correctly`.
- Cross-SDK in `test_http_v2_sessions.py`: parity diff for `?range=30d` and `?range=all`.

## 6. Acceptance criteria

- [ ] `GET /sessions/stats` matches the Python response shape field-for-field.
- [ ] `success_rate == 1.0` when `decided == 0` (explicit fallback parity).
- [ ] Effective-status logic flips abandoned-by-time rows correctly.
- [ ] Cross-SDK structural diff passes for all 5 range values.

## 7. References

- [Python stats handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L112)
- [LIB-03 — schema + entities](lib-03-session-records-schema.md)
- [LIB-05 — repository (`aggregate_stats`)](lib-05-session-records-repo.md)
