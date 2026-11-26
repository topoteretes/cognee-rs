# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

<!-- RELEASER: release-open.yml inserts the git-cliff-generated section directly
     after the `## [Unreleased]` marker above, which lands it BEFORE any
     hand-written prose below. When reviewing the release PR, move this
     `### Breaking changes` block to the TOP of the new version's section —
     ahead of Added/Changed/Fixed — so the breaking notes are the first thing a
     reader sees. git-cliff only flags breaking *commits* (`feat!:` → a
     **[BREAKING]** bullet); migration prose like the entries below is always
     hand-written and always needs this manual reposition. -->

### Breaking changes

- **Ladybug graph store moves from lbug 0.14.1 to 0.23.0.** 0.14.1 corrupted
  REL string properties at `CHECKPOINT`: once a store held about a thousand
  nodes, edges read back another edge's `relationship_name` and
  `properties`, so name lookups and edge dedup went silently wrong.
  - **Stores written by 0.14 are not repaired.** Corrupted edge names stay
    wrong after the upgrade; rebuild (re-cognify) any Ladybug store that was
    written by an earlier release.
  - **One-way storage upgrade.** 0.23.0 opens older stores in place and
    rewrites their header to storage code 53 at the first checkpoint, after
    which earlier releases (and any Ladybug before 0.23) cannot open them.
    Python cognee moves to the same Ladybug, 0.23.0; until it does, a store
    written by this release cannot be opened by Python.
  - **Build requirements:** a C++ standard library with `<format>` (GCC 13+
    on Linux), and network access at build time: the engine is the official
    prebuilt 0.23.0 archive, pinned by `LBUG_VERSION` in `.cargo/config.toml`
    and chosen by the build host's platform. Binaries with the `ladybug`
    feature now link `libssl` / `libcrypto` (OpenSSL 3) dynamically. See
    [docs/build/lbug-rebuilds.md](docs/build/lbug-rebuilds.md#which-engine-gets-linked).
  - **Android builds Ladybug from source** (no upstream prebuilt):
    `scripts/android-build-and-deploy.sh` sets `LBUG_BUILD_FROM_SOURCE=1` and
    now requires `OPENSSL_ROOT_DIR` pointing at an OpenSSL 3 built for
    `aarch64-linux-android`, whose `libssl.so` / `libcrypto.so` it bundles.
  - **One process, one live handle per embedded store (`STORE_IN_USE`).** A
    second live open of a Ladybug store the process already holds now fails
    with error code `STORE_IN_USE` (a typed `StoreInUse` variant in
    `GraphDBError` / `VectorDBError` / `ComponentError`, `StoreInUseError` in
    TS) instead of silently opening a second instance whose checkpoint could
    delete the first one's WAL. lbug 0.23 refuses it too ("already open in
    this process"). Close the first handle before opening the directory
    again. Store slots now survive config changes that do not touch the
    store, and a store released while an operation still held it is taken
    back on the next warm rather than reopened.
- **The hard delete's degree-one sweep keeps every node a ledger row still
  names (SDK-1694, stage O9).** `DeleteMode::Hard` sweeps the graph for
  `Entity` and `EntityType` nodes with exactly one edge. Since O6 it kept a
  type a document still claimed, but not an entity, so hard-deleting one
  document removed another document's ontology class entities (`class`,
  `organization`), whose only edge is their `is_a`. The sweep now asks the
  ledger about both kinds, in one batched lookup per 500 slugs, and keeps any
  node a row still names. A legacy nil-`data_id` row counts, as it does in the
  data-scoped delete: a type or ontology class only a nil row claims is no
  longer swept, and stays until its dataset is deleted, as nil-claimed
  types and structural edges already do (a type is then still one entity's;
  a nil-claimed ontology class may be garbage). This
  follows Python, which never removes a slug another ledger row names
  (`get_global_data_related_nodes`) and runs its degree sweep only for data
  its ledger does not know (`legacy_delete`). What the sweep still removes is
  a node no row names, such as one in a graph written before the ledger.
  Soft delete is unchanged (no sweep). **API:** the required
  `DeleteDb::get_node_slugs_owned_by_data` is renamed
  `DeleteDb::get_claimed_node_slugs` (and
  `ops::graph_storage::get_node_slugs_owned_by_data` to
  `get_claimed_node_slugs`) and now counts nil rows; an implementor renames
  its method.
- **cognify works one document at a time, as Python does (SDK-1694,
  overview).** The stage entries below and under Added / Changed / Fixed
  give the detail; together they change cognify as follows. The pipeline is
  described in [docs/operations.md](docs/operations.md#one-document-at-a-time-sdk-1694).
  - **Graph per block, several types per entity** (O1, O2): each document's
    blocks of at most `chunks_per_batch` chunks are expanded with fresh maps,
    the first type per block wins and the blocks' types are combined, so an
    entity two documents type differently has both types. Edge endpoints
    resolve within the block (`cognify.cross_chunk_endpoints`), and labels
    are normalised as Python's `generate_edge_name`.
  - **No batch barrier** (O3): every chunk's call goes through one stream of
    at most `max_parallel_extractions`; `chunks_per_batch` no longer limits
    concurrency.
  - **One streaming stage and one ordered writer** (O4a, O4b): extraction,
    summarization and `add_data_points` are one stage; each document is
    saved as soon as its results are in, in input order, with at most
    `cognify.max_unsaved_documents` (default 32) waiting.
  - **Per-document ledger rows, completion and rollback** (O5a, O5b, D4):
    types and structural edges get a row per producing document; each
    document is completed right after it is saved, and the default
    `rollback_scope` keeps the completed documents of a failed run
    (`failed_items` with `incremental_loading`, `whole_run` without).
  - **Per-document delete** (O6): deleting a document removes the types,
    `is_a` edges and semantic edges only it produced.

  Cross-SDK: `e2e-cross-sdk/harness/test_cognify_type_parity.py` drives both
  SDKs through a scripted OpenAI-compatible endpoint and checks that they
  store the same entity types and normalised edges, on a first and a second
  run (SDK-1694 O7). Existing graphs are not migrated: an entity gains
  further types only when its data is cognified again (with
  `incremental_loading` off, or re-added), and raw-label edges stay, with a
  normalised copy added beside them.

- **Deleting a document removes the entity types and `is_a` edges only it
  declared (SDK-1694, stage O6).** Python removes an EntityType with the last
  document whose walk reached it (`get_data_related_nodes`). Rust wrote a
  nil-`data_id` ledger row beside every EntityType, structural edge and
  ontology-first entity, which claimed it for every document, so no
  data-scoped delete removed one. cognify no longer writes those rows (only
  an artifact no document is known to have produced, such as a DLT schema
  node, still gets one), and delete counts the per-document rows stage O5a
  added:
  - an **EntityType** goes with its last declaring document, in soft and
    hard delete, and a dataset delete removes the types no other dataset
    declares, as before;
  - an **edge** is the last claim on itself when no other owner's row names
    the same `(source, target, relationship)` triple, in the data-scoped
    delete, the dataset delete and the run sweep. It used to be the slug,
    which every `is_a` row (and every `is_part_of` row) shares, so a
    document's own `is_a` edge was never its own. An edge whose last
    producing document goes — an `is_a` edge, or a semantic edge between
    two entities other documents keep — is removed from the graph even when
    both its endpoints stay (`GraphDBTrait::delete_edge_triples`), and its
    `EdgeType` point goes once no surviving row produces its text, as
    Python's graph-provenance delete (`provenance_delete_planner`) does with
    its per-triple source refs. Only a per-document row (one whose id folds
    its `data_id`) can say that: an edge only a Python-written or older Rust
    row names (one row per edge, its first writer's) leaves the graph only
    with an endpoint, and keeps its `EdgeType` point while it stays;
  - a document that reaches an entity, or an ontology edge whose endpoints
    it owns, only through an ontology lookup gets its own row, whether or
    not another document extracted it, so deleting the extractor keeps it;
  - the hard delete's sweep of degree-one EntityTypes keeps a type a
    document's ledger row still claims (new **required** method
    `DeleteDb::get_node_slugs_owned_by_data`, renamed
    `get_claimed_node_slugs` by O9 above, which also keeps entities and
    counts nil rows).

  **Existing databases:** nil rows already written stay claims. A type or
  structural edge written before this change is not removed by a
  data-scoped delete, as before, until its dataset is deleted; re-cognifying
  its documents does not remove the nil row. Migration
  `m20261125_000001_edges_endpoint_index` adds an index on `edges
  (source_node_id, destination_node_id)` for the triple lookups.

  `cognee_models::Entity::extra_types` is no longer serialised, so it is in
  no graph node, ledger row, vector payload or COGX export (Python has no
  such field, and the stored copy went stale when a later run or a delete
  changed the types); the `is_a` edges are an entity's type set. A payload
  that holds it still deserialises.

- **The default `cognify.rollback_scope` follows `incremental_loading`
  (SDK-1694 D4).** Unset, it is now `failed_items` with `incremental_loading`
  on (the default) and `whole_run` with it off — Python keeps the items its
  failed run stamped complete, and stamps nothing without incremental loading.
  A run with a failed document therefore still errors, as before and as in
  Python, but keeps the documents it completed instead of rolling everything
  back. Set
  `COGNEE_COGNIFY_ROLLBACK_SCOPE=whole_run` for the previous behaviour.
  **API:** `CognifyConfig::rollback_scope` is now `Option<RollbackScope>`
  (`None` = follow `incremental_loading`); read the resolved scope with
  `CognifyConfig::effective_rollback_scope()`. `with_rollback_scope` is
  unchanged.
- **A `failed_items` cognify run completes each document as it saves it, and
  a failed run keeps what it completed (SDK-1694, stage O5b).** Python stamps
  a data item complete once its own tasks ran, and its rollback keeps the
  items its failed run stamped (`cognify/rollback.py` `_kept_data_ids`). Under
  `RollbackScope::FailedItems` Rust now does the same:
  - The document stage's writer **marks each document complete right after
    saving it** (with `incremental_loading`); a code item is marked as soon as
    its graph is loaded; a DLT row only after the post-run step wrote its
    foreign-key edges. A document whose save failed or was cut off mid-save is
    not completed.
  - **A `failed_items` run with a failed document errors** (`RunFailed`,
    `ERRORED`) whatever the chunk failure ratio, as Python's `run_tasks` raises
    whenever a data item errored; `FailureReport::is_fatal` is true on any
    failed item under every scope, and `chunk_failure_ratio_threshold` /
    `COGNEE_COGNIFY_MAX_CHUNK_FAILURE_RATIO` no longer decide anything (still
    read and validated). It used to return `Ok` below the ratio, so a re-run
    that stopped at the same bad document reported success every time.
  - **A `failed_items` run that fails** (a failed document, a failed post-run
    step, a cancellation) **keeps the documents it completed**, markers and
    all, and sweeps everything else it wrote — as Python's rollback deletes
    every row of the run but the kept items'. It used to escalate to a
    whole-run sweep that deleted the completed documents and their markers
    too; select `whole_run` for that end state. The sweep is "the whole run
    except the kept documents" (`RunSweeper::sweep_run_keeping`): the kept
    rows are moved to a holding run id in batches, the run is swept whole and
    they are moved back, so no list of documents is bound into the sweep's
    queries and its size is not bounded by the backends' bind-variable caps.
    Run-level nil rows no kept document stands beside (a DLT schema node) go
    with the run.
  - **Startup recovery keeps the documents a killed run marked complete**, as
    Python's recovery keeps the stamped items, and sweeps the rest.
  - **A failed save of one document fails only that document** under
    `failed_items` (new `FailureStage::Persistence`): it is swept, the run
    goes on (under `FailFast` it stops dispatching) and errors at the end. Under
    `whole_run` and `nothing` a failed save still fails the run; the writer
    now returns that error before finishing its bulk-load scope.
  - **An item-scoped sweep now removes the run's nil-`data_id` row of an
    artifact** (EntityType, structural edge, ontology-first entity) when every
    document row beside it is swept — a type only the swept documents
    declared goes with them. Edges are matched on (source, target,
    relationship), not the slug every `is_a` row shares.
  - The kept documents' **edge evidence is detached from the errored run**
    (Python `keep_completed_items_evidence`), so `include_references` still
    cites it.
  - `nothing`: after a stop the documents saved before it keep their
    artifacts without a marker; complete documents behind the stop are
    discarded (unchanged, now documented and tested).

  **API changes:** `CognifyConfig` gains `completion` (an opaque
  `CompletionHandle` that `cognify()` fills; callers leave it `None`);
  `FailureStage` gains `Persistence` (`Display`: `persistence`);
  `ProvenanceDb` gains `keep_completed_items_evidence`, `retag_run_rows` and
  `retag_guarded_nil_rows` (required methods); `RunSweeper` gains
  `sweep_run_keeping` (+ `_logging_failure`) and `holding_run_id`;
  `MockVectorDB::set_index_error_for_point` and `bulk_load_closes` (testing).
- **cognify saves each document as soon as it is complete, from one ordered
  writer (SDK-1694, stage O4b).** Extraction, summarization and
  `add_data_points` are now **one streaming pipeline stage**,
  `make_cognify_documents_task` (`COGNIFY_DOCUMENTS_TASK_NAME =
  "extract_graph_summarize_and_add_data_points"`): every chunk's extraction
  and summary call go through one bounded stream each, results reach a
  per-document assembler as they arrive, a finished document's graph is built
  on the run's CPU pool (its blocks expanded with fresh maps, once per
  document) and a single writer saves it in input order through the O4a
  persistence layer. What a run stores is **unchanged as sets** — the
  `persist_equivalence` snapshots, now also pinning the vector collections
  and `CognifyResult`, pass unchanged on all three scenarios — with these
  rules for what several documents share:
  - **Rewrite on change.** An entity a later document changes (a new type, a
    new NodeSet, the first real extraction replacing an ontology placeholder)
    gets its graph node, its vector point and the ownership rows of the
    earlier documents rewritten, so it ends up as when the run was saved at
    once. With `retain_embeddings` the point keeps the vector it was embedded
    with while its name is unchanged; otherwise the name is embedded again,
    once per actual change (the vector store cannot hand a vector back).
    Description, importance weight and metadata stay the first document's
    (first block wins, D5); `contains` edge texts use that description too.
  - **Two documents of one URL** share a WebPage node; the last one in input
    order wins, as before.
  - **EdgeType** `number_of_edges` counts each edge the run wrote once, across
    documents, and an `EdgeType` point keeps the run's lexicographically
    smallest spelling, as before.
  - The existing-edge check runs per document, after the earlier documents
    were saved; an edge an earlier document of the run wrote is that run's,
    not "existing".
  - Under `FailFast`, a document with a failed or skipped call is not saved
    (only complete documents are, as before); a failed extraction **now also
    stops the summary stream** and vice versa, through one shared stop flag;
    a failed save stops both and fails the run, and dropping the stage
    aborts the LLM calls in flight. Under a scope other than `failed_items`
    a stop fails the run with the whole report (extraction and summaries).
  - New setting `cognify.max_unsaved_documents`
    (`COGNEE_COGNIFY_MAX_UNSAVED_DOCUMENTS`, default `32`): a document's
    chunks are dispatched only while fewer documents are waiting to be saved.
  - The graph backend path is driven the same way: blocks of at most
    `chunks_per_batch` chunks of admitted documents, each chunk's result sent
    to the assembler as its block returns.

  Stage O5b (above) since moved the completion markers into the writer and
  made a failed save fail only its own document under `failed_items`.
  `SummaryExtractor::summarize_chunks` runs on the same dispatch helper: a
  chunk's text is cloned when its call starts (not every chunk up front), and
  each call runs on its own task.
  **Signature changes:** `build_cognify_pipeline` has three stages, the third
  named `COGNIFY_DOCUMENTS_TASK_NAME` (the old
  `make_extract_graph_and_summarize_task*` / `make_add_data_points_task*`
  factories stay, for custom pipelines, and save the run as one group);
  `extract_dlt_fk_edges` takes the run's `&CognifyConfig`; `CognifyConfig`
  gains `max_unsaved_documents`. Also: the DLT step writes its schema nodes'
  vectors before its FK edges again and opens no empty bulk-load scope after
  the run.
- **One persistence layer for every cognify write (SDK-1694, stage O4a).** The
  graph, vector, ownership-ledger and edge-evidence writes of a run — the
  extracted entities, Timestamps and semantic edges, the claimed existing
  edges' ledger rows, the WebPage / WebSite nodes, the chunks, summaries,
  entity types, documents, custom-schema graphs, structural edges, vectors,
  edge evidence and the post-run DLT schema nodes and FK edges — now all go
  through one internal layer that writes the ledger rows first, then every
  graph node, then every graph edge, then the vectors, then the evidence.
  **What a run stores is unchanged** on the mock stores, pinned as sets by
  `tests/persist_equivalence.rs`; on Ladybug and the Postgres graph the
  node-first order also fixes edges that were dropped or failed (see Fixed).
  What moved:
  - The extraction stage (`make_extract_graph_task*`,
    `make_extract_graph_and_summarize_task*`) **writes nothing any more**; the
    `add_data_points` stage writes everything. A custom pipeline that runs the
    extraction task without `make_add_data_points_task` after it no longer
    stores the extracted graph or the WebPage / WebSite nodes. The factories'
    `db` argument is unused and kept for their signatures. The standalone
    `extract_graph_from_data` and `create_web_page_nodes` still store what they
    always did.
  - `SummarizedData` gains `claimed_existing_edges: Vec<GraphEdgePair>` (the
    edges an earlier run already wrote: ledger rows only) and
    `handoff: PersistHandoff` (opaque: the WebPage / WebSite artifacts and the
    entities' graph view), so struct literals need both
    (`claimed_existing_edges: vec![], handoff: PersistHandoff::default()`).
    `ExtractedGraphData` gains the same two fields; it is `#[non_exhaustive]`,
    so only readers outside the crate see them.
  - A run's failure window moved with the writes: an entity or semantic-edge
    write now fails in the `add_data_points` stage, after summarization,
    rather than in the extraction stage. Persistence failures stay run-fatal.
- **Code-project documents follow the registered loaders (SDK-1702).**
  `partition_repo_files`, `resolve_code_repository` and
  `resolve_data_directories` take the set of document extensions, normally
  `LoaderRegistry::document_extensions()` of the pipeline's registry, as
  Python's `_document_extensions()` asks its loader engine. The static
  `code_repo::DOCUMENT_EXTENSIONS` list is removed. A project file only an
  unregistered loader could read (a PDF in a build without a PDF loader, an
  image without an LLM) is now skipped instead of added and then failed.
- **Loader routing skips unregistered loaders in its default order too
  (SDK-1702).** As in Python's `LoaderEngine.get_loader`, a registry without
  the code loader reads a `.py` file with the text loader instead of failing
  with `UnsupportedDocumentType`. A file only an unregistered loader claims
  (an image without the image loader) still fails naming that loader.
- **`build_cognify_pipeline` routes code items (SDK-1702).** The public
  builder now starts with the code route task and a routed classify, as
  `cognify()` does for a run with code items, so a caller that builds the
  pipeline itself never sends a code repository manifest down the LLM path.
  Its task list gains `extract_code_repo_graph` first.
- **Repository cloning is hardened for API requests (SDK-1701).** For an HTTP
  API request (`RepoSourceSettings` with `api_request = true`,
  `Settings::api_repo_sources`) a clone's directory name now carries an 8-hex
  hash of the credential-free URL (`github.com-org-repo-1a2b3c4d`), so URLs
  Python's name merges no longer share a clone. **A code repository's data id
  is keyed on its clone directory, so a server's URL rows get new ids**: a
  repository re-added after upgrading is a new row, and the old row and old
  clone directory are left in place (forget the old row to remove both). SDK,
  CLI and binding calls keep Python's names and ids. An API request also
  refuses a clone whose refresh failed (an SDK call still reuses it), and pins
  git to the addresses the outbound check passed (`http.curloptResolve`, which
  needs git >= 2.37: with an older git such a clone is refused).
- **`ALLOW_HTTP_REQUESTS` is a cognee-wide setting (SDK-1701).** It moved from
  the `code_graph` section to `Settings::allow_http_requests`;
  `code_graph.allow_http_requests` stays an alias. The CLI config file moves
  to version 7, which hoists a saved `code_graph.allow_http_requests` entry
  into the setting. It now also gates the web
  pages `add` fetches, as in Python (`require_http_requests_allowed_for`): an
  `AddPipeline` that was never given `with_repo_sources` (fail-closed) refuses
  URL inputs, and an HTTP server needs `ALLOW_HTTP_REQUESTS=true` to fetch
  them. `RepoTransport::check_outbound_url` and `validate_outbound_url` now
  return the checked addresses (`Vec<IpAddr>`).
- **The outbound-address check blocks all of `192.0.0.0/24` (SDK-1701)**, as
  CPython >= 3.12.4 does, and web-page fetches use the same address table as
  repository clones (it was narrower).
- **The existing-edge check takes the built edges (SDK-1694, stage O2).**
  `retrieve_existing_edges` (re-exported from `cognee_cognify::graph_integration`)
  now takes the edges expansion built, `impl IntoIterator<Item =
  &GraphEdgePair>`, instead of the extracted `KnowledgeGraph`s, and returns
  their keys as `HashSet<(Uuid, Uuid, String)>` (`GraphEdgePair::dedup_key`)
  instead of `"{source}_{target}_{label}"` strings.
  `expand_extracted_graphs_by_unit` loses its `existing_edges_set` argument:
  its `Expansion` holds every edge it built, and the new
  `Expansion::split_existing_edges(&existing)` moves the ones the graph holds
  into `claimed_existing_edges` (an existing edge no chunk produced, an
  ontology edge, is dropped instead). The pipeline calls
  `retrieve_existing_edges(graph_db, &expansion.edges)` and then the split.
  `expand_with_nodes_and_edges`, `expand_with_nodes_and_edges_with_stats` and
  `expand_extracted_graphs` keep the argument, now a `&HashSet<(Uuid, Uuid,
  String)>`, and split their result with it.
- **Graph extraction no longer waits at `chunks_per_batch` boundaries
  (SDK-1694, stage O3).** Both LLM extraction paths (the default graph and a
  runtime `graph_schema`) send every chunk of the run through one stream with
  at most `max_parallel_extractions` calls in flight, so one slow chunk holds
  only its own slot instead of stalling every chunk after its 2000-chunk batch.
  A chunk's text is copied only when its call starts. Results are put back in
  chunk order before the graph is built, so the first type an entity is given
  in a block still wins. `chunks_per_batch` (`CHUNKS_PER_BATCH`) now only sets
  the blocks the graph is built from (Python's unit); a graph backend
  (`CognifyConfig::graph_backend`) is still handed one block per call.
  - **`chunks_per_batch` no longer limits LLM concurrency.** Extraction ran at
    most min(`chunks_per_batch`, `max_parallel_extractions`) calls at once; it
    now runs `max_parallel_extractions` alone. Anyone who set a small
    `chunks_per_batch` (by env, or per request in a downstream server) to limit
    LLM load must set `LLM_MAX_PARALLEL_REQUESTS` / `max_parallel_extractions`
    instead.
  - **`FailFast` stops at the in-flight window, not at a batch boundary.**
    After the first failed call nothing new is dispatched, the calls in flight
    finish and their failures are reported, and every file with a chunk that
    was never sent is unreached, as summarization already did. Which chunks
    were sent before the stop depends on timing, so on the same failing input
    a different set of files can end up unreached.
  - **Under `FailFast` + `FailedItems` this keeps far fewer files.** Before, a
    run under 2000 chunks dispatched everything, so one failed chunk cost one
    file. Now every file with a chunk beyond the in-flight window is
    unreached, and the calls it did finish are discarded with it. For example,
    with `LLM_MAX_PARALLEL_REQUESTS=32`, 82 files and 1,500 chunks, a failure
    at chunk 40 used to keep 81 files and now keeps a handful; a failure in the
    first file keeps none, so the run is fatal (`is_fatal`'s no-survivor rule)
    and is rolled back whole. This is Python's behaviour too (the first failure
    cancels the items still running and starts no more), and the plan for
    SDK-1694. Stage O5b (above) since saves and keeps every file whose calls
    all came back before the stop, so `FailFast` + `FailedItems` (the default
    with `incremental_loading`) no longer loses finished files.
  - **Log lines.** The per-batch `Processed graph extraction batch i/n` line is
    gone. Extraction logs `graph extraction: N of M chunks returned` at most
    every 30 seconds, and one `Extracted graphs from N of M chunks (K never
    dispatched)` line at the end.

### Added

- **Per-item cognify outcome (SDK-1694 O8).** `CognifyResult::items` lists
  every data item handed to `cognify()`, in input order, as an `ItemOutcome
  { data_id, status }`: `Completed` (processed and kept by this run),
  `AlreadyCompleted` (skipped because an earlier run completed it),
  `Failed`, `Unreached` (a fail-fast stop came first, or no loader matches
  it) or `Discarded` (processed by a run that errored and did not keep it).
  A run that errors with `CognifyError::RunFailed` carries the same list on
  its report: `FailureReport::item_outcomes()`, or
  `CognifyError::item_outcomes()`. It is read once the run's rollback is
  done, so `Completed` names only kept documents. It holds ids and enums
  only, so it adds nothing that grows with the documents (SDK-507). Hosts
  render Python's per-item `data_ingestion_info` entries
  (`PipelineRunCompleted` / `PipelineRunAlreadyCompleted` /
  `PipelineRunErrored`) from it. `ItemStatus` and `ItemOutcome` are
  `#[non_exhaustive]` (`ItemOutcome::new` builds one). A `CognifyResult`
  struct literal needs `items: vec![]`; nothing outside the crate builds one
  (`CognifyResult::empty()` covers that), so this is not marked breaking.
  `Completed` does not promise a completion marker: none is written without
  `incremental_loading`, and a failed marker write only warns.
- **`AddPipeline::with_upload_root` (SDK-1694 C5).** A host that spools
  client uploads to disk and adds them as `DataInput::FilePath` names the
  spool directory; a file under it records no `_cognee.source_uri`, as
  Python records no source for an upload (`ingest_data.py:180-188`). An
  explicit `source_uri` in the item's metadata still wins. Unset, nothing
  changes. The closed HTTP server sets it, so `/cognify`'s
  `data_location` is `null` for an upload instead of its spool path.
- **Recall's `code` scope, the CODE CLI options and `codeQuery` in recall
  (SDK-1056 P3).** `RecallScope::Code` now runs (Python's `_run_code`,
  `recall.py:1093-1157`): one CODE search with the new
  `RecallOptions::code_query` (or `explore` on the query text), its facts
  tagged `RecallSource::Code`, each Python's `ResponseCodeEntry` (the
  normalized graph-entry shape, kind `code`, `source` on the item;
  `recall_normalize::build_code_entry`). It is never implied by `auto` or
  `all`; seed misses — raised for one dataset or softened per dataset —
  contribute nothing, and an invalid operation fails with the new
  `ApiError::CodeQuery`, which keeps Python's `error_code` / `error_param`. A
  `code_query` without the `code` scope is refused before any source runs
  with Python's `InvalidCodeQueryError` message
  (`recall_scope::check_code_query`). `cognee-cli search` and `recall` gain
  `--diagram {mermaid,dot}` and `--diagram-out PATH` next to `--code-query`
  (a port of `cognee/cli/code_search.py`: a Mermaid `.html` page, Graphviz
  `.svg`/`.png`/`.pdf` through `dot` on `PATH`, or raw source) and print CODE
  results with their fenced diagrams; `recall -t CODE` runs the real CODE type
  (no more `CODING_RULES` stand-in) and, with a CODE option, runs it in the
  `code` source as Python's command does. The bindings' recall takes
  `codeQuery` and the `"code"` scope (Python, TS, Java and C docs and types
  updated), lists code entries in `normalizedResults`, and reports a refused
  CODE operation from search or recall as a validation error naming its
  `error_code`. A graph-lane `CODE` answer is normalized with the `code` kind,
  not `structured`. `RecallOptions` gained a field and `RecallSource` and
  `ApiError` a variant, so exhaustive matches need an arm;
  `cognee::cognee_code_graph` is re-exported, and
  `cognee_code_graph::pyjson::dumps_indented` writes Python's
  `json.dumps(indent=...)`. The query router still does not route to `CODE`.
  The bindings' search and `cognee-cli search` also report a
  `SearchError::QueryValidation` (a `codeQuery` without the CODE type, a
  `node_name` filter the type cannot apply) as a validation error, as Python
  raises `CogneeValidationError`; they reported it as a runtime failure.
  `SearchTuning::code_graph_cache` is now an `Option`: `None` (a config left
  at the default `code_graph.snapshot_cache_*`) no longer resets the
  process-wide CODE snapshot cache bounds another handle set
  (`CodeGraphConfig::snapshot_cache_override`).
- **CODE search: `search`, `flows`, `hubs` and `ask` (SDK-1056 P2).** The last
  four of Python's fifteen operations now run: `search` ranks facts and flows
  with the field-weighted BM25 of `code_search_index.py` (tokenizer, stemmer,
  k1/b/idf as Python; scores rounded with Python's round-half-even), `flows`
  lists, shows and traces Enola's flows (`list`, `show`, `through`), `hubs`
  ranks facts by degree, and `ask` assembles seeds, flows, a one-hop
  neighbourhood, file:line citations and a `coverage` block without a model.
  `capabilities` lists all fifteen operations, sorted, as Python does. A
  dataset without flows answers `flows` with `feature_unavailable` (param
  `operation`) as Python's `CodeGraphUnavailableError`, a per-dataset miss, and
  one without a code graph answers `flows` and `ask` with `no_code_graph`.
  `CodeOperation::implemented`/`is_implemented` and
  `CodeQueryError::not_implemented` are gone. Checked against 368 results
  Python produced (`scripts/generate_code_search_goldens.py` gained the shop
  graph without flows or docs, the test-heavy graph with score ties and the
  checkout graphs of `code_retriever_navigation_test.py`).
- **CODE search: the graph walks, overviews and diagrams (SDK-1056 P1b).**
  `SearchType::Code` now runs `explore` (the default operation),
  `traverse`, `find_path`, `impact_analysis`, `insights` and `architecture`
  with Python's result shapes, bounds, type/constructor roll-up and argument
  errors, and draws any graph-shaped result as Mermaid or Graphviz DOT source
  (`code_query={"diagram": "mermaid" | "dot" | true}`; `architecture` draws
  Mermaid by default, `delta` gets a null source and a note) — a port of
  `code_graph_diagram.py` in `cognee_code_graph::query` (`render_mermaid`,
  `render_dot`, `collect_diagram_graph`, `render_result_diagram`). The P1a
  `feature_unavailable` refusal of diagrams is gone; only `search`, `flows`,
  `hubs` and `ask` still answer `feature_unavailable`, and `capabilities`
  lists the eleven operations that run. Checked against 225 results and 4
  direct renders Python produced (`scripts/generate_code_search_goldens.py`
  now also loads the graphs of Python's `code_retriever_test.py`).
- **The `CODE` search type, first stage (SDK-1056 P1a).** A port of Python's
  deterministic code-graph search (`code_retriever.py`, parity pin
  `a59433e6b`): `SearchType::Code` (`"CODE"`), `SearchRequest::code_query`
  (merged over `retriever_specific_config`; refused with any other type, with
  Python's message), the `CodeRetriever` (no LLM, no session turn, a
  `Structured` result whose `only_context` form is Python's compact,
  key-sorted JSON context) and `SearchError::Code`, which carries Python's
  `error_code` and `error_param`. The engine lives in
  `cognee_code_graph::query`: the snapshot loader and indexes, Python's
  value normalization (JSON text, nested `properties`, blank docs), the BM25
  text index, `casefold`, and the operations `query_facts`, `neighbors`,
  `subgraph`, `delta` and `capabilities`, checked against 80 results Python
  produced (`scripts/generate_code_search_goldens.py`). The other ten
  operations and diagrams are validated as in Python but fail with
  `feature_unavailable` for now. Under per-dataset storage a seed that one of
  several datasets cannot resolve is that dataset's result
  (`{"seed_not_found": true, "error", "code", "param"}`), not a failed search,
  and a merged CODE search returns one result per dataset. Parsed snapshots
  are cached per dataset and graph engine (`code_graph.snapshot_cache_max_entries`
  / `CODE_GRAPH_CACHE_MAX_ENTRIES`, default 16; `code_graph.snapshot_cache_ttl_secs`
  / `CODE_GRAPH_CACHE_TTL_SECONDS`, default 60) and invalidated by every
  code-graph load and, unlike Python, by every delete, forget and graph prune.
  `cognee-cli search -t CODE` now runs it (with `--code-query`) instead of
  the `CODING_RULES` stand-in, and the bindings take `codeQuery` (`CODE` is in
  the Python, TS, Java and C search-type lists). `FEELING_LUCKY` no longer
  reads a selector's `CODE` answer as `CODING_RULES`: as in Python's
  `select_search_type`, CODE is never auto-selected and the answer falls back.
  `SearchRequest` gained a field, so struct literals need `code_query: None`.

- **`allowed_local_file_roots` (`COGNEE_ALLOWED_LOCAL_FILE_ROOTS`, SDK-1701).**
  Python's opt-in allowlist for local repository paths: empty is no
  restriction; set (even to `:`), a local repository must resolve under one
  of the roots, cognee's own storage roots or the clones directory. An HTTP
  API request (`Settings::api_repo_sources`) does not allow the storage roots,
  and allows only the clones directory its settings carry (a server's
  per-caller one). A refusal is a `CodeRepositoryError` with status 403
  (Python's is 500), also for a path that does not exist.
- **Code repository folders are cleaned up (SDK-1701).** Forgetting a code
  repository row, or deleting its dataset, removes the dataset's
  `.enola-<dataset_id>` snapshot from the repository, and removes a clone no
  remaining row points at. Rust-only. Removing a clone also removes the
  repository's other branch clones beside it that no row names (SDK-1709): a re-add on another
  branch moves the URL's one row to that branch's clone, and the clone it
  moved off used to stay on disk for good. A re-add itself never removes a
  clone.
- **Keyless `remember(content_type="code")` (SDK-1053).** A port of the
  `content_type == "code"` branch of Python's `_remember_inner`:
  `cognee::api::remember_code` (`RememberCodeParams`) stores each repository
  as one code-repo `Data` row without its documents
  (`AddPipeline::add_code_repository`), builds its code graph in a
  `code_graph_pipeline` run (`cognee_cognify::run_code_graph_pipeline`, the
  same builder as cognify's CODE_REPO route) and stamps the row complete under
  `cognify_pipeline` and `code_graph_pipeline`. No LLM is built or
  validated. Options are checked in Python's order (`content_type`,
  `index_vectors`, `repo_credentials`, `branch`, `session_id`); result items
  carry Python's `{kind: "code_repository", source, path, id,
  pipeline_run_id}` shape, with `status: "errored"` / `error` for a failed
  repository when `raise_on_error=false`. Telemetry event
  `cognee.remember.code_graph`. New `ProvenanceDb::mark_pipeline_status_complete`
  and `CODE_GRAPH_PIPELINE_NAME`. Bindings gain `contentType`, `nodeSet`,
  `raiseOnError` on `remember`; the CLI gains `remember --content-type code`.

- **Incremental code graph re-ingest (SDK-1053).** A reload of an unchanged
  snapshot is skipped when the repository node's stamp matches (snapshot id
  and mapping version); otherwise the load sweeps the code nodes and edges
  the snapshot no longer derives, strips node-set tags the call no longer
  passes, and stamps the snapshot last, as Python's `extract_code_graph`
  does. `last_delta` reports real removal counts. New
  `GraphDBTrait::delete_edge_triples(&[EdgeKey])` (deletes edges, keeps their
  endpoints) with a generic default, so out-of-tree backends keep compiling;
  Ladybug, pggraph and the mock override it. `forget(data_id)` of a code-repo
  row removes only that repository's graph. **Signature change:**
  `cognee_code_graph::extract_code_graph` takes the graph.

- **Git URLs for code repositories (SDK-1053).** New
  `cognee_ingestion::resolve_repo` (re-exported as `cognee::add::resolve_repo`)
  ports Python's `resolve_repo.py`: shallow clones with symlinks and LFS off,
  branch checks, refresh by fast-forward pull, credentials passed only as
  environment-level git config and scrubbed from errors, and Python's
  outbound address check before an http(s) clone. `remember(content_type=
  "code")` accepts git URLs and `repo_credentials` (a refused gate is the new
  `ApiError::CodeRepositoryForbidden`, 403), and `add(<GitHub/GitLab URL>)`
  clones the repository and stores a code-repo manifest plus its documents
  (`AddParams::branch`). New config keys `code_graph.clones_dir`
  (`COGNEE_REPOS_DIR`, default `~/.cognee/repos`) and
  `code_graph.allow_http_requests` (`ALLOW_HTTP_REQUESTS`). Remote
  repositories are refused until a host wires `AddPipeline::with_repo_sources`.
  Beyond Python: a new clone is made in a `<name>.partial-<random>` sibling
  and moved into place only once complete, so an interrupted clone is never
  reused; an API request's git follows no http redirect
  (`http.followRedirects=false`); git's stderr is scrubbed of credentials
  before it is cut to its tail. As in Python, an empty `repo_credentials` is
  no token.

- **A dataset's code repositories as one enola cluster (SDK-1053).** Ports
  Python's `cluster.py`: one enola run per dataset (`run_enola_cluster`) gives
  the edges between repositories, and each member is loaded, skip-checked and
  stamped under its own `Data` row (`load_cluster_member`). cognify's
  CODE_REPO route loads the whole cluster once per run;
  `remember(content_type="code")` plans the refresh under the dataset lock
  and rebuilds up to `CLUSTER_INLINE_REBUILD_LIMIT` existing repositories
  inline, marking the rest for the next cognify. New
  `ProvenanceDb::clear_pipeline_status_for_data`.

- **`remember(content_type="code", index_vectors=true)` (SDK-1053).** The
  opt-in vector index of a code graph, as Python's `index_vectors`: the code
  nodes the call writes are also embedded (each node's `name` into
  `{type}_name`, the retrieval texts of their structural edges into
  `EdgeType_relationship_name`), after their graph rows; the default still
  makes no embedding call. It applies to the call's own repositories only
  (the dataset's other repositories, rebuilt in the same cluster run, stay
  graph-only), and the cognify routes stay graph-only. `index_vectors=true` was
  refused before. API: `RememberCodeParams::{index_vectors, embedding_engine}`
  (`InvalidArgument` when asked without an engine), bindings `indexVectors`,
  CLI `remember --index-vectors`; `cognee_code_graph::{CodeGraphVectors,
  index_code_graph_vectors, IndexedCollections}` and new
  `CodeGraphError::{Embedding, Vector}` variants. **Signature changes:**
  `load_code_graph`, `load_cluster_member`, `extract_code_repo_graph` and
  `add_code_graph_data_points` take a trailing `Option<&CodeGraphVectors>`;
  `cognee_cognify::{run_code_routes, make_code_routes_task,
  run_code_graph_pipeline}` and `code_cluster::refresh_dataset_code_graph` a
  trailing vectors option; `code_cluster::MemberRun` gains `index_vectors`.

- **The per-file CODE cognify route (SDK-1053).** cognify loads a lone code
  file (a row stamped `system_metadata.source = "code"` by `code_loader`)
  into the deterministic code graph instead of failing the run with
  `CognifyRouteError::UnsupportedRoute`, as Python's `code_files.py` does:
  its stored text is staged under its original file name in a temporary
  directory named `<file name>_<data id>` (plus a `tsconfig.json` for
  `.js`/`.jsx`) and enola runs on it, with no LLM and no embedding call. The
  nodes are untagged and recorded in the ownership ledger under the row and
  the run; a file that fails to load is a per-item failure
  (`FailureStage::CodeGraph`), like a repo item. Code files and code
  projects in one dataset share one run. API: `cognee_code_graph::
  {extract_code_file_graph, stage_code_file, staged_repo_name,
  original_file_name, is_code_sourced, detection_markers,
  CODE_FILE_STAGING_PREFIX}`; new `CodeGraphError::{InvalidCodeItem, Staging}`
  variants (the enum is not `#[non_exhaustive]`);
  `cognee_models::code_repo::{SOURCE_CODE, system_metadata_source}`
  (`routing::SOURCE_CODE` re-exports it, and `routing::metadata_source`
  delegates).

- **`Data.system_metadata` and the cognify route policy (SDK-1053).** The
  `data` table gains Python's nullable `system_metadata` JSON column
  (migration `m20261118_000001_data_system_metadata`, Python alembic
  `f2b4c6d8e0a1`; skipped when Python added it first). It is the cognee-owned
  route stamp of a row and is never merged with the user's
  `external_metadata`. `DataInput::DataItem` gains `system_metadata`, which
  ingestion writes to the column; like Python's `dict | None` it must be a
  JSON object (`null` is no stamp), anything else fails the add with
  `IngestionError::InvalidSystemMetadata`. On a re-add it follows the content
  as in Python (an item's own stamp replaces the stored one; without one the
  column is not written unless the content changed, which clears it).
  `DataRowRefresh::system_metadata` is a patch: `None` leaves the column
  untouched. `cognee_cognify::routing` ports
  Python's `CognifyRoute` / `cognify_route_for`: `code` → `Code`,
  `code_repo` → `CodeRepo`, `dlt_source` → `DltSource`, anything else →
  `Standard`, and a legacy per-row `dlt` stamp is refused with Python's
  message. Nothing routes on it yet. Unlike Python's revision the migration
  does not move DLT stamps out of `external_metadata`: Rust's DLT route still
  reads them there; Python's revision still moves them on a shared SQLite
  file whoever added the column, so such rows lose their
  `external_metadata` stamp once Python upgrades it. **[BREAKING]** for code
  that builds `Data`,
  `DataInput::DataItem` or `DataRowRefresh` with a struct literal: each gains
  a `system_metadata` field.

- **`cognee-code-graph`: the deterministic enola code graph (SDK-1053).** A
  new crate ports Python's `cognee/tasks/code_graph` mapping at parity pin
  `a59433e6b` (enola 0.5.1). It reads an enola snapshot directory
  (`parse_enola_snapshot`: `facts.jsonl`, `insights.json` findings, and
  `flows.json` when `receipt.json` vouches for it; `validate_receipt` rejects
  an unknown artifact format), maps the facts onto the 15 code node types
  (`map_facts_to_data_points`) and resolves their relations into typed edges
  (`build_code_graph_edges`). Node ids (`fact_node_id`), `fact_hash` values,
  properties and edges match Python byte for byte, checked by goldens Python
  produced. It is the pure half: it runs no subprocess and writes no graph.
  serde_json's `float_roundtrip` is now enabled workspace-wide so a
  17-digit float parses to the same double as Python's `json.loads`.

- **enola runner, code graph writer and `code_graph.*` config (SDK-1053).**
  `cognee-code-graph` gains the runner and writer halves of Python's
  `cognee/tasks/code_graph`. `run_enola_generate` runs `enola --generate
  <repo>` as a subprocess (repository as cwd, update check and prompts off,
  a timeout, Python's not-installed and snapshot error messages).
  `add_code_graph_data_points` / `add_code_graph_edges` write the graph as a
  delta against what is stored (by `fact_hash` and edge key), record
  provenance ledger rows under the owning `Data` row and stamp the
  `CodeRepository` node last. `extract_code_graph` / `load_code_graph` run
  the whole sequence for one local repository. New config section
  `code_graph`: `code_graph.enola_path` (env alias `ENOLA_PATH`; unset means
  `enola` is looked up on `PATH`) and `code_graph.enola_timeout_secs`
  (default `600`), readable as `Settings::code_graph()`. Nothing calls it
  from cognify yet.

- **`cleanup_unused_data` (SDK-1048).** `cognee::api::cleanup_unused_data`
  ports Python's `cognee/tasks/cleanup/cleanup_unused_data.py`: it finds the
  documents not accessed for `minutes_threshold` minutes, or never, and —
  unless `dry_run` (the default) — hard-deletes each through the
  `DeleteService`. It is skipped while `enable_last_accessed` is off and
  until at least one document has been tracked, and returns Python's
  `{status, unused_count, deleted_count, cleanup_date, preview}`. As in
  Python, nothing exposes it beyond the library. It is an operator task:
  `user_id` narrows the selection to that user's datasets and the deletes run
  without an ACL check, so a run without `user_id` deletes every owner's
  unused documents; Python always deletes as the default user, so its
  deletes outside that user's datasets fail. `DeleteDb` gains
  `count_tracked_data` and `list_unused_data` for it.

- **Structured answers with `response_schema`, in search and recall
  (SDK-1043).** `cognee_search::response_schema::ResponseSchema` accepts the
  structural subset Python does (objects, primitives, arrays, enums, unions,
  `$defs` references; at most 10 levels and 200 properties) and refuses the
  rest with Python's `response_schema: …` messages. Completion retrievers
  conform the LLM's answer to it, as Python's model dump does: absent optional
  fields are `null`, undeclared keys are dropped, and a non-conforming answer
  is re-asked through the adapter's repair loop. Checking is strict (no
  string-to-number coercion). `RecallOptions.response_schema` brings it to
  `recall()`, whose normalised entries for such an answer are kind
  `STRUCTURED` with the answer in `structured`.

- **`improve` binding opts `runInBackground`, `buildGlobalContextIndex`,
  `buildTruthSubspace` (SDK-1051).** The TS, Python, Java and C bindings
  forward them, as Python's `improve()` does. With `runInBackground` the op
  resolves the dataset and validates the options first, then returns a
  `running` result at once (`background: true`, `finished: false`, no stages,
  the resolved `datasetId`/`datasetName`) and the run continues as a
  process-wide background task (no per-run handle). An unknown or unwritable
  `datasetId` is a validation error up front instead of a logged failure of
  the background run. The CLI gains `--build-global-context-index` and
  `--build-truth-subspace`.
- **`cognee::api::improve_in_background` (SDK-1051).** The library entry point
  of Python's `improve(run_in_background=True)`: it validates the improve
  configuration and `feedback_alpha`, resolves the dataset with the write-level
  rules (a name resolves or is created for the caller; a missing id is
  `InvalidArgument`, an unwritable one `PermissionDenied`), spawns the run on
  the process-wide background registry and returns the `running`
  `ImproveResult`. `OwnedImproveParams` is the owned counterpart of
  `ImproveParams`; `improve_in_background_then` runs a host's post-run work in
  the same task. `AddPipeline::acl_db()` exposes the wired ACL store.
- **`waitForBackgroundTasks` in every binding (SDK-1051).** TS
  (`Cognee.waitForBackgroundTasks({timeoutSeconds?})`), Python
  (`Cognee.wait_for_background_tasks({"timeout_seconds": …})`), Java
  (`Cognee.waitForBackgroundTasks(WaitForBackgroundTasksOptions)` →
  `BackgroundTasksResult`) and C (`cg_sdk_wait_for_background_tasks`) wait for
  the process-wide background tasks — a `runInBackground` improve, a session
  remember's automatic improve — and report `{finished, pending}`.

### Changed

- **The code routes task is named after the route that ran (SDK-1703).**
  cognify's one task for both code routes is now `extract_code_files_graph`
  (`EXTRACT_CODE_FILES_GRAPH_TASK_NAME`, Python's CODE-route task) when every
  code-routed item of the run is a per-file code item, so telemetry tells the
  two routes apart as Python's does. A run with a code repository (a mixed
  run included) and `build_cognify_pipeline` keep `extract_code_repo_graph`.
  `cognee_cognify::tasks::code_routes_task_name` picks the name.

- **The text loader translates line endings as Python does (SDK-1379,
  behaviour change).** `text_loader` now turns `\r\n` and a lone `\r` into
  `\n`, as Python's text-mode read does (a BOM is still kept). A CRLF text
  file keeps its `content_hash`, which hashes the original bytes, so re-adding
  it reuses its row; its stored `text_<hash>.txt`, its `raw_content_hash` and
  its chunk text and ids now match Python. Stored text is reused and never
  overwritten, as in Python (`store_derived_text`, `overwrite=False`), so
  CRLF text a previous version stored keeps being used, with its old
  `raw_content_hash`, by every new add of the same bytes in any dataset,
  until every row that uses it is deleted; cognify chunks it with LF anyway.
  To bring such data to Python parity, delete those rows and add the files
  again.
  The CSV and HTML loaders store the same text as before, but cognify reads
  that stored text back through the text loader, so a `\r\n` kept in it (a
  quoted CSV cell, say) is now chunked as `\n`, as in Python; re-cognify
  changes those chunks and their embeddings. `ExtractTextChunksPipeline`
  translates the stored text the same way before chunking, so it returns
  the chunks cognify does. `universal_newlines` now lives in
  `cognee_utils`, still re-exported from `loaders` and `loaders::code`.

- **cognify records type, structural and ontology ledger rows per document
  (SDK-1694, stage O5a).** Python's `upsert_nodes` / `upsert_edges` stamp the
  `data_id` of the document being processed on every node and edge its walk
  reaches; Rust wrote EntityType nodes and every structural edge with a nil
  `data_id` only, so nothing said which document produced
  `apple -is_a-> fruit`. The ledger now also has a row per producing
  document for: every **EntityType** node (each document that declared an
  entity with it), every structural **`is_a`** edge (each document that
  declared that entity with that type), every **`contains`** / **`is_part_of`**
  / **`made_from`** edge (its chunk's or summary's document), every
  **ontology-only entity** first seen through an ontology lookup, and every
  **ontology edge** whose existing row is nil (the document whose lookup
  brought it in, when it owns both endpoints). `ArtifactProducers` gains the producers behind them
  (`record_entity_type`, `entity_type_chunks`, `type_chunks`,
  `record_ontology_entity`, `ontology_entity_chunks`, `record_ontology_edge`,
  `ontology_edge_chunks`, `produces_edge`, `mark_ontology_first`,
  `is_ontology_first`); `entity_chunks` / `edge_chunks` still name only
  extracting chunks, so a chunk's `contains` links are unchanged.
  - **Deletion is unchanged by this stage, with two intended exceptions**
    (stage O6, above, then dropped the nil rows and made delete count the
    per-document ones). Each of those
    artifacts keeps the nil-`data_id` row it always had, and the
    per-document rows share its slug, so data-scoped deletes (both claimant
    scopes), dataset deletes and run sweeps select the same artifacts
    (pinned, with and without an ontology, over several runs and two
    datasets, by `the_per_document_rows_change_no_delete_selection` and
    `the_ontology_rows_change_no_delete_selection`). Removing types with
    their last document is per-document delete (O6), which drops the nil
    rows. The exceptions, both of which keep an artifact main removed while
    it was still in use:
    - a whole-run sweep of an earlier run no longer removes an EntityType
      node a later run's document in the same dataset also declares (both
      runs wrote one nil row, which kept the earlier run's id);
    - a document that claims an existing ontology edge (below) keeps its
      `EdgeType` point when the document that first wrote the edge is
      deleted or swept; main removed the point while the edge stayed.
  - **Existing ontology edges are claimed.** An ontology edge the graph
    already holds is still not written again, but every document whose
    ontology lookup produced it, and which owns both endpoints, now gets an
    ownership row for it, as an existing extracted edge does (O2 dropped it,
    as Python's ledger path does; Python's in-graph provenance attaches the
    document's ref).
  - An entity only an earlier document's ontology lookup brought in, which a
    later document extracts, now gets the same rows on both save paths (O4b
    wrote its nil row on the per-document path only): the extracting
    document's, the looking-up document's and the nil row. On the whole-run
    `add_data_points` path that is a change: it wrote only the extracting
    document's row. Whether the nil row is kept is decided per document: an
    entity one block of a document looks up and a later block of the same
    document extracts keeps only that document's row, as before.
  - More rows: a structural edge has two ledger rows instead of one, and an
    EntityType one per declaring document plus one. Existing databases keep
    their nil rows; re-cognifying adds the per-document rows beside them.

- **cognify builds the graph per data item, and entities keep several types
  (SDK-1694, stage O1).** Python builds the graph of each data item (or each
  block of `chunks_per_batch` chunks of one) with fresh maps, so within it the
  first type declared for an entity wins and across data items the types are
  combined. Rust built one graph for the whole run, so the first type in the
  run won and every other was dropped. cognify now groups the extracted chunk
  graphs into the same units (`extraction_units`, shared with the custom-schema
  path), expands each with fresh maps (`expand_extracted_graphs_by_unit`) and
  merges them (`merge_expansions`): an entity's types are the union in
  first-seen order, edges are combined by (source, target, label) and every
  producing chunk keeps its ownership row. An entity can therefore have
  several `is_a` edges, and each EntityType is written, indexed and recorded
  in the ledger. The rule lives in two places: graph building keeps the first
  type per unit, and `GraphNodePair::merge_types_from` combines the units, so
  switching to "every declared type, with counts" means keeping every type
  with a count inside a unit as well as summing counts in the merge.

  Deletes caught up in stage O6 (above): deleting a document removes the
  `is_a` edges and EntityTypes only it contributed. Data cognified between
  O1 and O6 has nil-`data_id` rows for them, which still claim them until
  their dataset is deleted.

  Across data items the first one in input order decides an entity's
  description, importance weight and metadata (an ontology individual known
  in one only through the `OntologyIndividual` placeholder gives way to the
  first real extraction of it). This is a deliberate divergence from Python,
  where the last data item to write wins, which with data items running
  concurrently depends on timing. NodeSet membership (`belongs_to_set`) is
  combined across data items. The units are expanded off the async runtime on
  the run's CPU pool (`TaskContext::thread_pool`, or `CognifyConfig::cpu_pool`)
  and merged in input order; ontology lookups are cached for the run.

  An edge endpoint no longer matches a node of another document. Within one
  unit it may still match a node an earlier chunk declared, or a node by its
  name; the new key `cognify.cross_chunk_endpoints`
  (`COGNEE_COGNIFY_CROSS_CHUNK_ENDPOINTS`, default `true`) turns that off,
  which resolves endpoints only within their own chunk and only by node id,
  as Python does. Existing graphs are not migrated: an entity gains further
  types only when its data is cognified again. The visualization payload still names one
  type per entity: the last `is_a` link, as Python's does. COGX export writes
  every `is_a` edge as its own fact.

- **Relationship labels are normalised, and existing edges are found by
  their final ids (SDK-1694, stage O2).** cognify stores an extracted edge's
  label the way Python does (`generate_edge_name`: lowercase, spaces to
  underscores, apostrophes dropped), so `Born In` and `born_in` are one
  `born_in` edge. It used to store the model's label as given. Before
  writing, cognify asks the graph which of the edges it built are already
  there, on the edges' final ids and labels, as Python's
  `find_existing_edge_identities` does; those are claimed (an ownership row,
  no graph write). The check used to derive the ids from the model's node ids,
  so it missed every edge whose entity is named differently from its node id
  (node `Rostov` named "Nicholas Rostov"), every edge to a Timestamp and every
  edge to an entity the ontology renamed. Those edges were written again on
  every run: no duplicate (the write is an upsert), but a second write, the
  stored edge text overwritten and the edge re-embedded. With
  `cognify.cross_chunk_endpoints=false` an endpoint now matches only the
  verbatim node id, as in Python: node ids `A` and `a` are two nodes.

  Labels such as `Is A` or `Contains` now become `is_a` and `contains`, the
  names of the structural edges, as in Python. Between two entities they stay
  extracted edges: their ledger rows carry the producing document, a
  `contains` row is keyed on its edge text (as Python's `upsert_edges` keys
  every `contains` edge). The graph stores' type lookup
  (`get_entity_type_names`) only follows an `is_a` edge to an EntityType; the
  visualization, like Python's, takes any `is_a` link's target name as the
  entity's type.

  Ontology edges go through the same check. Before, it never matched them,
  so every run wrote each ontology edge again and added a ledger row for it
  (with a nil `data_id`) under its own user and dataset. Now an ontology edge
  the graph already holds is skipped and gets no new row, as on Python's
  ledger path. So when two datasets share one graph store and ontology, the
  second records no row for an ontology edge the first one wrote, and the
  edge survives deleting the first dataset's data, because a nil-`data_id`
  row is only retired through an endpoint exclusive to the deleted item.
  Stage O5a (above) then adds the per-document rows: every document whose
  ontology lookup produced such an edge, and which owns both endpoints,
  claims it with a row of its own beside the first one.

  Graphs built before this change are not migrated. Their edges, ledger rows
  and `EdgeType` points keep the raw label and stay consistent with each
  other. Cognifying the same data again adds a normalised copy of every edge
  whose label was not already normalised (`works at` gains `works_at`), with
  its own ownership row; the raw copy stays until the data that produced it
  is deleted. Only a copy with no description gets a new `EdgeType` point
  (its default edge text, "{source} {label} {target}.", names the new label);
  one with a description shares the point keyed on that text.

- **Structured output asks for constrained decoding by default (behaviour
  change).** `LLM_STRUCTURED_OUTPUT_MODE=auto`, the default, now sends a strict
  `response_format: json_schema` request first, as Python's default
  `litellm_native` path does, and falls back to the `tools` → `functions` →
  `json` cascade when the endpoint refuses that shape or the answer is unusable.
  An endpoint that refuses it (Baseten's `gpt-oss-120b` answers HTTP 501) pays
  two rejected requests (strict, then non-strict) per schema per process.
  `json_schema` is now an alias of
  `auto`; set `tools`, `functions` or `json` to pin one shape as before. An
  unusable constrained answer falls back at once instead of being re-asked.
  Requests name the schema, tool or function after the schema's `title` (for
  example `KnowledgeGraph`) instead of `extract_structured_data`, which stays
  as the fallback. Generated schemas keep their definitions under `$defs`
  rather than `definitions`, and `required` lists properties in declaration
  order rather than sorted.

- **`cognify_datasets` and `cognify_dataset_refs` write no extra
  `pipeline_runs` row (SDK-946).** Each run now leaves only the executor's
  `STARTED` → `COMPLETED`/`ERRORED` pair, as Python's per-dataset loop does;
  the loops no longer append a dataset-level `COMPLETED` row after every run.
  `cognify_dataset_refs` given only `DatasetRef::ById` refs now cognifies just
  those datasets: it used to resolve an empty name list as "every dataset" as
  well, cognifying the caller's other datasets and the named ones twice. Both
  functions now also run a dataset reached more than once (by name and by id,
  or by a repeated ref) only once, returning one result for it.

- **Bedrock: `LLM_MAX_COMPLETION_TOKENS` bounds calls that pass no budget
  again.** A call without a budget of its own — cognify's graph extraction
  passes `max_tokens: None` — now sends `inferenceConfig.maxTokens =
  min(llm_max_completion_tokens, model cap)` instead of omitting it, as the
  Anthropic and OpenAI-compatible adapters already do. This reverses the
  "absent caller budget sends no `maxTokens`" entry below. Omitting it let a
  stochastic runaway extraction run to the model maximum (64000 output tokens
  for Claude Sonnet 4.5) with `LLM_MAX_COMPLETION_TOKENS=16384` configured,
  and the truncation re-ask paid that again; in the same production run no
  well-formed extraction out of 26068 exceeded 11170 output tokens.
  **Behaviour change:** with the default ceiling (16384) Bedrock extraction is
  now capped at 16384 output tokens. To keep the model's full output budget, set
  `LLM_MAX_COMPLETION_TOKENS` to it (64000 for Sonnet 4.5); values above the
  model cap are clamped to it. This intentionally diverges from Python, which
  sends no `maxTokens`.
  The truncation re-ask never raises the budget above the ceiling either (#190
  re-asked up to the model maximum). A dense answer longer than the ceiling
  now fails after its re-asks, so raise `LLM_MAX_COMPLETION_TOKENS` for dense
  corpora; with `COGNEE_COGNIFY_ROLLBACK_SCOPE=failed_items` one such chunk
  drops one document, not the run.
  A model missing from the Bedrock caps table is bounded by the ceiling alone,
  not by the table's 4096-token fallback guess, and the adapter logs a warning
  at construction.

- **Storing an agent trace step now extracts agent context, as Python
  (SDK-1048).** While `auto_feedback` is on, `add_agent_trace_step` runs
  Python's `_maybe_extract_agent_context` after the step is stored: an errored
  step with an error message becomes an `agent`-profile `failure_lessons`
  entry (no LLM), and, with an LLM wired, the batch pass distils the steps
  newer than the shared watermark once `TRACE_EXTRACTION_INTERVAL` (10) of
  them have piled up. It fails open (a warning; the trace write still
  succeeds) and shares its watermark with improve's `extract_agent_context`
  stage, so the two never extract the same steps twice. The pending check
  counts the session's steps instead of reading them
  (`SessionStore::count_trace_steps`, a `COUNT` / `LLEN` on the relational
  and Redis stores) and reads only the extraction window when a pass is due
  (`read_last_trace_steps`), as Python's `get_agent_trace_count` does;
  `SessionManager::get_agent_trace_session(last_n)` uses the same tail read.

- **`AUTO_FEEDBACK` now gates the LLM summary of agent trace steps
  (SDK-1048).** With `auto_feedback` off, a trace step that asks for LLM
  feedback (`generate_feedback_with_llm`) stores the deterministic
  `"<origin> succeeded."` / `"<origin> failed. …"` line and makes no LLM call,
  as Python's `SessionManager.add_agent_trace_step` does.
  `SessionManager::with_auto_feedback` / `auto_feedback_enabled` carry the
  switch; `Settings::session_manager(store)` builds a manager configured from
  the settings, and the bindings and CLI use it.

- **pgvector indexes 3072-d collections (ENG-33).** `text-embedding-3-large`
  collections used to be left without an HNSW index — the adapter capped
  indexing at 2000 dimensions — so every search of them read every stored
  vector: the Rust BYOC 100 MB run measured a hybrid-search median of 8.4 s
  against ~3 s for the indexed Python store, and probe searches timed out. The
  ceiling is now pgvector's own 4000 for the half-precision index (2000 on a
  pgvector below 0.7), as in the Python SDK (topoteretes/cognee#6158). Searches
  of collections wider than 2000 dimensions run with `SET LOCAL
  enable_seqscan = off`, because those vectors live in TOAST, the planner does
  not cost it, and it otherwise picks a sequential scan that casts every vector
  to fp16 (6x the exact scan) over the index on collections below ~9 000 rows.
  Over 12 000 clustered 3072-d rows a top-100 search is 3.2-7.6 ms on the index
  against 96 ms exact. Writes into such collections now maintain an index,
  which the bulk-load deferral absorbs during cognify.

  **Existing 3072-d collections stay unindexed until `cognee-cli
  vector-reindex` runs** (online, idempotent). Until then the adapter finds the
  missing index on its first `has_collection` of the collection, warns once
  naming `vector-reindex`, and searches it exactly, as before; it re-checks a
  minute later, so an index built by another process is picked up without a
  restart. The same check now covers a collection carrying only the legacy
  full-precision index, which used to be searched by a sequential scan ordered
  in fp16.

- **`feedback_weights` improve stage matches Python's bookkeeping (SDK-1051).**
  - Implicit ratings count: a rated answer's weight update also reads the
    `kind = "feedback"` session-context rows (`referenced_qa_rating` /
    `referenced_qa_ids`, latest row wins) and moves the elements at
    `alpha * 0.5`; an explicit `feedback_score` still wins. The reader is shared
    with `update_user_preferences` (`cognee_session::build_implicit_rating_map`,
    keyed by the newly public `cognee_session::qa_entry_uuid`, so a
    `referenced_qa_ids` entry matches its turn however the `qa_id` is spelled:
    uppercase or non-hyphenated UUID, or a foreign non-UUID id).
  - Each row records `feedback_weights_applied_node_ids` / `_edge_ids` /
    `_score` / `_source`, `feedback_weights_attempts` and
    `feedback_weights_pruned_ids` (only this stage's keys are written). A retry
    moves only the ids that did not move yet; a row re-rated with another score
    or source starts over; ids missing from the graph keep the row pending, and
    a row is marked done after `FEEDBACK_WEIGHTS_MAX_ATTEMPTS` (3) attempts.
  - A rated row with no usable element id is marked done (`true`, was `false`).
    A graph or session-store error on one row persists its bookkeeping, counts
    the row as skipped and no longer aborts the stage.
  - `FeedbackApplyResult` gains `done`, `errored`, `elements_moved`,
    `elements_pruned` and `elements_failed`; the stage's counts gain
    `entries_skipped`, `entries_done`, `entries_errored` and `elements_*`.
    New public API in `cognee_cognify::memify`: `extract_feedback_qas`,
    `apply_feedback_weights`, `resolve_feedback`, `FeedbackItem`,
    `FeedbackSource`. Edge ids keep the `source|||target|||relation` format.
  - A QA entry whose stored `qa_id` is not a lowercase hyphenated UUID (a
    foreign writer's id, or an uppercase UUID) reads back as a stable id and
    `update_qa` matches on it in the fs and redis stores, so its bookkeeping is
    saved instead of failing with `MetadataNotUpdated` on every run.
  - `apply_feedback_weights_pipeline` / `extract_feedback_qas` reject an empty
    `session_ids` list or an empty session id with
    `FeedbackError::InvalidSessionIds`, as Python does.

### Breaking changes

- **Several types per entity (SDK-1694).** `cognee_models::Entity` gains
  `extra_types: Vec<Uuid>` (the types after `is_a`; since stage O6 never
  serialised, the `is_a` edges are the stored type set) and `GraphNodePair` gains
  `extra_entity_types: Vec<EntityType>`. A struct literal of either must add
  the field; `GraphNodePair::new(entity, entity_type)` builds a single-typed
  one. `GraphNodePair::entity_types()` iterates every type. `CognifyConfig`
  gains `endpoint_scope` (`EndpointScope::Unit` by default) and `cpu_pool`
  (`None` by default; the pipeline fills it from the task context). A reader that
  assumes one `is_a` edge per entity should read all of them.

- **Source files are read by `code_loader` (SDK-1053, SDK-1379).** A file
  with one of enola's extensions (`loaders::code::SUPPORTED_CODE_EXTENSIONS`,
  Python's `SUPPORTED_CODE_EXTENSIONS`: `.py`, `.rs`, `.ts`, `.go`, ...) is
  claimed by name by the new `code_loader` (`loaders::code::CodeLoader`,
  registered as `"code"` in `LoaderRegistry::default_registry`), as Python's
  `code_loader.py` does. Its text is strict UTF-8 with Python's universal
  newlines and is stored as `code_<content_hash>.<ext lowercased>` instead
  of `text_<content_hash>.txt`; `loader_engine` is `code_loader` and the row
  is stamped `system_metadata = {"source": "code"}` unless the item carries
  its own stamp. `get_loader_name` returns `code_loader` for these
  extensions. `preferred_loaders = ["text_loader"]` keeps the old plain-text
  reading. cognify loads such a row down the per-file CODE route (see
  "The per-file CODE cognify route" under Added). Project directories are not affected.
  **cognify now needs the `enola` binary (0.5.1) for such rows**: on `PATH`,
  or named by `code_graph.enola_path` (`ENOLA_PATH`). Without it the first
  code file fails its item, and under the default fail-fast policy the whole
  cognify run, so the dataset's other documents are not cognified either.
  Install enola wherever cognify runs (Python ships it as its core
  `enola-cli==0.5.1` dependency; the Rust crates do not bundle it), or keep
  the old reading with `preferred_loaders = ["text_loader"]`. Cloning git
  URLs (`remember(content_type="code")`, `add(<GitHub/GitLab URL>)`) also
  needs `git` on `PATH`. The file's text is no longer chunked or embedded,
  and Rust has no CODE search type yet (SDK-1056).

- **A `Binary` upload is stored as it arrived (SDK-1592).** An add of
  `DataInput::Binary` (bare or in a `DataItem`; the `binary` input of every
  binding) now keeps the upload at `<content_hash>/<file name>`, as Python
  does, and records its `file://` URI as `original_data_location`, which used
  to be the marker `binary://<name>`. The file name is the part of `name`
  after the last `/` or `\`, and the add now fails with
  `IngestionError::InvalidUploadName` when that is empty, `.` or `..`, holds
  a NUL byte or is longer than 255 bytes; such names used to be accepted.
  Delete removes the upload, and its emptied `<hash>/` folder, once no row
  points at it. A re-add that moves a row on to other files (the same bytes
  under a new name) removes the files it left once no row points at them, as
  Python's `ingest_data` does. Rows that say `binary://<name>` are corrected
  when the same upload is added again. **Rust API:** `persist_data`,
  `persist_data_with_acl`, `persist_data_with_acl_and_locks`,
  `make_persist_data_task` and `make_persist_data_task_with_acl` take the
  storage as their second argument (first for the task builders), and
  `IngestDb` gains the required `count_data_location_references`.

- **Code repository re-adds, LLM errors and extractor checks (SDK-1053
  review of #466).**
  - The code repo manifest's data id is pinned to `(user, dataset, repo
    path)`, Python's `get_unique_data_id("code_repo:{repo_path}", user,
    dataset_id)`: re-adding a repository refreshes its one row instead of
    adding a row per version. A changed repository's row takes the new
    content and loses its cognify marker, so the next cognify reloads its
    code graph; an unchanged re-add keeps the marker. `addAndCognify` in the
    bindings now also cognifies such a refreshed duplicate (any duplicate
    with no `pipeline_status`).
  - `cognee add` (CLI) fails when the LLM configuration cannot be resolved
    (for example a misspelled `LLM_PROVIDER`), as `add-and-cognify` and the
    bindings do, instead of treating it as "no LLM" and dropping a code
    project's documents.
  - A cognify whose items all take a code route (no extractor needed) now
    validates a `graphBackend` or non-blank `extractor` the caller passes
    (shape, and registered); the configured default is still not checked,
    so keyless code-only cognify keeps working. New
    `graph_backend::check_explicit_graph_backend`.
  - `add(<project dir>)` no longer walks hidden or `SKIP_DIRS` directories:
    `RepoPartition::skipped` lists such a directory once instead of every
    file under it. An unreadable subdirectory or file, or one that vanishes
    mid-walk, is skipped with a warning (Python's `rglob` skips what it
    cannot list) instead of failing the add.
  - Code graph loads ledger every node and edge of the snapshot under the
    loading `Data` row (idempotent upserts), not only what they wrote, so
    deleting another row that loaded the same repository keeps the nodes
    this one still needs. They read only the snapshot's own nodes and
    candidate edges (`get_nodes` / `has_edges`) instead of the whole graph,
    run parsing, mapping and edge resolution in `spawn_blocking`, serialise
    concurrent loads of one repository in the process (cross-process
    locking is not done), and resolve a relative `code_graph.enola_path`
    against the process's working directory.
  API: `CodeGraphLoadState::existing_edge_keys` is replaced by
  `added_edge_keys` and `existing_nodes` now holds only the snapshot's
  nodes; `ParsedSnapshot` gains the public fields `facts_digest` and `edges`
  (struct literals need them) and `ParsedSnapshot::identity`;
  `find_enola_binary` returns an absolute path for a relative setting. New:
  `cognee_models::code_repo::{SOURCE_CODE_REPO, CODE_REPO_MANIFEST_KIND}`
  (the previous paths re-export them), `cognee_ingestion::generate_unique_data_id`,
  `code_repo::{code_repo_data_id, code_repo_stamp_path}`, and
  `cognee::add::{code_repo, generate_unique_data_id}`.

- **`add(<directory>)` and the CODE_REPO cognify route (SDK-1053).** A
  `DataInput::FilePath` naming a directory is now expanded on `add` instead
  of failing: a code project (Python's `detect_code_project`) becomes one
  repo manifest item stamped `system_metadata.source = "code_repo"` plus its
  documents, any other directory is flattened into its files. cognify runs a
  repo item down the deterministic code graph (`cognee-code-graph`'s
  `extract_code_repo_graph`, no LLM, no embedding) in the same run as the
  standard items; a repo item that fails to load is a per-item failure
  under the failure policy (`FailureStage::CodeGraph`), and one that loaded
  counts as a survivor. A DLT source stamped in `system_metadata` and a
  legacy `dlt` row fail the whole run. Without an
  LLM (`AddPipeline::with_llm_available(false)`, set by the services and the
  CLI) a code project's documents are left out at add and the repo item
  cognifies keyless. The manifest's data id is pinned to (user, dataset,
  repo path) as in Python, so a re-add updates one record (see the next
  entry).
  API: `CognifyConfig.code_graph` is a new public field (struct literals
  need it; `..CognifyConfig::default()` covers it); new variants on enums
  that are not `#[non_exhaustive]`: `CognifyError::{Route, CodeGraph}`,
  `CodeGraphError::InvalidRepoItem`,
  `CognifyRouteError::{UnsupportedRoute, InvalidNodeSet}` and
  `FailureStage::CodeGraph`; new `AddPipeline::with_llm_available`,
  `routing::routes_need_llm`, `tasks::run_code_routes` /
  `make_code_routes_task` / `RoutedCognifyInput` /
  `make_classify_routed_documents_task`, and
  `FailureReport::{note_routed_items, routed_items}`.

- **`token_count` is `null` until cognify runs, in every binding (SDK-951).**
  The data rows returned by `add`, `addAndCognify` and `listData` used to
  carry `token_count: -1` until `cognify` had counted the item, a sentinel
  the types did not express. They now carry `null` (TS `number | null`,
  Python `None`, Java `Long tokenCount` on `CogneeData`, JSON `null` in the
  C API). `addAndCognify` now returns the count cognify computed for the
  newly added rows instead of the pre-cognify value (still `null` for an
  item cognify could not count, such as one that is blank after trimming).
  The stored column keeps `-1`, for database parity with Python.
  **Java migration:** the `CogneeData` record gains a third component, so
  its canonical constructor is now `new CogneeData(id, name, tokenCount)`
  and record patterns need three bindings
  (`case CogneeData(var id, var name, var tokenCount)`).
  Closes gh #99 and gh #200.

- **Chunker selection is real (SDK-1042).** `chunk_strategy` (and the CLI's
  `--chunker`) now chooses the chunker cognify cuts documents with:
  `PARAGRAPH`/`TextChunker` (default), `RECURSIVE`/`LangchainChunker`,
  `CSV`/`CsvChunker`, `JSON_LIST`/`JsonListChunker` and
  `OVERLAP`/`TextChunkerWithOverlap`. Before, every value except the default
  was parsed and ignored. **`RECURSIVE` changes behaviour:** it cuts
  1024-word splits with `chunk_overlap` words of overlap and, like Python's
  `LangchainChunker`, fails a document whose split exceeds `max_chunk_size`
  tokens — with a ~512-token local embedding budget that is most documents
  over ~400 words. `chunk_overlap` is now read (by `RECURSIVE` only) and must
  be at most 1024 there. Every chunk carries `DocumentChunk.chunker_id`.
  API: `CustomChunker` now wraps `Arc<dyn cognee_chunking::Chunker>`
  (`with_custom_chunker(closure)` still works; `with_chunker` takes a
  `Chunker`); `ChunkStrategy` gains `Csv`, `JsonList` and `Overlap`;
  `extract_chunks_from_documents` and `make_extract_chunks_task(_with_rank)`
  take the chunker as a new argument; `CognifyConfig` gains
  `chunk_overlap_ratio`. CLI: `--chunker` no longer defaults to
  `TextChunker` (omitting it now defers to `chunk_strategy`) and accepts
  `JsonListChunker`.

- **`improve()` follows Python 1.6.7 stage defaults (SDK-1206).**
  `review_conflicts` and `global_context_index` now run by default
  (`IMPROVE_REVIEW_CONFLICTS` and the new `IMPROVE_GLOBAL_CONTEXT_INDEX`,
  both `true`). When off they report the skip reason `turned_off` instead of
  `opt_in_disabled` (`build_truth_subspace` keeps `opt_in_disabled`).
  `ImproveParams::build_global_context_index` is now `Option<bool>` and
  `ImproveParams` / `OwnedImproveParams` gain `review_conflicts: Option<bool>`;
  `None` follows the setting, an explicit value wins in either direction.
  The automatic improve after `remember()` no longer hardcodes the index off,
  so the setting decides. `ImproveConfig` gains `global_context_index`.
  CLI: `--build-global-context-index` takes an optional `=BOOL` (bare flag
  still means on) and `--review-conflicts[=BOOL]` is new. Bindings: the
  `buildGlobalContextIndex` opt, when omitted, now follows the setting, and
  `reviewConflicts` is new. Set `IMPROVE_GLOBAL_CONTEXT_INDEX=false` and
  `IMPROVE_REVIEW_CONFLICTS=false` to keep the old behaviour.
  `review_conflicts` gate order is now `turned_off`, `no_llm_configured`,
  `edge_evidence_disabled` (with an LLM and `EDGE_EVIDENCE_ENABLED=false`;
  previously `not_ported`), then `not_ported`. The CLI also accepts Python's
  `--no-review-conflicts` spelling.

- **Optional `DataItem` label and a checked `response_schema` (SDK-1043).**
  - `DataInput::DataItem.label` is `Option<String>` (was `String`). `None`
    sets no label and, on a re-add, keeps the stored one, as in Python; wrap
    an existing label in `Some`.
  - `SearchError` gains `InvalidResponseSchema`: exhaustive matches need a
    new arm.
  - `RecallOptions` gains `response_schema`: struct literals without
    `..Default::default()` no longer compile.
  - A `response_schema` outside the supported subset is now refused before
    the search runs (it used to reach the LLM as-is).

- **`feedback_weights` public API (SDK-1051).**
  - `FeedbackApplyResult` has new public fields (`done`, `errored`,
    `elements_moved`, `elements_pruned`, `elements_failed`): struct-literal
    construction no longer compiles.
  - `FeedbackError` gains the `MetadataNotUpdated` and `InvalidSessionIds`
    variants: exhaustive matches need new arms.
  - `cognee::api::improve::build_rating_map` now reads only `kind = "feedback"`
    rows (it shares `build_implicit_rating_map`, which filters like Python's
    reader); other rows are ignored.
  - A rated row with no usable element id persists
    `feedback_weights_applied = true` (was `false`).

- **`SessionQAEntry.memify_metadata` holds JSON values, not only bools (SDK-1051).**
  - The field (and `SessionQAUpdate.memify_metadata`) changes from
    `Option<HashMap<String, bool>>` to `Option<HashMap<String, serde_json::Value>>`;
    the TypeScript type is `Record<string, unknown> | null`. Python stores lists
    and ints there (e.g. `feedback_weights_applied_node_ids`); the old bool-only
    decode dropped the whole map when it met one, losing
    `feedback_weights_applied` and the `preference_*` flags. Read flags with
    `Value::as_bool`. A stored non-object value still decodes as `None`.

- **`remember` reports its automatic improve as Python does (SDK-1039).**
  - `RememberResult::improve_result` (formerly `#[serde(skip)]`) is renamed
    `improve` and serializes the `ImproveResult`; `improve_error`,
    `after_run_error` and `improve_skipped` are new. Each is serialized only
    when set. A failed improve (the error `improve()` returned, or its errored
    stages, as Python's `_improve_error_text`) is reported on `improve_error`
    and no longer on `error`. A session-mode improve runs detached, so `improve`
    stays `None` there (`improve_launched`).
  - **`selfImprovement` now defaults to `true` in every binding** (Python's
    `self_improvement=True`); pass `false` to keep the old behaviour.
  - New `register_auto_improve_admission` / `clear_auto_improve_admission`: a
    host check that can decline the automatic improve with a reason, reported as
    `RememberResult::improve_skipped` (Python's admission hook).
    `launch_session_auto_improve` asks it too, so a host that launches a
    session improve itself is gated the same way; it now returns a
    `SessionAutoImproveLaunch` (`launched`, and the check's `skipped` reason)
    instead of `bool`.
  - The bindings' improve opts accept `datasetId` (a UUID string), as
    Python's `improve(dataset=<UUID>)`: the improve targets that dataset,
    possibly one shared with the caller, instead of looking `datasetName` up
    among the caller's own datasets. A non-string or malformed id is an error.

- **`cognee-cognify`, `cognee-models`, `cognee-search`: DLT rows are `DltRow`
  nodes indexed into `DltRow_text`, with Python's schema and cell-value
  collections (SDK-1045).**

  | Item | Before | After |
  |---|---|---|
  | Graph type of a DLT row's chunk | `DocumentChunk` | `DltRow` (`DocumentChunk::dlt_row`, `cognee_models::DLT_ROW_TYPE`), `cut_type` `dlt_row`, `name` `table:primary_key_value` |
  | Vector collection of a DLT row | `DocumentChunk_text` | `DltRow_text` (payload: the DataPoint dump, `field`, `text`, `document_id`, `dataset_id`, `chunk_index`, plus Rust-only `name`, `cut_type`, `chunk_size` that the hybrid lane uses; Python's `document_name` is not written and `external_metadata` stays with SDK-1057) |
  | Source of `is_row_of`, foreign-key and cell-value edges | the row's document node | the row's `DltRow` chunk node (`dlt_row_chunk_id(document_id)`) |
  | Schema nodes in the vector store | none | `SchemaTable_{name,description}`, `SchemaRelationship_{name,description}` |
  | Cell values (`external_metadata.column_values`) | ignored | one shared `DltColumn` node per unique (table, column, value), embedded into `DltColumn_properties`, with an edge named after the column |
  | Graph completion (`SEARCH_COLLECTIONS`) | no DLT rows | also searches `DltRow_text` |
  | `DocumentChunk` serialization | no `name` | optional `name` (set only on DLT rows) |
  | `SchemaTable.description` / `row_count_estimate` | `DLT-ingested relational table 'X' from database 'db'.` / none | Python's `Relational table 'X' from db with N rows, loaded through dlt.` (N counts the rows of the cognified batch) / `N` |
  | `cognee_cognify::extract_dlt_fk_edges` | `(chunks, documents, graph_db, db, id)` | `(documents, chunks, graph_db, vector_db, embedding_engine, db, id)` |

  The hybrid chunk lane reads `DltRow_text` whenever it exists, and the entity
  lane then adds `DltColumn_properties`, `SchemaTable_name` and
  `SchemaRelationship_name` (that lane was already in place; this change is
  the producer that fills it).

  **Migration.** None is provided. A dataset cognified before this change has
  its DLT rows in `DocumentChunk_text` and as `DocumentChunk` nodes with
  edges on the document node; re-run `cognify` on it (after deleting the old
  graph and vector rows) to get `DltRow` rows, schema and cell-value
  collections. Callers of `extract_dlt_fk_edges` pass the run's chunks (only
  rows that produced a `DltRow` chunk get row-level edges) plus the vector
  store and embedding engine.


- **`cognee-search`: session-turn preparation follows Python's capability
  model; the legacy feedback detector is gone; `summarize_context` is removed
  (SDK-1045).**

  | Item | Before | After |
  |---|---|---|
  | Which retrievers prepare a session turn | a fixed list of search types | `SearchRetriever::supports_session_turn_preparation()` (provided, default `true`, as Python's `BaseRetriever`); chunks, lexical, summaries, Cypher, natural language and graph report opt out, as do feedback and feeling-lucky (not Python `BaseRetriever`s) |
  | `CodingRules`, custom / community retrievers in a session search | no turn preparation | prepare turns |
  | Opted-out retriever in a session search | one legacy `detect_feedback` LLM call, feedback written to the prior QA row | no analysis call, no feedback written (Python's `detect_feedback` has no callers) |
  | `cognee_search::detect_feedback`, `FeedbackDetectionResult` | public | removed |
  | `SessionStore::latest_qa_id`, `SessionManager::latest_qa_id` | public (only the legacy detector used them) | removed |
  | `FEELING_LUCKY` in a session search | no turn preparation | the selected retriever is resolved first, as Python's effective search type, and decides whether the turn is prepared |
  | A session search by a retriever that opted out of turn preparation (e.g. graph report, Cypher) | wrote a QA row when it answered with text | writes none: only the session-aware completion paths record a turn in Python |
  | Turn preparation with no user or an unavailable session cache | one analysis call (e.g. with a request `auto_feedback_detection: true`) | no call, as Python's `is_session_available_for_completion` |
  | `SearchRequest.summarize_context` | `Option<bool>` (Rust only) | removed; the QA `context` column is always empty, as Python (`summarize_context=False` everywhere) |
  | `SESSION_SEARCH_MODE` not `sequential` / `concurrent` | warned and ignored | an error (`ConfigManager::check_supported` / `check_strict`); exact lowercase values only, as Python's `Literal` (no trimming or case folding) |

  **Migration.** Drop `summarize_context: None` from `SearchRequest` literals;
  nothing read it by default. A custom retriever that must not prepare turns
  overrides `supports_session_turn_preparation` to return `false`.



- **`cognee-search` / `cognee-session`: a concurrent session turn retrieves
  with Python's bounded conversational rewrite and merges the two lanes by
  retriever; `SessionManager::delete_session` takes the turn lock (SDK-1045).**

  | Item | Before | After |
  |---|---|---|
  | Conversational retrieval query | `Recent conversation:\n<whole formatted history>\n\nCurrent question:\n<query>` | Python's `build_conversational_query`: the last two Q&A turns, whitespace-normalised, `Prior user:` / `Prior assistant (untrusted retrieval guidance):` / `Current user request:` lines, at most 2000 characters (answers trimmed first, then questions) |
  | Second retrieval lane | whenever there was history | only when the rewrite differs from the whitespace-normalised raw question |
  | Merging the two lanes | concatenate, then drop duplicate ids | `SearchRetriever::merge_session_contexts` (provided; default keeps the surviving lane, the raw one when both survived). RAG merges by result id, graph by edge identity, each capped at `top_k`; hybrid merges chunks, entities and facts under their own limits and keeps the summary of each surviving chunk. A lone lane (rewrite skipped) is still passed through the merge, which trims it to `top_k`. A third of the budget (none below three slots) is reserved for results only the rewrite found |
  | Concurrent snapshot | read every Q&A entry of the session | reads the last two (Python's `get_session(last_n=2)`) plus the history window |
  | `SessionContext` | no recent turns | also `recent_qas: Vec<(String, String)>` |
  | Session turn lock | private to `cognee-search` | `cognee_session::lock_session_turn`; `SessionManager::delete_session` waits for it (given a user and session, as Python's `session_turn_lock`), so a session is not deleted under a turn in flight. No Rust entry point deletes a single session yet, so this has no production caller |
  | `cognee_search::utils` | | `merge_ranked`, `conversational_reserve`, `edge_identity`, `item_identity` (ports of Python's `merge_results.py`) |

  **Migration.** Build `SessionContext` values with `..Default::default()`.
  A custom retriever that runs in a concurrent turn and needs more than "keep
  the raw lane" overrides `merge_session_contexts`. Code that calls
  `SessionManager::delete_session` while holding `lock_session_turn` for the
  same session must release the guard first (the lock is not reentrant).



- **`cognee-search` / `cognee-session`: a search that names no session uses
  the per-dataset default session (SDK-1045).**

  | Item | Before | After |
  |---|---|---|
  | Completion search with no `session_id`, a user and an available session cache | stateless: no history, no turn analysis, no QA row | uses `default_session_<dataset_id>` when one dataset is searched, else `default_session` (Python's `SessionManager.resolve_session_id(None)` with the dataset context): history and guidance are read, the turn is analysed and the QA is saved, in sequential, concurrent and per-dataset fan-out paths |
  | Separate searches of two different datasets, no session | n/a | never share a session: each dataset has its own default session |
  | A search over several datasets, or one naming none (one logical turn) | n/a | the unscoped `default_session`. Intentional divergence: Python's access-control path runs one leg, so one `default_session_<id>`, per dataset (the HTTP router does the same, one leg per dataset), and its non-access-control path scopes to the user's only dataset; `dataset_ids` is de-duplicated first. Under per-dataset storage with exactly one readable dataset the id is that dataset's |
  | Retrievers that opt out of turn preparation (chunks, summaries, lexical, Cypher, ...) | n/a | no default session: no store read, no analysis, no QA row, as Python resolves it only in the session-aware completion retrievers |
  | No user, or session memory off (`session.caching = false`) | stateless | unchanged: stateless, as Python's `is_session_available_for_completion` |
  | `SessionManager` | `resolve_session_id` private, always the global default | `default_session_id_for(Option<Uuid>)`; the default id stays fixed (`default_session`, no setting, as Python) |

  **Migration.** A sessionless completion search now persists a turn and runs
  one analysis call (unless `auto_feedback_detection` is off, or the retriever
  opts out of turn preparation). Turn session memory off with
  `session.caching = false`, or give the search an explicit `session_id`.
  `only_context` previews read the default session but neither analyse nor
  write; batch searches do not analyse but record the turn, as with an
  explicit session.


- **`cognee-search` / `cognee-session`: session searches serve Python's
  active-guidance block and record the entries they served; session history
  moves to the user message (SDK-1045).**

  | Item | Before | After |
  |---|---|---|
  | Session guidance | not rendered; stored `context` entries never reached a prompt | `cognee_session::build_active_context_block` (ranker, 400 / 1200-character budgets, `## Active session guidance` block) is ported; a session search serves it after the question and context |
  | Graph completion variants without a session (summary, context extension, CoT, decomposition) | no preference text | the standalone `## What this user prefers` block, as Python's inherited `GraphCompletionRetriever` completion |
  | CoT reasoning rounds | n/a | see the stored session entries only (no preference lines, nothing with `auto_feedback_detection` off); the returned answer carries the full guidance |
  | A failed guidance build | n/a | serves no guidance at all, preference lines included (Python's builder returns `("", [])`) |
  | `used_session_context_ids` on a QA row | never written, so served-context ratings never applied | an answered turn stores the served entry ids; an acknowledgement turn stores none; `last_served_at` is stamped on the served entries once the answer is recorded (not on `only_context` previews, nor when no answer is produced; an intentional divergence from Python, which stamps while building the block, see `docs/roadmap/not-implemented.md`) |
  | Durable preference lines on a session search | the standalone `## What this user prefers` block | the `Preferences` section of the guidance block, under the shared budgets; a sessionless search keeps the standalone block |
  | Guidance with `auto_feedback_detection` off | n/a | no stored entry is served; preference lines still render through the same block |
  | Session history in the completion prompt | prepended to the system prompt after a `\nTASK:` separator | the user prompt is the history, then the rendered template, then the guidance (`\n\n`-joined); the system prompt is the task template alone, as Python's `build_completion_prompts` |
  | `SessionContext` | `session_id`, `history`, `formatted_history` | also `guidance: Option<ActiveContextSelection>` and `served_context_ids` |
  | `SessionManager::save_qa` | 6 arguments | a seventh, `used_session_context_ids: Option<Vec<String>>` |
  | `build_messages_with_history` / `GraphPrompts::messages` | no guidance argument | take the rendered `guidance` |
  | `append_preference_guidance` | public in `cognee_search::user_preferences` | replaced by `completion_guidance` / `guidance_from` |

  **Migration.** Pass `None` as the new last argument of `save_qa` (or the
  served entry ids); build `SessionContext` values with `..Default::default()`;
  call `completion_guidance` where you appended preference text yourself.


- **`cognee-search`: recall notes a graph that is not built instead of
  searching it.** As Python's recall does, a graph-only recall first reads
  `pipeline_runs` for the datasets the caller can read. When no graph was ever
  built it returns one `system` item with `status: "memory_warming_up"`; when
  the last build failed, `status: "build_failed"` with the error. Previously it
  searched and returned nothing. Next to other sources a cold graph
  contributes nothing, and a caller asking for `only_context` always searches.
  The `recall` section configures it (`RECALL_WARMUP_SHORTCIRCUIT`,
  `RECALL_WARMUP_THRESHOLD`, `RECALL_WARMUP_CACHE_TTL`). Python's `partial`
  state (chunks searchable before extraction ends) is not ported: Rust has no
  indexing pipeline.

  **Migration.** `RecallSource` has a `System` variant, so an exhaustive
  `match` on it needs an arm. An orchestrator is probed only when given a
  `GraphWarmup` (`SearchBuilder::with_graph_warmup`); without one, recall
  searches as before. A recall's graph search is planned once:
  `recall_scope::plan_graph` decides the owner and the search type, and
  `run_graph` takes that `GraphPlan` instead of `query_type`, `auto_route`
  and `user_id`. The guard is `GraphPlan::warmup_note`, which `recall()`
  calls before the graph search; `run_graph` itself never notes. A caller
  that runs its own per-dataset searches plans and calls `warmup_note` once,
  with all the recall's datasets, before the first. On a note it does not
  search: it answers the note when the graph is the one source, nothing next
  to others, and reports the plan's `search_type()`, `auto_routed()` and
  `empty_response()`. A search it runs itself goes through
  `GraphPlan::search_with_fallback`, which retries a failed routed pick as
  the fallback as `run_graph` does, with `only_context(caller)` on the
  request. A skipped search sets `cognee.recall.warmup_shortcircuit`
  and `cognee.recall.warmup_state` on the span `warmup_note` is given. A
  typed `recall()` scope runs a repeated source once (`dedupe_scopes`, shared
  with `normalize_scope`); `Auto` stays, so `[auto, graph]` is not graph-only.

- **`cognee-search`: recall routing honours what the deployment can run.** As
  Python's router does on `dev` (SDK-751, cognee `5ae7523fb`; v1.6.5 still
  answers keyless recall with `CHUNKS`), `RoutingConstraints { llm_available,
  cypher_allowed }` is applied after the rules. With Cypher disabled, a routed
  Cypher type would become the fallback, but no rule routes to one; an explicit
  one is refused by its retriever. With no LLM, `CHUNKS_LEXICAL` and `CODING_RULES` run as they are,
  `HYBRID_COMPLETION`, `GRAPH_COMPLETION`, `RAG_COMPLETION` and
  `TRIPLET_COMPLETION` (in Python a deprecated alias of `GRAPH_COMPLETION`) — routed or
  explicit — run with `only_context`, and any other routed type becomes the
  fallback with `only_context`. Recall reads the constraints from the search
  orchestrator, which `SearchBuilder` sets from its LLM (`Llm::is_configured`)
  and `search.allow_cypher_query`; an orchestrator built by hand allows
  everything unless given `with_routing_constraints`.

  **Migration.** `route_query` takes `&RoutingConstraints` (pass
  `&RoutingConstraints::default()` for the previous behaviour), and
  `RouteDecision` has an `only_context` field.

- **The session graph-context feature is removed; `graph_context` recall is an
  alias of `graph` (SDK-1044).** Mirrors Python 3401da38a ("Remove graph
  session sync").

  | Item | Before | After |
  |---|---|---|
  | `cognee_cognify::memify::sync_graph_session` (`sync_graph_to_session`, `checkpoint_key`, `SyncResult`, `SyncError`) | public | **removed** |
  | `SessionStore` / `SessionManager::{get,set}_graph_context`, `SessionContext.graph_context` | public | **removed**; the fs and Redis stores still delete a legacy snapshot with its session |
  | `SessionQaDb` graph-context ops, `session_graph_context` table | present | **removed**; migration `m20261029_000001_drop_session_graph_context` drops the table |
  | `cognee::session::{get,set}_graph_context` | public | **removed** |
  | Binding graph-context ops (bindings-common `run_{get,set}_graph_context`, C `cg_sdk_{get,set}_graph_context`, Python, TS, Java) | present | **removed** |
  | `RecallScope::GraphContext`, `RecallSource::GraphContext`, `fetch_graph_context` | present | **removed**; `"graph_context"` normalizes to `"graph"` with a deprecation warning and deserializes as `RecallScope::Graph`; `"all"` is graph, session and trace |
  | Session history prefix "Background knowledge from the knowledge graph" | prepended | gone |

- **Agent context, user preferences and personalized retrieval (SDK-1044).**

  | Item | Before | After |
  |---|---|---|
  | `ImproveParams`, `SessionAutoImprove` | `personalization_enabled: bool` | `personalization: cognee_search::PreferenceConfig`, `auto_feedback: bool` |
  | `cognee::api::remember` | `…, personalization_enabled: bool` | `…, personalization: PreferenceConfig, auto_feedback: bool` |
  | Session QA `memify_metadata` updates | replaced the whole map | merge keys (Python's `update_qa`); `feedback_weights` sends only its own key |
  | `SearchRetriever` | — | `previews_empty_context(&params)` (provided; graph completion with the global context index previews the prelude alone) |
  | RAG completion turns | recorded no used ids | record the chunks' ids (`used_graph_element_ids.node_ids`) |
  | RAG personalization | scaled the similarity | scales the distance (`1 - score`), as Python |
  | Hybrid fallback | read only an explicit `feedback_influence` | a positive `DEFAULT_FEEDBACK_INFLUENCE` also defers hybrid to graph completion |
  | `AgentLesson` parsing | exact lowercase section, numeric confidence | section trimmed and lowercased, `null` confidence is 0, clamped to [0, 1] |

  The stages `extract_agent_context` and `update_user_preferences` are ported.
  The preference text was inert on Rust-only data until session-turn
  preparation landed (SDK-1045; historical note, see the entries below).

- **Improve waits on its dataset and sessions; one dataset lock registry;
  deletes and writing improves are changes (SDK-1044).**

  | Item | Before | After |
  |---|---|---|
  | `cognee-session` improve lock | `try_acquire_improve_lock(session_id) -> bool`, `release_improve_lock`, `ImproveLockGuard` (a concurrent improve of the session skipped) | `acquire_improve_lock_many(&keys) -> ImproveClaim` (waits; released on drop) and `improve_lock_keys(sessions, user)` (`session:{user}:{session}`) |
  | `ImproveParams` | — | `+ dataset_locks` (struct literals must set it) |
  | `DatasetLocks` | per-host registries only | `DatasetLocks::global()`, the process-wide registry: the default of `AddPipeline::new` and `DeleteService::new` (was none), and of `improve` |
  | `cognee_cognify::tasks::cognify`, `memify` | took no dataset lock | hold the dataset in `DatasetLocks::global()` for the run (re-entrant for a caller holding it there), as Python's `dataset_lock` |
  | `DeleteService` | wrote no row for a data-item delete | records a `forget` `pipeline_runs` row on every dataset it touched (`FORGET_PIPELINE_NAME`, `CHANGE_RECORD_PIPELINE_NAMES`) when a pipeline-run repo is wired |
  | `improve()` | wrote no row of its own | records one `improve` row (`IMPROVE_WRITE_PIPELINE_NAME`) after its first stage that may have written to the graph |

  `global_context_index` is `already_completed` (`no_writes_since_last_improve`)
  when nothing but reads and host `improve_pipeline` rows ran since its stamp.

- **`improve()` is Python's staged orchestrator and returns its
  `ImproveResult` (SDK-1044).** `cognee::api::improve` runs Python's nine
  stages in registry order and reports one `StageResult` per stage.

  | Item | Before | After |
  |---|---|---|
  | `ImproveResult` | `stages_run`, `memify_result`, `feedback_entries_processed`, `feedback_entries_applied`, `sessions_persisted`, `edges_synced` | Python's shape: `dataset_id`, `dataset_name`, `session_ids`, `stages: Vec<StageResult>`, `background`, `finished`, `error`, `memify_run`, computed `status()`; the old counters are stage `counts` |
  | `ImproveParams` | — | `+ dataset_id` (struct literals must set it); `node_name` and the custom-task fields are deprecated and ignored |
  | `ApiError::ImproveAborted { result, budget_exhausted }` | — | new: the fatal `persist_session_qa` stage errored; carries the partial result |
  | Stages `memify` / `sync_graph_to_session` | ran | removed; `global_context_index` runs last |
  | Binding / C API improve JSON | `{stagesRun, stages, memifyResult, feedbackEntriesProcessed, …}` | `{datasetId, datasetName, sessionIds, stages, background, finished, error, status}` |
  | Binding improve errors | a fatal-stage abort was a plain runtime error | `SdkError::ImproveAborted { message, improve_result }` (code `IMPROVE_ABORTED`): Python `CogneeImproveAbortedError.improve_result`, JS `ImproveAbortedError.improveResult`; C (`CG_ERR_RUNTIME`) and Java append `improve_result: <json>` to the message |
  | CLI `improve --node-name` | scoped the run | deprecated, ignored |

  An exhausted LLM budget is recognised from the typed
  `LlmError::PaymentRequired` / `CognifyError::PaymentRequired` — including a
  budget failure inside cognify, now carried by `FailureReport::budget_exhausted`
  (`record_error`) — and from a stage's rendered error
  (`cognee_llm::is_budget_exhausted_message`, Python's
  `is_budget_exhausted_record`). The temporal extractors keep a 402 typed
  (`CognifyError::PaymentRequired` instead of `FactExtractionError`).

- **`cognee-search`: recall routes with Python's ordered rule router, and an
  unmatched query runs `HYBRID_COMPLETION`.** `route_query` checks three rules
  in order — a quoted phrase → `CHUNKS_LEXICAL`, coding-rules wording →
  `CODING_RULES`, a time preposition before an absolute date → `TEMPORAL` — and
  falls back to `ROUTER_FALLBACK_TYPE` (`HYBRID_COMPLETION`, was
  `GRAPH_COMPLETION`). Recall with `auto_route = false` and no type also runs
  `HYBRID_COMPLETION`. A routed type the backend rejects
  (`SearchError::UnsupportedSearchType`), or a routed `CHUNKS_LEXICAL` /
  `CODING_RULES` search that finds nothing, is retried as `HYBRID_COMPLETION`.
  The recall span records the matched rule as `cognee.recall.route_rule`.

  **Migration.** `route_query` returns `RouteDecision { search_type, rule }`
  instead of `RouteResult`, which is removed with its scores (`confidence`,
  `runner_up`, `runner_up_score`, `all_scores`, `is_confident`). The override
  statistics are removed, as in
  Python: `record_override`, `override_counts_snapshot`,
  `clear_override_counts` and the `query_router_stats` module.

- **`data_per_batch` and `EMBEDDING_ONNX_BATCH_SIZE` are removed; ONNX batches
  by `embedding.batch_size` (SDK-1039).** `data_per_batch`
  (`cognify.data_per_batch`, `COGNEE_DATA_PER_BATCH`,
  `CognifyConfig::data_per_batch` / `with_data_per_batch`) was read by no stage;
  Python's value is a per-call argument with no config or env name, so the key
  only invited settings Rust ignored. `embedding.onnx.batch_size`
  (`EMBEDDING_ONNX_BATCH_SIZE`) is gone: Python's fastembed engine is built
  with the single `EMBEDDING_BATCH_SIZE`, and the Rust ONNX engine now does the
  same (inference batch = `embedding.batch_size`), and also honours
  `embedding.max_concurrent_data_points` for its in-flight batches. Setting a
  removed key is now an unknown-key error / ignored env var. This also removes
  a direct environment read from `cognee-embedding`.

- **The pipeline-cache gate, the reset API and the `AlreadyRunning` gate are
  gone; runs are serialized by a per-dataset lock and a killed run is recovered
  at startup (SDK-1039, Python `dc0b4ad6a`).** Python removed
  `use_pipeline_cache`, `reset_pipeline_run_status` and
  `reset_dataset_pipeline_run_status` with no shim and relies on its
  per-dataset lock, its per-item incremental skip and
  `modules/cognify/recovery.py`. Rust now does the same:
  - Removed: `CognifyConfig::use_pipeline_cache` / `with_pipeline_cache`;
    `cognee::api::{reset_pipeline_run_status, reset_dataset_pipeline_run_status}`
    and their wrappers in the Python, TypeScript (`users.resetPipelineRunStatus`,
    `users.resetDatasetPipelineRunStatus`), Java and C bindings
    (`cg_sdk_reset_*`); `run_info_for_initiated`; the `qualification` module
    (`Qualification`, `check_pipeline_run_qualification`);
    `CognifyError::PipelineAlreadyRunning`; the exclusive-run claim
    (`PipelineRunRepository::{try_claim_pipeline_run, release_pipeline_run_claim,
    get_pipeline_run_claim, try_release_pipeline_run_claim,
    release_all_pipeline_run_claims}`, `PipelineRunClaim`, `CLAIM_STALE_AFTER`
    and the `pipeline_run_claims` table, dropped by a new migration);
    `PipelineRunRepository::reset_orphans` and
    `DefaultPipelineRunRegistry::new_with_orphan_reset`; the
    `cognee-cli pipeline-unblock` command; and the **`single_process` setting**
    (`COGNEE_SINGLE_PROCESS`, `Settings::single_process`,
    `ConfigManager::{set,clear}_single_process`, `cognee_database::*single_process*`),
    whose only job was to gate the unscoped startup sweep that the ownership
    markers below replace.
  - **A failed terminal `COMPLETED` write fails the run (Python `run_tasks.py`,
    where `log_pipeline_run_complete` sits inside the `try`).**
    `PipelineWatcher::on_pipeline_run_completed` now returns
    `Result<(), WatcherError>`; on `Err`, `execute` reports the new
    `ExecutionError::RunRecordFailed`, writes the `ERRORED` row and the caller
    rolls the run back like any failed one. The run stays `STARTED` (marker kept
    for startup recovery) only if the `ERRORED` write fails too. Custom
    watchers return `Ok(())`.
  - A second `cognify` of a dataset **waits** on the dataset's lock
    (`cognee_core::DatasetLocks::global()`, shared with `add` and `delete`)
    instead of failing with `PipelineAlreadyRunning`. Re-cognifying a complete
    dataset still skips every item, and now writes its own `STARTED` /
    `COMPLETED` pair, as Python's run does.
  - A `cognify` or `add` run writes `{"recovery_lock": "<token>"}` in its `STARTED` row and holds an
    OS file lock on `<system_root>/pipeline_run_locks/<token>/owner.lock`
    (`cognee_core::RunOwnership`; `Pipeline::recovery_lock`,
    `PipelineRunInfo::recovery_lock`, `CognifyConfig::run_ownership_dir`,
    `run_info_for_started(Option<Uuid>)`, `AddPipeline::with_run_ownership_dir`,
    `Settings::run_ownership_dir`). `cognee_delete::recover_stale_pipeline_runs`
    (replacing `sweep_orphaned_run_artifacts`) rolls back and closes an
    abandoned run with `{"error": "AbandonedPipelineRunError: …"}`, only when it
    can claim the marker, so it is safe with any number of processes; it is
    keyed by the run, so a run abandoned before a newer run completed on the
    dataset is still closed, and a run whose dataset is gone is left alone, as
    in Python. Its age floor is the new key
    `cognify.stale_run_recovery_min_age_seconds`
    (`COGNEE_STALE_RUN_RECOVERY_MIN_AGE_SECONDS`, default `3600`, as in Python).
    The bindings run it once per database per process, at the first operation.
    `PipelineRunRepository::list_orphan_runs` became `list_unterminated_runs`.
  - `DATASET_PROCESSING_INITIATED` rows written by older versions are still read
    (Python keeps the enum value too) and gate nothing.
  - Closed-side callers (HTTP server) must call `recover_stale_pipeline_runs`,
    drop `new_with_orphan_reset` and `COGNEE_SINGLE_PROCESS`, and pass
    `DatasetLocks::global()` to their `add` / `delete` wiring.

- **Improve stage stamps live in the operation row's `run_info`, and
  `graph_sync_checkpoints` is gone (SDK-1039).** Python keeps a stage's
  watermark on the `improve` operation's `pipeline_runs` row, as
  `run_info[<stage>] = {"status", "started_at"}`
  (`cognee/modules/improve/graph_changes.py:46-48`, read by
  `last_improve_watermark`, `:66-97`), and has no checkpoint table. Rust now
  does the same.

  | Item | Before | After |
  |---|---|---|
  | Stage stamp storage | `graph_sync_checkpoints` row `improve_stage:{stage}:{dataset_id}` | the run's `improve` `pipeline_runs` operation row (`user_id` the owner) whose `run_info` holds `{stage: {status, started_at}}`; only a `Completed` row is read as a watermark |
| `improve()` change record | one `improve` row after the first stage that may have written to the graph | one `improve` operation row per run, written when it ends (`record_improve_operation`): `Completed` (Python `succeeded`), `Errored` (Python `failed`, keeps its stamps, never a watermark), none when every stage skipped (Python `noop`). As in Python every improve row but the stamped one counts as a change (`graph_changes.py:128-143`) |
  | `ImproveParams`, `cognee::api::remember`, `RememberRequest`, `CogneeServices` | `checkpoint_store` | **removed** |
  | `cognee_database` | `CheckpointStore`, `SeaOrmCheckpointStore`, `RelationalDb::checkpoints()`, `ops::checkpoint`, the `graph_sync_checkpoint` entity | **removed** |
  | `PipelineRunRepository` | `count_runs_created_after(since, dataset, exclude_names)` | `+ exclude_run_ids` (the stamped row itself is not a change); `+ latest_stamped_run(dataset, name, key)` |
  | `stage_watermark_key(stage, dataset_id)` | `improve_stage:{stage}:{dataset_id}` | `stage_watermark_key(stage)` is the stage name (the `run_info` key) |
  | Migration | | `m20261031_000001_improve_stamps_to_run_info` moves each existing `improve_stage:*` row into an `improve` row and drops the table, so a stage is not rerun because of the upgrade |

- **`update()` keeps the data id, and legacy data ids resolve (SDK-1039).**
  As in Python (`update.py`, `resolve_data_id`): the id passed to `update()` is
  resolved in the dataset (the exact id, else the `legacy_id` a row got when
  the dataset-scoping migration forked it from a row shared by several
  datasets; an id that resolves to nothing is `ApiError::UpdateTargetNotFound`,
  HTTP 404, and never creates a document). The row is then **refreshed in
  place** instead of replaced: the document's graph and vector memory is
  dropped (memory-only forget), the replacement is added pinned to the same id
  (`AddParams::pinned_id`: no content-hash lookup, the row's name, locations,
  extensions, MIME types, loader, hashes, size and metadata are rewritten,
  metadata and node set as a PATCH, the cognify marker cleared when the content
  changed; a row of another dataset is refused), and the document is cognified.
  `update()` takes exactly one replacement item, and the bindings'
  `incrementalLoading` option of `update` now defaults to `true`, as Python's
  (the re-cognify covers the whole dataset, as `cognify(datasets=[dataset_id])`
  does in Python, and skips the other items already cognified). The
  metadata PATCH follows Python's `merge_existing_row_metadata`: an item with
  no metadata (a plain text, file or URL) keeps the stored keys and only
  refreshes `_cognee.source_uri`, an explicit `{}` clears them, an object
  replaces them. `UpdateResult` is now
  `{ data_id, delete_result, cognify_result }`: `deleted_data_id` and
  `new_data` are gone (the bindings' `deletedDataId` / `newData` become
  `dataId`). Replacement content equal to another row's leaves two rows, as in
  Python. Delete by a legacy id (`DataInDataset`, and `Data` with a dataset
  name) resolves it the same way, and `DeleteResult::data_id` reports the
  resolved id. The `IngestDb` / `DeleteDb` traits gain `resolve_data_id`
  (`IngestDb` also `update_data`), and `ProcessedInput` gains `pinned_id`.
  New `cognee-cli update <data> --data-id <UUID> -d <dataset>` drives it
  (Python has no CLI verb for it; `--ontology-file` as in `cognify`).
  The dataset-scoping migration now clears the cognify marker of the rows it
  forks, so the next cognify rebuilds them under their new ids (it cannot
  reach the graph and vector stores: the old memory is dropped by deleting or
  updating the item, which the repointed ledger finds under the new id).
- **`add` routes loaders by the file's content, as Python does (SDK-1039).**
  The bytes are sniffed (new `infer` dependency, the counterpart of Python's
  `filetype`; `.txt`, `.csv`, `.md`, `.json`, `.xml` and `.yaml` are not
  sniffed, and unrecognised bytes are plain text; only the first 8192 bytes are
  read, and a type is named as `filetype` 1.2.0 names it, so an `.m4a` is
  `audio/mp4` and an `.ico` is `image/x-icon`, which no loader claims, as in
  Python), and the loader is the first,
  in Python's priority order, that accepts the name's extension or the sniffed
  extension together with the sniffed MIME type. The `data` row stores the
  **sniffed** `original_extension` / `original_mime_type`. So a PNG named
  `.jpg` goes to `image_loader` and is recorded as `png` / `image/png`, and UTF-8 text named `notes.conf` is `text_loader`,
  `txt` / `text/plain`. An image routed to `image_loader` whose NAME has no
  image extension (for example a PNG with no extension) fails with
  `IngestionError::ImageMimeTypeUndetermined`, because Python's vision call
  takes the MIME type from the name (`native_adapter.py:633-638`). **Breaking:** non-UTF-8 bytes routed to the text loader
  (for example a PNG named `.txt`) now fail the add, as Python's
  `UnreadableFileContentError` does, instead of being stored as a
  `text_<hash>.bin` blob, and a file no loader claims (for example a `.zip`)
  fails with `IngestionError::NoLoaderForFile` (Python: "No loader found")
  instead of falling back to the text loader. **Differences kept:** a local
  `.html` file still goes to the HTML text extractor and keeps its `html` /
  `text/html` metadata (D14). (Source files have since moved to
  `code_loader`; see "Source files are read by `code_loader`".)

- **URL add stores the raw page and `external_metadata` is Python's (SDK-1039).**
  `add` of an `http(s)` URL that returns HTML now stores the page as Python's
  `text_loader` does: the fetched HTML is kept whole (`original_data_location`
  `text_<md5(html)>.html`), `raw_data_location` is `text_<md5(html)>.txt` with
  the same HTML, `content_hash` is the MD5 of the page, `loader_engine` is
  `text_loader`, and `extension` / `original_extension` are `txt`
  (`text/plain`). The text is no longer extracted at add time, and the
  `source` / `url` / `final_url` / `content_type` / `title` metadata keys are
  gone: every file or URL add writes `external_metadata`
  `{"_cognee": {"source_uri": <url or absolute file:// URI>}}` merged over the
  item's own metadata (an explicit `source_uri` in the item wins), and a text
  add writes `{}` instead of NULL. `Document.external_metadata` is formatted
  like Python's `json.dumps(..., indent=4)` (four spaces, non-ASCII escaped)
  and is `{}` for a row without metadata. `UrlMetadata` loses `title` and
  `raw_bytes`. WebPage/WebSite provenance nodes are now derived from an
  `http(s)` `_cognee.source_uri` and no longer carry a `title`. **Documented
  difference:** a local `.html` file keeps Rust's text extraction
  (`beautiful_soup_loader`); Python chunks the raw HTML for it. A fetched
  page is decoded like httpx's `Response.text`: with the Content-Type
  `charset` (UTF-8 otherwise), undecodable bytes replaced instead of failing
  the add.

- **The notebooks feature is removed (SDK-1039).** Python removed
  `cognee/modules/notebooks` and both bundled tutorials in `52bcc7494`. Rust
  follows: gone are the `cognee::api::notebooks` module and its
  `list_notebooks` / `create_notebook` / `update_notebook` / `delete_notebook`
  / `NotebookError` re-exports; `cognee_database`'s `NotebookDb`, `Notebook`,
  `NotebookUpdatePatch`, the `notebook` entity, `ops::notebooks` and the
  tutorial seeder (`seed_tutorials_if_first_call`, `TUTORIAL_BASICS_ID`,
  `TUTORIAL_PYTHON_DEV_ID`) with its bundled `assets/notebooks/tutorials`;
  `NotebookDb` as a `RelationalDb` supertrait; and the binding surfaces —
  Python `cognee.notebooks` / `CogneeNotebooks`, TS `c.notebooks` /
  `CogneeNotebook` / `CogneeNotebookObject` and the `cognee*Notebook(s)`
  natives, Java `notebooks()` / `CogneeNotebooks` / `CogneeNotebook`, and the
  C API's `cg_sdk_{list,create,update,delete}_notebook(s)`. The `notebooks`
  table stays: like Python, whose migration chain creates it and never drops
  it, the baseline migration keeps creating it and nothing reads it.
- **Delete counts a legacy `Triplet_text` point only when it exists
  (SDK-1039).** The delete preview and the delete run both counted one
  `Triplet_text` point per deleted edge, whether or not the point existed;
  cognify no longer writes them (SDK-1115), so both overcounted. Both now look
  the points up (`VectorDB::retrieve`) and count, and delete, only those found.
  With per-dataset storage the dataset's own store is dropped whole and cannot
  be probed, so no `Triplet_text` point is counted there, previewed or run.
  The legacy cleanup is best-effort, as Python's is: a failed `Triplet_text`
  lookup or delete is a warning on the result and counts no point, instead
  of failing the delete.
- **The event-graph `temporal_cognify` pipeline is removed (SDK-1039).**
  Python deprecated `temporal_cognify` and ignores it (cognee 1.6.2 was the
  last release in which it built the event graph); by the SDK-1115 rule Rust
  goes straight to the post-removal state, with no shim. Default cognify
  already writes dates as `Timestamp` nodes, and TEMPORAL search reranks by
  them, so TEMPORAL needs no special cognify. Removed:
  `CognifyConfig::temporal_cognify` / `with_temporal_cognify`, the CLI's
  `--temporal-cognify` flag (now an unknown argument), the bindings'
  `temporalCognify` option (now an unknown key, warned about and ignored, as
  Python warns and ignores it; also gone from the Python `CognifyOpts`, TS
  `CogneeCognifyOptions`, Java `CognifyOptions` and C header docs),
  `build_temporal_cognify_pipeline`, `extract_temporal_events`,
  `add_temporal_data_points`, their `make_*_task` factories,
  `ExtractedTemporalEvents`, `AttributedEvent`, the
  `cognee_cognify::temporal_extraction` module (`TemporalEventExtractor`,
  `TemporalEntityEnricher`), `FailureStage::{TemporalExtraction,
  TemporalEnrichment}`, and `cognee_models::{TemporalEvent, EventAttribute,
  CognifyTimestamp, CognifyInterval, to_cognify_timestamp}` with the public
  `cognee_models::temporal_event` module (`RawExtractedTimestamp`, which
  TEMPORAL search uses for the question's window, stays at the crate root).
  `graph_schema` together with the flag is no longer a validation error.
  `cognee-cli pipeline-unblock --pipeline` knows only `cognify_pipeline`.
  `data_per_batch` was kept at that point and has since been removed. A dataset an older build cognified with the flag keeps `Event` nodes
  that nothing reads (TEMPORAL answers on it with plain hybrid results);
  rebuild it with a memory-only forget and a fresh cognify, as Python advises.
  The store-wide wipe still lists the legacy `Event_name` collection.

- **`TextSummary.source_task` is `extract_graph_and_summarize` (SDK-1039).**
  Summaries were stamped `summarize_text`; Python stamps them with the merged
  task's name (`run_tasks_base._stamp_provenance`, `cognify.py:643-653`).
  Entity and EntityType keep `extract_graph_from_data`. The
  `SUMMARIZE_TEXT_TASK_NAME` constant is removed; use
  `EXTRACT_GRAPH_AND_SUMMARIZE_TASK_NAME`. Already-stored nodes keep their old
  stamp.
- **Pipeline runs write Python's lifecycle and `run_info` (SDK-1039).** A run
  writes `DATASET_PROCESSING_STARTED`, then `…_COMPLETED` or `…_ERRORED`; the
  `DATASET_PROCESSING_INITIATED` row is gone, as in Python `dc0b4ad6a`. `run_info`
  no longer carries the run's data items (Python `a88402500`, COG-6089):
  STARTED and COMPLETED are `{}`, ERRORED is `{"error": …}` (a tolerantly completed cognify run no longer adds a
  Rust-only `cognify_failures` key; its failures are in the returned report). The
  orphan sweep's ERRORED row is `{"error": <reason>}` instead of
  `{"reason": …}`. Migration `m20261029_000001_drop_run_info_data` strips the
  `data` key from stored rows, as Python's alembic `b7e4c2a9d6f1` does. Deleting
  a dataset or forgetting its memory no longer appends `INITIATED` reset rows.
  API: `cognee_core::PipelineRunStatus::Initiated`,
  `PipelineWatcher::on_pipeline_run_initiated`, `PipelineRunInfo::data_ids`,
  `RunSpec::data_ids` and `pipeline_run_registry::{data_info, run_info_for_running}`
  are removed (`DeleteService::with_pipeline_run_repo` stays: it now wires only
  the delete's `forget` change row);
  `run_info_for_errored` takes only the error, and `run_info_for_started` /
  `run_info_for_completed` are new. The stored-row enum keeps `Initiated` to
  read old rows, and the explicit `reset_pipeline_run_status` re-arm API (for
  the opt-in pipeline cache) still writes it. A caller that sets
  `CognifyConfig::use_pipeline_cache` must now call
  `reset_dataset_pipeline_run_status` itself after a dataset delete or a
  memory-only forget, or the next cognify of that dataset is a cache hit.

- **`data` rows are dataset-scoped, with `uuid4` ids, as in Python (SDK-1039).**
  A new `data` row gets a random `uuid4` id (it was
  `uuid5(content_hash, owner, tenant)`), and its dataset is the new
  `data.dataset_id` column. Re-adding content reuses the row that holds the same
  `content_hash` in the same dataset for the same owner and tenant (Python's
  `identify_many`); the same content in another dataset is another row, and a
  re-add after delete gets a new id. The `dataset_data` membership table is
  retired: migration `m20261028_000001_dataset_scoped_data` adds
  `data.dataset_id` and `data.legacy_id`, stamps every row from its memberships
  and drops the table, as Python's alembic `d6e8f0a2b4c6` does. A row shared by
  several datasets is split: the oldest membership keeps the id; every other
  dataset gets a uuid4 copy with `legacy_id` = the original id, and its `nodes`
  and `edges` ledger rows are repointed to the copy. Stop every writer before
  upgrading (not an online migration). Rust now reads a Python-written database's
  membership. `cognee_ingestion::generate_data_id` is removed; `Data` gains
  `dataset_id` and `legacy_id`; `IngestDb` gains `find_data_in_dataset` and
  `DeleteDb` gains `count_data_location_references` (a stored file shared by
  several rows is deleted with its last row); `attach_data_to_dataset` now
  stamps `data.dataset_id` and refuses a row of another dataset.

- **Embedding batch size defaults to 32 and drives batching (SDK-1039).**
  `embedding.batch_size` (`EMBEDDING_BATCH_SIZE`) drops from 36 to 32, Python's
  `embedding_batch_size`, because Text Embeddings Inference rejects more than 32
  inputs per request. Cognify now cuts every embedding call into batches of the
  engine's batch size (`cognee_embedding::embed_in_batches`). New key
  `embedding.max_concurrent_data_points` (`EMBEDDING_MAX_CONCURRENT_DATA_POINTS`,
  default 150) bounds the batches in flight at
  `max(1, max_concurrent_data_points / batch_size)`, replacing the fixed 8 of the
  OpenAI-compatible and Bedrock engines; Ollama stays sequential. The dead
  `CognifyConfig.embedding_batch_size` field and `with_embedding_batch_size`
  builder are removed (they were never read; set `embedding.batch_size`).
  `EmbeddingConfig` and `EmbeddingInputs` gain `max_concurrent_data_points`.

- **Vector payloads have Python's `IndexSchema` shape (SDK-1039).** Every row
  cognify and `edge-reindex` write is built by the new
  `cognee_models::index_schema_payload`: `id`, `text` (the indexed value; EdgeType
  and Document rows now carry it too), `document_id`, `document_name`,
  `chunk_index`, `source_chunk_id`, `importance_weight` (copied from the source
  point), `external_metadata`, `belongs_to_set` (node-set names), the base fields
  at their IndexSchema defaults (`type = "IndexSchema"`, `version = 1`,
  `topological_rank = 0`, `source_* = null`, …) and `metadata`, plus one
  Rust-only key, `dataset_id`. Absent values are `null`, not omitted. The LanceDB
  adapter no longer stores the `metadata` key, as Python's LanceDB adapter does;
  pgvector keeps it. Dropped keys: `field`, `user_id`, `tenant_id`, Entity
  `entity_type` and `description`, TextSummary `chunk_id`, EdgeType
  `relationship_name` and `number_of_edges`, Document `name`, and a custom
  model's own properties. `DataPoint::vector_metadata()` and
  `EdgeReindexOptions::{user_id, tenant_id}` are removed. Hybrid search no longer
  shows an entity description line, as Python does for every entity whose facts
  were not reviewed. Rows written by earlier versions stay readable (no
  migration): facts fall back to `relationship_name`, and an old `description`
  key is ignored.

- **Cognify no longer embeds triplets (SDK-1039).** Python stopped writing the
  `Triplet_text` collection in SDK-1115, so `CognifyConfig::embed_triplets`,
  `CognifyConfig::with_triplet_embeddings`, `IndexedFieldsStats::triplet_count`,
  the `triplet` cognify option of every binding (Python, TS, Java, C; an unknown
  option is only warned about, so existing callers keep running) and the
  `cognee_cognify::triplet_creation` module (`create_triplets_from_graph`,
  `create_triplets_from_graph_with_timestamps`) are removed. `cognee_models::Triplet`
  stays, and so does the delete-side cleanup: deleting data still removes the
  legacy `Triplet_text` rows an earlier version wrote, and search on a database
  that holds one keeps working.

- **`remember(self_improvement=true)` runs `improve()`, not memify (SDK-1039).**
  In permanent mode the self-improvement step now calls `improve()` on the
  dataset, as Python's `remember` does; session mode already did.
  `RememberResult::memify_result` is replaced by `RememberResult::improve_result`
  (`Option<ImproveResult>`, not serialised), and an `improve()` failure is
  reported in `RememberResult::error` instead of being logged and dropped.

- **Public memify and improve stage 3 are removed (SDK-1039).** Python deprecated
  `cognee.memify`, `POST /api/v1/memify` and the memify CLI in SDK-1115 and
  removes them in the next release; Rust goes straight to the removed state,
  with no shim. Gone: the `cognee-cli memify` command, `cognee::run_memify`,
  `MemifyConfig`, `MemifyTask`, `MemifyResult`, `MemifyError`,
  `build_memify_index_only_pipeline` and the triplet extract/index tasks, the
  memify binding ops (`memify` in Python, TS, Java, `cg_sdk_memify` in C and
  their option/result types), `ApiError::Memify`, the improve "memify" stage
  (stage 3; the global-context-index stage is now stage 3 and sync stays stage
  4), `ImproveResult::memify_result` (and `memifyResult` on the binding wire
  shape), the `memify` entry of the schema view's operation catalog, and the
  `memify_pipeline` name from `cognee-cli pipeline-unblock` (it now knows
  `cognify_pipeline` and `temporal-cognify`).

  `improve()` also drops the options that only scoped or fed memify:
  `ImproveParams::{node_name, extraction_tasks, enrichment_tasks, data}`, the CLI
  `improve --node-name` flag, and the `nodeName` option of every binding. Over
  REST, a client that still sends them is ignored, as in Python (pydantic
  `extra="ignore"`). The `cognee_cognify::memify` module keeps its name but only
  holds the improve stages (feedback weights, session persist, distill,
  truth subspace, session sync). `cognee_models::Triplet` and the delete-side
  cleanup of legacy `Triplet_text` rows stay.

  **Migration.** Remove `memify` calls; cognify and `improve()` cover what they
  did for search (triplet search itself was removed with `TRIPLET_COMPLETION`).

- **`TRIPLET_COMPLETION` and `TripletRetriever` are removed (SDK-1039).**
  Python deprecated triplet search in SDK-1115 and removes it in the next
  release; Rust goes straight to the removed state, with no
  `TRIPLET_COMPLETION → GRAPH_COMPLETION` alias. `SearchType::TripletCompletion`,
  `cognee_search::TripletRetriever` and its `SearchBuilder` registration are
  gone, and the Python, TS, Java and C `SearchType` lists drop the value
  (17 search types remain). `"TRIPLET_COMPLETION"` is now an unknown search
  type everywhere: serde/`SearchRequest` deserialization, the binding
  `parse_search_type` (validation error) and the CLI `--query-type` (invalid
  value). A database that still holds a `Triplet_text` vector collection keeps
  serving every other search type.

  **Migration.** Use `GRAPH_COMPLETION` (what Python's deprecation shim answers
  with) or `HYBRID_COMPLETION`.

- **Delete lock hook, search scoping accessor, pipeline-runs listing +
  `pipeline_runs.user_id`.** Python serializes deletes against pipeline runs
  with one per-dataset lock (`dataset_lock.py`, cognee 37f788bd2); its
  activity feed filters runs by the triggering user.

  | Item | Before | After |
  |---|---|---|
  | `DatasetLocks` / `DatasetLockGuard` | defined in `cognee-ingestion` | moved to `cognee_core::dataset_locks` (still re-exported by `cognee-ingestion`); re-entrant per task through the new `DatasetLocks::hold(ids, fut)` (+ `is_held`, `DatasetLockGuard::is_reentrant`) |
  | `DatasetLocks::lock_for_name` | method | **removed**; `cognee_ingestion::lock_dataset_identity(&locks, name, owner, tenant)` |
  | `DeleteService::with_dataset_locks` | — | new: every dataset-scoped delete takes the dataset's lock (none without it) |
  | `DeleteService::execute_datasets_by_id` | a dataset missing at its turn failed the loop | re-read under its lock and **skipped** when gone; a real failure still stops with `Partial` (now "nothing completed yet" → the error as is) |
  | `SearchOrchestrator::{dataset_scoping, refuses_dataset_scope}` | — | new; the refusal `search` applies |
  | `pipeline_runs.user_id` | — | new nullable column + `ix_pipeline_runs_user_id` (migration `m20261020_000001_pipeline_runs_user_id`, guarded for a Python-first shared SQLite); written by the run watchers / registry from `PipelineRunInfo.user_id` |
  | `PipelineRun`, `PipelineRunWithAttributionRow`, `entities::pipeline_run::Model` | — | `+ user_id` (struct literals must set it) |
  | `PipelineRunRepository::{log_pipeline_run_entry, list_runs}` | — | new provided methods (`PipelineRunEntry`, `PipelineRunListQuery`, `PipelineRunVisibility`); `SeaOrmPipelineRunRepository::with_list_bind_limit` |

- **Delete parity: tenant-strict names, tenant-scoped delete-all, grant
  reach by id only (SDK-936).** Every by-name dataset lookup is now
  tenant-strict (there is no tenant-loose lookup left), the owner-scoped
  delete scopes carry their tenant, and "delete everything" means the
  caller's datasets in its tenant, deleted one by one by id and stopping at
  the first failure — Python's `datasets.delete_all` (cognee 37f788bd2).

  | Item | Before | After |
  |---|---|---|
  | `ops::datasets::get_dataset_by_name`, `IngestDb::get_dataset_by_name`, `DeleteDb::get_dataset_by_name` | `None` = any tenant | **removed**; use `get_dataset_by_name_in_tenant` (same signature, strict: `None` ⇔ tenant-less) |
  | `DeleteScope::{Data, Dataset, User}` | no tenant | `+ tenant_id: Option<Uuid>` (`#[serde(default)]`), strict |
  | `DeleteService` `User` | one batch over the owner's datasets in every tenant | the owner's datasets in `tenant_id`, per dataset by id, `DeleteError::Partial` on a later failure; **no longer deletes the owner's search history** (Python's `delete_all` never does, and the history table has no tenant) |
  | `DeleteService::execute_datasets_by_id` | — | new |
  | `AuthorizedDeleteService` | `Data{dataset_name: None}` checked the owner's linked datasets; `User` cover over every tenant | `Data{None}` → `Validation`; a scope tenant other than the caller's → `PermissionDenied`; `User` tenant-scoped, by id, no history; a by-name scope (`Dataset`, `Data{Some}`) whose `owner_id` is not the caller → `PermissionDenied` (names never reach grants; grant reach is by id only) |
  | `DeleteScope::DataInDataset` / `Data{Some(name)}` with no `Data` row | flag ignored | `delete_dataset_if_empty` deletes the dataset when it is empty (Python) |
  | `DeleteScope::Data{Some(name)}` with no `Data` row and a name that does not resolve | `Ok` (ghost cleanup) | `Validation("Dataset '…' was not found for owner …")`: the dataset is resolved first |
  | `cognee::api::forget(ForgetTarget::All)` | owner-wide `User` batch | tenant-scoped, by id, `Partial` |
  | `cognee::api::DatasetRef::to_name` | pub | **removed** (use `DatasetRef::resolve`) |
  | `DatasetManager::{list_data, has_data}` without an ACL | no check | owner + tenant, else `DatasetError::NotFound`; bindings `hasData`/`listData` reject an unknown or foreign id |
  | bindings `has` / `hasData` on a non-UUID id | `false` | validation error (like `listData`) |
  | `cognee_cli::commands::delete::build_request_async` | `(args, owner_id, db)` | `(args, owner_id, tenant_id, db, RequestAuthorization)`; new `resolve_caller`, `RequestAuthorization::{Ownership, Grant}` |
  | `cognee-cli delete` | `--all` = store-wide wipe; `--user-id` only as the User scope; `--data-id` + dataset rejected; no `--tenant-id` | `--all` = the caller's datasets in its tenant; `--user-id` = the acting user when a target is given; `--data-id` + dataset accepted; `--tenant-id` |
  | `cognee-cli cognify / memify / add-and-cognify / pipeline-unblock` | names resolved in any tenant; no-name cognify/memify spanned tenants | `--tenant-id`; strict |
  | `add` by name (`persist_data_with_acl_and_locks`) | a tenant-less add could attach to a tenanted same-named dataset | strict: creates/uses the tenant-less row |

  **Migration.** Rename `get_dataset_by_name` calls to
  `get_dataset_by_name_in_tenant` and pass the caller's tenant (a `None`
  that relied on "any tenant" now finds only tenant-less rows). Add
  `tenant_id` to `DeleteScope::{Data, Dataset, User}` literals (the caller's
  authenticated tenant), or prefer the by-id scopes. Handle
  `DeleteError::Partial` from `User`, `forget(All)` and
  `execute_datasets_by_id`. Under an ACL, send item deletes with their
  dataset. Scripts that used `cognee-cli delete --all` to wipe the store
  must now wipe per tenant (or call the library's plain `DeleteScope::All`).
  `forget(All)` no longer matches Python in one respect: it does not prune
  the whole session cache (unsafe from a per-tenant call). Callers that
  relied on `User` / `forget(All)` to clear a user's search history must
  clear it themselves (`DeleteDb::delete_search_history_for_user`, across
  all of the user's tenants). Under an ACL, delete another user's dataset by
  id (`DatasetById` / `DataInDataset`), not by its name.

  **Implementors of `IngestDb` / `DeleteDb`** (custom relational backends):
  delete your `get_dataset_by_name` impl — it is no longer a trait method —
  and override the provided `get_dataset_by_name_in_tenant` with a point
  query (owner, name and `tenant_id` with `None` ⇔ NULL). Without the
  override the default lists all of the owner's datasets and filters them,
  which is correct but slower.

- **Tenant-filtered per-dataset permission checks; strict tenant-less
  fallback (SDK-974, SDK-687, SDK-689).** Every per-id check now applies the
  caller's tenant like Python (`dataset.tenant_id == user.tenant_id`, strict:
  `None` reaches only tenant-less datasets), and a grant on another tenant's
  dataset is denied with the same error as no grant.

  | Item | Before | After |
  |---|---|---|
  | `AclDb::has_permission_with_roles` | required method | **removed** |
  | `AclDb::has_permission_in_tenant(user, dataset, perm, tenant)` | — | new, provided (default = membership in `authorized_dataset_ids_in_tenant`) |
  | `DatasetManager::{list_datasets, list_data, has_data, empty_dataset, delete_data, delete_all}` | `(…, owner_id, …)` | `(…, owner_id, tenant_id: Option<Uuid>, …)` |
  | `DatasetManager` per-id methods under an ACL | unknown id `NotFound`; cross-tenant grant admitted | `PermissionDenied` for unknown id, no grant and other tenant |
  | `DatasetManager::list_datasets` without an ACL | every owned dataset | owned datasets with `tenant_id == tenant_id` |
  | `AuthorizedDeleteService::{preview, execute}` | `(request, principal_id)` | `(request, principal_id, tenant_id)` |
  | `AuthorizedDeleteService` `DeleteScope::All` | delete **everything** if the caller covers every dataset in the DB, else `PermissionDenied` naming one | delete, by id, the caller's `delete`-authorized datasets in its tenant (Python `datasets.delete_all`); no system-wide graph/vector/search-history wipe |
  | `AuthorizedDeleteService` User-cover denial message | names the uncovered dataset | generic (`delete permission denied for the requested scope`) |
  | `DatasetManager::delete_all` | lists `read`-authorized (ACL) / owned datasets and deletes each **by name** | lists `delete`-authorized in tenant (ACL) / owned in tenant, deletes each **by id** |
  | `cognee-cli search` / `recall` | no tenant | new `--tenant-id` (strict match) |
  | `IngestDb` / `DeleteDb` | — | new provided `get_dataset_by_name_in_tenant` (strict, `None` ⇔ NULL; point query on SeaORM) and `get_datasets_by_ids` (batch on SeaORM) |
  | `cognee_database::ops::acl::authorized_datasets_in_tenant` | — | new: existing datasets a user holds a permission on in a tenant (one ACL listing + one batch lookup) |
  | `AuthorizedDeleteService` by-name scopes (`Dataset`, `Data` with a name) | resolved with no tenant predicate, inner service re-resolved by name | resolved once in the caller's tenant (strict), inner service driven **by id** |
  | `AuthorizedDeleteService` `DeleteScope::All` failure | first error, progress lost | `DeleteError::Partial { completed, failed_dataset_id, source }` after the first dataset |
  | `AuthorizedDeleteService::preview(All)` | sum of per-dataset previews | union preview (`DeleteService::preview_datasets_by_id`, new) |
  | `DeleteError` | — | new variant `Partial` (exhaustive matches break) |
  | `DeleteResult` / `DeletePreview` | — | `AddAssign` (field-wise sum) |
  | `DatasetManager::{empty_dataset, delete_data}` without an ACL | by name (tenant-loose re-resolution) | owner + strict-tenant check, then by id |
  | `cognee::api::update` delete step | `DeleteScope::Data` by name | `DeleteScope::DataInDataset` by the authorized `dataset_id` |
  | `cognee::api::forget(target, owner_id, …)` | no tenant; names by owner only | `forget(target, owner_id, tenant_id, …)`; references resolve in the tenant (strict) and delete by id; `db` required for any dataset reference |
  | `cognee-cli forget --tenant-id` | parsed and ignored | honoured |
  | `cognee::api::update` denial | `ApiError::InvalidArgument` | new `ApiError::PermissionDenied(String)` |
  | `SearchOrchestrator` without an ACL, `tenant_id: None` | no tenant filter for ids and names | tenant-less datasets only |
  | Bindings `list_datasets` / `delete_all_datasets` | every owned dataset | tenant-less datasets only (the handle user has no tenant) |
  | `HandleState::tenant_id()` | — | new accessor |

  **Migration.** An external `AclDb` impl deletes its `has_permission_with_roles`
  (it is no longer a trait member) and may override `has_permission_in_tenant`
  for an O(1) path that gives the same answer:

  ```rust
  async fn has_permission_in_tenant(
      &self, user_id: Uuid, dataset_id: Uuid, perm: &str, tenant_id: Option<Uuid>,
  ) -> Result<bool, DatabaseError> {
      // dataset row exists with dataset.tenant_id == tenant_id (NULL for None)
      // AND a direct / tenant / role grant of `perm` on it
  }
  ```

  Callers pass the caller's tenant: `mgr.list_datasets(owner, tenant)`,
  `auth_svc.execute(&req, principal, tenant)`. `match`es on `ApiError` add the
  `PermissionDenied` arm. Data written with a per-call `tenant` opt (or
  `cognee-cli add/remember/improve --tenant-id`) is no longer listed or deleted
  by the tenant-less binding dataset ops, and is not reachable from a
  tenant-less search; pass the same tenant on read (`cognee-cli search/recall
  --tenant-id`, the bindings' `tenant` search opt) (Python parity).
  Without an ACL the `DatasetManager` per-id methods still perform no
  ownership or tenant check.

- **`cognee-database`: `IngestDb::update_data(Data)` is replaced by
  `IngestDb::update_data_node_set(data_id, node_set, external_metadata)`
  (SDK-1129 follow-up).** The full-row method (added in #302 for the re-add
  node-set refresh) wrote every column from the ingest pipeline's earlier
  read, rolling back a `token_count`, `last_accessed` or `pipeline_status`
  written in between by a concurrent cognify or search. The replacement is a
  targeted `UPDATE` of the two columns plus `updated_at`, returning the
  `updated_at` it wrote (so the pipeline patches its snapshot instead of
  reading the row back) and `DatabaseError::NotFound` for a missing row. Implementors of `IngestDb`
  outside this repository rename and narrow the method; the free function
  `ops::data::update_data` is unchanged. In the same change the ingest
  pipeline refuses a bad `node_set` *before* it resolves or creates the
  dataset (no orphan dataset or owner grant is left behind), decides "already
  carries this set" on parsed JSON (a Python-written `["a", "b"]` row is no
  longer rewritten on every re-add), serializes the re-add refresh per
  `data_id` within the process (two concurrent re-adds of the same content no
  longer merge into one stale snapshot; across processes the last writer still
  wins), and surfaces `NotFound` when the row is deleted between its read and
  the refresh instead of returning the stale row. `cognee::api::update` now
  adds the replacement **before** it removes the old document, so any add
  failure (a bad `node_set`, a missing backend, an unreadable file, a loader
  or storage error) leaves the old data intact; a missing dataset or an item
  the dataset does not hold is still refused before anything is written, and
  a replacement with the old content keeps its row and has only its memory
  dropped and rebuilt (`delete_result` is then a memory-only delete). The new
  `cognee_ingestion::validate_add_inputs` / `validate_node_set_names` /
  `validate_item_node_set` are public for callers that do irreversible work
  before an add. `classify_documents` falls back to the `Data.node_set` column
  whenever `external_metadata` carries no `node_set` key — absent, not JSON,
  not an object, or an object without the key — which covers every pre-#302
  Rust row, including those whose `DataItem` metadata text was stored
  verbatim (`"crm export"`, `"[1,2]"`); Python always writes the key inside a
  dict, so parity is unaffected. Known limitation, kept for Python parity: a re-add that
  only changes the node set of content an earlier `cognify` already completed
  refreshes the row but not the graph — the completion marker stays, so an
  incremental `cognify` skips the item and its document and chunks keep the
  old `belongs_to_set`. Use `update()` to re-tag cognified content.

- **`cognee-search`: `TEMPORAL` search is hybrid retrieval reranked by the
  question's time window, and `TemporalRetriever` is gone.** `SearchType::Temporal`
  now maps to the new `TemporalHybridRetriever`, a port of Python's
  `TemporalHybridRetriever`: it returns `HybridCompletion`-shaped context
  (`"kind"`-tagged chunks, entities and facts) reordered by the `Timestamp` nodes
  default cognify writes, and falls back to the plain hybrid result with a reason
  (`TemporalDiagnostics`, returned by `TemporalHybridRetriever::retrieve`). The
  Event-based `TemporalRetriever` — which read only the `Event` nodes of the
  separate temporal-cognify pipeline, answered everything else from triplet
  search, and stamped its prompt with the machine's *local* date — is removed
  outright, as in Python: graphs built only by temporal cognify now get plain
  hybrid answers. The window prompt is Python's `QUERY_INTERVAL_PROMPT`, which
  reads no clock. An LLM or transport failure during window extraction now
  fails the search, as in Python, instead of silently falling back; so does a
  window with out-of-range calendar fields (month 13, year 0), which Python's
  `QueryTime` validation rejects. A date that does not exist (`2021-02-29`)
  falls back with reason `invalid_interval`, as in Python. Other behaviour that follows
  Python: an empty graph returns an empty context (not `NotFound`), batches are
  rejected, and `search.triplet_neighbor_cap` no longer applies to TEMPORAL
  (`search.hybrid` does).

  **Migration.** Replace `TemporalRetriever::new(...)` with
  `TemporalHybridRetriever::new(vector_db, embedding_engine, graph_db, llm,
  top_k, candidate_top_k, system_prompt, system_prompt_path,
  user_prompt_template, generation_options)`. Its `top_k` is what the result
  shows per lane (default 5) and `candidate_top_k` what the hybrid fetch gathers
  first (default `4 × top_k`; per request,
  `retriever_specific_config.candidate_top_k`, the new
  `SearchParams::candidate_top_k`). `SearchParams` is not `#[non_exhaustive]`,
  so out-of-tree code that builds it with a full struct literal must add
  `candidate_top_k` or end the literal with `..Default::default()`.
  `GraphDBTrait` gains
  `get_timestamps_in_range` and `get_temporal_anchors` with default
  implementations, so an out-of-tree backend keeps compiling and working; it may
  override them with a native query that returns the same result as
  `cognee_graph::default_timestamps_in_range` / `default_temporal_anchors`.

- **`cognee-cognify`: summarization is re-typed over `ExtractedChunks`, and the
  two graph/summarize pipeline stages are now one.** Graph extraction and
  summarization run concurrently, as Python's single `extract_graph_and_summarize`
  task always has (`asyncio.gather` over the same `data_chunks`). Summarization
  never had a data dependency on graph extraction — it reads chunk text; it
  consumed `ExtractedGraphData` only because that is what the stage in front of
  it handed over.

  Signatures that changed:

  | Item | Before | After |
  |---|---|---|
  | `summarize_text` | `&ExtractedGraphData -> SummarizedData` | `&ExtractedChunks -> SummarizedChunks` |
  | `make_summarize_text_task` | `TypedTask<ExtractedGraphData, SummarizedData>` | `TypedTask<ExtractedChunks, SummarizedChunks>` |
  | `make_summarize_text_task_with_rank` | as above, plus `rank` | as above, plus `rank` |

  `SummarizedChunks { summaries, failures }` is new; `SummarizedData` is
  unchanged and is now produced by the fused stage.

  **Migration.** A custom pipeline that chained `make_extract_graph_task` into
  `make_summarize_text_task` replaces *both* with the new
  `make_extract_graph_and_summarize_task` (or its `_with_rank` variant), which
  takes exactly the arguments `make_extract_graph_task` took and keeps the
  `ExtractedChunks -> SummarizedData` shape the pair had end to end. The two
  halves remain separately available for a caller who wants to fuse one of them
  against a different sibling, via the new `TypedTask::try_parallel` in
  `cognee-core`. Provenance is unaffected: entities still stamp
  `source_task = "extract_graph_from_data"` and summaries `"summarize_text"`
  (changed to `"extract_graph_and_summarize"` in SDK-1039), both at
  `topological_rank` 3.

  Three behavioural consequences, all three shared with Python: the stage's
  peak in-flight LLM calls is now the sum of the two `max_parallel_extractions`
  semaphores rather than one of them (the transport-level
  `cognee_llm::in_flight` ceiling still bounds the process); under
  `RollbackScope::FailedItems` an abort in extraction no longer prevents
  summarization having already paid for the excluded files' chunks — wasted
  spend only, since those chunks are kept out of `chunk_failure_ratio()`, so a
  run's fatality decision is the same as the sequential pipeline's; and one
  stage is one retry unit, so a custom `RetryPolicy::Limited` re-dispatches
  *both* branches rather than each independently. The last is inert on shipped
  paths — `build_cognify_pipeline` sets no policy and `RetryPolicy::NoRetry` is
  the builder default — and a caller who needs separate retry boundaries can
  still compose the two halves as separate stages.

- **`FailureReport`'s serialized shape changed: `failed_chunks` is an array of
  chunk uuids, not a count.** It counted *failures carrying a chunk id* rather
  than distinct chunks, so a chunk that failed in two stages was charged twice
  and `chunk_failure_ratio()` could exceed 1.0 — enough on its own to escalate
  a run below its configured threshold into a fatal one that sweeps the files
  that completed. Reachable before this release too, under `RunToEnd`, where
  extraction drops no chunks and summarization can fail one extraction already
  failed.

  There is deliberately **no compatibility shim for the old numeric form**: a
  legacy `"failed_chunks": 1` carries no ids, so it cannot populate the set,
  and accepting it would yield a report that merges wrong — undercounting,
  which hides a fatal run rather than inventing one. Failing loudly on a stale
  payload is the better error. Nothing in this workspace serialises a report
  (the `run_info` row is hand-built and stores the derived ratio), so this
  affects only a caller who persisted one directly.

- **`GenerationOptions::default()` no longer carries a `max_tokens`.** It set
  `Some(16384)`; it now leaves `None`. The field had to stop carrying a value so
  that `Some(n)` means "a caller chose n" — the OpenAI adapter refuses to raise a
  caller's own output budget when a structured-output answer is truncated, and it
  could not tell a deliberate 16384 from the one the `Default` supplied. A caller
  writing `GenerationOptions { temperature: Some(0.1), ..Default::default() }`
  therefore had a budget it never picked treated as a constraint, and the call
  failed terminally naming that number.

  Both option-less paths are unchanged: `generate` with no options still gets the
  configured `llm_max_completion_tokens`, and option-less structured output still
  gets 16384 (now named at that call site rather than inherited from `Default`).

  What changes is one spelling — options built with `..Default::default()` that
  never set `max_tokens`. Such a caller now expresses "no budget of my own", and
  each path resolves that its own way:

  1. **OpenAI `generate` sends no cap**, so the provider's default applies rather
     than 16384. Note this path does *not* substitute the configured ceiling for
     an explicit `None`, by long-standing design ("an explicit `max_tokens`,
     including `None` = no cap, always wins over config"). If you relied on the
     old default to bound completion length, set `max_tokens` explicitly.
  2. **Anthropic chat takes the configured ceiling**
     (`llm_max_completion_tokens`), clamped to the model cap — where it
     previously clamped such callers to 16384 even if the operator had configured
     more. This is what its docs already claimed to do. **Bedrock no longer does
     this** — see the entry below, which supersedes it for that adapter.
  3. **`transcribe_image` on Anthropic and Bedrock uses its 300-token vision
     default** instead of 16384. Pass an explicit `max_tokens` for longer image
     descriptions.

- **Bedrock: an absent caller budget now sends no `maxTokens` at all**, so the
  model's own maximum applies (64000 for Claude Sonnet 4.5) instead of the
  16384 configured ceiling. This matches the Python engine, whose Bedrock
  adapter never serialises a budget — its request body carries a literal
  `"inferenceConfig": {}`.

  **`llm_max_completion_tokens` therefore no longer bounds such a call.** On the
  cognify path, which passes no budget by design, a runaway generation can now
  bill up to the model maximum per chunk with no setting to cap it. That is also
  Python's behaviour, where the setting only sizes input chunks. A caller that
  needs a bound must pass `max_tokens` explicitly; it is still clamped by the
  ceiling and then the model cap.

  Motivation: with the ceiling substituted, three chunks of a 55-chunk document
  truncated at 16384, and because the ceiling equalled the retry cap the
  truncation ladder had nothing to raise into — so `RollbackScope::WholeRun`
  discarded the other 52 and cognify returned HTTP 500.

- **`LLM_TEMPERATURE` changes from `0.0` to unset**, and `Settings::llm_temperature`
  changes type from `f64` to `Option<f64>`. `None` means no `temperature` is sent,
  which is deliberately not the same as `Some(0.0)`: Python draws the same
  distinction because the default gpt-5 family rejects any temperature but the
  provider's own.

  A CLI config written by an earlier version persists `"llm_temperature": 0.0`,
  which would otherwise read back as an explicit zero on upgrade. `ConfigDocument`
  v3 migrates a persisted default back to unset; a value you actually chose is
  kept. `config set llm_temperature null` now unsets it.

  Rust SDK consumers reading or writing `Settings::llm_temperature` directly must
  handle the `Option`.

  Extraction previously pinned `temperature: 0.1` regardless of configuration; it
  now sends the configured value when there is one and nothing otherwise. **This
  reaches the wire on Bedrock only** — the OpenAI-compatible, Azure and Anthropic
  factories do not yet consume the setting.

  Python parity: `acreate_structured_output` passes no output cap either.

- **Teardown now closes every backend, not just the relational pool — and a
  closed graph/vector store rejects later operations.** `ComponentManager::close`
  (reached by every binding's handle teardown and by the CLI on exit) used to
  release the relational connection and leave every other component in its cache,
  on the documented premise that they "release everything on drop". They do —
  but the cache is the last strong reference, so a slot that is never emptied is
  a destructor that never runs. It now takes every slot out and closes what is
  closable.

  Three consequences for callers:

  1. **`close()` is no longer a cheap relational reset.** A caller that closes and
     then keeps using the manager pays a full re-warm: a fresh TLS handshake per
     HTTP-backed engine, and for the ONNX provider a re-read of the model file.
  2. **Operations after a close now fail** on the graph and vector stores
     (`graph database is closed`, or a closed-pool error from Postgres) where they
     previously succeeded by silently reopening. This extends the relational
     contract introduced in the previous release rather than inventing a new one,
     but it is user-visible. The `release` tier is unchanged: the handle stays
     re-warmable.
  3. **`GraphDBTrait::close` / `VectorDB::close` are new trait methods** with
     default no-op bodies, so every existing implementation — including
     out-of-tree adapters — keeps compiling untouched. That default means "this
     backend owns nothing closable beyond memory". **An external adapter that does
     own OS resources will now leak invisibly until it overrides `close`.** The
     in-tree pattern to copy is `crates/graph/tests/ladybug_close.rs`: assert no
     sidecar file survives an awaited `close()`. In-tree, LanceDB and the
     in-memory brute-force store keep the default deliberately — both were
     measured to hold no descriptor open between calls.

  `PgGraphAdapter::from_connection` / `PgVectorAdapter::from_connection` gain a
  documented guarantee alongside this: an adapter wrapping a **caller-supplied**
  connection never closes it. In the shared-Postgres layout that connection is the
  relational pool, so closing it from a store teardown would turn a leak fix into
  an outage.

  Two clarifications to that contract, from review of the change:

  - **The two teardown tiers differ in what they may touch, not just in a flag.**
    The explicit tier (`HandleState::close`, `Cognee.close()`, `cg_sdk_close`)
    closes the stores unconditionally — including one that an operation still in
    flight is holding, which will then fail on its next query. The implicit tier
    (`HandleState::release`, reached from a GC finalizer: `Drop for PyCognee`,
    neon's `Finalize`) closes a component **only when the cache holds its last
    reference.** A store's `close()` mutates state behind the shared `Arc`, so it
    is visible to every clone, and a garbage collector has no mandate to fail
    somebody's in-flight query — an explicit `close()` does. In the ordinary
    finalizer case, with nothing in flight, the two tiers release exactly the same
    resources, sidecars included.
  - **`LadybugAdapter::close` guarantees "closed to new work and checkpointed",
    not "the file is free".** It empties the slot (so every clone fails with
    `graph database is closed`) and checkpoints the `.wal`, both on the blocking
    pool. It cannot promise the descriptor and lbug's write lock are gone by the
    time it returns: an in-flight query owns a database handle for its duration, so
    a close racing one drops a reference rather than the last one. A caller that
    needs the file free — to reopen it, or to delete its directory — has to stop
    issuing queries first. The earlier wording promised the write lock was
    released; it is released once the last query finishes, which is not the same
    claim.

- **`cognee-core`: `PipelineContext::current_data` changed meaning.** It is now
  pinned, once per data item, to the value that *entered* the pipeline, and is
  never rebound as that item flows through the task chain. Previously the
  executor overwrote it before every single-value task call, so a task at
  position *k* saw the output of task *k-1* — that is, its own input, which it
  already receives as an argument. Batch-dispatched (`*Batch`) tasks previously
  got the run-level context, where the field was `None`; they now inherit the
  same item value as every other task. This matches Python cognee, where
  `PipelineContext(data_item=…)` is built once per data item in `run_tasks.py`
  and never rebound per task.

  The field's type is unchanged, so **this does not break compilation** — only
  behaviour, and only for custom `Task` implementations. An out-of-tree task that
  read `ctx.pipeline().current_data` expecting *its own* input must read its input
  argument instead. A task that wanted the originating data item (the field's
  documented purpose) now gets it at any depth in the chain.

  Pinning the value also keeps it reachable for the whole chain. This is an
  `Arc` clone — nothing is duplicated — but the allocation is now released when
  the item finishes rather than when its first task returns. Only pipelines
  whose input value *owns* a large buffer are affected (`DataInput::Binary` is
  the one to watch); those should stream the payload instead of materialising it
  in the input value. Python behaves the same way, holding `data_item` on its
  per-item context for the duration of the run.

- **`cognee-cognify`: `source_pipeline` is now stamped with Python's pipeline
  names.** Rust stamped the bare `"cognify"` / `"memify"`; Python stamps
  `"cognify_pipeline"` / `"memify_pipeline"`, so the value persisted on every
  node and edge differed between the two SDKs. Rust now matches Python. Anything
  that filtered graph data on the old short names must be updated — but note the
  mismatch already broke in-tree lookups: `dataset_resolver`,
  `cognee::api::datasets` and three HTTP routers all queried the long names the
  executor never wrote, so those paths could never match and are fixed by this
  change. `"temporal-cognify"` is unchanged (it has no Python counterpart).

- **`cognee-cognify`: `extract_graph_from_data` and `expand_with_nodes_and_edges`
  each take one additional argument.** Both gained a `task_rank` parameter so a
  caller composing a custom pipeline can supply the correct
  `DataPoint::topological_rank`; without it the deep pre-stamp inside graph
  expansion would defeat any override. Both are `pub` and therefore
  semver-visible, though every caller in this workspace and in the C/Python/TS/
  Java bindings is internal to `cognee-cognify`.

- **`cognee-cognify`: `cognify()` now honours `incremental_loading`, which
  defaults to on.** After a successful run every data item the run finished is
  marked complete on its `Data` row —
  `pipeline_status["cognify_pipeline"][<dataset id>] =
  "DATA_ITEM_PROCESSING_COMPLETED"`, Python's format byte for byte. The next run
  reads those markers before it builds a pipeline and skips the items that
  already carry one: they are not classified, not chunked, and never sent to an
  LLM.

  **Re-cognifying an already-complete dataset is therefore a no-op.** The call
  returns `Ok` with `already_completed = true`, empty payloads and no LLM spend,
  where it previously re-processed everything. `CognifyConfig` has advertised
  `incremental_loading = true` since the field was introduced; this release is
  where the claim becomes true, so **every existing deployment sees the change
  without editing its configuration** — and to anyone relying on the old
  behaviour it will look like "cognify stopped doing anything".

  Nothing is skipped that was not genuinely finished: a failed run marks nothing,
  and a sweep clears the markers of every item it rolled back, so the next run
  redoes exactly the work that was lost. A run that completed with failures still
  outstanding is likewise not a pipeline-cache hit.

  To restore the previous behaviour, build the config with
  `with_incremental_loading(false)`; every run then reprocesses every item.
  `docs/configuration.md` § "Completion markers and incremental loading" has the
  full rules.

- **`cognee-cognify`: the temporal pipeline joins the `cognify_pipeline` marker
  namespace, so a standard and a temporal cognify over one dataset are now
  no-ops for each other.** Python's temporal cognify runs under
  `pipeline_name="cognify_pipeline"` — the same string as its standard branch —
  and therefore writes and reads the same markers on both; Rust now matches.
  That is also what makes a temporal sweep's marker-clearing phase meaningful
  instead of a guaranteed no-op. Callers who want both a semantic and a temporal
  graph over the same dataset must set `with_incremental_loading(false)`.

- **`cognee-cognify`: `ExtractedTemporalEvents::events` is now
  `Vec<AttributedEvent>`, and `add_temporal_data_points` /
  `make_add_temporal_data_points_task` each take two more arguments.** Ownership
  rows are keyed per (artifact, producing data item), and the temporal stage
  previously flattened its events with no attribution at all, so there was
  nothing to key on; `AttributedEvent` carries the producing `data_id` alongside
  the event. `add_temporal_data_points` gains a `&DatabaseConnection` and an
  `Option<Uuid>` run id, and the task builder a `Arc<DatabaseConnection>`, for
  the ledger write. All are `pub` and therefore semver-visible; every caller in
  this workspace and in the C/Python/TS/Java bindings is internal to
  `cognee-cognify`.

- **`cognee-http-server` and `cognee-bench` are removed from this workspace.**
  The HTTP server moved out of this repository, taking the
  28 `test_http_*.py` cross-SDK files, their fixtures, the dual-server
  entrypoint and the locust suite with it; `e2e-cross-sdk` keeps only the
  CLI/DB lane, and the `http-parity` workflow now runs that lane alone.
  `cognee-bench` and its two driver scripts went with it (`cognee-bench` was
  never published; the `cognee-cli` `bench` subcommand is unaffected).

  **Migration.** Nothing in the library API changes — no `cognee`,
  `cognee-database`, `cognee-search`, `cognee-llm`, `cognee-logging` or
  `cognee-observability` item is added, removed or re-typed by this. Only
  users who built the `cognee-http-server` binary from this repository are
  affected.
  `cognee-http-server` 0.1.0 (published 2026-06-26, the only non-yanked
  release; the crate has carried `publish = false` ever since) is scheduled
  for yank on crates.io, since it can no longer be built from this source
  tree.

### Added

- **Re-adding audio reuses its stored transcript.** The audio loader calls a
  paid model at `add`, and running it again can change the text. A re-add of
  the same content into the same dataset (the row `persist_data` deduplicates
  against) now reuses the transcript when the audio loader wrote it and it is
  still in storage, also when no transcriber is configured any more, so
  sequential re-adds transcribe the file once. A transcript that is gone, or a
  row that holds raw audio (as a URL add stores it), is transcribed again and
  the re-add points the row at the new file. A storage error while checking
  transcribes again instead of failing the add. Two concurrent adds of the same
  new file can still both transcribe it.

- **Concurrent session-turn execution parity (SDK-1045).** Eligible built-in
  RAG, graph, and hybrid completions now overlap turn analysis with retrieval
  and answering, serialize turns per user/session identity, and retain
  independent-session parallelism. `session_search_mode` defaults to
  `concurrent` and accepts `sequential`; automatic feedback now defaults on,
  matching Python `dev`. Batch, preview, feeling-lucky, community, and
  sessionless calls retain the sequential path.

- **Cross-SDK session-turn parity fixtures (SDK-1045).** Deterministic fixtures
  derived from Python `dev` (`610119f2`) now lock raw/effective-query routing, first-turn
  preference capture, feedback-only and feedback-plus-follow-up behavior, and
  the hand-off of rated QA rows to the existing improve stage. Session-turn
  tracing reports routing and persistence counts without recording query or
  preference text.
  Concurrent analysis is bounded at Python's 30-second timeout and fails open
  to the raw query if the model does not return.

- **Sequential session-turn preparation parity (SDK-1045).** Generative
  session searches analyze feedback and corrections before retrieval, rewrite
  the effective query while preserving the raw user message in history, and
  acknowledge feedback-only turns without running retrieval. Session QA rows
  now round-trip Python's `used_session_context_ids`, including a relational
  schema migration, so only guidance proven to have been served can be rated.
  Nothing writes that field yet (the session guidance block is not ported), so
  until it is, no served-context rating is accepted.

- **`add` takes images and audio, as Python does (SDK-1039).** The CLI, the
  bindings and `ComponentManager::add_loader_registry()` give the add pipeline
  the image loader (vision LLM) and the audio loader (Whisper, when the LLM
  provider has a transcriber), so `add photo.png` / `add speech.wav` no longer
  fail with "Unsupported document type at ingest". As in Python 1d3ed15, the
  stored file is the description or transcript `text_<md5(raw)>.txt`
  (`extension` `txt`, `mime_type` `text/plain`, `raw_content_hash` its MD5,
  `loader_engine` `image_loader` / `audio_loader`) and `original_data_location`
  keeps the raw file. New API: `AddPipeline::with_loader_registry`,
  `LoaderRegistry::register_image_loader` / `register_audio_loader`,
  `process_input_with_loaders`, `make_process_input_task_with_loaders`. An
  `AddPipeline` built without a registry keeps the default loaders and still
  rejects images and audio. As Python's image loader does, `add` also sends
  `.jpeg`, `.jpe` and `.tiff` files to the image loader. Before this change
  they were stored as raw blobs.

- **`LLM_STRUCTURED_OUTPUT_MODE=json_schema` — constrained decoding on
  OpenAI-compatible endpoints, with a demotion ladder.** The adapter can now send
  `response_format: {"type": "json_schema", "json_schema": {"strict": true,
  "schema": …}}` ahead of its `tools` → `functions` → `json` cascade. On a
  backend that supports it the sampler itself is constrained to the schema,
  rather than being asked to follow one; measured on a runaway-prone chunk, the
  unconstrained `tools` arms dropped a required `target_node_id` in 8 of 11
  payloads where the strict arms were 5/5 clean. This is the shape Python sends
  on its default `litellm_native` path.

  **Opt-in, and not a pin.** `auto` is unchanged and never sends the shape:
  Python gates it on litellm's `supports_response_schema` table, there is no such
  table for an arbitrary `LLM_ENDPOINT`, and one deployment target cognee is used
  against answers HTTP 501 to a constrained request. Turning the knob on is safe
  even so, because the mode demotes rather than failing — `strict: true`, then
  the same envelope without `strict` (for gateways that reject only the keyword),
  then out to the ordinary cascade. Only an outright refusal of the request
  *shape* moves that ladder, and each step is remembered per schema, so a
  refusing endpoint pays one wasted request per distinct schema per process. The
  first probe for a schema is single-flighted, so that holds under cognify's
  concurrency — where a whole wave of chunks shares one schema — and not only for
  a serial caller.

  The Bedrock adapter does not read the knob: it has the equivalent of litellm's
  table and already picks Converse's native
  `outputConfig.textFormat.jsonSchema` for the models that advertise support.

  Also in this change: **HTTP 501 is now terminal** on the chat path, surfacing
  as `LlmError::FeatureNotSupported` instead of being retried into
  `MaxRetriesExceeded`. HTTP defines 501 as the server lacking the capability, so
  no wait can make it succeed — and a retried one would have arrived as an error
  the cascade treats as fatal, which would have made the demotion unreachable.
- **`cognee-cli edge-reindex` / `cognee_cognify::reindex_edge_types`: repair for
  `EdgeType` vector rows a killed run never wrote.** Cognify commits extracted
  edges to the graph a whole pipeline stage before it writes their
  `EdgeType_relationship_name` vector points, so a process killed between the
  two leaves graph edges with no vector row — and nothing repairs them. A retry
  finds the edge already in the graph and routes it into the claim-only bucket,
  which is never vector-indexed; the rollback sweeper is reached only from
  in-process error paths, so a SIGKILL runs none of it. Those edges then take
  the full `triplet_distance_penalty` in every retrieval lane that reads the
  collection, silently and permanently.

  This is a **robustness improvement over Python, not a parity fix** — Python
  has the identical dead end, and a comment in `add_data_points.py` calling the
  order "self-healing" on the same false in-process premise. What Python does
  ship, and this is modelled on, is the whole-graph repair
  `index_graph_edges(edges_data=None)`. Two deliberate differences: Python
  re-embeds every row whether or not it was missing, whereas this establishes
  the orphan set first with an id probe that embeds nothing and writes only
  what is genuinely absent; and reporting is the default rather than
  fire-and-forget, because the count is free and applying is billed per row.

  Orphan-ness is per distinct *retrieval text*, not per edge — one row serves
  every edge carrying that text. The reported count is deliberately not a
  crash-damage count: cognify builds `EdgeType` rows from LLM-extracted edges
  only, so the structural families (`is_part_of`, `contains`, `made_from`,
  written by a separate `add_edges` after the counting) and the DLT foreign-key
  edges have no row on a graph that never crashed either. Expect a small
  constant floor there; applying writes it too, which makes those edges take a
  real vector distance instead of the `triplet_distance_penalty` — again what
  Python's whole-graph repair does, since it counts whatever `get_graph_data()`
  returns. The CLI says so next to the number. `--limit` plus the reported `--resume-after`
  cursor make a large repair restartable; both bound the embedding work only,
  as the graph read is whole-graph and not streamed. `--dataset-id` labels the
  written rows and deliberately does **not** scope the scan: the point id is
  hashed from the retrieval text alone, so one row is shared by every dataset
  holding an edge with that text and there is no per-dataset row to repair.
  Refused inside `run-sequence`, like `vector-reindex`.

  `EdgeType::point_id_for(edge_text, relationship_name)` is added to
  `cognee-models` as the single `retrieval_text` → `deterministic_id`
  derivation, replacing the copies each lane was restating; the search lane's
  `edge_type_point_id` now delegates to it, unchanged in behaviour including
  the blank-input → `None` convention.

- **`cognee-cli vector-reindex`: an operator entry point for the pgvector ANN
  index backfill.** The pgvector HNSW index (#196, also unreleased — it ships in
  this same release) is built by `create_collection`, not by a migration, and
  the backfill that repairs a collection without one
  (`PgVectorAdapter::create_missing_vector_indexes`) was
  deliberately left un-automatic — building HNSW over a large collection is
  expensive and the operator should choose when — but it had no caller outside
  tests, so "choose when" meant writing Rust. It is now a subcommand.

  Two things leave a collection unindexed, and the second is the stronger
  reason: a collection may predate the index entirely, *or* it was created
  afterwards and its index build failed. That build is best-effort on purpose —
  propagating the error would leave the table created and unregistered, and
  every retry would then fail at `CREATE TABLE` with `already exists` — so
  pgvector without the `hnsw` access method, a restricted role, or too little
  `maintenance_work_mem` each yield a collection that answers every query
  correctly by sequential scan and reports nothing but latency. This is the
  repair for both.

  The command does the work rather than reporting first (the build is online via
  `CREATE INDEX CONCURRENTLY`, idempotent, and skips anything already indexed),
  and is refused inside `run-sequence`, where an unbounded index build would
  land on the following step's timing. `VectorDB` grows a defaulted
  `create_missing_vector_indexes()` returning `Ok(0)` so the CLI reaches it
  through `Arc<dyn VectorDB>` without a downcast; on a non-pgvector backend the
  command names the configured provider instead of printing a bare `0`.

- **`cognee-cognify`: the temporal pipeline records artifact ownership, and is
  therefore rollback-able.** `add_temporal_data_points` wrote `Event`,
  `Timestamp`, `Interval` and entity nodes, their edges, and `Event_name` vector
  points, and never touched the relational database — so nothing could name what
  a temporal run had created, and a sweep of one would have removed nothing
  while reporting success. It now claims every one of those artifacts in the
  ownership ledger, in one transaction, *before* the first store write, with one
  row per producing data item so a shared event is removed only when its last
  owning file goes. A failed temporal run now converges to its pre-run state
  exactly as a standard one does. (Python has no equivalent gap: its temporal
  task list reuses the same `add_data_points` as the standard one.)

- **`cognee-visualization`: the Python multi-view frontend is now what Rust
  renders.** `crates/visualization` previously shipped a fork of Python's
  pre-refactor single-view template, which meant no Memory tab, no Story/Flow
  layouts, no inspector, and a Schema tab that was dead code (`schema_data` was
  hardcoded `None` at both call sites). Python's `preprocessor.py` is now ported
  (the enriched node/link payload — `stage`, `visual_rank`, `degree`,
  `importance`, `label_priority`, `t_created`, `provenance`, and Python-exact
  `derive_node_name` / `edge_class` / `bundle_key` — plus the four colour maps,
  the schema graph and the operations catalog), and Python's `template.html` and
  `views/*.js` are vendored verbatim. Two areas are deliberately not ported: the
  Semantic tab emits `null` (its Python tests assert exact float equality against
  `np.linalg.svd` PCA and seeded k-means++, which is unattainable rather than
  merely expensive), and bounded k-hop rendering is unimplemented — Rust renders
  the whole graph, which is Python's `full=True` mode.

- **`cognee-models`: `DataPoint::topological_rank` is now written at runtime.**
  The field existed but was only ever `None`, so graphs produced by Rust and read
  by Python tooling fell back to the legacy `node_type_rank` layout with
  `has_meaningful_topological_rank = false`. Ranks now match Python's *default*
  pipeline numbering: because Python fuses graph extraction and summarization into
  one task, `summarize_text` shares rank 3 with `extract_graph`. Rank is inherently
  pipeline-shape-dependent in both SDKs, so the new
  `make_*_task_with_rank` builders let a custom pipeline supply its own values.

### Fixed

- **Code graph: `-0` and quoted values read and print as in Python
  (SDK-1703).** A bare `-0` number in an enola snapshot (`facts.jsonl`,
  `receipt.json`, `insights.json`, `flows.json`) is now the integer `0`, as
  Python's `json.loads` reads it, so a fact holding one hashes and describes
  as Python's does (serde_json read the float `-0.0`); `-0.0` stays a float.
  `pyjson::py_repr` is Python's `repr` for strings (quote choice, escapes,
  non-printable characters), lists and dicts, so the unsupported
  `format_version` error is Python's text, and the receipt quality log lines
  use `str` as Python's `%s` does. The code graph goldens cover three more
  snapshots (`insights`, `all_kinds`, `cluster`) and compare every node field,
  None fields included, with its `part_of`, its `belongs_to_set` points and
  Python's node order. `MockGraphDB` gains `set_get_node_error` /
  `clear_get_node_error`.

- **Ontology-derived entity graph nodes carry their provenance stamp
  (SDK-1694, stage O4b).** Graph building does not pre-stamp the entities an
  ontology lookup adds, and cognify used to write their graph nodes without
  `source_pipeline` / `source_task` / `source_user` / `topological_rank`
  (O4a kept that on purpose, to store exactly what main stored), while their
  ledger rows carried the stamp. They are now stamped before anything is
  written, as Python stamps every constructed DataPoint
  (`extract_graph_from_data.py:213-223`,
  `construct_data_points_and_edges_with_ontology.py:382`), so the graph node
  agrees with the ledger. The internal `PersistHandoff::entity_graph_stamps`
  that carried the old values is gone. The `persist_equivalence` snapshot
  changes on exactly those three nodes.
- **URL documents and ontology class hierarchies store all their edges
  (SDK-1694, O4a review).** Cognify wrote some edges before their endpoint
  nodes: a URL document's `SOURCED_FROM` chunk → WebPage edges before the
  chunks, and an ontology's type → type `is_a` edges (`Algorithm → Technology`)
  before the EntityType nodes. On Ladybug, whose edge write matches both
  endpoints first, those edges were silently dropped while their ledger rows
  were kept; on the Postgres graph, whose `graph_edge` has foreign keys,
  cognifying such a document failed with `graph_edge_source_id_fkey`. The
  persistence layer now writes every node of a run before any edge. Regression tests: `nodes_before_edges_ladybug` and, in the Postgres
  graph CI lane, `nodes_before_edges_pggraph` (new `pggraph` test feature of
  `cognee-cognify`).
- **The standalone `extract_graph_from_data` and `create_web_page_nodes` store
  every edge they claim (O4a review, follow-up).** The two public writers
  outside the pipeline still wrote edges before their endpoints. With an
  ontology, `extract_graph_from_data` wrote the ontology's entity → class and
  class → class `is_a` edges but no EntityType node; `create_web_page_nodes`
  wrote the `SOURCED_FROM` chunk → WebPage edges and left it to the caller to
  have stored the chunks first. On Ladybug those edges were dropped while their
  ledger rows stayed; on the Postgres graph the call failed on
  `graph_edge_source_id_fkey` after the ledger rows were committed. Each now
  writes the missing nodes, with their ownership rows, before any edge, as the
  pipeline's persistence layer does: `extract_graph_from_data` the EntityType
  nodes of its entities (the same rows and nodes Python's `add_data_points`
  writes for them), `create_web_page_nodes` the chunks a page sources (only
  only those the graph does not hold yet; a stored chunk is left as stored, so
  passing the extraction's input chunks cannot strip the `contains` list its
  returned chunks carry). Regression tests:
  `standalone_extraction_edges_reach_a_*_graph` and
  `standalone_web_page_edges_reach_a_*_graph` in `nodes_before_edges_ladybug`
  and `nodes_before_edges_pggraph`, plus
  `create_web_page_nodes_leaves_a_stored_chunk_as_stored` in `tasks.rs`.
- **Code graph correctness (SDK-1702).**
  - The stale sweep and the node-set detag read every code node and node set
    by type (`get_filtered_graph_data`), as Python reads the whole graph,
    instead of the repositories' one-hop neighbourhood: a stale code node that
    lost its `part_of` edge is now swept.
  - A timed-out enola run is killed and reaped before the timeout error
    returns, so it no longer writes `.enola/` afterwards. A working directory
    that vanishes before enola starts is reported as such, not as "enola not
    installed".
  - `cli cognify` and the bindings decide whether a run needs the LLM (and
    the graph extractor) from the items it will process, after items an
    earlier run completed are skipped: a dataset whose documents are already
    cognified cognifies a new code repository without an LLM. A legacy
    per-row `dlt` record is refused even when an earlier run completed it,
    instead of being skipped.
  - `cli add-and-cognify` takes the keyless path `cli cognify` has: a code
    project is cognified without an LLM, and a graph extractor that cannot be
    built for the keyless `auto` default (no LLM, no GLiNER) fails only a run
    with a document to extract; any other extractor that cannot be built
    still fails before the add. Both commands no longer fail keyless over the
    transcriber (`ComponentManager::cognify_transcriber`).
  - The bindings' `addAndCognify` checks the graph extractor before the add
    with `graph_backend::check_graph_backend_before_add`, which leaves the
    keyless `auto` default to the cognify half: a code project is added and
    cognified without an LLM, and a document is still refused with the
    keyless remedy.
  - `cli cognify` checks `--extractor` and `graph_extraction.backend` up front
    again (`ComponentManager::check_graph_extractor_selection`), so a kind
    nothing is registered under fails even on a code-only dataset.
  - `AddPipeline::add_code_repository` resolves a relative repository path,
    as a directory `add` does, so the stored `repo_path` does not depend on
    the working directory of a later run.
  - A code graph load retried after a failed edge write now ends with every
    edge. The failed load had already stored its new nodes, so the retry
    found them unchanged, skipped them, and never wrote their `part_of`
    edges. The node write now restores the missing structural edges
    (`part_of`, `belongs_to_set`) of the whole snapshot, and the node sets
    they point to. With `index_vectors`, the nodes whose edges are restored
    are embedded again with those edges, so a retry leaves no node without
    its vectors.
  - Ladybug's `get_filtered_graph_data` no longer inlines every matched node
    id (twice) into its edge query: it repeats the core-field filter on both
    endpoints and keeps the edges between matched nodes in Rust. The query
    stays the size of the filter however large the graph is, which the code
    graph sweep's read by type relies on.
  - `cli add-and-cognify` and the bindings' `addAndCognify` refuse a keyless
    `auto` default that cannot be served before the add when no input may be
    code (`code_repo::may_need_no_graph_extractor`: only text, existing
    non-code files or uploads), so such a call stores nothing, as before.
    With a directory, a code file or a URL among the inputs the add goes
    ahead, and a document among the added items then fails the cognify half,
    staying in the dataset for a retry.
  - `AddPipeline::add_code_repository` canonicalizes every path, absolute
    ones included, as Python's `realpath` does: a repository first added
    through a symlink (`/tmp/repo` on macOS) is stored under its resolved
    path when added again after upgrading, as a new row.
  - A code project's documents are now exactly the extensions of the
    registered built-in loaders. Video files, which the removed static list
    named, and files only a custom loader reads are skipped in a project.
  - `MockGraphDB::get_filtered_graph_data` applies its filters (it returned
    the whole graph).

- **A labelled text keeps its stored file as its original location
  (SDK-1043).** A `Text` wrapped in a `DataItem` (with a label or metadata)
  was stored with `original_data_location = "text://inline"` instead of its
  `file://` URI, unlike the same text unwrapped and unlike Python. Existing
  rows are not migrated: a row already written with `text://inline` keeps it
  until the same content is added again. Delete still removes its stored file
  (`raw_data_location`), so the practical impact is small.

- **`cognee-llm`: `flac` and `ogg` audio are transcribed.** The Whisper format
  whitelist left out two formats the OpenAI transcription API and Python's
  audio loader accept, so both failed with `Unsupported audio format`.

- **An audio `add` without a usable transcriber says why.** When the provider
  transcribes but its transcriber cannot be built (no API key, say),
  `ComponentManager::add_loader_registry` registers an audio loader that fails
  with the build error, as a keyless LLM already does for images. An audio
  input now fails with `audio needs a transcriber, which could not be built:
  llm_api_key must be configured` instead of `Unsupported document type at
  ingest: audio`; text adds stay quiet. A provider without transcription still
  leaves the audio loader out.

- **Delete counts an `EdgeType` point only when it exists, once (SDK-1039).**
  The delete preview counted one `EdgeType_relationship_name` point per
  text-bearing edge and the dataset run counted the requested ids, so both
  overcounted (edges sharing a retrieval text are one point; the point may be
  absent) and disagreed when the collection was missing (preview N, run 0).
  Both now look the distinct point ids up (`VectorDB::retrieve`) and count, and
  delete, only those found. A point several deleted datasets share (a `User`
  or `All` delete) counts once in the preview, as the run deletes it once. A
  failed probe fails the run and counts 0 in the preview. With per-dataset
  storage each dataset's store is dropped whole and cannot be probed, so both
  count each dataset's distinct ids.

- **`cognee-cognify`: the graph-extraction request is Python's (SDK-1039).**
  Default cognify sent a different request from Python's, and Rust wrote
  Timestamp nodes irregularly (0 in four of five captured runs, where the
  model's own answer held none). The system prompt is now Python's
  `generate_graph_prompt.txt` from `1d3ed15` byte for byte (it includes
  `71d702212`, and the Rust-only "Node Descriptions" paragraph is gone), and the
  model-facing `KnowledgeGraph` schema carries Python's text: the field
  descriptions are removed, except `Edge.description`, and the struct
  descriptions and titles are Python's. The structured-output replay cassettes
  were re-recorded because their hash includes the prompt and the schema. The
  prompt no longer reminds the model that `Node.description` is required (issue
  #66); `Node.description` stays required in the schema, and a non-strict
  provider that omits it goes through the adapter's corrective retry. Rust's default structured-output mode is still a forced tool
  call; Python's `strict` `json_schema` request is the opt-in
  `LLM_STRUCTURED_OUTPUT_MODE=json_schema`.

- **Session-turn persistence parity (SDK-1045).** A structured answer
  (`response_schema`) is now recorded as the turn's QA row, as compact JSON like
  Python's `model_dump_json`, in the sequential, concurrent and fan-out paths
  (it was dropped); intentional divergence: Python's concurrent path stores
  `model_dump_json()` but its sequential path stores `str(model)` (the pydantic
  repr), whereas both modes store the JSON here so they agree; several answer texts are joined by a blank line everywhere;
  concurrent execution also needs the session cache to be available
  (`SessionManager::is_available`, Python's `is_session_available_for_completion`).


- **Session-turn commit parity (SDK-1045).** `cognee-search` now matches
  Python's turn semantics: a concurrent or fan-out turn that produced no
  answer (empty context) commits nothing, acknowledges nothing and saves no QA
  row; a declined (feedback-only) turn is recorded in the search history, in
  every mode and also for fan-out and legacy feedback acknowledgements, and a
  fan-out one announces completion; a declined turn acknowledges even when a
  `response_schema` is set; a previous-answer rating is stored only when a
  previous turn exists, and candidate guidance is applied before served-context
  ratings, and repeated ratings of one entry within a turn all count towards
  its `helpful_count` / `harmful_count`; sequential retrieval and the answer use the rewritten query alone
  (the raw message is still what is stored and logged); batch searches skip
  turn preparation; a search with no readable dataset logs an empty result text.
  Documented intentional divergences: one logical turn across a multi-dataset
  fan-out, TEMPORAL honours `include_global_context_index`, and temporal window
  expansion re-projects external metadata
  (`docs/roadmap/not-implemented.md`).

- **`cognee-cognify`: temporal LLM failures are no longer silent.** Both temporal
  passes caught their own errors and returned `Ok`:
  `TemporalEventExtractor::extract_events` warned and returned an empty `Vec`,
  and `TemporalEntityEnricher::enrich` warned and returned the events unenriched
  — that is, stripped of every entity node and edge the pass exists to produce.
  A temporal run in which every LLM call rate-limited therefore *succeeded*, with
  zero events, no error and no failure report; the only trace was one `warn!` per
  chunk. Both now return their errors, and the extraction stage collects them the
  way the standard stage does: per chunk for extraction, and — because one
  enrichment call covers a whole batch — against every chunk that fed the batch
  for enrichment, so the failure ratio stays meaningful. The two new
  `FailureStage` variants (`TemporalExtraction`, `TemporalEnrichment`) name the
  stage honestly in the report. Python propagates at both sites, so this was a
  Rust-only divergence independent of rollback. **This is a behaviour change for
  anyone relying on the old silence**: a temporal run whose LLM is failing now
  errors instead of quietly producing nothing.

- **`cognee-core`: batch-dispatched tasks now observe cancellation.** A run
  cancelled while its *terminal* task was a `*Batch` variant used to drain the
  upstream producer, keep writing every remaining batch to the graph and vector
  stores, and then return `Ok` with status `Completed`. The executor's only
  cancellation check sat at the top of `execute_from`, *after* the empty-tasks
  base case, so `execute_from(rest=[], ..)` returned before reaching it; neither
  the `process_iter` / `process_stream` accumulation loops nor the batch branch
  of `dispatch_batch` checked at all. Both loops now stop pulling from the
  producer once cancelled, and `dispatch_batch` re-checks before invoking a batch
  task. Such a run now returns `ExecutionError::Cancelled`.

- **`cognee-core`: batch-dispatched tasks are now rate-limited.**
  `Pipeline::with_rate_limiter` and `TaskInfo::with_rate_limiter` were threaded
  only into `call_with_retry`, so a configured limiter silently did not apply to
  `*Batch` tasks — a pipeline whose LLM or embedding work happened in a batch
  task ran unthrottled. `dispatch_batch` now acquires the effective limiter
  (per-task override, else pipeline-level) once per batch call. Note the
  granularity: single-value tasks acquire once per *retry attempt*, batch tasks
  once per *batch*.

- **`cognee-core`: batch-dispatched tasks now report progress.** The per-task
  progress subtoken was completed only by `execute_from`, so a batch consumer's
  slice never advanced and `ProgressToken::root_fraction()` — exported by all
  three bindings — under-reported permanently after a *successful* run (a
  two-task pipeline ending in a batch task stuck at 0.5). The task could not
  compensate either: it was handed the run-level context whose `progress` is the
  root token, and `ProgressToken::split` had already zeroed the root's width, so
  reporting from inside was a silent no-op. Batch tasks now receive their own
  subtoken, and the accumulation loops complete that slice once the producer is
  exhausted.

- **`cognee-core`: batch-dispatched tasks now have their output provenance
  stamped.** This is the last of the four services listed above to move, leaving
  two genuinely withheld (retries and per-task watcher events); the module docs in
  `crates/core/src/pipeline.rs` record the revision. Provenance went last because
  its cost looked recoverable — a missing pipeline/task label could be filled in
  downstream. `DataPoint::topological_rank` removes that recovery: the rank write
  is single-shot (`None | Some(0)`, mirroring Python's `if current_rank is None or
  current_rank == 0`), so the first task to touch a DataPoint fixes its rank
  forever, and every DataPoint born in a batch stage was stamped with the
  *predecessor's* rank — one pipeline column short, permanently. `dispatch_batch`
  now builds a fresh `ProvenanceInputs` for the batch task itself and applies it to
  the task's `Single`, `Iter` and `Stream` outputs alike; the `Single` case was
  previously not stamped at all. Python has no such gap — `handle_task` assigns
  every task its own `task_index` regardless of batching.

- **`cognee-graph`: `created_at` now reaches consumers of `get_graph_data()`.**
  Both adapters stripped it before persisting the properties blob (Ladybug removed
  it outright; the Postgres adapter excluded it via `core_keys`), and both parsed
  it as an RFC 3339 string although `DataPoint` stores epoch milliseconds, so every
  real write silently fell back to `Utc::now()`. Python keeps the value in the
  properties blob and pops only `{id, name, type}`, so Rust now does the same, and
  a shared `parse_audit_timestamp` accepts both epoch-ms integers and RFC 3339.
  Without this the visualization's Memory-tab timeline was inert on real data.

- **`cognee-models`: `DataPoint::topological_rank` gained `#[serde(default)]`.**
  Unlike its `source_*` neighbours the field had no serde attributes, and an
  `Option<T>` without a default is still a required key — so deserializing any
  node-properties blob written before the field existed failed outright. It now
  defaults to Python's `0` sentinel and serializes as `0` rather than `null`.

- **`cognee-visualization`: embedded payloads can no longer corrupt the rendered
  page.** The 20 template tokens were substituted with sequential
  `String::replace` calls over the whole document, so a graph containing the
  literal text of a *later* token had that token rewritten inside an
  already-embedded JSON string — producing invalid JSON and a `SyntaxError` that
  blanked every tab. Substitution is now a single left-to-right pass that never
  rescans injected values, which also replaces 20 whole-document copies with one.

- **`cognee-visualization`: node colours and per-user attribution match Python.**
  `ontology_valid` was `#D8D8D8`, the value Python replaced with `#FF5CA8`
  precisely because it was indistinguishable from the `#DBD8D8` unknown-type
  fallback; `NodeSet` was missing from the type map entirely; and
  `render_multi_user` treated a present-but-falsy `source_user` (`""`, `0`,
  `false`) as already-attributed, where Python's `if not …` overwrites it.

## [0.2.0](https://github.com/topoteretes/cognee-rs/compare/v0.1.3...v0.2.0) - 2026-07-30

### Breaking changes

- **Umbrella crate renamed `cognee-lib` → `cognee`.** Depend on `cognee` instead
  (`cargo add cognee`) and import from it (`use cognee::api::remember;` rather
  than `use cognee_lib::api::remember;`). `cognee-lib` is still published as a
  thin re-export shim so existing dependents keep compiling unchanged, but it is
  deprecated and will not be maintained indefinitely. Every Cargo feature
  forwards 1:1, so no feature flags need changing. See [#93].

- **Graph data:** `Entity`, `EntityType`, and `EdgeType` node/point
  ids are now **deterministic and class-namespaced** —
  `uuid5(NAMESPACE_OID, "{ClassName}:{normalized_value}")` — matching upstream
  Python cognee's `DataPoint.id_for`. Previously entities/entity-types were
  assigned random `uuid4` ids, so the same entity duplicated across `cognify`
  runs instead of merging (issue [#57]), and database-backed edge dedup never
  matched. Ontology/temporal/memify id sites were also brought onto the same
  scheme. Graphs created before this change hold the old ids and will **not**
  merge with newly-created nodes — re-run `cognify` on existing datasets (no
  automatic migration is provided). See [#57] for details.

[#57]: https://github.com/topoteretes/cognee-rs/issues/57
[#93]: https://github.com/topoteretes/cognee-rs/pull/93

### Added

- Port HybridRetriever (HYBRID_COMPLETION) to Rust — Phase 1 core + default-off Phase 2 truth-subspace (#107)
- Azure OpenAI support (Tier 3) (#41)
- **[BREAKING]** Rename umbrella crate cognee-lib to cognee (#93)
- Native Anthropic Messages API adapter (Tier 2)
- Java SDK bindings (JNI) for cognee-rust (#82)
- Batch multiple chunks per extraction request (#19) (#63)
- Add iOS bindings with Swift async/await wrapper

### Changed

- Wrap bulk provenance writes in a transaction + configure connection pool (#35)
- Remove now-unused summarization_batch_size knob
- Bound summarization concurrency and add retry jitter
- Extract cognee-components + pluggable adapter registry (#56)

### Documentation

- Correct cognee-cli install note and workspace tree root (#92)
- Package is live on Maven Central; use version-agnostic install (#86)
- Add Swift package README

### Fixed

- Harden connect_sqlite + review follow-ups from #35 (#103)
- Accept single & repeated dataset params on GET /datasets/status (#101)
- Apply llm_max_completion_tokens to recall/search (#67) (#97)
- Single-database (relational + pgvector + pggraph) deploys (#95)
- Give aux migrators their own tracking tables for shared-Postgres deploys (#89)
- Reliably extract year-only temporal intervals (#90)
- Require node descriptions in prompt so non-strict LLMs don't fail (#66) (#88)
- Require KnowledgeGraph edges so extraction captures relationships (#83)
- Litellm-parity for OpenAI-compatible adapter (custom endpoints) (#78)
- Deterministic class-namespaced Entity/EntityType/EdgeType ids (#57) (#77)
- Type remember() result & document snake_case parity (#46) (#70)
- Npm publish path + capi-release platform/cross fixes (#62)
- Mirror ONNX Runtime downloads so builds no longer fail on upstream CDN 403s (#64)

## [0.1.3](https://github.com/topoteretes/cognee-rs/compare/v0.1.0...v0.1.3) - 2026-07-02

### Added

- Route ollama, mistral, gemini, and custom OpenAI-compatible providers (#30)

### Changed

- Optimize embeddings generation and engines (#34)
- Consolidate redundant queries and add native pgvector batch search (#36)
- Eliminate two N+1 query loops (has_edges, update_last_accessed) (#24)

### Fixed

- Enable the HTML loader in the Neon (Node.js) binding for URL ingestion (#50)
- Fail loudly when NATURAL_LANGUAGE search is unsupported by the backend (#51)
- Fix reported TypeScript SDK bugs and cross-dataset deduplication (#11)

## [0.1.1](https://github.com/topoteretes/cognee-rs/compare/cognee-models-v0.1.0...cognee-models-v0.1.1) - 2026-06-26

### Other

- reflect published registries (crates.io / npm); fix cognee-cli publish flag ([#8](https://github.com/topoteretes/cognee-rs/pull/8))
- Merge pull request #3 from topoteretes/docs/readme-point-to-site

## [0.1.0](https://github.com/topoteretes/cognee-rs/releases/tag/cognee-models-v0.1.0) - 2026-06-25

### Other

- Revise README for clarity and detail
- remove migration-plan ledger + strip phase labels from public docs
- cognee-rs v0.1.0

Release sections are generated by git-cliff when a `release:X.Y.Z` label is
applied (see docs/RELEASE.md). In-progress work lives under [Unreleased] above.
