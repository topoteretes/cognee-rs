# Item 1 — Wire `PgGraphAdapter` into `ComponentManager`

Parent: [../.internal/cognify-compatibility-implementation-plan.md](../.internal/cognify-compatibility-implementation-plan.md)
Effort: **small** · Impact: **highest (blocking)**
Status: ✅ Implemented

---

## Problem

`PgGraphAdapter` is fully implemented and the `pggraph` feature is already in the
default feature set of `cognee-lib` and `cognee-cli`, but it is **unreachable at
runtime**. `ComponentManager::init_graph_db()` rejects every provider except
`ladybug`/`kuzu` before any feature gate is consulted.

Current rejection
([crates/lib/src/component_manager.rs:103-142](../../crates/lib/src/component_manager.rs#L103-L142)):

```rust
async fn init_graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
    let (provider, graph_path) = {
        let s = self.config.read();
        let provider = s.graph_database_provider.to_lowercase();
        if provider != "ladybug" && provider != "kuzu" {
            return Err(ComponentError::Config(format!(
                "Unsupported graph_database_provider '{}'. Supported: ladybug, kuzu.",
                s.graph_database_provider
            )));
        }
        // ...graph_path computed for the ladybug/kuzu file path...
    };
    // ...ladybug-only init...
}
```

Python reference treats postgres as a first-class graph backend:
[`get_graph_engine.py:316-371`](/tmp/cognee-python/cognee/infrastructure/databases/graph/get_graph_engine.py) (clone the
Python repo as documented in the project guide if the path is absent).

## Constructor available

`PgGraphAdapter::new(database_url: &str) -> GraphDBResult<Self>`
([crates/graph/src/pg_graph_adapter.rs:134](../../crates/graph/src/pg_graph_adapter.rs#L134)) — connects via
`Database::connect()` and runs its own graph-table migrations via `Migrator::up()`
**inside `new()`**. Verified: **no separate `initialize()` call is required**
(unlike `LadybugAdapter`, which needs an explicit `initialize()`).

There is also `PgGraphAdapter::from_connection(db: DatabaseConnection)`
([pg_graph_adapter.rs:150](../../crates/graph/src/pg_graph_adapter.rs#L150)) which wraps an existing SeaORM
connection and only runs the graph migrations — relevant to
[Item 3](03-pghybrid-full-adapter.md), where graph and vector share one connection.

The crate re-exports it under the `postgres` feature; `cognee-lib` maps its own
`pggraph` feature to `cognee-graph/postgres`
([crates/lib/Cargo.toml:38](../../crates/lib/Cargo.toml#L38)).

---

## Steps

### Step 1.1 — Restructure provider validation

Replace the early hard-rejection with a structure that allows `postgres`/
`postgresql` through. The current code reads several `ladybug`-specific fields
(graph file path) inside the guard; keep that path for `ladybug`/`kuzu` but move
provider dispatch to a `match` so the Postgres branch can read the URL instead of
the file path.

Suggested shape:

```rust
async fn init_graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
    let provider = self.config.read().graph_database_provider.to_lowercase();

    match provider.as_str() {
        "ladybug" | "kuzu" => self.init_ladybug_graph_db().await,

        #[cfg(feature = "pggraph")]
        "postgres" | "postgresql" => {
            let url = {
                let s = self.config.read();
                self.resolved_graph_db_url(&s)?   // see Item 2; for Item 1 alone,
                                                  // require graph_database_url explicitly
            };
            let adapter = PgGraphAdapter::new(&url).await.map_err(|e| {
                ComponentError::GraphDb(format!("pggraph init failed: {e}"))
            })?;
            Ok(Arc::new(adapter))
        }

        #[cfg(not(feature = "pggraph"))]
        "postgres" | "postgresql" => Err(ComponentError::Config(
            "graph_database_provider=postgres requires the `pggraph` crate feature".into(),
        )),

        other => Err(ComponentError::Config(format!(
            "Unsupported graph_database_provider '{other}'. Supported: ladybug, kuzu, postgres.",
        ))),
    }
}
```

Extract the existing ladybug body into a private `init_ladybug_graph_db()` helper
to keep the match arm readable, preserving the `system_root_directory`/
`graph_file_path` logic and the `#[cfg(feature = "ladybug")]` / `not` pair
verbatim ([component_manager.rs:113-141](../../crates/lib/src/component_manager.rs#L113-L141)).

> **Note on Item 2 dependency:** `resolved_graph_db_url()` is introduced in
> [Item 2](02-postgres-graph-credential-fallback.md). If Item 1 lands first,
> read `s.graph_database_url` directly and return
> `ComponentError::Config("graph_database_url required for postgres provider")`
> when empty; then swap in `resolved_graph_db_url()` when Item 2 lands.

### Step 1.2 — Import the adapter under the feature gate

At the top of `component_manager.rs`, add a gated import next to the existing
`LadybugAdapter`/`PgVectorAdapter` imports:

```rust
#[cfg(feature = "pggraph")]
use cognee_graph::PgGraphAdapter;
```

Match the exact import style already used for `PgVectorAdapter`
(`#[cfg(feature = "pgvector")] use cognee_vector::PgVectorAdapter;`).

### Step 1.3 — Update the error message

The catch-all arm now reads `Supported: ladybug, kuzu, postgres.` Grep the
codebase and tests for the old `"Supported: ladybug, kuzu."` string and update
any assertions.

### Step 1.4 — Confirm default features (already done — verify only)

`pggraph` is already present in both default lists
([crates/lib/Cargo.toml:6-28](../../crates/lib/Cargo.toml#L6-L28),
[crates/cli/Cargo.toml:12-21](../../crates/cli/Cargo.toml#L12-L21)). No change
needed; just confirm during review.

---

## Files touched

- [crates/lib/src/component_manager.rs](../../crates/lib/src/component_manager.rs) — provider dispatch, import, helper extraction
- (verify only) [crates/lib/Cargo.toml](../../crates/lib/Cargo.toml), [crates/cli/Cargo.toml](../../crates/cli/Cargo.toml)

## Acceptance criteria

- `cargo check --all-targets` and `cargo check -p cognee-lib --no-default-features --features sqlite,ladybug,qdrant` (Postgres-off) both compile — the `#[cfg(not(feature = "pggraph"))]` arm returns a clean config error.
- With `GRAPH_DATABASE_PROVIDER=postgres` and a valid `graph_database_url`, `ComponentManager` returns an `Arc<dyn GraphDBTrait>` backed by `PgGraphAdapter` (covered concretely by [Item 5](05-postgres-full-stack-e2e-test.md)).
- The unsupported-provider error message lists `postgres`.

## Risks / notes

- The current code computes `graph_path` (a filesystem path) inside the config
  guard and creates parent dirs unconditionally. The Postgres branch must **not**
  do filesystem path creation — make sure the `create_dir_all` only runs on the
  ladybug/kuzu path after the refactor.
- No explicit `initialize()` is needed after `PgGraphAdapter::new()` — migrations
  run inside `new()` (verified). Do **not** add an `initialize()` call that the
  trait doesn't require.
