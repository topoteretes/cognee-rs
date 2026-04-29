# E-11 — `GET /api/v1/sessions/cost-by-model`

| | |
|---|---|
| Wire path | `GET /api/v1/sessions/cost-by-model?range=` |
| Status | **Missing** |
| Depends on | LIB-05 (`SessionLifecycleDb::cost_by_model`) + LIB-03 (`session_model_usage` table). |
| Effort | ~0.5 day (most cost is in LIB-03 + LIB-05). |
| Owner crate | `cognee-http-server` |

## 1. Goal

Per-model cost + token breakdown for the dashboard's "Spend by model" widget. Aggregates `session_model_usage` rows joined back to `session_records` to scope by `last_activity_at >= since`.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `GET /cost-by-model` handler | `cognee/api/v1/sessions/routers/get_sessions_router.py` | 198–253 |

### Response shape

```json
[
  {
    "model": "gpt-4o-mini",
    "session_count": 42,        // distinct session_ids using this model
    "cost_usd": 1.23,
    "tokens_in": 12345,
    "tokens_out": 6789
  },
  ...
]
```

Sorted by `cost_usd` desc. Empty list when no usage rows.

Notes:
- `model` falls back to `"unknown"` when null in the source row (`row.model or "unknown"`).
- Visibility joins through `session_records` so dataset-permission filtering works.
- `session_count` uses `COUNT(DISTINCT session_id)` — not raw row count.

## 3. Current Rust state

No route. No `session_model_usage` table (added in LIB-03) or repository method (added in LIB-05).

## 4. Implementation steps

1. **Handler `cost_by_model`** in `crates/http-server/src/routers/sessions.rs`:
   ```rust
   pub async fn cost_by_model(
       user: AuthenticatedUser,
       State(state): State<AppState>,
       Query(q): Query<CostByModelQuery>,    // { range }
   ) -> Result<Json<Vec<CostByModelDTO>>, ApiError> { ... }
   ```
   - Resolve permitted datasets.
   - Translate range to `Option<DateTime<Utc>>`.
   - Call `state.components.session_lifecycle_db.cost_by_model(user.id, permitted, since)`.

2. **DTO**:
   ```rust
   /// Response DTO — wire is camelCase per Decision 10.
   #[derive(Debug, Serialize, ToSchema)]
   #[serde(rename_all = "camelCase")]
   pub struct CostByModelDTO {
       pub model: String,
       pub session_count: i64,                     // → "sessionCount"
       pub cost_usd: f64,                          // → "costUsd"
       pub tokens_in: i64,                         // → "tokensIn"
       pub tokens_out: i64,                        // → "tokensOut"
   }
   ```

3. **Repository** (lives in LIB-05):
   ```rust
   async fn cost_by_model(...) -> Result<Vec<CostByModelRow>, DbError>;
   ```
   SeaORM SQL skeleton:
   ```sql
   SELECT
       COALESCE(NULLIF(smu.model, ''), 'unknown') AS model,
       COUNT(DISTINCT smu.session_id)             AS session_count,
       COALESCE(SUM(smu.cost_usd), 0)             AS cost_usd,
       COALESCE(SUM(smu.tokens_in), 0)            AS tokens_in,
       COALESCE(SUM(smu.tokens_out), 0)           AS tokens_out
   FROM session_model_usage smu
   JOIN session_records sr ON sr.session_id = smu.session_id AND sr.user_id = smu.user_id
   WHERE (sr.user_id = :user_id OR sr.dataset_id IN :permitted)
     AND (:since IS NULL OR sr.last_activity_at >= :since)
   GROUP BY model
   ORDER BY SUM(smu.cost_usd) DESC;
   ```

4. **Wire** at `/api/v1/sessions/cost-by-model` (router built in E-09).

## 5. Tests

- `crates/http-server/tests/test_sessions_cost_by_model.rs`:
  - `single_model_session_yields_one_row`.
  - `mixed_model_session_splits_correctly` — one session, two models, two rows in the response.
  - `null_model_falls_back_to_unknown`.
  - `range_24h_filters_through_join`.
  - `visibility_through_dataset_permissions`.
- Cross-SDK in `test_http_v2_sessions.py`.

## 6. Acceptance criteria

- [ ] Empty response is `[]` (not `null`).
- [ ] `session_count` uses `COUNT(DISTINCT)`.
- [ ] Sorted by `cost_usd` descending.
- [ ] Null/empty model name → `"unknown"`.
- [ ] Cross-SDK structural diff passes.

## 7. References

- [Python cost_by_model handler](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sessions/routers/get_sessions_router.py#L198)
- [LIB-03 — `session_model_usage` table](lib-03-session-records-schema.md)
- [LIB-05 — `cost_by_model` repository method](lib-05-session-records-repo.md)
