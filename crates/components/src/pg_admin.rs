//! Postgres administration for the Postgres dataset database handlers
//! (`pgvector`, `pgvector_shared`, `postgres_graph`, `postgres_graph_shared`):
//! create and drop a database per dataset, or a schema per dataset in one
//! shared database, and build the connection URLs that reach them.
//!
//! Python parity: `cognee/infrastructure/databases/postgres/admin.py`. The
//! names, the identifier check, the advisory provisioning locks and the
//! maintenance database are the same, so a server provisioned by one SDK is
//! usable by the other.
//!
//! Every helper opens its own pool of exactly one connection and closes it
//! before returning: `CREATE DATABASE` cannot run inside a transaction, and a
//! session advisory lock is only released by the connection that took it, so
//! every statement of one helper must reach the same backend.

use std::time::Duration;

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, DbErr,
    Statement, TransactionTrait,
};
use url::Url;
use uuid::Uuid;

use crate::error::ComponentError;

/// The database the cluster-level statements connect to (`CREATE DATABASE`,
/// `DROP DATABASE`), as in Python's `_MAINTENANCE_DB_NAME`.
pub(crate) const MAINTENANCE_DATABASE: &str = "postgres";

/// Postgres' default port, for a URL that names none.
const DEFAULT_PORT: u16 = 5432;

/// Python's `_POSTGRES_IDENTIFIER_RE`: `^[A-Za-z0-9_][A-Za-z0-9_$-]{0,62}$`.
///
/// Looser than an unquoted identifier, because dataset databases are named
/// after dataset UUIDs (hyphens, leading digits). Every statement quotes the
/// name anyway; the check rejects quotes, whitespace and other metacharacters
/// as defence in depth and enforces the 63-byte identifier limit.
pub(crate) fn validate_database_name(name: &str) -> Result<&str, ComponentError> {
    let mut chars = name.chars();
    let valid = name.len() <= 63
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '-'));
    if valid {
        Ok(name)
    } else {
        Err(ComponentError::Config(format!(
            "invalid Postgres database name {name:?}"
        )))
    }
}

/// A schema name that is also a valid *unquoted* lower-case identifier.
///
/// Stricter than [`validate_database_name`] because the name also goes, bare,
/// into the `search_path` connection option (see [`with_search_path`]), where
/// quoting would have to survive libpq option parsing. Every name this module
/// creates is `ds_<hex>` ([`dataset_schema_name`]), which passes.
pub(crate) fn validate_schema_name(name: &str) -> Result<&str, ComponentError> {
    let mut chars = name.chars();
    let valid = name.len() <= 63
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if valid {
        Ok(name)
    } else {
        Err(ComponentError::Config(format!(
            "invalid Postgres schema name {name:?}"
        )))
    }
}

/// The schema that holds a dataset in shared-database mode: `ds_<uuid hex>`
/// (Python `dataset_schema_name`). The prefix keeps it a valid identifier
/// although a UUID may start with a digit, and keeps it clear of `public` and
/// the relational tables; 35 characters is well under the 63-byte limit.
pub(crate) fn dataset_schema_name(dataset_id: Uuid) -> String {
    format!("ds_{}", dataset_id.simple())
}

/// `name` as a quoted SQL identifier.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Parse a resolved Postgres URL. `what` names the setting in the error; the
/// URL itself never appears in one, because it carries the password.
pub(crate) fn parse_postgres_url(url: &str, what: &str) -> Result<Url, ComponentError> {
    let parsed = Url::parse(url)
        .map_err(|e| ComponentError::Config(format!("{what} is not a valid Postgres URL: {e}")))?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") {
        return Err(ComponentError::Config(format!(
            "{what} must be a Postgres URL (postgres://…), not a {:?} one",
            parsed.scheme()
        )));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(ComponentError::Config(format!(
            "{what} names no Postgres host"
        )));
    }
    Ok(parsed)
}

/// The host and port `url` connects to.
pub(crate) fn host_port(url: &Url) -> (String, u16) {
    (
        url.host_str().unwrap_or_default().to_string(),
        url.port().unwrap_or(DEFAULT_PORT),
    )
}

/// `server` pointed at `database` (and at `host`/`port` when given), keeping
/// its credentials and query parameters (`sslmode`, …).
pub(crate) fn database_url(
    server: &Url,
    database: &str,
    host: Option<&str>,
    port: Option<u16>,
) -> Result<Url, ComponentError> {
    let mut url = server.clone();
    if let Some(host) = host.filter(|h| !h.is_empty()) {
        url.set_host(Some(host))
            .map_err(|e| ComponentError::Config(format!("invalid Postgres host {host:?}: {e}")))?;
    }
    if let Some(port) = port {
        url.set_port(Some(port))
            .map_err(|()| ComponentError::Config(format!("invalid Postgres port {port}")))?;
    }
    url.set_path(&format!("/{database}"));
    Ok(url)
}

/// `url` with its connections' `search_path` pinned to `schemas`, through
/// libpq's `options=-c search_path=…` startup parameter.
///
/// sqlx parses `options` into `PgConnectOptions` and appends the adapters' own
/// `PgConnectOptions::options` pairs to it (`sqlx-postgres` 0.8,
/// `options/parse.rs` and `options/mod.rs`), so the pin survives the session
/// tuning `PgVectorAdapter::new` / `PgGraphAdapter::new` add. Every name must
/// pass [`validate_schema_name`] (or be `public`), because it goes in bare.
pub(crate) fn with_search_path(url: &Url, schemas: &[&str]) -> Result<Url, ComponentError> {
    for schema in schemas {
        if *schema != "public" {
            validate_schema_name(schema)?;
        }
    }
    let mut url = url.clone();
    url.query_pairs_mut()
        .append_pair("options", &format!("-c search_path={}", schemas.join(",")));
    Ok(url)
}

/// The database a URL connects to.
fn database_of(url: &Url) -> &str {
    url.path().trim_start_matches('/')
}

fn sql_error(action: &str, target: &str) -> impl Fn(DbErr) -> ComponentError {
    let context = format!("{action} {target:?}");
    move |e| ComponentError::Database(format!("Postgres {context} failed: {e}"))
}

/// One connection to the database `url` names, as a pool of one (see the
/// module docs). The errors name the database and server, never the URL.
async fn connect(url: &Url) -> Result<DatabaseConnection, ComponentError> {
    let (host, port) = host_port(url);
    let mut options = ConnectOptions::new(url.as_str());
    options
        .min_connections(1)
        .max_connections(1)
        .connect_timeout(Duration::from_secs(30))
        .sqlx_logging(false);
    Database::connect(options).await.map_err(|e| {
        ComponentError::Database(format!(
            "connecting to Postgres database {:?} on {host}:{port} failed: {e}",
            database_of(url)
        ))
    })
}

fn statement(sql: &str, name: &str) -> Statement {
    Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, [name.into()])
}

/// The advisory lock both SDKs serialize provisioning on: a fixed namespace
/// plus the provisioned resource (Python `_ACQUIRE_PROVISIONING_LOCK`).
const ACQUIRE_LOCK: &str = "SELECT pg_advisory_lock(hashtext('cognee_provisioning'), hashtext($1))";
const RELEASE_LOCK: &str =
    "SELECT pg_advisory_unlock(hashtext('cognee_provisioning'), hashtext($1))";
const ACQUIRE_XACT_LOCK: &str =
    "SELECT pg_advisory_xact_lock(hashtext('cognee_provisioning'), hashtext($1))";

/// Create database `name` on `server`'s cluster unless it exists. `true` when
/// this call created it.
///
/// `CREATE DATABASE` has no `IF NOT EXISTS`, so the existence check and the
/// create run under a session advisory lock on the name, on the maintenance
/// database: concurrent first uses of a dataset, in any process, create it
/// once. Postgres drops the lock with the connection if this one dies.
pub(crate) async fn create_database_if_not_exists(
    server: &Url,
    name: &str,
) -> Result<bool, ComponentError> {
    validate_database_name(name)?;
    let admin = database_url(server, MAINTENANCE_DATABASE, None, None)?;
    let db = connect(&admin).await?;
    let failed = sql_error("CREATE DATABASE", name);
    db.execute(statement(ACQUIRE_LOCK, name))
        .await
        .map_err(&failed)?;
    let created = async {
        let exists = db
            .query_one(statement(
                "SELECT 1 AS present FROM pg_database WHERE datname = $1",
                name,
            ))
            .await?
            .is_some();
        if exists {
            return Ok(false);
        }
        db.execute_unprepared(&format!("CREATE DATABASE {}", quote_ident(name)))
            .await?;
        Ok::<_, DbErr>(true)
    }
    .await;
    // Release before reporting: a failed create must not keep the name locked
    // for as long as a pooled connection might live.
    let released = db.execute(statement(RELEASE_LOCK, name)).await;
    let created = created.map_err(&failed)?;
    released.map_err(&failed)?;
    db.close().await.map_err(&failed)?;
    Ok(created)
}

/// Drop database `name` from `server`'s cluster if it exists.
///
/// The caller closes its own pools to the database first (`DatasetStorage`
/// evicts a dataset's engines before its handlers run); sessions another
/// process still holds are terminated (`WITH (FORCE)`), as Python's handler
/// terminates them before dropping. A dataset being deleted is not one anyone
/// may keep using.
///
/// Needs Postgres 13 or later (`DROP DATABASE … WITH (FORCE)`).
///
/// The drop holds the session advisory lock [`create_database_if_not_exists`]
/// takes, so it never interleaves with another process's existence check and
/// `CREATE DATABASE` of the same name: a concurrent first bind either finishes
/// creating before the drop, or creates afresh after it.
pub(crate) async fn drop_database_if_exists(
    server: &Url,
    name: &str,
) -> Result<(), ComponentError> {
    validate_database_name(name)?;
    let admin = database_url(server, MAINTENANCE_DATABASE, None, None)?;
    let db = connect(&admin).await?;
    let failed = sql_error("DROP DATABASE", name);
    db.execute(statement(ACQUIRE_LOCK, name))
        .await
        .map_err(&failed)?;
    // `WITH (FORCE)` (Postgres 13+) terminates the sessions still connected:
    // another process's engine pool would otherwise fail the drop with "being
    // accessed by other users". Python terminates them with
    // `pg_terminate_backend` before dropping, to the same effect.
    let dropped = db
        .execute_unprepared(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            quote_ident(name)
        ))
        .await;
    // Release before reporting, as in `create_database_if_not_exists`.
    let released = db.execute(statement(RELEASE_LOCK, name)).await;
    dropped.map_err(&failed)?;
    released.map_err(&failed)?;
    db.close().await.map_err(&failed)
}

/// Inside the database `database` connects to, create `schema` (when given)
/// and the pgvector extension (when asked), in one transaction.
///
/// `IF NOT EXISTS` is not race-free in Postgres (two transactions that both
/// pass the check collide on the catalog's unique index), so creators take a
/// transaction advisory lock on the database name, which covers the extension
/// too (Python `create_pg_schema_if_not_exists`). The connection keeps the
/// server's default `search_path`, so the extension lands in `public`, where a
/// schema-pinned adapter's `search_path = <schema>, public` finds its types.
async fn provision_in_database(
    database: &Url,
    schema: Option<&str>,
    with_vector_extension: bool,
) -> Result<(), ComponentError> {
    if let Some(schema) = schema {
        validate_schema_name(schema)?;
    }
    let name = database_of(database).to_string();
    let failed = sql_error("provisioning in", &name);
    let db = connect(database).await?;
    let txn = db.begin().await.map_err(&failed)?;
    txn.execute(statement(ACQUIRE_XACT_LOCK, &name))
        .await
        .map_err(&failed)?;
    if with_vector_extension {
        txn.execute_unprepared("CREATE EXTENSION IF NOT EXISTS vector")
            .await
            .map_err(&failed)?;
    }
    if let Some(schema) = schema {
        txn.execute_unprepared(&format!(
            "CREATE SCHEMA IF NOT EXISTS {}",
            quote_ident(schema)
        ))
        .await
        .map_err(&failed)?;
    }
    txn.commit().await.map_err(&failed)?;
    db.close().await.map_err(&failed)
}

/// Ensure the pgvector extension in the database `database` connects to.
pub(crate) async fn ensure_vector_extension(database: &Url) -> Result<(), ComponentError> {
    provision_in_database(database, None, true).await
}

/// Create `schema` in the database `database` connects to unless it exists,
/// and the pgvector extension with it when `with_vector_extension`.
pub(crate) async fn create_schema_if_not_exists(
    database: &Url,
    schema: &str,
    with_vector_extension: bool,
) -> Result<(), ComponentError> {
    provision_in_database(database, Some(schema), with_vector_extension).await
}

/// Drop `schema` and everything in it from the database `database` connects
/// to: one statement, so a dataset's tables go together (Python
/// `drop_pg_schema_if_exists`).
///
/// Runs in a transaction holding the advisory lock schema creation takes
/// ([`provision_in_database`], keyed on the database name), so a concurrent
/// first bind's `CREATE SCHEMA` lands either before the drop or after it,
/// never half way through.
pub(crate) async fn drop_schema_if_exists(
    database: &Url,
    schema: &str,
) -> Result<(), ComponentError> {
    validate_schema_name(schema)?;
    let name = database_of(database).to_string();
    let failed = sql_error("DROP SCHEMA", schema);
    let db = connect(database).await?;
    let txn = db.begin().await.map_err(&failed)?;
    txn.execute(statement(ACQUIRE_XACT_LOCK, &name))
        .await
        .map_err(&failed)?;
    txn.execute_unprepared(&format!(
        "DROP SCHEMA IF EXISTS {} CASCADE",
        quote_ident(schema)
    ))
    .await
    .map_err(&failed)?;
    txn.commit().await.map_err(&failed)?;
    db.close().await.map_err(&failed)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn database_names_follow_pythons_identifier_rule() {
        let id = Uuid::new_v4().to_string();
        assert!(
            validate_database_name(&id).is_ok(),
            "a dataset id is a valid name"
        );
        assert!(validate_database_name("cognee_db").is_ok());
        assert!(validate_database_name(&"a".repeat(63)).is_ok());
        for bad in [
            "",
            "-leading-dash",
            "with space",
            "quo\"te",
            "semi;colon",
            &"a".repeat(64),
        ] {
            assert!(
                validate_database_name(bad).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn schema_names_are_bare_lowercase_identifiers() {
        let schema = dataset_schema_name(Uuid::new_v4());
        assert!(validate_schema_name(&schema).is_ok());
        for bad in ["", "Ds_x", "1ds", "ds-x", "ds x", "ds\"x", "public,evil"] {
            assert!(
                validate_schema_name(bad).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn the_dataset_schema_is_ds_and_the_uuid_hex() {
        let id = Uuid::parse_str("8a3c2a3e-1f0b-4c4e-9d5e-0123456789ab").unwrap();
        assert_eq!(
            dataset_schema_name(id),
            "ds_8a3c2a3e1f0b4c4e9d5e0123456789ab"
        );
    }

    #[test]
    fn identifiers_are_quoted() {
        assert_eq!(quote_ident("8a3c-x"), "\"8a3c-x\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn a_database_url_keeps_credentials_and_query() {
        let server =
            parse_postgres_url("postgres://u%40x:p%2Fw@db:6543/cognee?sslmode=require", "t")
                .unwrap();
        let url = database_url(&server, "abc-def", None, None).unwrap();
        assert_eq!(
            url.as_str(),
            "postgres://u%40x:p%2Fw@db:6543/abc-def?sslmode=require"
        );
        let moved = database_url(&server, "postgres", Some("other"), Some(5433)).unwrap();
        assert_eq!(moved.host_str(), Some("other"));
        assert_eq!(moved.port(), Some(5433));
        assert_eq!(moved.password(), Some("p%2Fw"));
        assert_eq!(
            host_port(&parse_postgres_url("postgres://h/db", "t").unwrap()),
            ("h".to_string(), 5432)
        );
    }

    #[test]
    fn non_postgres_urls_are_refused_without_echoing_them() {
        let error = parse_postgres_url("sqlite://secret:pw@x/db", "relational_db_url")
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("relational_db_url"), "{error}");
        assert!(!error.contains("pw"), "{error}");
        assert!(parse_postgres_url("not a url", "x").is_err());
    }

    #[test]
    fn the_search_path_rides_in_the_options_parameter() {
        let server =
            parse_postgres_url("postgres://u:p@h:5432/cognee?sslmode=disable", "t").unwrap();
        let schema = dataset_schema_name(Uuid::nil());
        let url = with_search_path(&server, &[&schema, "public"]).unwrap();
        let options: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            options,
            [
                ("sslmode".to_string(), "disable".to_string()),
                (
                    "options".to_string(),
                    format!("-c search_path={schema},public")
                ),
            ]
        );
        assert!(with_search_path(&server, &["evil,public"]).is_err());
    }

    /// sqlx reads the pin from the URL, and the adapters' own
    /// `PgConnectOptions::options` calls append to it rather than replace it.
    #[test]
    fn sqlx_keeps_the_pin_next_to_the_adapters_session_options() {
        use sea_orm::sqlx::postgres::PgConnectOptions;
        let server = parse_postgres_url("postgres://u:p@h:5432/cognee", "t").unwrap();
        let url = with_search_path(&server, &["ds_abc", "public"]).unwrap();
        let options: PgConnectOptions = url.as_str().parse().unwrap();
        let options = options.options([("plan_cache_mode", "force_custom_plan")]);
        assert_eq!(
            options.get_options(),
            Some("-c search_path=ds_abc,public -c plan_cache_mode=force_custom_plan")
        );
    }
}
