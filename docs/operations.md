# Operations

What cognee-rust *does*. The primary surface is the **memory API** —
**`remember`**, **`recall`**, **`improve`**, **`forget`** — four high-level
operations that compose the lower-level pipeline (`add → cognify →
search`). Every operation is reachable from each interface (CLI, language
bindings, HTTP server) — see [tools/](tools/README.md). API/type detail lives in
rustdoc (`cargo doc --no-deps --open`); this page is the conceptual map.

## The memory API

Cognee's primary surface is four operations that turn raw input into queryable,
self-improving memory. They live in the [`cognee`](../crates/lib/) `api`
module (`cognee::api::{remember, recall, improve, forget}`) and surface as
the always-built `cognee-cli` verbs `remember` / `recall` / `improve` / `forget`.

```
input ──remember──▶ memory (graph + vectors, optionally session)
query ──recall────▶ auto-routed answers
        improve───▶ enriched / bridged memory
        forget────▶ removed memory
```

### remember

Stores input as memory: it runs **add + cognify** and then, by default, the
**improve** enrichment pass. Accepts inline text and/or file paths.

- **Session memory** — pass a `--session-id` to scope the turn to a session
  (session-backed QA history). The improve that bridges the session into the
  graph runs as a detached background task, as in Python: `remember` returns
  once the entry is stored. It is skipped when `improve.auto_enabled` is off
  (`IMPROVE_AUTO_ENABLED=false`) or an `improve.*` setting is invalid, and
  debounced by `improve.debounce_entries` / `improve.debounce_seconds` (state
  kept in the session as an internal `auto_improve_debounce` row). The CLI
  waits for it before exiting and a binding's `close()` before closing the
  handle (`cognee::wait_for_background_tasks`; bindings-common
  `ops::lifecycle::wait_for_background_tasks`). A detached task that panics is
  logged at `error`.
- **Permanent graph memory** — omit `--session-id` and the input is persisted as
  permanent, graph-backed memory. Its self-improvement pass (`improve()`) follows
  the same gate: off when `improve.auto_enabled` is off or an `improve.*`
  setting is invalid, as Python's `self_improvement and auto_improve_enabled()`.

- **Code repositories** — `content_type = "code"` (CLI `--content-type code`,
  bindings `contentType: "code"`) indexes each input, a local repository path
  or a remote git URL, as a deterministic code graph with no LLM: Python's
  `remember(content_type="code")`. Each repository is stored as its one
  code-repo item without its documents, and its graph is built by one
  `code_graph_pipeline` run over that item (the CODE_REPO route's enola
  adapter), which also stamps it complete for `cognify_pipeline`, so a later
  `cognify` of the dataset does not rebuild it. Runs are incremental: an
  unchanged repository (same enola snapshot) writes nothing, and a changed one
  writes only changed facts and removes what left it. The result has one item per repository, `{kind:
  "code_repository", source, path, id, pipeline_run_id}`; with
  `raise_on_error = false` a failed repository is reported on its item
  (`status: "errored"`, `error`) and the rest go on. No LLM or embedding engine
  is built or validated on this path, and `node_set` is ignored, as in Python.
  `index_vectors = true` (CLI `--index-vectors`, bindings `indexVectors`)
  also embeds the code nodes the call writes (each node's `name` into
  `{type}_name`, its structural edges' texts into `EdgeType_relationship_name`)
  for the semantic and completion retrievers; it needs an embedding provider,
  and applies to the call's own repositories only: the dataset's other
  repositories, rebuilt with them, stay graph-only.
  `session_id`, `branch`, an unknown content type, and `index_vectors` or
  `repo_credentials` without code are refused as in Python. A remote git URL
  (`https://`, `http://`, `git@`, `ssh://`) is shallow-cloned under
  `code_graph.clones_dir` (default `~/.cognee/repos`) and reused, refreshed,
  on later calls (an HTTP API request refuses a clone it could not refresh); `repo_credentials` (bindings `repoCredentials`)
  is a token for private https remotes that reaches git through its
  environment only, and a token in the URL is redacted from the item, the
  clone's directory and remote, logs and errors. Remote repositories follow
  `ALLOW_HTTP_REQUESTS` (`allow_http_requests`; allowed when unset
  outside an HTTP server) and an http(s) host must not be internal. A
  dataset's repositories are extracted together as one enola cluster, which
  gives the edges between them (a frontend call to the backend route that
  serves it): the dataset's other repositories, up to five, are rebuilt with
  the new ones by one enola run, each still under its own item and run;
  beyond five only the new ones are built and the rest are marked for the
  next `cognify`. A failure of a repository already in the dataset is reported
  in the result's `error`, never raised. Not ported yet, and refused:
  `content_type = "skills"`. rustdoc:
  `api::remember_code`, `cognee_cognify::code_cluster`.

  `add` of a GitHub/GitLab repository URL (a repository root, or any http(s)
  URL ending in `.git`) clones it the same way and resolves it like a local
  code project: one code-repo item, recording the credential-free URL as
  `system_metadata.repo_url`, plus the repository's documents. Bindings `add`
  takes `branch` to clone another branch than the remote's default, into a
  directory of its own; every branch of a URL shares the dataset's one row.

`remember ≈ add + cognify + improve`. rustdoc: `api::remember`.

### recall

Queries memory with **auto-routing**: when no query type is given, `recall`
picks the retrieval strategy by the first matching rule, as Python does: a
quoted phrase → `CHUNKS_LEXICAL`, coding-rules wording → `CODING_RULES`, a time
preposition before an absolute date → `TEMPORAL`, anything else →
`HYBRID_COMPLETION`. A routed type that the backend rejects, or a lexical or
rules search that finds nothing, is retried as `HYBRID_COMPLETION`. With no
LLM API key, recall still routes: `CHUNKS_LEXICAL` and `CODING_RULES` run as
they are, and any other routed type falls back to `HYBRID_COMPLETION`; it and an
explicit `HYBRID_COMPLETION`, `GRAPH_COMPLETION` or `RAG_COMPLETION` return
their context instead of an answer (`only_context`). No rule routes to
`CYPHER` or `NATURAL_LANGUAGE`; requested explicitly with
`ALLOW_CYPHER_QUERY=false`, they are refused. When no graph was ever built for
the datasets, or the last build failed, a graph-only recall returns a `system`
note saying so instead of searching
([configuration](configuration.md#recall--warm-up-short-circuit)). It is session-aware (reads
session history when given a `--session-id`) and graph-backed. Results are
returned to the caller (printed to stdout by the CLI).

The `scope` picks the sources: `graph`, `session`, `trace`,
`session_context`, `all`, or `auto` (the default). `all` is `graph`,
`session`, `trace` and `session_context`, in that order. With a session id and
no query type, `auto` reads the session and then the graph: with no dataset
filter a session hit skips the graph, with one both contribute. Without a
session id, or with a query type, `auto` reads the graph only. `session_first`
asks for the short-circuit explicitly, whatever else the call carries. An
explicit empty scope list runs no source and returns nothing, as Python's
`scope=[]` does; only an absent scope means `auto`. An unknown name fails with
Python's message, both lists in Python `repr` form (`Unknown recall scope(s):
['foo']. Valid values: ['all', 'auto', ...]`).
`session_context` returns the session's active guidance block (the lessons
`improve()` stored) as one entry, `{content, context_profile}`, rendered
read-only for the `context_profile` option (`qa`, the default, or `agent`);
it is empty without a session id or stored guidance. `tools` and `code` are
valid names that fail with "not supported yet" before any source runs
(SDK-1537). The `tools_trigger` option
(`always` | `on_empty`) is validated, and has no effect until the `tools`
source exists.

The `session`, `trace` and `session_context` sources read the session cache,
which is keyed by the session's owner. By default they read the caller's own
entries. A caller that can read the session's dataset reads its owner's
entries instead, when it sets the `session_cache_user_id` option to the id
`resolve_session_cache_user_id` resolves from the session records and the
caller's readable datasets (Python `_resolve_session_cache_user_id`). The
bindings do this themselves when the handle has a `ReadableDatasets` hook
(`HandleState::with_readable_datasets`), which a build with dataset
permissions attaches; without one (the OSS build, and the CLI) only the
caller's own entries are read.

The `node_name` option scopes the graph search, read according to the
`node_type` option, as Python does: without `node_type` (Python's `NodeSet`
default) the names are node-set tags, and with a graph node type's class name
(`Entity`, `TextDocument`, ...) they name nodes of that type and their one-hop
neighbours. The router then picks only a type that applies the scope
(`TEMPORAL` and `CODING_RULES` apply node sets only; a lexical pick becomes
`HYBRID_COMPLETION` with `only_context`), and an explicit query type that
cannot apply it fails with Python's message when the graph search runs, after
the warm-up note (a graph that was never built answers its note first).
Unlike Python, `CHUNKS` and `RAG_COMPLETION` apply node-set names only: Python
serves a graph `node_type` for them with a graph-scoped chunk search that
Rust has not ported, so here they refuse it rather than match the names
against node-set tags. Names with an empty `node_type`
fail before any source runs, whatever the scope; the Rust API has no
equivalent of Python's explicit `node_type=None`, so the empty string stands
in for it. Session sources are not scoped by `node_name`. The `verbose`
option is forwarded to the graph search, whose response then keeps its
context and graph.

The CLI and the bindings expose these options. `cognee-cli recall` takes
`--scope` (repeatable), `--context-profile`, `--tools-trigger`, `--node-type`,
`--node-name` (repeatable) and `--verbose` (which also prints the graph
search's response after the items). The bindings' recall opts take
`scope`, `contextProfile`, `toolsTrigger`, `nodeType`, `nodeName`,
`nodeNameFilterOperator` and `verbose` (the Python binding also accepts the
snake_case spellings). An unknown scope or `tools_trigger`, a `tools` / `code`
scope and node names with an empty `node_type` are validation errors on every
surface.

A recall (and a search) can run with its own LLM and embedding model, as
Python's `llm_config` / `embedding_config` arguments do. In Rust that is the
`engine_override` option, built with
`ComponentManager::build_engines_with_overrides` from an `llm` and an
`embedding` config section; the bindings take them as the `llmConfig` /
`embeddingConfig` opts, objects keyed by the settings' Python names
(`llm_model`, `llm_api_key`, `embedding_dimensions`, ...) or canonical keys
(`llm.model`). As in Python, each replaces the whole configured section for
the call: a setting it leaves out takes the value the environment (or `.env`
file) gave it, as Python's per-call `LLMConfig` / `EmbeddingConfig`
(`BaseSettings`) read it, else the section's default. The environment is not
read again: the values come from the configuration's env layer
(`Settings::env_layer`), captured when it was read from the environment. A
value set in code or loaded from a file is not inherited, as a fresh Python
`LLMConfig()` does not see the runtime-modified global config. The cache of
built engines is keyed by a digest of the configs, never their plain text.
One difference remains: `embedding.api_key` binds `EMBEDDING_API_KEY` and
then `LLM_API_KEY`, so with only `LLM_API_KEY` set the embedder keeps that key
under an `llm_config` with another key, where Python's embedder would follow
the call's LLM key. The call's LLM also decides
routing: one without a key answers completion types with `only_context`. An
embedding model whose width differs from the configured one fails before
anything is searched. The engines are built through the deployment's
component registry, so any registered provider can be selected, and are
cached per configuration. The CLI has no per-call overrides; it has the
configuration.

`recall ≈ auto-routed search`. rustdoc: `api::recall`.

### improve

Runs the self-improvement loop over a dataset as nine gated stages, in
Python's registry order: `feedback_weights`, `persist_session_qa`,
`persist_agent_traces`, `extract_agent_context`, `distill_sessions`,
`update_user_preferences`, `build_truth_subspace`, `review_conflicts`,
`global_context_index`. The first seven read the given sessions and are
skipped with `no_session_ids` without them; the last two work on the graph
alone and are opt-in. Each stage reports a `StageResult` (`status`, `reason`,
`error`, `counts`, `duration_ms`) on the returned `ImproveResult`, whose
`status` summarises them (`completed`, `errored`, `skipped`, `running`).
Stages named in `improve.stages_disabled` (`IMPROVE_STAGES_DISABLED`, a
comma-separated list) are skipped first, with `disabled_by_config`; an unknown
name, or the fatal `persist_session_qa`, makes the configuration invalid and
the run fails before any stage. `feedback_alpha` defaults to
`improve.feedback_alpha` (0.1). Triplet enrichment (memify) is gone, as in
Python (SDK-1115), and so are the deprecated custom-task and scoping options
(`extraction_tasks`, `enrichment_tasks`, `data`, `node_name`, `node_type`,
`vector_db_config`, `graph_db_config`); over REST they are ignored.

**Background runs.** `cognee::api::improve_in_background` is Python's
`improve(run_in_background=True)`: it validates the configuration and
`feedback_alpha` and resolves the dataset first — a name resolves to the
caller's dataset or is created for it, a dataset id that names no dataset
(`InvalidArgument`) or one the caller may not write (`PermissionDenied`; the
ACL's `write` grant, or ownership without an ACL) is refused — then spawns the
run as a process-wide background task and returns the `running` result with
the resolved dataset id and name. Wait for it with
`cognee::wait_for_background_tasks`; every binding exposes the same wait
(`waitForBackgroundTasks` in TS and Java, `wait_for_background_tasks` in
Python, `cg_sdk_wait_for_background_tasks` in C), and an `improve` op with
`runInBackground: true` uses this entry point. `ImproveParams::run_in_background`
on `improve()` itself is only a label, for a host that dispatches the run on
its own.

The run fails open — a failed stage is recorded and the next one runs —
except that a failure in `persist_session_qa` (losing session Q&A would be data
loss) stops the run with `ApiError::ImproveAborted` carrying the partial
result, and an exhausted LLM budget skips every later stage with
`budget_exhausted`. The stages that draft text with an LLM are skipped with
`no_llm_configured` when none is configured. `review_conflicts` and
`global_context_index` run by default (`improve.review_conflicts`,
`improve.global_context_index`; a per-call `review_conflicts` /
`build_global_context_index` wins either way) and report `turned_off` when off.
`review_conflicts` is not ported yet and always reports a skip reason
(`turned_off`, then `no_llm_configured`, then `edge_evidence_disabled` while
`cognify.edge_evidence_enabled` is off, then `not_ported`).

`extract_agent_context` runs while automatic session context is on
(`auto_feedback` with session caching; otherwise `auto_feedback_disabled`)
and an LLM is configured. Per session it reads the trace steps newer than the
session's extraction watermark (plus 3 already-processed ones, at most 40),
shows the model the session's existing agent lessons with Python's prompt,
and stores at most 5 new lessons as `agent`-profile session-context entries
(sections `tool_rules`, `workflow_state`, `success_patterns`,
`failure_lessons`, `environment_facts`). A lesson is stored only with
confidence ≥ 0.75 and at most 280 characters; one whose normalized text an
entry of the same section already has is not stored again. The watermark
advances only after the session's extraction succeeded. Counts: `lessons`,
`sessions_failed`; the stage is `errored` when any session failed, and the
other sessions still run. Error texts shown to the model are redacted
(tokens, secrets, UUIDs, long numbers).

`update_user_preferences` runs while `personalization_enabled` is on. It folds
the sessions' turns into one `UserPreference` node per (user, dataset), an
internal node (never embedded, never returned by a search) in the
`user_preferences` node set, and weighted `prefers` edges from it to the graph
nodes the turns' answers used (`used_graph_element_ids.node_ids`). A turn's
rating is its `feedback_score`, else the rating a feedback row inferred for
it; a rating `r` pulls each node's weight toward `(r - 1) / 4` by
`preference_alpha`, from its decayed value
`0.5 + (w - 0.5) * (1 - preference_beta)^(idle turns)`. Every turn advances
the node's turn counter once; an edge that decayed to within 0.01 of neutral
is retired. The node's text collects the usable `preferences`-section
session-context entries newer than its watermark, newest first, capped at
2000 characters on line boundaries. Turns are marked in `memify_metadata`
(`preference_turn_counted`, `preference_weights_applied`), so a re-run
counts and applies each once. Counts: `turns_applied`, `edges_written`,
`edges_pruned`, `text_lines_added`; nothing to fold is `completed` with the
reason `no_changes`.

With personalization on, searches read these back for the requesting user in
the search's single dataset: the decayed `prefers` weights scale the matched
nodes' distances in graph triplet ranking (every graph completion type) and
re-rank RAG completion's chunks (fetching `wide_search_top_k` candidates
first, then trimming to `top_k`), by at most `personalization_influence`;
graph and RAG completion also append the preference text (under
`## What this user prefers`) to the question and context. The hybrid search
uses them too: they weight its chunk ranking, and its completion and preview
carry the preference text as guidance. On a session search the preference
lines instead join the `Preferences` section of the one "Active session
guidance" block (see the session search docs): there is no second block.

A run holds its dataset's lock for its whole length, then a claim on every
session it targets (`session:{user_id}:{session_id}`), and releases both once
it ends. Both wait, with no timeout: improves that share a dataset or a session
run one after the other, as pipeline runs on one dataset do, instead of the
later one skipping; runs on different datasets and sessions do not wait. The
dataset lock is Python's one `dataset_lock` registry: `cognee_core::DatasetLocks::global()`
is the default of `add` (`AddPipeline`), dataset-scoped deletes
(`DeleteService`) and `improve` (`ImproveParams::dataset_locks`, else the add
pipeline's registry, else the global one), and `cognify` always
holds it for its run, so in one process those writers queue per dataset. A
host with its own registry wires it into the add pipeline, the delete service
and `improve`; the HTTP server shares the global one. The stages' own adds and
cognifies re-enter the improve's hold. A dataset that does not exist yet is
held by the id an `add` of it would create.

`global_context_index` (on by default, `improve.global_context_index`; per-call `build_global_context_index`) builds the
dataset's global context index with Python's default `from_entities` method,
which makes no LLM call: one `GlobalContextSummary` bucket per document
naming the entities it is distinctively about (tf·idf over the dataset's
chunks, up to 20) and up to 40 relation lines among them, plus a root of
three lines (`Topics:`, `Entities:`, `Documents:`). Ids are Python's uuid5
ids, so the text and ids match Python's byte for byte. Each run computes the
whole index, writes only the nodes whose text changed (graph node, vector in
`GlobalContextSummary_text`, `summarized_in` edge to the root and
`summarizes` edge to the document) and deletes the index nodes it did not
compute; the nodes' ownership rows name the dataset, so deleting the
dataset removes them. Its count `nodes_written` is the nodes it wrote. The
opt-in LLM method (vector-placed buckets of `TextSummary` nodes, levels, LLM
bucket and root summaries with Python's prompts) is
`cognee_cognify::build_global_context_index` with
`GlobalContextIndexMethod::Llm`; `improve()` does not use it, as Python's stage
does not. A graph completion search with `include_global_context_index`
prepends the root and the `global_context_index_top_k` (3) nearest bucket
summaries to its context (`World summary:` / `Relevant areas:`). A
dataset-scoped search reads only its datasets' root and summaries (their
`dataset_id`), so datasets sharing one graph and vector store never see each
other's index. With no triplet found, the prelude alone is the context, and an
`only_context` search previews the prompt built from it, as Python's
`get_context_from_objects` does.

`global_context_index` reports `already_completed` (`no_writes_since_last_improve`)
when no `pipeline_runs` row other than a `search`, a `recall` or a host's
`improve_pipeline` record was written since it last ran. A delete records a
`forget` row on every dataset it touched, and an improve run whose stage may
have written to the graph (anything but the index itself, unless its write
counts say it wrote nothing) records one `improve` row, so both count as
changes, as in Python. A run that did not end `errored` stamps the stage's start in the `run_info` of
its `improve` operation row (`{"global_context_index": {"status", "started_at"}}`),
where Python keeps it (`graph_changes.py`); the newest such row is the
watermark, and the row itself is not a change. Without a relational database,
a stamp, or on any error reading them, the stage runs.
rustdoc: `api::improve`.

### forget

Removes memory: a whole dataset, a specific data item, or everything. Cascades
across the relational, graph, and vector backends and file storage. rustdoc:
`api::forget`.

Deleting a data item removes what no other data item still claims in the
ownership ledger, as Python's graph-provenance delete does with its source
refs: an entity, an entity type or a chunk goes with the last document that
produced it (or, for an entity or ontology edge, reached it through an
ontology lookup). An edge is counted by its `(source, target, relationship)`
triple and goes with the last document that produced it even when both its
endpoints stay — an `is_a` edge with the last document that declared that
entity with that type, a semantic edge with the last document that extracted
it — and its `EdgeType` point goes once no surviving edge produces its text.
Only a per-document ledger row says no other document holds an edge: an edge
only an older or Python-written row names (one row per edge, its first
writer's) leaves the graph only with an endpoint. Ledger rows with a nil
`data_id`, which databases written before SDK-1694 O6 hold for every entity
type and structural edge, still claim their artifact until its dataset is
deleted. A delete does not lock against a running cognify of the same data.

## Lower-level pipeline

The memory API composes these building blocks. They remain available directly
when you need fine-grained control over each stage. The classic flow is:

```
raw data ──add──▶ stored + deduplicated ──cognify──▶ knowledge graph + vectors ──search──▶ answers
```

### add (ingest)

Streams input, computes a content hash, deduplicates, and persists the data plus
metadata. Accepts text, file paths, and HTTP(S) URLs (fetched and routed by MIME
type). As in Python, `data` rows are dataset-scoped: a new row gets a random
`uuid4` id, and re-adding content dedups only within the same dataset, owner
and tenant (by content hash), so the same content in two datasets is two rows.
Dataset ids stay deterministic (UUID5 of name, owner and tenant). Pipeline:
[`cognee-ingestion`](../crates/ingestion/) (`AddPipeline`).

Stored files are named by content hash under `data_root_directory`, as in
Python: extracted text as `text_<hash>.txt`, a fetched page as
`text_<hash>.html`, other fetched content as `fetched_<hash>.<ext>`, and an
upload (`DataInput::Binary`) as it arrived, as `<hash>/<file name>`, which the
row's `original_data_location` points at. The upload's file name is the part
of its name after the last `/` or `\`; an add fails when that is empty, `.` or
`..`, holds a NUL byte or is longer than 255 bytes. Rows from older builds
that say `binary://<name>` are corrected when the same upload is added again.
A stored file is shared by every row that points at it. Delete removes it,
with an emptied `<hash>/` folder, only once no row points at it any more, and
a re-add that moves a row on to another file (say, the same upload under a
new name) removes the file it left the same way.
As in Python, fetched content is rewritten in place on a re-add (write, then
rename), while a loader's text keeps the first version stored: a second
transcription or image description of the same file, say for another
dataset, does not replace it. A path that would
leave the storage root is refused. While an `add` runs, the files it stored are
claimed, so a concurrent delete in another dataset keeps them until the add's
rows are saved (within one process, as in Python). Rows from builds before this
change point at random `ab/cd/` folders; they keep working as they are.

A directory path is resolved first (Python's `resolve_data_directories`). A
directory whose root carries a project marker (`pyproject.toml`, `go.mod`,
`package.json`, `Cargo.toml`, `.git`, ...) is a **code project**: it is added as
one repo manifest item (a small JSON document naming the repository and its
source files, stamped `system_metadata.source = "code_repo"`) plus its document
files (`README.md`, PDFs, images, ...), each added on its own. Hidden files,
build directories (`node_modules`, `target`, ...), binaries, media, symlinks and
Git LFS pointers are skipped. Without a usable LLM the documents are left out
too: their routes need one, the code graph does not. Any other directory is
added file by file. See [`cognee-ingestion`'s `code_repo`](../crates/ingestion/src/code_repo.rs).

### cognify (knowledge-graph extraction)

Turns stored data into a knowledge graph in four stages: **classify** documents →
**chunk** text → **extract** entities/relationships (LLM) **and summarize**
(conditional), then **add data points** (six vector collections + provenance to
the relational DB) — one streaming stage that saves **each document as soon as
all its extraction and summary results are in**, as Python's per-data-item
`extract_graph_and_summarize` → `add_data_points` does: every chunk's calls go
through one bounded stream (no batch barrier), each finished document's graph is
built on the CPU pool and saved by a single writer in input order, and at most
`cognify.max_unsaved_documents` documents are in progress at once (see
[configuration](configuration.md)) → **extract DLT FK edges**, after the run. A DLT row (a document whose `external_metadata` has
`source: "dlt"`) is one `DltRow` chunk indexed into `DltRow_text`, not
`DocumentChunk_text`; the last step adds its `SchemaTable`, `SchemaRelationship`
and `DltColumn` nodes (vector collections `SchemaTable_*`,
`SchemaRelationship_*`, `DltColumn_properties`) and the row edges. Configurable via `CognifyConfig`
(chunk strategy, custom prompts/schemas). Pipeline:
[`cognee-cognify`](../crates/cognify/) (`cognify()` / `cognify_datasets()`).

**Shared SQLite with Python.** Python's alembic revision `f2b4c6d8e0a1` moves
every `external_metadata.source` of `"dlt"` / `"dlt_source"` into
`system_metadata`, Rust-written rows included. A per-row DLT record whose stamp
was moved (`system_metadata.source == "dlt"`) is then refused as a legacy
record (`CognifyRouteError::LegacyDltRecord`, Python's message), even when an
earlier run completed it, as Python refuses it. A moved `"dlt_source"`
manifest is refused too (`UnsupportedRoute`): Rust has no DLT-source route.
Delete such rows and re-add the data.

Each item first takes its **route** (`cognee_cognify::routing`, Python's
`cognify_route_for`), read from its `system_metadata`: a code project's repo
item (`code_repo`) runs the deterministic code graph instead of the stages
above — one `enola --generate` over the original directory, its snapshot
mapped to code nodes and typed edges ([`cognee-code-graph`](../crates/code-graph/)),
tagged with the item's node set, recorded in the ownership ledger, with no LLM
and no embedding call. The load is incremental: an unchanged snapshot of an
untagged repository is skipped, and a changed one writes only changed facts,
sweeps the nodes and edges that left it and strips node-set tags the call no
longer passes. A dataset holding several code repositories is one enola
cluster: the run's first repo item runs enola once over all of them (into
each one's `.enola-<dataset_id>`) and loads every repository from that
snapshot, each under its own item, with the edges between them; the run's
other repo items have nothing left to do. Forgetting the item removes its code graph and leaves other
repositories' alone. Every other item runs the stages above. Both kinds share
one pipeline run and its completion markers. A per-file code item
(`source: "code"`, a lone source file added through `code_loader`) loads the
code graph of that one file: its stored text is staged in a temporary
directory under its original name (plus a `tsconfig.json` for plain
JavaScript) and enola runs on it, as a repository named `<file name>_<data
id>`, untagged, with no LLM and no embedding. A run whose items
are all code-routed resolves no graph extractor, so a deployment without an
LLM key builds a code project's graph.

Dates come out of the default extraction, with no separate temporal pipeline
(Rust's event-graph `temporal_cognify` was removed in SDK-1039; Python ignores
the flag since 1.6.3): a node the LLM types `Timestamp` whose name is an
absolute time (`1867`, `1969-07`, `23 March 1947`, `from 1803 to 1815`) becomes
a shared `Timestamp` graph node — one per normalized time string, carrying
`time_at`/`time_until` (ms since the epoch, UTC, half-open) and its `precision`
— that `*_at` edges point at and every chunk naming it `contains`. Timestamps
are not embedded. A name that does not parse stays an ordinary entity. Year-less
dates in the text ("27 April", "four weeks later") are resolved against the last
stated year earlier in the document and handed to the default prompt as
normalization hints; a custom prompt receives none.

An extracted entity's id derives from its **name** (`Entity.id_for(name)`), as in
Python, so the same name from two chunks or documents is one node; several nodes
sharing a name inside one chunk's graph get deterministic ordinal ids instead.
An extracted node with an empty type gets no `EntityType` and no `is_a` edge.
`COGNEE_COGNIFY_EXTRACTION_STRATEGY=cascade` swaps the single extraction call
for Python's multi-round cascade (see
[configuration](configuration.md#chunking--tokenizer)).

#### One document at a time (SDK-1694)

cognify processes each document the way Python processes each data item, and
saves it on its own:

- **The graph is built per block.** A block is one document's chunks, or
  `chunks_per_batch` of them for a longer document (Python's unit). Each block
  is expanded with fresh maps, so within a block the first type the model
  gives an entity wins, and an edge endpoint matches only a node of the same
  block — never another document's. `cognify.cross_chunk_endpoints=false`
  narrows that to the edge's own chunk, by node id, as Python does.
- **An entity keeps every document's type.** The blocks' types are combined,
  one `is_a` edge each, so an entity two documents type differently has two
  types and, without an ontology, never more types than documents. Its
  description, importance weight and metadata come from the first block in
  input order (Python's last writer, which depends on timing, wins there);
  NodeSet memberships are combined.
- **Relationship labels are normalised** as Python's `generate_edge_name` does
  (lower case, spaces to `_`, apostrophes dropped), so `Born In` and `born_in`
  are one edge. An edge already in the graph — matched on its final ids and
  label — is claimed by the new document (a ledger row, no write). Edges
  stored under a raw label by an older build stay as they are, and
  re-cognifying their data adds a normalised copy beside them.
- **Each document is saved once all its results are in.** Extraction and
  summary calls of every chunk go through one stream each, at most
  `max_parallel_extractions` in flight and no wait at block boundaries. A
  per-document assembler builds a finished document's graph on the CPU pool,
  and one writer saves documents in input order — ledger rows, then nodes,
  then edges, then vectors and edge evidence. An entity a later document
  changes (a new type, a new NodeSet) is rewritten. At most
  `cognify.max_unsaved_documents` documents (default 32) are dispatched but
  not yet saved, which bounds the LLM results held in memory.
- **Each document is completed on its own.** With `incremental_loading` (the
  default), the writer marks a document complete as soon as it is saved, and
  a failed run keeps what it completed — the default rollback scope is
  `failed_items` (see [When a cognify run fails](#when-a-cognify-run-fails)).
- **The ledger names the documents.** Every entity, entity type, `is_a`,
  `contains`, `is_part_of` and `made_from` edge, and every ontology edge a
  document's lookup brought in, gets an ownership row per producing document,
  so [deleting a document](#forget) removes the types and edges only it
  produced.

#### When a cognify run fails

A chunk or a file the pipeline cannot process is not a bare error: the chunking,
graph-extraction and summarization stages *collect* their failures, and the run
result carries a report naming which file, which chunk, which stage and what
went wrong. `CognifyResult.failures` carries it on success;
`CognifyError::RunFailed` carries the same report when the run failed.

What happens to what the run already wrote is decided at the end, from two
independent settings — *when to stop* (`FailFast`, the default, or `RunToEnd`)
and *what to sweep* (`WholeRun`, `FailedItems`, or `Nothing`; unset, it is
`FailedItems` with `incremental_loading` — the default — and `WholeRun`
without, as Python keeps stamped items only with incremental loading). The
full matrix is in
[Cognify failure handling](configuration.md#cognify-failure-handling); the end
states are:

- **`WholeRun` (the default with `incremental_loading` off).** The run stops
  dispatching at the first failure — the calls in flight still finish, see
  below —
  everything it created is removed, the run is recorded `ERRORED` and the call
  returns `Err`. The store converges to its pre-run state — what earlier runs
  completed is untouched, because the sweep selects only rows naming *this*
  run. Python's end state without incremental loading.
- **`FailedItems` (the default with `incremental_loading`).** A run in which
  any file failed errors and returns `Err` with the failed-file list in its
  report, as Python's does — but keeps the files it completed, indexed and
  marked, as Python's per-item rollback does. Each file is saved and marked
  complete as soon as it is done (SDK-1694 O5b), so only what the run did not
  complete is removed, and the next run redoes exactly the failed files. (The
  chunk failure ratio no longer decides anything.)
- **`Nothing`** removes nothing. The escape hatch: the files saved before a
  stop stay, unmarked, and nothing more is saved after it.

Under `FailFast` a run partitions its files three ways at the abort point:
*complete* (every call back — persisted and marked), *failed* (at least one
chunk failed, or its save failed) and *unreached* (never attempted). Only
complete files are kept, which is what keeps a data item all-or-nothing;
failed and unreached files are indistinguishable to the next run and are
simply redone. A DLT row is complete only once the post-run step has written
its foreign-key edges; a code item once its graph is loaded.

The abort point is not a file boundary, and how much work `FailFast` actually
saves depends on how many calls are in flight. Graph extraction and
summarization each run every chunk of the run through one stream, with no
barrier between `chunks_per_batch` blocks: they stop dispatching as soon as a
failure comes back, but the calls already in flight (up to
`max_parallel_extractions`) are neither cancelled nor discarded. A file with a
chunk that was never dispatched is unreached, even if its other chunks
succeeded. A graph backend is the exception: it is handed `chunks_per_batch`
chunks per call and stops after the call that failed.
`FailFast` is an upper bound on wasted spend, not a promise of none — see
[Cognify failure handling](configuration.md#cognify-failure-handling).

A sweep deliberately keeps anything still claimed from outside its scope: an
entity a surviving file also produced, an artifact another run or another
dataset also names, or a row written before ownership tracking existed. The
worst case is a surplus artifact that keeps an owner, never an artifact with no
owner at all.

That claim is recorded even when the artifact is not written. Graph extraction
filters out edges the graph already holds, so a file cognified in a later run
adds no second copy of an edge an earlier file created — but it still records
ownership of it. Without that row the earlier file would be the edge's only
owner, and deleting it would take the edge's vectors away from a file that still
references it. Ownership is per (dataset, data item): an edge shared across two
*datasets* is still governed by each dataset's own exclusivity check, the same
limitation entity ownership has.

One caveat. **Cancellation is swept**, unlike Python — though there is no
caller-facing cancel handle on `cognify()` today, and simply dropping the future
runs no sweep at all.

#### Re-cognifying is now incremental — a behaviour change

`incremental_loading` is on by default and now does what it says. A run marks
each data item it finished, in Python's own `pipeline_status` format — under
`FailedItems` the moment the item is saved, under `WholeRun` once the run
succeeded — and the next run skips the marked items before it builds a
pipeline — no classification, no chunking, no LLM calls.

**Re-cognifying an already-complete dataset is therefore a no-op**, returning
`Ok` with `already_completed = true`. Deployments that relied on cognify
re-processing everything on every call will see it "stop doing anything"; set
`with_incremental_loading(false)` to restore the previous behaviour. A failed
run keeps the marks of the files it completed (none under `WholeRun`), and a
sweep clears the markers of whatever it rolled back, so the next run redoes
exactly the work that was lost.

The markers live under Python's one `cognify_pipeline` key.

### search (retrieval)

Unified orchestration across 17 retrieval strategies selected by `SearchType`
([`crates/search/src/types/search_type.rs`](../crates/search/src/types/search_type.rs)):
`GraphCompletion`, `GraphCompletionCot`, `GraphCompletionContextExtension`,
`GraphCompletionDecomposition`, `GraphSummaryCompletion`, `GraphReport`, `RagCompletion`, `Chunks`,
`Summaries`, `Temporal`, `Cypher`, `NaturalLanguage`, `FeelingLucky`, `Feedback`,
`CodingRules`, `ChunksLexical`, `HybridCompletion` (default; combines vector search over
chunks/summaries/entities/edge-facts and
1-hop graph-neighborhood expansion around matched entities, then answers via
LLM completion). A hybrid search falls back to `GraphCompletion` when it sets
`node_type`, `neighborhood_depth` or a positive `feedback_influence` (an unset
one takes `DEFAULT_FEEDBACK_INFLUENCE`, so a positive default falls back too), or when the
`DocumentChunk_text` collection is missing; setting `wide_search_top_k` or
`triplet_distance_penalty` with hybrid is an error. Entry: [`cognee-search`](../crates/search/)
(`SearchBuilder` / `SearchOrchestrator`).

The graph retrievers (`GraphCompletion` and its CoT, context-extension and
summary variants) rank triplets around the vector-search hits. A triplet's
score, as in Python, sums its source node's, target node's and edge's cosine
distances, each scaled by `2 - importance_weight` (0.5 when absent) and, with
`feedback_influence > 0`, blended with its `feedback_weight` (higher feedback
ranks better). An element with no vector match takes `triplet_distance_penalty`
(6.5) before that scaling — 9.75 at the default weight — and is not blended.
`importance_weight` is not clamped, as in Python: above about 1.69 the scaled
penalty is blended like a real match, and above 2 it turns negative and ranks
first. By default they
rank only the edges touching a hit. `neighborhood_depth` (a positive integer;
the CLI's `--neighborhood-depth`, the bindings' `neighborhoodDepth`) switches to
Python's neighborhood projection: load the `depth`-hop neighborhood of the
nearest `neighborhood_seed_top_k` hits (all of them when unset), score the
nodes the expansion found against the query (`VectorDB::score_by_ids`; an
adapter without it leaves them at the distance penalty), and rank every loaded
edge. Combined with `node_name`, which disables the seed search, it returns no
triplets, as in Python.

`brute_force_triplet_search_batch` is Python's batch mode
(`query_batch=`): several queries share one graph load and come back as one
ranked list each. As in Python it searches every vector collection without a
limit and loads the whole graph (or the NodeSet subgraph), so it costs far more
than a single search on a large store — `search.triplet_neighbor_cap` does not
bound it, and `wide_search_top_k`, `node_name` (in the vector search) and
`neighborhood_depth` do not apply. Unlike Python it skips `Triplet_text`, whose
ids are never graph nodes. `GraphRetrieverCore::search_batch` sends a lone
query to the single-query search instead, as `get_triplets_batch` does.

`GraphCompletionDecomposition` (`GraphCompletionDecompositionRetriever`, Python's
retriever of the same name) is its main caller. One structured LLM call splits the
question into at most five sub-questions (falling back to the question itself when
the call fails), one batch search fetches their triplets, and the batches are merged
without duplicates. With `retriever_specific_config.decomposition_mode`
`"answer_per_subquery"` (the default) each sub-question is answered over its own
triplets, concurrently, and the question is answered over those answers;
`"combined_triplets_context"` answers once over the merged triplets. Each context
item carries a `decomposition` entry (the sub-questions and the triplet's rank in
each) so the completion can rebuild the per-sub-question contexts. An empty graph is
`NotFound`, and a batch of queries is rejected.

`GraphReport` (`GraphReportRetriever`, Python's retriever of the same name) ignores
the query and reports on the whole graph from `get_graph_data`: the top-`top_k`
hubs (default 10) by normalised degree plus normalised PageRank (networkx's
defaults, computed here), entity-to-entity edges across different node sets, and
EXTRACTED vs DERIVED edge counts, followed by LLM-suggested questions. `NodeSet`
containers and `belongs_to_set` edges are left out and parallel edges count once,
as in Python's `nx.DiGraph`. Its context is one item holding the Markdown report and
the structured hubs, connections and counts; an `only_context` search answers the bare
report text, as Python's does. An empty graph gets a fixed report, not
`NotFound`. Python computes PageRank only when `scipy` is installed (it is not a
cognee dependency) and otherwise ranks by degree alone, so its PageRank column reads
`0.0000` on a default install; Rust always computes it.

`Temporal` is `HybridCompletion` reranked by the time window the question asks
about (`TemporalHybridRetriever`, Python's retriever of the same name). The hybrid
candidate fetch — chunks and entities at `candidate_top_k`, default `4 × top_k`,
settable as `retriever_specific_config.candidate_top_k` — runs concurrently with an
LLM call that extracts the window (UTC, start inclusive, end exclusive; relative
dates are not resolved, so no clock is read). The chunks attached to the
`Timestamp` nodes inside the window join the candidates, the candidates attached to
one — directly, or for a chunk through an entity it `contains` — move to the front,
tightest date first (for "18 March 1965" a chunk dated that day beats one that only
says "1965"; dates at least as precise as the question keep their similarity order),
and the same cut plain hybrid uses keeps `top_k`. With nothing to rerank by it
answers with the plain hybrid result and records why: `empty_graph`,
`no_time_constraint` / `invalid_interval` (no window), `no_temporal_match`,
`no_candidate_overlap` (`TemporalHybridRetriever::retrieve` returns these
diagnostics with the context). The `Timestamp` nodes are default cognify's. A
graph an older build wrote with the removed event-graph pipeline holds `Event`
nodes nothing reads, so it gets plain hybrid answers, as in Python.

## Additional operations

These live in the [`cognee`](../crates/lib/) `api` module (and `DatasetManager`):

| Operation | What it does | rustdoc |
|---|---|---|
| **delete** | Cascading removal of data/datasets across relational → graph → vector → file storage (with dry-run preview). | [`cognee-delete`](../crates/delete/) `DeleteService` |
| **update** | Replace one document and re-cognify it. The document **keeps its `data_id`** (a pre-fork `legacy_id` resolves to the row it forked into; an unknown id is a 404): its memory is dropped, the row is re-added pinned to the same id, and cognify rebuilds it. | `api::update` |
| **prune** | Reset system or all state (`prune_system` / `prune_data`). | `api::prune` |
| **cleanup unused data** | Hard-delete the documents no search has touched for `minutes_threshold` minutes (or ever), optionally only in one user's datasets; dry run by default. Needs `ENABLE_LAST_ACCESSED` and is skipped until a search has tracked something. An operator task that deletes without an ACL check — without a user it deletes every owner's unused documents, where Python (always deleting as the default user) only manages the default user's; like Python's `cleanup_unused_data`, no CLI, route or binding exposes it. | `api::cleanup_unused_data` |
| **visualize** | Render the graph to a single-file d3.js HTML page (Graph / Schema / Memory / Semantic tabs + inspector). | [`cognee-visualization`](../crates/visualization/) |
| **export** | Write the graph as a COGX archive — the portable format Python cognee re-imports, locally or by uploading the packed `.cogx.tar.gz` to its remember endpoint. | [`cognee-migration`](../crates/migration/) |

## Operation → interface map

| Operation | CLI | HTTP route | Binding method |
|---|---|---|---|
| remember | `cognee-cli remember` | `POST /api/v1/remember` | `remember()` |
| recall | `cognee-cli recall` | `POST /api/v1/recall` | `recall()` |
| improve | `cognee-cli improve` | `POST /api/v1/improve` | `improve()` |
| forget | `cognee-cli forget` | `POST /api/v1/forget` | `forget()` |
| add | `cognee-cli add` | `POST /api/v1/add` | `add()` |
| cognify | `cognee-cli cognify` | `POST /api/v1/cognify` | `cognify()` |
| add + cognify | `cognee-cli add-and-cognify` | _(two calls)_ | — |
| search | `cognee-cli search` | `POST /api/v1/search` | `search()` |
| delete | `cognee-cli delete` | `POST /api/v1/delete` | `delete*()` |
| update | _(via run-sequence)_ | `POST /api/v1/update` | `update()` |
| visualize | `cognee-cli visualize` | `POST /api/v1/visualize` | `visualize()` |
| export | `cognee-cli export` | _(none yet)_ | — |

CLI flags and feature gates: [tools/cli.md](tools/cli.md). HTTP request/response
shapes: documented with the server, which is not part of this repository (see
[tools/http-server.md](tools/http-server.md)). Binding method
names per language: [tools/bindings.md](tools/bindings.md).
