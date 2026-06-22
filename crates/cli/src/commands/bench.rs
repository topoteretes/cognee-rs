//! `cognee-cli bench` — the performance orchestrator driver.
//!
//! Ports Python's `bench_cognee.py`: runs the full
//! `prune → setup → add → cognify → search` pipeline once, times each phase,
//! and writes a result JSON with the exact Python schema so the shared
//! orchestrator/reporter can drive either SDK unchanged.
//!
//! Exit-code policy (Python parity): once the run completes and the result
//! file is written, exit `0` even if individual phases failed (failures are
//! captured in `status` / `success`). Exit nonzero only for catastrophic
//! errors: bad arguments, an unreadable corpus, or an unwritable `--output`.

use std::sync::Arc;
use std::time::Instant;

use cognee_lib::add::AddPipeline;
use cognee_lib::api::prune::{PruneTarget, prune_data, prune_system};
use cognee_lib::cognify::{ChunkStrategy, CognifyConfig, cognify};
use cognee_lib::core::RayonThreadPool;
use cognee_lib::database::{IngestDb, PipelineRunRepository, SeaOrmPipelineRunRepository, ops};
use cognee_lib::models::DataInput;
use cognee_lib::ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee_lib::search::{
    SeaOrmSessionStore, SearchBuilder, SearchRequest, SearchType, SessionManager,
};
use cognee_lib::{ComponentManager, PipelineContext};
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
    status: BenchStatus,
    success: bool,
    config: BenchConfig,
}

/// Per-phase status: `"success"` or `"failed: <msg>"` (Python parity).
#[derive(Debug, Serialize)]
struct BenchStatus {
    prune: String,
    db_setup: String,
    add: String,
    cognify: String,
    search: String,
}

const PHASE_OK: &str = "success";

/// Round to 3 decimals to match Python's `round(x, 3)` output.
fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
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

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::Runtime(format!("Failed to create async runtime: {error}")))?;

    let result = runtime.block_on(run_phases(
        &cm,
        owner_id,
        &args.dataset_name,
        &memories,
        BenchConfig {
            llm_model,
            embedding_model,
            embedding_dimensions,
            dataset_name: args.dataset_name.clone(),
            mock_llm: args.mock_llm,
        },
    ));

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
    config: BenchConfig,
) -> BenchResult {
    let n = memories.len();
    let mut status = BenchStatus {
        prune: PHASE_OK.to_string(),
        db_setup: PHASE_OK.to_string(),
        add: PHASE_OK.to_string(),
        cognify: PHASE_OK.to_string(),
        search: PHASE_OK.to_string(),
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
    let t_add_start = Instant::now();
    if let Err(msg) = phase_add(cm, owner_id, dataset_name, memories).await {
        warn!("Add FAILED: {msg}");
        status.add = format!("failed: {msg}");
    }
    let t_add = t_add_start.elapsed().as_secs_f64();

    // ── Cognify ──────────────────────────────────────────────────────────
    eprintln!("Phase 2: Running cognify (knowledge graph build)...");
    let t_cognify_start = Instant::now();
    if let Err(msg) = phase_cognify(cm, owner_id, dataset_name).await {
        warn!("Cognify FAILED: {msg}");
        status.cognify = format!("failed: {msg}");
    }
    let t_cognify = t_cognify_start.elapsed().as_secs_f64();

    let t_total = t_add + t_cognify;

    // ── Search ───────────────────────────────────────────────────────────
    eprintln!("Phase 3: Running search query...");
    let t_search_start = Instant::now();
    if let Err(msg) = phase_search(cm, owner_id, dataset_name).await {
        warn!("Search FAILED: {msg}");
        status.search = format!("failed: {msg}");
    }
    let t_search = t_search_start.elapsed().as_secs_f64();

    let success = status.prune == PHASE_OK
        && status.db_setup == PHASE_OK
        && status.add == PHASE_OK
        && status.cognify == PHASE_OK
        && status.search == PHASE_OK;

    BenchResult {
        memories_count: n,
        add_time_s: round3(t_add),
        cognify_time_s: round3(t_cognify),
        total_ingest_time_s: round3(t_total),
        prune_time_s: round3(t_prune),
        db_setup_time_s: round3(t_db_setup),
        search_time: t_search,
        status,
        success,
        config,
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

    let thread_pool: Arc<dyn cognee_lib::core::CpuPool> =
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
            .with_chunk_size(s.chunk_size as usize)
            .with_chunk_overlap(s.chunk_overlap as usize)
            .with_chunk_strategy(chunk_strategy)
            .with_max_parallel_extractions(s.llm_max_parallel_requests.max(1) as usize)
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

/// One `search("What is in the document", only_context=true)` query.
async fn phase_search(
    cm: &Arc<ComponentManager>,
    owner_id: Uuid,
    dataset_name: &str,
) -> Result<(), String> {
    let vector_db = cm.vector_db().await.map_err(|e| e.to_string())?;
    let embedding_engine = cm.embedding_engine().await.map_err(|e| e.to_string())?;
    let graph_db = cm.graph_db().await.map_err(|e| e.to_string())?;
    let llm = cm.llm().await.map_err(|e| e.to_string())?;
    let database = cm.database().await.map_err(|e| e.to_string())?;

    let session_store = SeaOrmSessionStore::new(Arc::clone(&database))
        .await
        .map_err(|e| e.to_string())?;
    let session_manager = Arc::new(SessionManager::new(Arc::new(session_store)));
    let search_history_db = Arc::clone(&database) as Arc<dyn cognee_lib::database::SearchHistoryDb>;
    let orchestrator = SearchBuilder::new(
        vector_db,
        embedding_engine,
        graph_db,
        llm,
        search_history_db,
    )
    .with_session_manager(session_manager)
    .with_dataset_resolver(Arc::clone(&database) as Arc<dyn IngestDb>)
    .build();

    let request = SearchRequest {
        query_text: "What is in the document".to_string(),
        search_type: SearchType::GraphCompletion,
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
    };

    orchestrator
        .search(&request)
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
