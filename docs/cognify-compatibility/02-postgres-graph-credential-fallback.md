# Item 2 — Graph → relational credential fallback

Parent: [../.internal/cognify-compatibility-implementation-plan.md](../.internal/cognify-compatibility-implementation-plan.md)
Effort: **small** · Depends on / pairs with [Item 1](01-wire-pggraph-component-manager.md)
Status: ✅ Implemented

> **Decision D1 (resolved):** add a dedicated `graph_database_host` field to
> `Settings` + a `GRAPH_DATABASE_HOST` env binding for full Python parity. When
> empty, the resolver falls back to `db_host`. Steps below reflect this.

---

## Problem

When the relational DB is already PostgreSQL, the user must *also* set a separate
`graph_database_url` to use the Postgres graph backend, even though both can point
at the same instance. Python falls back to the relational DB credentials when the
graph-specific Postgres credentials are not fully configured
([`get_graph_engine.py:332-367`](/tmp/cognee-python/cognee/infrastructure/databases/graph/get_graph_engine.py)):

```python
else:  # graph creds not all set
    relational_config = get_relational_config()
    db_username = relational_config.db_username
    db_password = relational_config.db_password
    db_host     = relational_config.db_host
    db_port     = relational_config.db_port
    db_name     = relational_config.db_name
    if not (db_host and db_port and db_name and db_username and db_password):
        raise EnvironmentError("Missing required Postgres graph credentials!")
    connection_string = (
        f"postgresql+asyncpg://{db_username}:{db_password}"
        f"@{db_host}:{db_port}/{db_name}"
    )
```

Rust has **no** `resolved_graph_db_url()` and no fallback. It mirrors the
relational and vector resolvers but is missing for graph.

## Existing patterns to mirror

- `Settings::resolved_relational_db_url()`
  ([config.rs:524-533](../../crates/lib/src/config.rs#L524-L533)) — assembles a
  `postgres://` URL from `db_*` fields when `db_provider == "postgres"`.
- `ComponentManager::resolved_vector_db_url()`
  ([component_manager.rs:208-251](../../crates/lib/src/component_manager.rs#L208-L251)) — uses the
  `url` crate to percent-encode credentials, accepts a pre-formed
  `postgres://`/`postgresql://` URL as-is, and falls back to the `db_*` fields.

Relevant `Settings` fields
([config.rs:39-46, 70-75](../../crates/lib/src/config.rs#L39-L75)):

```rust
pub graph_database_provider: String,
pub graph_database_url: String,
pub graph_database_name: String,
pub graph_database_username: String,
pub graph_database_password: String,
pub graph_database_port: u16,
// ...
pub db_provider: String,
pub db_host: String,
pub db_port: u16,
pub db_name: String,
pub db_username: String,
pub db_password: String,
```

---

## Steps

### Step 2.1 — Add `resolved_graph_db_url()` on `ComponentManager`

Model it on `resolved_vector_db_url()`. Precedence (matching Python):

1. If `graph_database_url` is already a full `postgres://`/`postgresql://` URL →
   return as-is.
2. Else if the graph-specific fields (`graph_database_username`,
   `graph_database_password`, host, `graph_database_port`, `graph_database_name`)
   are **all** non-empty → assemble from those.
3. Else → **fall back** to the relational `db_*` fields (host/port/name/
   username/password), warning once via `tracing::warn!` exactly like Python.
4. If neither set is complete → `ComponentError::Config("Missing required Postgres graph credentials")`.

> Per **D1**, add a `graph_database_host` field to `Settings` (default empty) with
> a `GRAPH_DATABASE_HOST` env binding (Step 2.3). The resolver treats an empty
> `graph_database_host` as "graph-specific host not set" and routes to the
> relational fallback. This must be done **before** Step 2.1's `graph_creds_complete`
> check can reference a graph host.

```rust
#[cfg(feature = "pggraph")]
fn resolved_graph_db_url(&self, s: &Settings) -> Result<String, ComponentError> {
    if s.graph_database_url.starts_with("postgres://")
        || s.graph_database_url.starts_with("postgresql://")
    {
        return Ok(s.graph_database_url.clone());
    }

    // Try graph-specific creds, then fall back to relational db_* fields.
    let (host, port, name, user, pass) = if graph_creds_complete(s) {
        (graph_host(s), s.graph_database_port, &s.graph_database_name,
         &s.graph_database_username, &s.graph_database_password)
    } else {
        warn!(
            "Postgres graph credentials not fully configured; falling back to the \
             relational database configuration. Set GRAPH_DATABASE_* explicitly to avoid this."
        );
        if s.db_host.is_empty() || s.db_name.is_empty() || s.db_username.is_empty() {
            return Err(ComponentError::Config(
                "Missing required Postgres graph credentials".into(),
            ));
        }
        (s.db_host.as_str(), s.db_port, &s.db_name, &s.db_username, &s.db_password)
    };

    // build with url crate exactly like resolved_vector_db_url()
    build_postgres_url(host, port, name, user, pass)
}
```

Factor the `url::Url` assembly shared with `resolved_vector_db_url()` into a small
private helper to avoid duplication.

### Step 2.2 — Use it in the Postgres graph branch

In the `init_graph_db()` Postgres arm added in [Item 1](01-wire-pggraph-component-manager.md), call
`self.resolved_graph_db_url(&s)?` instead of reading `graph_database_url`
directly.

### Step 2.3 — Add `graph_database_host` to `Settings` (per D1)

Add the field next to the other `graph_database_*` fields
([config.rs:39-46](../../crates/lib/src/config.rs#L39-L46)) with an empty default,
and bind it to `GRAPH_DATABASE_HOST` wherever `Settings` reads env vars (match the
existing `graph_database_*` env-binding style). Keep the
`#[derive(Serialize, Deserialize)]` config round-trip intact and update any
config-loading / default-snapshot tests that assert the full field set.

Do this step first — Steps 2.1/2.2 depend on the field existing.

---

## Files touched

- [crates/lib/src/component_manager.rs](../../crates/lib/src/component_manager.rs) — new resolver + call site
- [crates/lib/src/config.rs](../../crates/lib/src/config.rs) — (optional) `graph_database_host` field + env binding

## Acceptance criteria

- With `DB_PROVIDER=postgres`, the `db_*` fields set, and **no** `graph_database_*`
  fields, `GRAPH_DATABASE_PROVIDER=postgres` resolves to the relational
  `postgres://` URL and cognify runs.
- An explicit `graph_database_url` still takes precedence.
- A unit test covers all three precedence branches (explicit URL → graph creds →
  relational fallback) and the missing-creds error.
- Passwords with special characters are percent-encoded (reuse the `url`-crate
  path).

## Risks / notes

- Keep the warning text close to Python's so log-scraping parity holds.
- This item is low-risk on its own but only observable once [Item 1](01-wire-pggraph-component-manager.md) is in; land them together or 1-then-2.
