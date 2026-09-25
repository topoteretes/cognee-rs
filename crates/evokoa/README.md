# Evokoa PostgreSQL adapters

This crate implements Cognee's `GraphDBTrait` and `VectorDB` contracts using
pgGraph 1.2.1 and pgContext 0.3.0.

## Shape

- `EvokoaGraphAdapter` keeps `graph_node` and `graph_edge` authoritative through
  the existing `PgGraphAdapter`, then registers those tables as a pgGraph node
  table and dynamic-label edge table.
- `EvokoaVectorAdapter` owns ordinary PostgreSQL source tables, registers their
  dense vector and JSONB metadata columns with pgContext, and implements the
  complete required `VectorDB` surface.
- `EvokoaHybridAdapter` creates both adapters over one SeaORM pool.
  `search_graph_with_vectors` performs pgContext similarity search and pgGraph
  one-hop expansion in one SQL statement and one PostgreSQL snapshot.

Both adapters are registered in `ComponentRegistry` as the `evokoa` provider.
Set `GRAPH_DATABASE_PROVIDER=evokoa` and/or `VECTOR_DB_PROVIDER=evokoa`; both
providers use the configured PostgreSQL connection settings.

## Requirements

- PostgreSQL 17 or 18.
- pgGraph 1.2.1 or newer. Initialization checks for the native components API.
- pgContext 0.3.0.
- A database role permitted to install the extensions and initialize their
  schemas. pgGraph component metrics additionally require superuser privileges
  or `CREATE` on the `graph` schema, plus access to the registered source tables.

The graph adapter uses pgGraph's native, database-side union-find implementation
for connected-component metrics. It intentionally returns pgGraph errors rather
than falling back to the generic recursive PostgreSQL query.

## Known compatibility gap

pgContext's filter grammar compares a JSONB array as one value. In 0.3.0,
scalar `match`, `match.value`, and `match.any` cannot express “this stored array
contains this requested set name.” Cognee requires OR/AND membership over the
mixed string/object entries in `metadata.belongs_to_set`.

The POC therefore implements `search_similar_filtered` as an exact PostgreSQL
JSONB `?|`/`?&` filter-before-limit query. This is correct but does not use
pgContext ANN for that method. Unfiltered and combined hybrid searches use
`pgcontext.search`.

## Live test

The ignored test requires PostgreSQL 17 or 18 with both extensions:

```sh
EVOKOA_TEST_DATABASE_URL=postgres:///evokoa_cognee_poc \
  cargo test -p cognee-evokoa --test live_smoke -- --ignored --nocapture
```
