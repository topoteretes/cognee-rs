# Python `IndexSchema` vector payloads (golden fixtures)

`python_lancedb_payloads.json` and `python_pgvector_payloads.json` hold the
vector payload Python cognee **stores** for each collection cognify writes
(`DocumentChunk_text`, `Entity_name`, `EntityType_name`, `TextSummary_text`,
`EdgeType_relationship_name`, `TextDocument_name`), one row each. They were
dumped from a real run, not read from the code. The Rust payload tests compare
what `cognee_models::index_schema_payload` writes with them (SDK-1039, part B):

- `crates/cognify/src/tasks.rs`: `every_index_site_writes_python_index_schema_rows`
  (every cognify index site against the pgvector dump);
- `crates/cognify/tests/edge_reindex_backfill.rs`: the `edge-reindex` site;
- `crates/vector/src/lancedb_adapter.rs`:
  `stored_index_schema_rows_match_python_lancedb_rows` (no `metadata` key);
- `crates/vector/src/pgvector_index_tests.rs`:
  `stored_index_schema_rows_match_python_pgvector_rows` (needs `PGVECTOR_TEST_URL`).

- Python cognee commit: `1d3ed15b1d7faf93af3ae7efff8167d164ee0847` (also in each file's `python_commit`).
- Script: [`generate_python_payloads.py`](generate_python_payloads.py). It builds
  synthetic DataPoints the way cognify does, indexes them with Python's own
  `index_data_points` task through `LanceDBAdapter` and `PGVectorAdapter` with a
  stub embedding engine (no LLM, no network), and reads the stored rows back.
- What to compare: key sets, `null` handling, and the deterministic values
  (`type`, `text`, `version`, `topological_rank`, `source_*`, …). `created_at` /
  `updated_at` are the write time.

## Regenerating

The interpreter needs Python cognee's dependencies. The fixtures were produced
in the cross-SDK lane image (`cognee-e2e-cross-sdk`, which installs cognee at the
same commit), importing cognee from the checkout itself, against a throwaway
`pgvector/pgvector:pg16` container:

```bash
docker network create p6-pg
docker run -d --name p6-pgvector --network p6-pg -e POSTGRES_USER=cognee \
  -e POSTGRES_PASSWORD=pw -e POSTGRES_DB=cognee pgvector/pgvector:pg16
FX=crates/cognify/tests/fixtures/index_schema
docker run --rm --network p6-pg -v "$COGNEE_PY:/cognee-src:ro" \
  -v "$PWD/$FX:/fixture:ro" -v "$PWD/$FX:/out" --user "$(id -u):$(id -g)" \
  -e HOME=/tmp --entrypoint sh cognee-e2e-cross-sdk:latest -c '
    /usr/local/bin/pip install -q --target /tmp/extra "asyncpg>=0.30,<1" "pgvector>=0.3.5,<0.4"
    cd /tmp && /opt/python-venv/bin/python -I /fixture/generate_python_payloads.py \
      --cognee-src /cognee-src --out-dir /out --python-commit <sha> \
      --extra-sys-path /tmp/extra \
      --pg-dsn postgresql+asyncpg://cognee:pw@p6-pgvector:5432/cognee'
```

Any virtualenv with `pip install -e "$COGNEE_PY[postgres]"` works too; drop
`--pg-dsn` to dump LanceDB only.
