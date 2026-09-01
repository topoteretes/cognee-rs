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

### Added

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
