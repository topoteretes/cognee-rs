//! Repository implementation for `SessionLifecycleDb` (LIB-05).
//!
//! Line-for-line port of Python's
//! `cognee/modules/session_lifecycle/metrics.py` plus the three
//! aggregate queries that live inline in
//! `cognee/api/v1/sessions/routers/get_sessions_router.py` (stats /
//! cost-by-model). The trait, public domain types, and effective-status
//! semantics live in `traits::session_lifecycle_db`.
//!
//! Implementation choices:
//!   * UUIDs persist as 32-char hex (per LIB-03 / `uuid_hex.rs`); the
//!     repository converts at the boundary so the trait surface is plain
//!     `Uuid`.
//!   * `ensure_and_touch_session` and the per-model upsert in
//!     `accumulate_usage` use raw SQL `INSERT ... ON CONFLICT DO UPDATE`
//!     via `Statement::from_sql_and_values` to express the COALESCE
//!     dataset-backfill and the `WHERE status = 'running'` clause on the
//!     update — neither of which `sea_orm::sea_query::OnConflict`
//!     surfaces portably. The dialect is SQLite/Postgres-shared syntax;
//!     branching on `get_database_backend` selects the right backend
//!     marker.
//!   * The effective-status helper computes `now - threshold` in Rust
//!     and binds it as a parameter (mirrors Python at
//!     `metrics.py:281-282`), so no SQL function for elapsed time is
//!     needed and the expression is portable across SQLite / Postgres.
//!   * Duration aggregation pulls `(started_at, ended_at,
//!     last_activity_at)` rows and folds in Rust — Python does the same
//!     fallback at `get_sessions_router.py:148-158` because SQLite has
//!     no `EXTRACT(epoch ...)`.

use std::env;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait,
    FromQueryResult, QueryFilter, Statement, Value,
};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::entities::session_record;
use crate::traits::{
    CostByModelRow, SessionLifecycleDb, SessionListFilters, SessionListPage, SessionRowWithStatus,
    SessionStats,
};
use crate::types::DatabaseError;
use crate::uuid_hex;

/// Read the abandonment threshold (seconds) from the environment.
/// Default `1800` (30 min) — Decision 12. Mirrors Python's
/// `_abandon_after_seconds` at `metrics.py:47-52`: a non-numeric or
/// empty value falls through to the default.
pub fn abandon_after_seconds() -> i64 {
    env::var("SESSION_ABANDON_AFTER_SECONDS")
        .ok()
        .and_then(|s| {
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                trimmed.parse::<i64>().ok()
            }
        })
        .unwrap_or(1800)
}

/// Compute the `<` cutoff for `last_activity_at` that flips a running
/// row to `abandoned`. Centralized so callers (effective-status
/// expression in raw SQL, status-bucket query, list query) all use the
/// same wall-clock snapshot.
fn abandon_threshold_ts() -> DateTime<Utc> {
    Utc::now() - Duration::seconds(abandon_after_seconds())
}

/// Render the `effective_status` SQL fragment used inside SELECT lists.
/// Returns the literal SQL plus the bound parameter (the cutoff
/// timestamp) so callers can splice it into larger statements.
///
/// Matches Python's `get_effective_status_sql` at
/// `cognee/modules/session_lifecycle/metrics.py:271-292`.
fn effective_status_sql_fragment(threshold: DateTime<Utc>) -> (String, Value) {
    // Both SQLite (sqlx-sqlite) and Postgres (sqlx-postgres) support
    // CASE WHEN ... THEN ... ELSE ... END; the bound timestamp is
    // dialect-portable via the `Value` enum.
    let sql =
        "CASE WHEN status = 'running' AND last_activity_at < ? THEN 'abandoned' ELSE status END"
            .to_string();
    (sql, threshold.into())
}

// ---------------------------------------------------------------------------
// ensure_and_touch_session
// ---------------------------------------------------------------------------

pub async fn ensure_and_touch_session(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Uuid,
    dataset_id: Option<Uuid>,
) -> Result<(), DatabaseError> {
    let now = Utc::now();
    let backend = db.get_database_backend();

    let user_hex = uuid_hex::to_hex(user_id);
    let dataset_hex = uuid_hex::to_hex_opt(dataset_id);

    // SQLite & Postgres share `INSERT ... ON CONFLICT(...) DO UPDATE
    // SET ... WHERE ...` syntax. SeaORM's `OnConflict` doesn't expose
    // the WHERE clause on the update or COALESCE-style backfill, so we
    // hand-roll the SQL.
    //
    // The COALESCE on dataset_id mirrors Python's `case(...)` at
    // `metrics.py:100-103`: if the existing row's dataset_id is NULL,
    // adopt the new value; otherwise keep the existing one.
    let sql = match backend {
        DatabaseBackend::Sqlite | DatabaseBackend::Postgres => {
            "INSERT INTO session_records (\
                session_id, user_id, dataset_id, status, started_at, \
                last_activity_at, ended_at, tokens_in, tokens_out, \
                cost_usd, error_count, last_model\
             ) VALUES ($1, $2, $3, 'running', $4, $4, NULL, 0, 0, 0.0, 0, NULL)\
             ON CONFLICT (session_id, user_id) DO UPDATE SET \
                last_activity_at = $4, \
                dataset_id = COALESCE(session_records.dataset_id, $3) \
             WHERE session_records.status = 'running'"
        }
        DatabaseBackend::MySql => {
            return Err(DatabaseError::QueryError(
                "ensure_and_touch_session: MySQL backend not supported".to_string(),
            ));
        }
    };

    db.execute(Statement::from_sql_and_values(
        backend,
        sql,
        [
            session_id.into(),
            user_hex.into(),
            Value::from(dataset_hex),
            now.into(),
        ],
    ))
    .await
    .map_err(map_sea_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// accumulate_usage
// ---------------------------------------------------------------------------

// Argument list mirrors Python's `accumulate_usage` keyword arguments at
// `cognee/modules/session_lifecycle/metrics.py:133-141` for line-for-line
// parity; introducing a struct just to silence clippy would diverge from
// the reference shape without adding value.
#[allow(clippy::too_many_arguments)]
pub async fn accumulate_usage(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Uuid,
    model: Option<&str>,
    tokens_in: i64,
    tokens_out: i64,
    cost_usd: f64,
    errored: bool,
) -> Result<(), DatabaseError> {
    // Skip the no-op shortcut from Python `metrics.py:150-151`: nothing
    // to credit, no error to count, no model to remember.
    if tokens_in == 0 && tokens_out == 0 && cost_usd == 0.0 && !errored && model.is_none() {
        return Ok(());
    }

    let backend = db.get_database_backend();
    let user_hex = uuid_hex::to_hex(user_id);

    // Step 1: gated UPDATE on session_records. We build the SET clause
    // dynamically because Python only includes columns that change.
    //
    // The `WHERE status = 'running'` gate keeps terminal sessions
    // frozen so a late straggler can't resurrect or distort them
    // (Python `metrics.py:172-181`).
    let mut set_parts: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    let mut next_idx: usize = 1;

    let push_inc = |col: &str,
                    delta: Value,
                    set_parts: &mut Vec<String>,
                    params: &mut Vec<Value>,
                    next_idx: &mut usize| {
        set_parts.push(format!("{col} = {col} + ${next_idx}"));
        params.push(delta);
        *next_idx += 1;
    };

    if tokens_in != 0 {
        // session_records.tokens_in is INTEGER (i32 in SeaORM model);
        // i64 deltas overflow at ~2.1B which we treat as caller error.
        let v = i32::try_from(tokens_in).map_err(|_| {
            DatabaseError::QueryError("accumulate_usage: tokens_in delta overflows i32".to_string())
        })?;
        push_inc(
            "tokens_in",
            Value::from(v),
            &mut set_parts,
            &mut params,
            &mut next_idx,
        );
    }
    if tokens_out != 0 {
        let v = i32::try_from(tokens_out).map_err(|_| {
            DatabaseError::QueryError(
                "accumulate_usage: tokens_out delta overflows i32".to_string(),
            )
        })?;
        push_inc(
            "tokens_out",
            Value::from(v),
            &mut set_parts,
            &mut params,
            &mut next_idx,
        );
    }
    if cost_usd != 0.0 {
        push_inc(
            "cost_usd",
            Value::from(cost_usd),
            &mut set_parts,
            &mut params,
            &mut next_idx,
        );
    }
    if errored {
        set_parts.push(format!("error_count = error_count + ${next_idx}"));
        params.push(Value::from(1_i32));
        next_idx += 1;
    }
    if let Some(m) = model {
        set_parts.push(format!("last_model = ${next_idx}"));
        params.push(Value::from(m.to_string()));
        next_idx += 1;
    }

    if !set_parts.is_empty() {
        // Append WHERE bindings.
        let where_session_idx = next_idx;
        params.push(Value::from(session_id.to_string()));
        next_idx += 1;
        let where_user_idx = next_idx;
        params.push(Value::from(user_hex.clone()));
        next_idx += 1;

        let sql = format!(
            "UPDATE session_records SET {set_clause} \
             WHERE session_id = ${sid} AND user_id = ${uid} AND status = 'running'",
            set_clause = set_parts.join(", "),
            sid = where_session_idx,
            uid = where_user_idx,
        );
        let _ = next_idx;

        db.execute(Statement::from_sql_and_values(backend, sql, params))
            .await
            .map_err(map_sea_err)?;
    }

    // Step 2: per-model upsert. Only when there's actual usage to
    // credit (Python `metrics.py:184`). Errored-only or model-only
    // calls don't touch session_model_usage.
    if let Some(m) = model
        && (tokens_in != 0 || tokens_out != 0 || cost_usd != 0.0)
    {
        let now = Utc::now();
        let ti = i32::try_from(tokens_in).map_err(|_| {
            DatabaseError::QueryError("accumulate_usage: tokens_in delta overflows i32".to_string())
        })?;
        let to = i32::try_from(tokens_out).map_err(|_| {
            DatabaseError::QueryError(
                "accumulate_usage: tokens_out delta overflows i32".to_string(),
            )
        })?;

        let sql = match backend {
            DatabaseBackend::Sqlite | DatabaseBackend::Postgres => {
                "INSERT INTO session_model_usage (\
                    session_id, user_id, model, tokens_in, tokens_out, cost_usd, updated_at\
                 ) VALUES ($1, $2, $3, $4, $5, $6, $7)\
                 ON CONFLICT (session_id, user_id, model) DO UPDATE SET \
                    tokens_in = session_model_usage.tokens_in + $4, \
                    tokens_out = session_model_usage.tokens_out + $5, \
                    cost_usd = session_model_usage.cost_usd + $6, \
                    updated_at = $7"
            }
            DatabaseBackend::MySql => {
                return Err(DatabaseError::QueryError(
                    "accumulate_usage: MySQL backend not supported".to_string(),
                ));
            }
        };

        db.execute(Statement::from_sql_and_values(
            backend,
            sql,
            [
                Value::from(session_id.to_string()),
                Value::from(user_hex.clone()),
                Value::from(m.to_string()),
                Value::from(ti),
                Value::from(to),
                Value::from(cost_usd),
                Value::from(now),
            ],
        ))
        .await
        .map_err(map_sea_err)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// get_session_row
// ---------------------------------------------------------------------------

pub async fn get_session_row(
    db: &DatabaseConnection,
    session_id: &str,
    user_id: Uuid,
    permitted_dataset_ids: &[Uuid],
    prefer_other_owner: bool,
) -> Result<Option<SessionRowWithStatus>, DatabaseError> {
    let user_hex = uuid_hex::to_hex(user_id);

    // Visibility: caller's own OR session's dataset in permitted set.
    // Mirrors Python `metrics.py:315-324`.
    let mut query =
        session_record::Entity::find().filter(session_record::Column::SessionId.eq(session_id));

    if permitted_dataset_ids.is_empty() {
        query = query.filter(session_record::Column::UserId.eq(user_hex.clone()));
    } else {
        let permitted_hex: Vec<String> = permitted_dataset_ids
            .iter()
            .map(|u| uuid_hex::to_hex(*u))
            .collect();
        // user_id == :u OR dataset_id IN :permitted
        let cond = sea_orm::Condition::any()
            .add(session_record::Column::UserId.eq(user_hex.clone()))
            .add(session_record::Column::DatasetId.is_in(permitted_hex));
        query = query.filter(cond);
    }

    let rows = query.all(db).await.map_err(map_sea_err)?;
    if rows.is_empty() {
        return Ok(None);
    }

    // prefer_other_owner: when multiple rows match the visibility OR,
    // return one whose owner is NOT the caller. Python `metrics.py:329-332`.
    let chosen = if prefer_other_owner {
        rows.iter()
            .find(|r| r.user_id != user_hex)
            .cloned()
            .unwrap_or_else(|| rows[0].clone())
    } else {
        rows[0].clone()
    };

    let threshold = abandon_threshold_ts();
    let effective = compute_effective_status(&chosen, threshold);
    Ok(Some(SessionRowWithStatus {
        record: chosen,
        effective_status: effective,
    }))
}

/// Python `get_effective_status_sql` evaluated in Rust for a single row.
fn compute_effective_status(row: &session_record::Model, threshold: DateTime<Utc>) -> String {
    if row.status == "running" && row.last_activity_at < threshold {
        "abandoned".to_string()
    } else {
        row.status.clone()
    }
}

// ---------------------------------------------------------------------------
// list_session_rows
// ---------------------------------------------------------------------------

/// Map an `order_by` string to a real column. Anything unrecognized
/// falls back to `last_activity_at` (Python `metrics.py:415-423`).
fn sortable_column(order_by: &str) -> &'static str {
    match order_by {
        "started_at" => "started_at",
        "ended_at" => "ended_at",
        "cost_usd" => "cost_usd",
        "tokens_in" => "tokens_in",
        "tokens_out" => "tokens_out",
        // "last_activity_at" or anything else
        _ => "last_activity_at",
    }
}

#[derive(Debug, FromQueryResult)]
struct ListRow {
    session_id: String,
    user_id: String,
    dataset_id: Option<String>,
    status: String,
    started_at: DateTime<Utc>,
    last_activity_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    tokens_in: i32,
    tokens_out: i32,
    cost_usd: f64,
    error_count: i32,
    last_model: Option<String>,
    effective_status: String,
}

#[derive(Debug, FromQueryResult)]
struct CountRow {
    n: i64,
}

pub async fn list_session_rows(
    db: &DatabaseConnection,
    filters: SessionListFilters,
) -> Result<SessionListPage, DatabaseError> {
    let backend = db.get_database_backend();
    let threshold = abandon_threshold_ts();
    let (eff_sql, eff_param) = effective_status_sql_fragment(threshold);
    let user_hex = uuid_hex::to_hex(filters.user_id);

    // ---- Build WHERE clause ----------------------------------------------
    // We track parameter index so we can splice the effective-status
    // fragment plus user-supplied filter values in order.
    //
    // The `eff_param` is bound *only* when `status_filter` is set; the
    // SELECT list always references it though, so when listing without
    // a status filter we still need the bound value at SELECT time.
    let mut where_parts: Vec<String> = Vec::new();
    let mut where_params: Vec<Value> = Vec::new();

    // visibility predicate
    if filters.permitted_dataset_ids.is_empty() {
        where_parts.push("user_id = ?".to_string());
        where_params.push(Value::from(user_hex.clone()));
    } else {
        let mut placeholders = Vec::with_capacity(filters.permitted_dataset_ids.len());
        let mut perm_params: Vec<Value> = Vec::with_capacity(filters.permitted_dataset_ids.len());
        for ds in &filters.permitted_dataset_ids {
            placeholders.push("?");
            perm_params.push(Value::from(uuid_hex::to_hex(*ds)));
        }
        where_parts.push(format!(
            "(user_id = ? OR dataset_id IN ({}))",
            placeholders.join(", ")
        ));
        where_params.push(Value::from(user_hex.clone()));
        where_params.extend(perm_params);
    }

    if let Some(since) = filters.since {
        where_parts.push("last_activity_at >= ?".to_string());
        where_params.push(Value::from(since));
    }

    if let Some(ref status_filter) = filters.status_filter {
        // The effective-status SQL fragment binds the threshold timestamp.
        where_parts.push(format!("({eff_sql}) = ?"));
        where_params.push(eff_param.clone());
        where_params.push(Value::from(status_filter.clone()));
    }

    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_parts.join(" AND "))
    };

    // ---- Count query -----------------------------------------------------
    let count_sql = format!("SELECT COUNT(*) AS n FROM session_records {where_clause}");
    let count_row = CountRow::find_by_statement(Statement::from_sql_and_values(
        backend,
        &count_sql,
        where_params.clone(),
    ))
    .one(db)
    .await
    .map_err(map_sea_err)?;
    let total = count_row.map(|r| r.n).unwrap_or(0);

    // ---- Page query ------------------------------------------------------
    let sort_col = sortable_column(&filters.order_by);
    let direction = if filters.descending { "DESC" } else { "ASC" };

    // SELECT must always bind the effective-status threshold. Build
    // params in the order: SELECT params, WHERE params, LIMIT/OFFSET.
    let mut page_params: Vec<Value> = Vec::with_capacity(where_params.len() + 3);
    page_params.push(eff_param.clone()); // for the SELECT list expression
    page_params.extend(where_params);

    let page_sql = format!(
        "SELECT session_id, user_id, dataset_id, status, started_at, \
                last_activity_at, ended_at, tokens_in, tokens_out, cost_usd, \
                error_count, last_model, ({eff_sql}) AS effective_status \
         FROM session_records {where_clause} \
         ORDER BY {sort_col} {direction} \
         LIMIT ? OFFSET ?"
    );
    page_params.push(Value::from(i64::from(filters.limit)));
    page_params.push(Value::from(i64::from(filters.offset)));

    let raw_rows = ListRow::find_by_statement(Statement::from_sql_and_values(
        backend,
        &page_sql,
        page_params,
    ))
    .all(db)
    .await
    .map_err(map_sea_err)?;

    let sessions: Vec<SessionRowWithStatus> = raw_rows
        .into_iter()
        .map(|r| SessionRowWithStatus {
            record: session_record::Model {
                session_id: r.session_id,
                user_id: r.user_id,
                dataset_id: r.dataset_id,
                status: r.status,
                started_at: r.started_at,
                last_activity_at: r.last_activity_at,
                ended_at: r.ended_at,
                tokens_in: r.tokens_in,
                tokens_out: r.tokens_out,
                cost_usd: r.cost_usd,
                error_count: r.error_count,
                last_model: r.last_model,
            },
            effective_status: r.effective_status,
        })
        .collect();

    Ok(SessionListPage {
        sessions,
        total,
        limit: filters.limit,
        offset: filters.offset,
    })
}

// ---------------------------------------------------------------------------
// aggregate_stats
// ---------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct TotalsRow {
    sessions: i64,
    tokens_in: i64,
    tokens_out: i64,
    cost_usd: f64,
}

#[derive(Debug, FromQueryResult)]
struct DurRow {
    started_at: Option<DateTime<Utc>>,
    last_activity_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, FromQueryResult)]
struct StatusBucketRow {
    s: String,
    c: i64,
}

pub async fn aggregate_stats(
    db: &DatabaseConnection,
    user_id: Uuid,
    permitted_dataset_ids: &[Uuid],
    since: Option<DateTime<Utc>>,
) -> Result<SessionStats, DatabaseError> {
    let backend = db.get_database_backend();
    let user_hex = uuid_hex::to_hex(user_id);

    // Shared visibility / since predicate. The same WHERE clause is
    // reused across totals / duration / status-bucket queries — Python
    // builds it once at `get_sessions_router.py:124-131` and reuses.
    let mut where_parts: Vec<String> = Vec::new();
    let mut base_params: Vec<Value> = Vec::new();

    if permitted_dataset_ids.is_empty() {
        where_parts.push("user_id = ?".to_string());
        base_params.push(Value::from(user_hex.clone()));
    } else {
        let mut placeholders = Vec::with_capacity(permitted_dataset_ids.len());
        let mut perm_params: Vec<Value> = Vec::with_capacity(permitted_dataset_ids.len());
        for ds in permitted_dataset_ids {
            placeholders.push("?");
            perm_params.push(Value::from(uuid_hex::to_hex(*ds)));
        }
        where_parts.push(format!(
            "(user_id = ? OR dataset_id IN ({}))",
            placeholders.join(", ")
        ));
        base_params.push(Value::from(user_hex.clone()));
        base_params.extend(perm_params);
    }
    if let Some(s) = since {
        where_parts.push("last_activity_at >= ?".to_string());
        base_params.push(Value::from(s));
    }
    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_parts.join(" AND "))
    };

    // ---- (a) Totals ------------------------------------------------------
    let totals_sql = format!(
        "SELECT COUNT(*) AS sessions, \
                COALESCE(SUM(tokens_in), 0) AS tokens_in, \
                COALESCE(SUM(tokens_out), 0) AS tokens_out, \
                COALESCE(SUM(cost_usd), 0.0) AS cost_usd \
         FROM session_records {where_clause}"
    );
    let totals = TotalsRow::find_by_statement(Statement::from_sql_and_values(
        backend,
        &totals_sql,
        base_params.clone(),
    ))
    .one(db)
    .await
    .map_err(map_sea_err)?
    .unwrap_or(TotalsRow {
        sessions: 0,
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: 0.0,
    });

    // ---- (b) Duration ---------------------------------------------------
    // SQLite has no `EXTRACT(epoch FROM ...)`. Python falls back to
    // loading `(started, ended, last_activity)` rows and folding in
    // Python (`get_sessions_router.py:142-159`); we mirror that
    // exactly for cross-backend portability.
    let dur_sql = format!(
        "SELECT started_at, last_activity_at, ended_at \
         FROM session_records {where_clause}"
    );
    let dur_rows = DurRow::find_by_statement(Statement::from_sql_and_values(
        backend,
        &dur_sql,
        base_params.clone(),
    ))
    .all(db)
    .await
    .map_err(map_sea_err)?;

    let mut total_seconds: f64 = 0.0;
    let mut session_count: i64 = 0;
    for row in &dur_rows {
        let Some(started) = row.started_at else {
            continue;
        };
        let end = row.ended_at.or(row.last_activity_at);
        let Some(end) = end else { continue };
        let delta = (end - started).num_milliseconds() as f64 / 1000.0;
        total_seconds += delta.max(0.0);
        session_count += 1;
    }
    let avg_seconds = if session_count > 0 {
        total_seconds / session_count as f64
    } else {
        0.0
    };

    // ---- (c) Status buckets ---------------------------------------------
    let threshold = abandon_threshold_ts();
    let (eff_sql, eff_param) = effective_status_sql_fragment(threshold);
    // SELECT params: eff_param first, then base_params (where).
    let mut bucket_params: Vec<Value> = Vec::with_capacity(base_params.len() + 1);
    bucket_params.push(eff_param);
    bucket_params.extend(base_params.clone());

    let bucket_sql = format!(
        "SELECT ({eff_sql}) AS s, COUNT(*) AS c \
         FROM session_records {where_clause} \
         GROUP BY s"
    );
    let buckets = StatusBucketRow::find_by_statement(Statement::from_sql_and_values(
        backend,
        &bucket_sql,
        bucket_params,
    ))
    .all(db)
    .await
    .map_err(map_sea_err)?;

    let mut completed: i64 = 0;
    let mut failed: i64 = 0;
    let mut abandoned: i64 = 0;
    let mut running: i64 = 0;
    for b in &buckets {
        match b.s.as_str() {
            "completed" => completed = b.c,
            "failed" => failed = b.c,
            "abandoned" => abandoned = b.c,
            "running" => running = b.c,
            _ => {}
        }
    }
    let decided = completed + failed + abandoned;
    let success_rate = if decided > 0 {
        completed as f64 / decided as f64
    } else {
        1.0
    };

    let sessions_count = totals.sessions;
    let avg_spend = if sessions_count > 0 {
        totals.cost_usd / sessions_count as f64
    } else {
        0.0
    };

    Ok(SessionStats {
        sessions: sessions_count,
        total_spend_usd: totals.cost_usd,
        avg_spend_per_session_usd: avg_spend,
        tokens_in: totals.tokens_in,
        tokens_out: totals.tokens_out,
        tokens_total: totals.tokens_in + totals.tokens_out,
        agent_time_s: total_seconds,
        avg_session_s: avg_seconds,
        success_rate,
        completed,
        failed,
        abandoned,
        running,
    })
}

// ---------------------------------------------------------------------------
// cost_by_model
// ---------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct CostRow {
    model: Option<String>,
    session_count: i64,
    cost_usd: f64,
    tokens_in: i64,
    tokens_out: i64,
}

pub async fn cost_by_model(
    db: &DatabaseConnection,
    user_id: Uuid,
    permitted_dataset_ids: &[Uuid],
    since: Option<DateTime<Utc>>,
) -> Result<Vec<CostByModelRow>, DatabaseError> {
    let backend = db.get_database_backend();
    let user_hex = uuid_hex::to_hex(user_id);

    let mut where_parts: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if permitted_dataset_ids.is_empty() {
        where_parts.push("sr.user_id = ?".to_string());
        params.push(Value::from(user_hex.clone()));
    } else {
        let mut placeholders = Vec::with_capacity(permitted_dataset_ids.len());
        let mut perm_params: Vec<Value> = Vec::with_capacity(permitted_dataset_ids.len());
        for ds in permitted_dataset_ids {
            placeholders.push("?");
            perm_params.push(Value::from(uuid_hex::to_hex(*ds)));
        }
        where_parts.push(format!(
            "(sr.user_id = ? OR sr.dataset_id IN ({}))",
            placeholders.join(", ")
        ));
        params.push(Value::from(user_hex.clone()));
        params.extend(perm_params);
    }
    if let Some(s) = since {
        where_parts.push("sr.last_activity_at >= ?".to_string());
        params.push(Value::from(s));
    }
    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_parts.join(" AND "))
    };

    // COUNT(DISTINCT smu.session_id) — not raw row count — matches
    // `get_sessions_router.py:220`. ORDER BY total cost descending.
    let sql = format!(
        "SELECT smu.model AS model, \
                COUNT(DISTINCT smu.session_id) AS session_count, \
                COALESCE(SUM(smu.cost_usd), 0.0) AS cost_usd, \
                COALESCE(SUM(smu.tokens_in), 0) AS tokens_in, \
                COALESCE(SUM(smu.tokens_out), 0) AS tokens_out \
         FROM session_model_usage smu \
         JOIN session_records sr ON smu.session_id = sr.session_id \
                                 AND smu.user_id = sr.user_id \
         {where_clause} \
         GROUP BY smu.model \
         ORDER BY SUM(smu.cost_usd) DESC"
    );

    let rows = CostRow::find_by_statement(Statement::from_sql_and_values(backend, &sql, params))
        .all(db)
        .await
        .map_err(map_sea_err)?;

    Ok(rows
        .into_iter()
        .map(|r| CostByModelRow {
            model: r.model.unwrap_or_else(|| "unknown".to_string()),
            session_count: r.session_count,
            cost_usd: r.cost_usd,
            tokens_in: r.tokens_in,
            tokens_out: r.tokens_out,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Trait impl on DatabaseConnection
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl SessionLifecycleDb for DatabaseConnection {
    async fn ensure_and_touch_session(
        &self,
        session_id: &str,
        user_id: Uuid,
        dataset_id: Option<Uuid>,
    ) -> Result<(), DatabaseError> {
        ensure_and_touch_session(self, session_id, user_id, dataset_id).await
    }

    async fn accumulate_usage(
        &self,
        session_id: &str,
        user_id: Uuid,
        model: Option<&str>,
        tokens_in: i64,
        tokens_out: i64,
        cost_usd: f64,
        errored: bool,
    ) -> Result<(), DatabaseError> {
        accumulate_usage(
            self, session_id, user_id, model, tokens_in, tokens_out, cost_usd, errored,
        )
        .await
    }

    async fn get_session_row(
        &self,
        session_id: &str,
        user_id: Uuid,
        permitted_dataset_ids: &[Uuid],
        prefer_other_owner: bool,
    ) -> Result<Option<SessionRowWithStatus>, DatabaseError> {
        get_session_row(
            self,
            session_id,
            user_id,
            permitted_dataset_ids,
            prefer_other_owner,
        )
        .await
    }

    async fn list_session_rows(
        &self,
        filters: SessionListFilters,
    ) -> Result<SessionListPage, DatabaseError> {
        list_session_rows(self, filters).await
    }

    async fn aggregate_stats(
        &self,
        user_id: Uuid,
        permitted_dataset_ids: &[Uuid],
        since: Option<DateTime<Utc>>,
    ) -> Result<SessionStats, DatabaseError> {
        aggregate_stats(self, user_id, permitted_dataset_ids, since).await
    }

    async fn cost_by_model(
        &self,
        user_id: Uuid,
        permitted_dataset_ids: &[Uuid],
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<CostByModelRow>, DatabaseError> {
        cost_by_model(self, user_id, permitted_dataset_ids, since).await
    }
}
