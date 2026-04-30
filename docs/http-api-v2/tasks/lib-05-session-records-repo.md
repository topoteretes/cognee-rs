# LIB-05 — `SessionLifecycleDb` trait + `DatabaseConnection` impl + tests

| | |
|---|---|
| Scope | The `SessionLifecycleDb` trait, its concrete impl on `DatabaseConnection`, the effective-status helper, and 8 repository tests. |
| Status | **Done (commit 60c934a)** — `SessionLifecycleDb` trait + impl + 8 repo tests landed. Raw SQL via `Statement::from_sql_and_values` was used for the `ensure_and_touch_session` UPSERT (COALESCE backfill + WHERE clause on update — neither portably expressible via SeaORM's `OnConflict`) and the per-model upsert in `accumulate_usage`. `aggregate_stats` uses a Rust-side row-load fold for durations to match Python's SQLite parity at `get_sessions_router.py:148-158`. `accumulate_usage` includes an `errored: bool` parameter mirroring Python `metrics.py:142`. |
| Blocks | E-09, E-10, E-11, E-12 (entire `/sessions` router). |
| Depends on | LIB-03 (entities + migration must exist). |
| Effort | ~1.25 days. |
| Owner crate | `cognee-database` |

> **Decision (2026-04-29) — Decision 13**: this task is the second half of the original LIB-03 scope, split per Decision 13 option (b). LIB-03 lands the schema/entities/migration; this task lands the trait + impl + tests as a separate single commit. The split keeps each commit under ~500 LOC and lets reviewers focus on schema-vs-API concerns separately. Investigation agent: do not re-litigate.

## 1. Goal

Land the async repository the v2 sessions endpoints (E-09, E-10, E-11, E-12) call into, plus the effective-status logic that infers `"abandoned"` at read time. All seven dashboard queries (list, stats, cost-by-model, detail, plus the two write helpers `ensure_and_touch_session` and `accumulate_usage`) live here.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `get_effective_status_sql()` | `cognee/modules/session_lifecycle/metrics.py` | grep `def get_effective_status_sql` |
| `get_session_row` | `cognee/modules/session_lifecycle/metrics.py` | 295–334 |
| `list_session_rows` (paginated) | `cognee/modules/session_lifecycle/metrics.py` | 365–435 |
| `ensure_and_touch_session` | `cognee/modules/session_lifecycle/metrics.py` | grep `def ensure_and_touch_session` |
| `accumulate_usage` | `cognee/modules/session_lifecycle/usage_tracking.py` | grep |

Effective-status SQL behavior: `status == "running" AND last_activity_at < now() - threshold` is reported as `"abandoned"` at read time **without writing the row**. Mirror in SeaORM via a derived column expression.

## 3. Current Rust state

- After LIB-03 lands, `crates/database/src/entities/session_record.rs` and `crates/database/src/entities/session_model_usage.rs` exist, plus the migration. No consumer uses them yet.
- `crates/database/src/lib.rs` exposes `IngestDb`, `SearchHistoryDb`, `DeleteDb` traits — this task adds `SessionLifecycleDb` alongside.
- The investigation agent must confirm LIB-03 has landed before starting; if not, report BLOCKED.

## 4. Implementation steps

1. **`SessionLifecycleDb` trait** in `crates/database/src/lib.rs`:
   ```rust
   #[async_trait]
   pub trait SessionLifecycleDb: Send + Sync {
       async fn ensure_and_touch_session(
           &self, session_id: &str, user_id: Uuid, dataset_id: Option<Uuid>,
       ) -> Result<(), DbError>;

       async fn accumulate_usage(
           &self, session_id: &str, user_id: Uuid, model: &str,
           tokens_in: i64, tokens_out: i64, cost_usd: f64,
       ) -> Result<(), DbError>;

       async fn get_session_row(
           &self, session_id: &str, user_id: Uuid,
           permitted_dataset_ids: &[Uuid], prefer_other_owner: bool,
       ) -> Result<Option<SessionRowWithStatus>, DbError>;

       async fn list_session_rows(
           &self, filters: SessionListFilters,
       ) -> Result<SessionListPage, DbError>;

       async fn aggregate_stats(
           &self, user_id: Uuid, permitted_dataset_ids: &[Uuid],
           since: Option<DateTime<Utc>>,
       ) -> Result<SessionStats, DbError>;

       async fn cost_by_model(
           &self, user_id: Uuid, permitted_dataset_ids: &[Uuid],
           since: Option<DateTime<Utc>>,
       ) -> Result<Vec<CostByModelRow>, DbError>;
   }
   ```

2. **Concrete domain types** alongside the trait — field-for-field with the Python dicts in [`get_sessions_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py):
   - `SessionListFilters` — query inputs (range, status_filter, limit, offset, order_by, descending, plus the visibility predicate).
   - `SessionListPage` — `{ sessions: Vec<SessionRowWithStatus>, total: i64, limit: u32, offset: u32 }` with a `has_more()` method.
   - `SessionRowWithStatus` — wraps `entities::session_record::Model` plus `effective_status: String`.
   - `SessionStats` — full set of dashboard counters from [`get_sessions_router.py:112-197`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L112-L197).
   - `CostByModelRow` — `{ model, session_count, cost_usd, tokens_in, tokens_out }`.

3. **Effective-status helper** as a SeaORM column expression:
   ```rust
   fn effective_status_expr(threshold_seconds: i64) -> Expr {
       Expr::case()
         .when(
            Cond::all()
              .add(Expr::col(Column::Status).eq("running"))
              .add(Expr::col(Column::LastActivityAt).lt(now_minus(threshold_seconds))),
            Expr::value("abandoned"),
         )
         .finally_(Expr::col(Column::Status))
   }
   ```
   **Decision (2026-04-29) — Decision 12**: threshold from env var `SESSION_ABANDON_AFTER_SECONDS`, default `1800` (30 min). Verified against Python at [`cognee/modules/session_lifecycle/metrics.py:47-52`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/metrics.py#L47-L52). Rust implementation: parse `std::env::var("SESSION_ABANDON_AFTER_SECONDS").ok().and_then(|s| s.parse::<i64>().ok()).unwrap_or(1800)`.

4. **Implement `SessionLifecycleDb` for `DatabaseConnection`** in `crates/database/src/...`. Follow Python's logic exactly:
   - `ensure_and_touch_session` is an upsert that only sets `dataset_id` when currently null (idempotent re-touch on every request).
   - `accumulate_usage` is an upsert on the composite PK that adds to `tokens_in`/`tokens_out`/`cost_usd` and bumps `updated_at`.
   - `get_session_row` resolves visibility via `WHERE user_id = :u OR dataset_id IN :permitted` and supports `prefer_other_owner` when multiple rows match.
   - `list_session_rows` builds the query in three stages: count → sortable_columns_lookup → page query. Sort key fallback to `last_activity_at` happens in the handler (E-09), NOT here.
   - `aggregate_stats` issues three queries: totals (count + sum tokens/cost), durations (use SQL `EXTRACT(epoch FROM coalesce(ended_at, last_activity_at) - started_at)` to avoid loading every row), and status buckets via `effective_status_expr` group-by.
   - `cost_by_model` joins `session_model_usage` to `session_records`, groups by model, sorts by cost descending — see [E-11 §4](e-11-sessions-cost-by-model.md) for the SQL skeleton.

5. **Re-exports** from `cognee_database` for `SessionLifecycleDb`, `SessionListFilters`, `SessionListPage`, `SessionRowWithStatus`, `SessionStats`, `CostByModelRow`. Wire into `ComponentHandles` per the existing `IngestDb` pattern (the actual `ComponentHandles` field lands in E-09).

## 5. Tests

`crates/database/tests/test_session_lifecycle_repo.rs` (new), in-memory SQLite (sibling to LIB-03's schema test file):

- `test_ensure_and_touch_session_upserts` — second call updates `last_activity_at` only.
- `test_ensure_and_touch_session_backfills_dataset_id` — first call had `dataset_id=None`; second call provides one; row gets the dataset_id.
- `test_accumulate_usage_increments` — concurrent writes converge.
- `test_list_session_rows_pagination` — `total`, `has_more`, `limit`, `offset`.
- `test_list_session_rows_status_filter_with_abandoned` — running row past threshold matches `status_filter="abandoned"` purely via the read-time SQL expression (no row mutated).
- `test_list_session_rows_visibility` — caller's own + permitted_dataset OR'd together.
- `test_aggregate_stats_buckets` — completed/failed/abandoned/running counts, `success_rate`, durations.
- `test_cost_by_model_groups_correctly` — mixed-model session attributes per row, `COUNT(DISTINCT session_id)` not raw row count.

## 6. Acceptance criteria

- [x] `SessionLifecycleDb` trait defined with all 6 methods.
- [x] All 6 methods implemented on `DatabaseConnection` with row-for-row Python parity.
- [x] Effective-status logic reports `"abandoned"` purely at read time (no rows mutated).
- [x] All 8 new tests pass under `cargo test -p cognee-database --features sqlite --test test_session_lifecycle_repo`.
- [x] No regression in LIB-03's schema tests or any other previously-passing test.
- [x] `scripts/check_all.sh` clean (Rust + C API + Python; pre-existing JS jest `node:path` issue safe to ignore per IMPLEMENTATION-PROMPT.md §0).

## 7. References

- [Python `metrics.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/metrics.py)
- [LIB-03 — schema + entities + migration](lib-03-session-records-schema.md) (prerequisite)
- [E-09](e-09-sessions-list.md), [E-10](e-10-sessions-stats.md), [E-11](e-11-sessions-cost-by-model.md), [E-12](e-12-sessions-detail.md) — consumers
