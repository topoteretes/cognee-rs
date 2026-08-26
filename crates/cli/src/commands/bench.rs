//! `cognee-cli bench` — the performance orchestrator driver.
//!
//! Ports Python's `bench_cognee.py`: runs the full
//! `prune → setup → add → cognify → search → dataset delete` pipeline once,
//! times each phase, and writes a result JSON with the exact Python schema so
//! the shared orchestrator/reporter can drive either SDK unchanged.
//!
//! Exit-code policy (Python parity): once the run completes and the result
//! file is written, exit `0` even if individual phases failed (failures are
//! captured in `status` / `success`). Exit nonzero only for catastrophic
//! errors: bad arguments, an unreadable corpus, or an unwritable `--output`.

use std::sync::Arc;
use std::time::Instant;

use cognee::add::AddPipeline;
use cognee::api::prune::{PruneTarget, prune_data, prune_system};
use cognee::api::{DatasetDb, DatasetManager};
use cognee::cognify::{ChunkStrategy, CognifyConfig, TokenCounterKind, cognify};
use cognee::core::RayonThreadPool;
use cognee::database::{IngestDb, PipelineRunRepository, SeaOrmPipelineRunRepository, ops};
use cognee::models::DataInput;
use cognee::ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee::search::{
    SeaOrmSessionStore, SearchBuilder, SearchRequest, SearchType, SessionManager,
};
use cognee::{ComponentManager, PipelineContext};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::BenchArgs;
use crate::error::CliError;

/// A single corpus entry: `{title, content, references}`.
///
/// `references` is permissive — it accepts either a JSON array of strings or a
/// plain string, matching the Python loader's tolerance.
#[derive(Debug, Deserialize)]
struct Memory {
    #[serde(default)]
    title: Option<String>,
    content: String,
    #[serde(default)]
    references: Option<serde_json::Value>,
}

/// Config block echoed back in the result JSON (Python parity).
#[derive(Debug, Serialize)]
struct BenchConfig {
    llm_model: String,
    embedding_model: String,
    embedding_dimensions: u32,
    dataset_name: String,
    mock_llm: bool,
}

/// Result document written to `--output`. Field order matches Python so the
/// emitted JSON is byte-comparable where it matters.
#[derive(Debug, Serialize)]
struct BenchResult {
    memories_count: usize,
    add_time_s: f64,
    cognify_time_s: f64,
    total_ingest_time_s: f64,
    prune_time_s: f64,
    db_setup_time_s: f64,
    search_time: f64,
    dataset_delete_time_s: f64,
    status: BenchStatus,
    success: bool,
    config: BenchConfig,
    /// Graph size after cognify. Also drives the stale-cassette guard.
    node_count: i64,
    edge_count: i64,
}

/// Per-phase status: `"success"` or `"failed: <msg>"` (Python parity).
#[derive(Debug, Serialize)]
struct BenchStatus {
    prune: String,
    db_setup: String,
    add: String,
    cognify: String,
    search: String,
    dataset_delete: String,
}

const PHASE_OK: &str = "success";

/// Start a SIGPROF sampling profiler for one phase, if profiling is enabled and
/// `--profile-dir` was given. Returns `None` (a no-op) otherwise. pprof-rs uses
/// signal-based sampling, so it needs no `perf` permissions and no root. It
/// captures every thread in the process, both the tokio workers and the Rayon
/// pool, which is what the mocked replay needs.
#[cfg(feature = "profiling")]
fn start_phase_profiler(profile_dir: Option<&str>) -> Option<pprof::ProfilerGuard<'static>> {
    profile_dir?;
    match pprof::ProfilerGuardBuilder::default()
        .frequency(997)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
    {
        Ok(guard) => Some(guard),
        Err(error) => {
            warn!("profiler: failed to start: {error}");
            None
        }
    }
}

/// Stop a phase profiler and write `<profile_dir>/<phase>.svg`.
#[cfg(feature = "profiling")]
fn finish_phase_profiler(
    guard: Option<pprof::ProfilerGuard<'static>>,
    profile_dir: Option<&str>,
    phase: &str,
) {
    let (Some(guard), Some(dir)) = (guard, profile_dir) else {
        return;
    };
    let report = match guard.report().build() {
        Ok(report) => report,
        Err(error) => {
            warn!("profiler: failed to build report for {phase}: {error}");
            return;
        }
    };
    if let Err(error) = std::fs::create_dir_all(dir) {
        warn!("profiler: cannot create dir '{dir}': {error}");
        return;
    }
    let svg_path = format!("{dir}/{phase}.svg");
    match std::fs::File::create(&svg_path) {
        Ok(file) => match report.flamegraph(file) {
            Ok(()) => info!("profiler: wrote {svg_path}"),
            Err(error) => warn!("profiler: flamegraph write failed for {phase}: {error}"),
        },
        Err(error) => warn!("profiler: cannot create '{svg_path}': {error}"),
    }
}

// No-op shims so the call sites stay identical when the feature is off.
#[cfg(not(feature = "profiling"))]
fn start_phase_profiler(_profile_dir: Option<&str>) -> Option<()> {
    None
}

#[cfg(not(feature = "profiling"))]
fn finish_phase_profiler(_guard: Option<()>, _profile_dir: Option<&str>, _phase: &str) {}

/// Arm the per-stage span-timing telemetry for one phase (see
/// `bench_telemetry`). Complements the flamegraph: attributes the off-CPU
/// await/IO time the sampling profiler cannot see. No-op unless profiling is
/// enabled and `--profile-dir` was given.
#[cfg(feature = "profiling")]
fn start_phase_telemetry(profile_dir: Option<&str>) {
    if profile_dir.is_some() {
        super::bench_telemetry::arm();
    }
}

/// Disarm the telemetry and write `<profile_dir>/<phase>.telemetry.json`.
#[cfg(feature = "profiling")]
fn finish_phase_telemetry(profile_dir: Option<&str>, phase: &str) {
    if let Some(dir) = profile_dir {
        super::bench_telemetry::finish_phase(dir, phase);
    }
}

#[cfg(not(feature = "profiling"))]
fn start_phase_telemetry(_profile_dir: Option<&str>) {}

#[cfg(not(feature = "profiling"))]
fn finish_phase_telemetry(_profile_dir: Option<&str>, _phase: &str) {}

/// Run one pipeline phase with profiling/telemetry armed, timing only the
/// workload. The timer starts *after* the profiler/telemetry are armed and
/// stops *before* the flamegraph/telemetry artifacts are written, so the
/// returned elapsed is workload-only — not inflated by profiler startup or
/// report generation. Centralizes the bracketing so the measured phases (add /
/// cognify / search / dataset delete) cannot drift out of sync. Returns
/// `(elapsed_secs, result)`.
async fn timed_phase(
    profile_dir: Option<&str>,
    phase: &str,
    work: impl std::future::Future<Output = Result<(), String>>,
) -> (f64, Result<(), String>) {
    start_phase_telemetry(profile_dir);
    let guard = start_phase_profiler(profile_dir);
    let start = Instant::now();
    let result = work.await;
    let elapsed = start.elapsed().as_secs_f64();
    finish_phase_profiler(guard, profile_dir, phase);
    finish_phase_telemetry(profile_dir, phase);
    (elapsed, result)
}

/// Round to 3 decimals to match Python's `round(x, 3)` output.
fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Read `(node_count, edge_count)` from the graph after cognify.
///
/// Returns `None` if the metrics cannot be read (backend unavailable or the
/// metrics query itself failed), which the caller distinguishes from a
/// genuinely empty graph: an empty graph trips the stale-cassette guard with a
/// "N nodes < floor" message, while unreadable metrics fail the run with a
/// distinct "metrics unreadable" message — rather than being coerced to a
/// fabricated 0-node count that a parity comparison would read as real.
async fn graph_counts(cm: &Arc<ComponentManager>) -> Option<(i64, i64)> {
    let graph_db = match cm.graph_db().await {
        Ok(db) => db,
        Err(error) => {
            warn!("graph metrics unavailable: {error}");
            return None;
        }
    };
    match graph_db.get_graph_metrics(false).await {
        Ok(metrics) => {
            let get = |k: &str| metrics.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
            Some((get("node_count"), get("edge_count")))
        }
        Err(error) => {
            warn!("graph metrics query failed: {error}");
            None
        }
    }
}

/// Shape a memory into the document text — mirrors Python `memory_to_text`:
/// `"Title: {title}\n\n{content}\n\nReferences: {refs}"`.
fn memory_to_text(mem: &Memory) -> String {
    let title = mem.title.as_deref().unwrap_or("Untitled");
    let refs = match &mem.references {
        Some(serde_json::Value::Array(items)) => {
            if items.is_empty() {
                "none".to_string()
            } else {
                items
                    .iter()
                    .map(|v| match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        }
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Null) | None => "none".to_string(),
        Some(other) => other.to_string(),
    };
    format!("Title: {title}\n\n{}\n\nReferences: {refs}", mem.content)
}

pub fn run(args: BenchArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    // ── Load corpus (catastrophic on failure — exit nonzero) ─────────────
    let corpus_raw = std::fs::read_to_string(&args.memories).map_err(|error| {
        CliError::Runtime(format!(
            "Failed to read memories file '{}': {error}",
            args.memories
        ))
    })?;
    let mut memories: Vec<Memory> = serde_json::from_str(&corpus_raw).map_err(|error| {
        CliError::Validation(format!(
            "memories file '{}' must be a JSON array of {{title, content, references}}: {error}",
            args.memories
        ))
    })?;
    if memories.is_empty() {
        return Err(CliError::Validation(format!(
            "memories file '{}' must contain a non-empty JSON array",
            args.memories
        )));
    }
    if let Some(limit) = args.num_memories {
        memories.truncate(limit);
        // Truncating to an empty corpus would skip the graph sanity guard in
        // run_phases (which is gated on a non-empty corpus) and let a 0/0 graph
        // be reported with success=true. Reject it up front.
        if memories.is_empty() {
            return Err(CliError::Validation(
                "--num-memories must be at least 1 (0 leaves an empty corpus)".to_string(),
            ));
        }
    }

    // ── Mock plumbing: configure Settings BEFORE any component init ──────
    // Setters bump the config version, so the ComponentManager's cached
    // components are (lazily) rebuilt against the new settings on first use.
    if args.mock_llm {
        let cassette = args.mock_memories.clone().ok_or_else(|| {
            CliError::Validation("--mock-llm requires --mock-memories <cassette path>".to_string())
        })?;
        cm.config().set_llm_mock(true);
        cm.config().set_llm_cassette(&cassette);
        // Deterministic mock embeddings (T5) so search is meaningful offline.
        // `init_embedding_engine` reads MOCK_EMBEDDING as well as the provider.
        // SAFETY: single-threaded set during CLI startup, before any async
        // task or component reads the environment.
        unsafe {
            std::env::set_var("MOCK_EMBEDDING", "deterministic");
        }
        cm.config().set_embedding_provider("mock");
        // Dummy key so any config validation that inspects it passes.
        cm.config().set_llm_api_key("mock-key");
        cm.config().set_embedding_api_key("mock-key");
    }

    // CLI flag overrides (apply for both real and mock modes).
    if let Some(model) = args.llm_model.as_deref() {
        cm.config().set_llm_model(model);
    }
    if let Some(provider) = args.llm_provider.as_deref() {
        cm.config().set_llm_provider(provider);
    }
    if let Some(model) = args.embedding_model.as_deref() {
        cm.config().set_embedding_model(model);
    }
    if !args.mock_llm
        && let Some(provider) = args.embedding_provider.as_deref()
    {
        // In mock mode the provider is forced to `mock` above.
        cm.config().set_embedding_provider(provider);
    }
    if let Some(dims) = args.embedding_dims {
        cm.config().set_embedding_dimensions(dims);
    }

    // ── Isolated per-run state ──────────────────────────────────────────
    // Repeated orchestrator runs must not share/clobber state. The prune
    // phase still runs and is timed (Python parity).
    let temp_dir = tempfile::tempdir().map_err(|error| {
        CliError::Runtime(format!("Failed to create temp run directory: {error}"))
    })?;
    // Persist the directory for the lifetime of the process: the embedded
    // vector DB cached inside `ComponentManager` flushes on drop, which happens
    // *after* this function returns. If the temp dir were auto-removed here, that
    // late flush would panic against missing files. Each `bench` invocation runs
    // in its own (orchestrator-spawned) process, so leaking one dir per run is
    // fine and OS /tmp cleanup reclaims it.
    let root = temp_dir.keep();
    let root_str = root.to_string_lossy();
    cm.config()
        .set_data_root_directory(&format!("{root_str}/data"));
    cm.config()
        .set_system_root_directory(&format!("{root_str}/system"));
    // `set_system_root_directory` only cascades to `graph_file_path` /
    // `vector_db_url` when they were under the *old* default root. A user with
    // a customized config (e.g. after running the demo) has those — and the
    // relational DB — pointed at fixed paths that the cascade leaves untouched,
    // so the bench would run against (and clobber) the real configured backends
    // and fail when the DB lacks `?mode=rwc`. Redirect every on-disk backend
    // explicitly so each invocation is fully self-contained.
    cm.config()
        .set_relational_db_url(&format!("sqlite://{root_str}/cognee.db?mode=rwc"));
    cm.config()
        .set_graph_file_path(&format!("{root_str}/system/graph.ladybug"));
    cm.config()
        .set_vector_db_url(&format!("{root_str}/system/vectors"));

    let owner_id = Uuid::parse_str(&cm.settings().default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            cm.settings().default_user_id
        ))
    })?;

    // Snapshot config values for the result block (after overrides applied).
    let (llm_model, embedding_model, embedding_dimensions) = {
        let s = cm.settings();
        (
            s.llm_model.clone(),
            s.embedding_model_name.clone(),
            s.embedding_dimensions,
        )
    };

    // Wrapped so the components this command opened are closed, and telemetry
    // flushed, on the runtime that dispatched them — see `crate::teardown`.
    let result = crate::teardown::run_command(Arc::clone(&cm), async {
        Ok(run_phases(
            &cm,
            owner_id,
            &args.dataset_name,
            &memories,
            args.profile_dir.as_deref(),
            args.min_graph_nodes,
            BenchConfig {
                llm_model,
                embedding_model,
                embedding_dimensions,
                dataset_name: args.dataset_name.clone(),
                mock_llm: args.mock_llm,
            },
        )
        .await)
    })?;

    // ── Serialize & write (catastrophic on failure — exit nonzero) ───────
    let json = serde_json::to_string_pretty(&result)
        .map_err(|error| CliError::Runtime(format!("Failed to serialize bench result: {error}")))?;

    if let Some(output) = args.output.as_deref() {
        std::fs::write(output, &json).map_err(|error| {
            CliError::Runtime(format!("Failed to write result file '{output}': {error}"))
        })?;
        info!("Bench results written to {output}");
    } else {
        // No --output: still emit machine result on stdout for piping.
        println!("{json}");
    }

    Ok(())
}

/// Run all phases, recording failures in `status` rather than aborting.
async fn run_phases(
    cm: &Arc<ComponentManager>,
    owner_id: Uuid,
    dataset_name: &str,
    memories: &[Memory],
    profile_dir: Option<&str>,
    min_graph_nodes: u64,
    config: BenchConfig,
) -> BenchResult {
    let n = memories.len();
    let mut status = BenchStatus {
        prune: PHASE_OK.to_string(),
        db_setup: PHASE_OK.to_string(),
        add: PHASE_OK.to_string(),
        cognify: PHASE_OK.to_string(),
        search: PHASE_OK.to_string(),
        dataset_delete: PHASE_OK.to_string(),
    };

    // ── Prune ────────────────────────────────────────────────────────────
    eprintln!("Pruning previous data...");
    let t_prune_start = Instant::now();
    if let Err(msg) = phase_prune(cm).await {
        warn!("Prune FAILED: {msg}");
        status.prune = format!("failed: {msg}");
    }
    let t_prune = t_prune_start.elapsed().as_secs_f64();

    // ── DB setup (component init) ──────────────────────────────────────────
    eprintln!("Initializing components (DB setup)...");
    let t_db_start = Instant::now();
    if let Err(msg) = phase_db_setup(cm).await {
        warn!("DB setup FAILED: {msg}");
        status.db_setup = format!("failed: {msg}");
    }
    let t_db_setup = t_db_start.elapsed().as_secs_f64();

    // ── Add ────────────────────────────────────────────────────────────────
    eprintln!("Phase 1: Adding {n} memories...");
    let (t_add, add_res) = timed_phase(
        profile_dir,
        "add",
        phase_add(cm, owner_id, dataset_name, memories),
    )
    .await;
    if let Err(msg) = add_res {
        warn!("Add FAILED: {msg}");
        status.add = format!("failed: {msg}");
    }

    // ── Cognify ──────────────────────────────────────────────────────────
    eprintln!("Phase 2: Running cognify (knowledge graph build)...");
    let (t_cognify, cognify_res) = timed_phase(
        profile_dir,
        "cognify",
        phase_cognify(cm, owner_id, dataset_name),
    )
    .await;
    if let Err(msg) = cognify_res {
        warn!("Cognify FAILED: {msg}");
        status.cognify = format!("failed: {msg}");
    }

    let t_total = t_add + t_cognify;

    // ── Graph sanity guard ─────────────────────────────────────────────────
    // Cognify over a non-empty corpus must produce a non-empty graph. An empty
    // (or below-floor) graph means the replay cassette fell through to the
    // empty-graph fallback, which happens with a stale cassette. Fail the phase
    // loudly instead of reporting a silent "success" over nothing.
    let counts = graph_counts(cm).await;
    match counts {
        Some((node_count, edge_count)) => {
            eprintln!("Graph after cognify: {node_count} nodes, {edge_count} edges");
        }
        None => eprintln!("Graph after cognify: metrics unavailable"),
    }
    if status.cognify == PHASE_OK && !memories.is_empty() {
        let floor = min_graph_nodes.max(1);
        match counts {
            // Genuinely-empty (or below-floor) graph: a stale cassette fell
            // through to the empty-graph fallback. Fail loudly.
            Some((node_count, _)) if (node_count as u64) < floor => {
                let msg = format!(
                    "graph sanity: {node_count} nodes < floor {floor} (stale cassette / empty-graph fallback?)"
                );
                warn!("{msg}");
                status.cognify = format!("failed: {msg}");
            }
            // Healthy graph — guard passes.
            Some(_) => {}
            // Metrics unreadable: we cannot confirm cognify produced a
            // non-empty graph. Fail rather than emit a fabricated 0/0 count
            // alongside success=true, which a parity comparison would read as a
            // genuine 0-node measurement of a successful run.
            None => {
                let msg = "graph sanity: graph metrics unreadable; cannot verify non-empty graph"
                    .to_string();
                warn!("{msg}");
                status.cognify = format!("failed: {msg}");
            }
        }
    }
    // `BenchResult` requires integer counts (Python parity schema). Unreadable
    // metrics are reported as 0/0, but the guard above has already flipped
    // cognify to "failed" in that case, so 0/0 never coincides with success.
    let (node_count, edge_count) = counts.unwrap_or((0, 0));

    // ── Search ───────────────────────────────────────────────────────────
    eprintln!("Phase 3: Running search query...");
    let (t_search, search_res) = timed_phase(
        profile_dir,
        "search",
        phase_search(cm, owner_id, dataset_name),
    )
    .await;
    if let Err(msg) = search_res {
        warn!("Search FAILED: {msg}");
        status.search = format!("failed: {msg}");
    }

    // ── Extra retrievers (profiling only, untimed) ───────────────────────
    // Not part of the reported contract — see `phase_search_retrievers`. A
    // failure here is warned about but deliberately does NOT touch `status`:
    // these queries do not exist in the Python bench, so letting them fail the
    // run would make `success` mean different things across the two SDKs.
    // `cfg!` rather than `#[cfg]` so both configurations stay type-checked; the
    // branch const-folds away in a non-profiling build, where `--profile-dir`
    // is documented as ignored and no flamegraph would be written anyway.
    if cfg!(feature = "profiling") && profile_dir.is_some() {
        eprintln!("Profiling: running the extra no-LLM retrievers...");
        let (t_retrievers, retriever_res) = timed_phase(
            profile_dir,
            "search_retrievers",
            phase_search_retrievers(cm, owner_id, dataset_name),
        )
        .await;
        match retriever_res {
            Ok(()) => info!("search_retrievers took {t_retrievers:.3}s (not reported)"),
            Err(msg) => warn!("Extra retrievers FAILED (not reported): {msg}"),
        }
    }

    // ── Dataset delete (populated) ───────────────────────────────────────
    // Runs last, so it measures deletion with nodes, edges and vectors all
    // present — the meaningful case, and what Python's Phase 4 measures.
    eprintln!("Phase 4: Deleting the populated dataset...");
    let (t_dataset_delete, dataset_delete_res) = timed_phase(
        profile_dir,
        "dataset_delete",
        phase_dataset_delete(cm, owner_id, dataset_name),
    )
    .await;
    if let Err(msg) = dataset_delete_res {
        warn!("Dataset delete FAILED: {msg}");
        status.dataset_delete = format!("failed: {msg}");
    }

    let success = status.prune == PHASE_OK
        && status.db_setup == PHASE_OK
        && status.add == PHASE_OK
        && status.cognify == PHASE_OK
        && status.search == PHASE_OK
        && status.dataset_delete == PHASE_OK;

    BenchResult {
        memories_count: n,
        add_time_s: round3(t_add),
        cognify_time_s: round3(t_cognify),
        total_ingest_time_s: round3(t_total),
        prune_time_s: round3(t_prune),
        db_setup_time_s: round3(t_db_setup),
        search_time: t_search,
        dataset_delete_time_s: round3(t_dataset_delete),
        status,
        success,
        config,
        node_count,
        edge_count,
    }
}

/// Wipe storage + graph + vector + session cache (clean slate).
async fn phase_prune(cm: &Arc<ComponentManager>) -> Result<(), String> {
    let storage = cm.storage().await.map_err(|e| e.to_string())?;
    prune_data(storage.as_ref())
        .await
        .map_err(|e| e.to_string())?;

    let graph_db = cm.graph_db().await.map_err(|e| e.to_string())?;
    let vector_db = cm.vector_db().await.map_err(|e| e.to_string())?;
    let database = cm.database().await.map_err(|e| e.to_string())?;
    let session_store = SeaOrmSessionStore::new(Arc::clone(&database))
        .await
        .map_err(|e| e.to_string())?;

    prune_system(
        &PruneTarget::default_system(),
        Some(graph_db.as_ref()),
        Some(vector_db.as_ref()),
        Some(&session_store),
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Force initialization of the relational DB + remaining backends.
async fn phase_db_setup(cm: &Arc<ComponentManager>) -> Result<(), String> {
    cm.database().await.map_err(|e| e.to_string())?;
    cm.graph_db().await.map_err(|e| e.to_string())?;
    cm.vector_db().await.map_err(|e| e.to_string())?;
    cm.embedding_engine().await.map_err(|e| e.to_string())?;
    cm.llm().await.map_err(|e| e.to_string())?;
    Ok(())
}

/// `add(text_list, dataset)` — ingest the corpus.
async fn phase_add(
    cm: &Arc<ComponentManager>,
    owner_id: Uuid,
    dataset_name: &str,
    memories: &[Memory],
) -> Result<(), String> {
    let storage = cm.storage().await.map_err(|e| e.to_string())?;
    let database = cm.database().await.map_err(|e| e.to_string())?;
    let graph_db = cm.graph_db().await.map_err(|e| e.to_string())?;
    let vector_db = cm.vector_db().await.map_err(|e| e.to_string())?;
    let thread_pool =
        Arc::new(RayonThreadPool::with_default_threads().map_err(|e| format!("thread pool: {e}"))?);
    let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)));

    let pipeline = AddPipeline::new(storage, Arc::clone(&database) as Arc<dyn IngestDb>)
        .with_thread_pool(thread_pool)
        .with_graph_db(graph_db)
        .with_vector_db(vector_db)
        .with_database(Arc::clone(&database))
        .with_pipeline_run_repo(pipeline_run_repo);

    let inputs: Vec<DataInput> = memories
        .iter()
        .map(|mem| DataInput::from_string(memory_to_text(mem)))
        .collect();

    pipeline
        .add(inputs, dataset_name, owner_id, None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// `cognify(dataset)` — build the knowledge graph.
async fn phase_cognify(
    cm: &Arc<ComponentManager>,
    owner_id: Uuid,
    dataset_name: &str,
) -> Result<(), String> {
    let database = cm.database().await.map_err(|e| e.to_string())?;
    let storage = cm.storage().await.map_err(|e| e.to_string())?;
    let graph_db = cm.graph_db().await.map_err(|e| e.to_string())?;
    let vector_db = cm.vector_db().await.map_err(|e| e.to_string())?;
    let embedding_engine = cm.embedding_engine().await.map_err(|e| e.to_string())?;
    let llm = cm.llm().await.map_err(|e| e.to_string())?;

    let dataset = ops::datasets::get_dataset_by_name(&database, dataset_name, owner_id, None)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("dataset '{dataset_name}' not found for owner {owner_id}"))?;

    let data_items = ops::datasets::get_dataset_data(&database, dataset.id)
        .await
        .map_err(|e| e.to_string())?;

    // OSS build has no DB-backed user lookup (the `users` table is owned by
    // the closed cloud build), so `user_email` always falls back to `None`.
    let user_email: Option<String> = None;

    let thread_pool: Arc<dyn cognee::core::CpuPool> =
        Arc::new(RayonThreadPool::with_default_threads().map_err(|e| format!("thread pool: {e}"))?);
    let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)));
    let ontology_resolver: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());

    let chunk_strategy = match cm.settings().chunk_strategy.to_uppercase().as_str() {
        "RECURSIVE" => ChunkStrategy::Recursive,
        _ => ChunkStrategy::Paragraph,
    };
    let cognify_config = {
        let s = cm.settings();
        CognifyConfig::default()
            .with_chunk_size_opt(s.chunk_size.map(|n| n as usize))
            .with_chunk_overlap(s.chunk_overlap as usize)
            .with_chunk_strategy(chunk_strategy)
            .with_max_parallel_extractions(s.llm_max_parallel_requests.max(1) as usize)
            // Pin the token counter so chunk boundaries are deterministic and
            // independent of ambient env. CognifyConfig::default() derives the
            // counter from TokenCounterKind::from_env() (reads EMBEDDING_PROVIDER
            // / COGNEE_TOKEN_COUNTER / a discovered .env, with a silent WordCounter
            // fallback). That made the record/replay cassette hash depend on the
            // host environment, not the commit — a cassette recorded in one env
            // replayed to empty-graph fallbacks in another. gpt-4o-mini uses
            // cl100k, so TikToken is the correct, portable choice for the bench.
            .with_token_counter(TokenCounterKind::TikToken)
    };

    cognify(
        data_items,
        dataset.id,
        Some(owner_id),
        user_email,
        dataset.tenant_id,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        Arc::clone(&database),
        pipeline_run_repo,
        thread_pool,
        ontology_resolver,
        &cognify_config,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Build a bench `SearchRequest` for one query type over the bench dataset.
fn bench_search_request(
    query_text: &str,
    search_type: SearchType,
    dataset_name: &str,
    owner_id: Uuid,
) -> SearchRequest {
    SearchRequest {
        query_text: query_text.to_string(),
        search_type,
        top_k: Some(10),
        datasets: Some(vec![dataset_name.to_string()]),
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: Some(true),
        use_combined_context: Some(false),
        session_id: None,
        node_type: None,
        node_name: None,
        node_name_filter_operator: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        summarize_context: None,
        save_interaction: Some(false),
        user_id: Some(owner_id),
        verbose: None,
        feedback_influence: None,
        retriever_specific_config: None,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection: None,
        neighborhood_depth: None,
        neighborhood_seed_top_k: None,
    }
}

/// The query text both SDKs benchmark against.
const BENCH_QUERY: &str = "What is in the document";

/// Build the search orchestrator used by the search phases.
async fn bench_search_orchestrator(
    cm: &Arc<ComponentManager>,
) -> Result<cognee::search::SearchOrchestrator, String> {
    let vector_db = cm.vector_db().await.map_err(|e| e.to_string())?;
    let embedding_engine = cm.embedding_engine().await.map_err(|e| e.to_string())?;
    let graph_db = cm.graph_db().await.map_err(|e| e.to_string())?;
    let llm = cm.llm().await.map_err(|e| e.to_string())?;
    let database = cm.database().await.map_err(|e| e.to_string())?;

    let session_store = SeaOrmSessionStore::new(Arc::clone(&database))
        .await
        .map_err(|e| e.to_string())?;
    let session_manager = Arc::new(SessionManager::new(Arc::new(session_store)));
    let search_history_db = Arc::clone(&database) as Arc<dyn cognee::database::SearchHistoryDb>;
    Ok(SearchBuilder::new(
        vector_db,
        embedding_engine,
        graph_db,
        llm,
        search_history_db,
    )
    .with_session_manager(session_manager)
    .with_dataset_resolver(Arc::clone(&database) as Arc<dyn IngestDb>)
    .build())
}

/// The measured search phase: the single graph-completion query Python times.
///
/// Python's `bench_cognee.py` Phase 3 is one
/// `cognee.search(query_text=..., only_context=True)` call, so this is one
/// `GraphCompletion` request with `only_context`. Keep it that way — the whole
/// point of `search_time` is that the Python and Rust nightly arms report the
/// same unit of work. Extra retrievers belong in `phase_search_retrievers`.
async fn phase_search(
    cm: &Arc<ComponentManager>,
    owner_id: Uuid,
    dataset_name: &str,
) -> Result<(), String> {
    let orchestrator = bench_search_orchestrator(cm).await?;
    let request = bench_search_request(
        BENCH_QUERY,
        SearchType::GraphCompletion,
        dataset_name,
        owner_id,
    );
    orchestrator
        .search(&request)
        .await
        .map_err(|e| format!("graph_completion: {e}"))?;
    Ok(())
}

/// Extra retrieval paths, run only under `--profile-dir` and never reported.
///
/// The no-LLM retrievers (`Chunks`, `Summaries`) surface the real retrieval
/// cost — vector KNN plus chunk/summary materialization — that the completion
/// path hides, especially under a mocked LLM where the completion call is
/// near-free. That makes them worth a flamegraph, but folding them into
/// `search_time` would make it incomparable with Python's single query, so they
/// get their own `search_retrievers.svg` and their elapsed is discarded.
async fn phase_search_retrievers(
    cm: &Arc<ComponentManager>,
    owner_id: Uuid,
    dataset_name: &str,
) -> Result<(), String> {
    let orchestrator = bench_search_orchestrator(cm).await?;
    let queries = [
        ("chunks", SearchType::Chunks),
        ("summaries", SearchType::Summaries),
    ];

    for (label, search_type) in queries {
        let request = bench_search_request(BENCH_QUERY, search_type, dataset_name, owner_id);
        let t = Instant::now();
        orchestrator
            .search(&request)
            .await
            .map_err(|e| format!("{label}: {e}"))?;
        info!("search[{label}] took {:.3}s", t.elapsed().as_secs_f64());
    }
    Ok(())
}

/// `datasets.empty_dataset(dataset)` — delete the populated dataset.
///
/// Mirrors Python's Phase 4, which resolves the dataset by name and calls
/// `datasets_api.empty_dataset(dataset.id, user)`. Both sides delete the
/// dataset's relational rows, graph nodes/edges and vectors.
///
/// They are NOT byte-for-byte the same unit of work, so do not read a small
/// `dataset_delete_time_s` gap as an SDK difference. Rust's
/// `DatasetManager::empty_dataset` hardcodes `DeleteMode::Hard`, which makes
/// `DeleteService::execute` additionally run `sweep_orphan_nodes` /
/// `sweep_orphan_edge_types`; Python's `empty_dataset` has no equivalent (its
/// degree-one sweep lives in `legacy_delete`, off the `delete_data` path). The
/// sweep runs after this dataset's own nodes are already deleted, so on a
/// single-dataset bench it scans a near-empty graph and costs a small
/// near-constant amount rather than scaling with the corpus — but it is
/// Rust-side-only overhead, so compare the two series by trend, not level.
async fn phase_dataset_delete(
    cm: &Arc<ComponentManager>,
    owner_id: Uuid,
    dataset_name: &str,
) -> Result<(), String> {
    let database = cm.database().await.map_err(|e| e.to_string())?;

    // Python guards this with `if found:` — a failed add leaves nothing to
    // delete, and the phase still reports its (near-zero) elapsed rather than
    // failing a run that has already recorded the real failure in `add`.
    let Some(dataset) = ops::datasets::get_dataset_by_name(&database, dataset_name, owner_id, None)
        .await
        .map_err(|e| e.to_string())?
    else {
        warn!("dataset '{dataset_name}' not found — nothing to delete");
        return Ok(());
    };

    // Shared with `cognee-cli delete` / `forget` so the benchmark cannot drift
    // into measuring a differently-wired deletion than the commands it mirrors.
    let delete_service = super::build_delete_service(cm)
        .await
        .map_err(|e| e.to_string())?;

    let datasets = DatasetManager::new(Arc::clone(&database) as Arc<dyn DatasetDb>);
    datasets
        .empty_dataset(dataset.id, owner_id, &delete_service)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_to_text_shapes_like_python() {
        let mem = Memory {
            title: Some("T".to_string()),
            content: "body".to_string(),
            references: Some(serde_json::json!(["a", "b"])),
        };
        assert_eq!(memory_to_text(&mem), "Title: T\n\nbody\n\nReferences: a, b");
    }

    #[test]
    fn memory_to_text_defaults() {
        let mem = Memory {
            title: None,
            content: "body".to_string(),
            references: None,
        };
        assert_eq!(
            memory_to_text(&mem),
            "Title: Untitled\n\nbody\n\nReferences: none"
        );
    }

    #[test]
    fn memory_to_text_empty_refs_array() {
        let mem = Memory {
            title: Some("X".to_string()),
            content: "c".to_string(),
            references: Some(serde_json::json!([])),
        };
        assert_eq!(memory_to_text(&mem), "Title: X\n\nc\n\nReferences: none");
    }

    #[test]
    fn round3_matches_python() {
        assert_eq!(round3(1.23456), 1.235);
        assert_eq!(round3(0.0), 0.0);
    }
}
