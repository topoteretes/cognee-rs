# Evokoa PostgreSQL adapter POC

This experimental crate evaluates pgGraph 1.2.1 and pgContext 0.3.0 against
Cognee's `GraphDBTrait` and `VectorDB` contracts.

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

This is intentionally not registered in `ComponentRegistry` yet. Selecting it
globally would turn an extension experiment into a supported provider before
upgrade, migration, and operational policies are decided.

## Known compatibility gap

pgContext's filter grammar compares a JSONB array as one value. In 0.3.0,
scalar `match`, `match.value`, and `match.any` cannot express “this stored array
contains this requested dataset ID.” Cognee requires OR/AND membership over the
accumulated `metadata.dataset_ids` array.

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
