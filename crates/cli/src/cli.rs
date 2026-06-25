use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "cognee-cli")]
#[command(about = "Cognee CLI - Manage your knowledge graphs and cognitive processing pipelines.")]
pub struct Cli {
    #[arg(long)]
    pub debug: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Add(AddArgs),
    Cognify(CognifyArgs),
    #[command(name = "add-and-cognify")]
    AddAndCognify(AddAndCognifyArgs),
    Memify(MemifyArgs),
    Search(SearchArgs),
    Remember(RememberArgs),
    Recall(RecallArgs),
    Forget(ForgetArgs),
    Improve(ImproveArgs),
    Delete(DeleteArgs),
    Config(ConfigArgs),
    #[command(name = "run-sequence")]
    RunSequence(RunSequenceArgs),
    #[cfg(feature = "visualization")]
    Visualize(VisualizeArgs),
    #[cfg(feature = "bench")]
    Bench(BenchArgs),
}

/// Arguments for `cognee-cli bench` — the performance orchestrator driver.
///
/// Mirrors the Python `bench_cognee.py` flags so the same orchestrator
/// (`statistics_percentile_report.py`) can drive either SDK and reuse the
/// reporter unchanged. Runs the full `prune → setup → add → cognify → search`
/// pipeline once, timing each phase, and writes the result JSON to `--output`.
#[cfg(feature = "bench")]
#[derive(Debug, Args)]
pub struct BenchArgs {
    /// JSON corpus file: an array of `{title, content, references}` objects.
    #[arg(long = "memories")]
    pub memories: String,

    /// Cassette path for the replay mock LLM (used when `--mock-llm` is set).
    #[arg(long = "mock-memories")]
    pub mock_memories: Option<String>,

    /// LLM model (default: configured/env value).
    #[arg(long = "llm-model")]
    pub llm_model: Option<String>,

    /// LLM provider (default: configured/env value).
    #[arg(long = "llm-provider")]
    pub llm_provider: Option<String>,

    /// Embedding model (default: configured/env value).
    #[arg(long = "embedding-model")]
    pub embedding_model: Option<String>,

    /// Embedding provider (default: configured/env value).
    #[arg(long = "embedding-provider")]
    pub embedding_provider: Option<String>,

    /// Embedding dimensions (default: configured/env value).
    #[arg(long = "embedding-dims")]
    pub embedding_dims: Option<u32>,

    /// Limit the number of memories loaded from the corpus (default: all).
    #[arg(long = "num-memories")]
    pub num_memories: Option<usize>,

    /// Use the deterministic mock LLM + mock embeddings instead of real APIs.
    #[arg(long = "mock-llm", default_value_t = false)]
    pub mock_llm: bool,

    /// Dataset name to add/cognify/search against.
    #[arg(long = "dataset-name", default_value = "bench_memories")]
    pub dataset_name: String,

    /// Write the result JSON to this file.
    #[arg(long = "output", short = 'o')]
    pub output: Option<String>,
}

#[cfg(feature = "visualization")]
#[derive(Debug, Args)]
pub struct VisualizeArgs {
    /// Destination HTML file. If omitted, writes to `~/graph_visualization.html`.
    #[arg(long = "output", short = 'o')]
    pub output: Option<String>,
}

#[derive(Debug, Args)]
pub struct MemifyArgs {
    /// Dataset(s) to run memify on. If empty, runs on all datasets for the current owner.
    #[arg(long = "datasets", short = 'd')]
    pub datasets: Vec<String>,

    /// Filter to specific node type in the graph (e.g., "Entity").
    #[arg(long = "node-type")]
    pub node_type: Option<String>,

    /// Filter to specific node names (OR logic).
    #[arg(long = "node-name")]
    pub node_names: Vec<String>,

    /// Triplet extraction/embedding batch size.
    #[arg(long = "batch-size", default_value_t = 100)]
    pub batch_size: usize,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[arg(required = true)]
    pub data: Vec<String>,

    #[arg(long = "dataset-name", short = 'd', default_value = "main_dataset")]
    pub dataset_name: String,

    #[arg(long = "tenant-id")]
    pub tenant_id: Option<String>,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum ChunkerArg {
    #[value(name = "TextChunker")]
    Text,
    #[value(name = "LangchainChunker")]
    Langchain,
    #[value(name = "CsvChunker")]
    Csv,
}

#[derive(Debug, Args)]
pub struct CognifyArgs {
    #[arg(long = "datasets", short = 'd')]
    pub datasets: Vec<String>,

    #[arg(long = "chunk-size")]
    pub chunk_size: Option<u32>,

    #[arg(long = "ontology-file")]
    pub ontology_file: Option<String>,

    #[arg(long = "chunker", default_value = "TextChunker")]
    pub chunker: ChunkerArg,

    #[arg(long = "background", short = 'b', default_value_t = false)]
    pub background: bool,

    #[arg(long = "llm-max-retries", value_parser = clap::value_parser!(u32).range(1..))]
    pub llm_max_retries: Option<u32>,

    #[arg(long = "llm-max-parallel-requests", value_parser = clap::value_parser!(u32).range(1..))]
    pub llm_max_parallel_requests: Option<u32>,

    /// Use temporal cognify pipeline (event/timestamp extraction instead of standard KG extraction).
    /// Mirrors Python's `temporal_cognify=True` parameter.
    #[arg(long = "temporal-cognify", default_value_t = false)]
    pub temporal_cognify: bool,
}

#[derive(Debug, Args)]
pub struct AddAndCognifyArgs {
    #[arg(required = true)]
    pub data: Vec<String>,

    #[arg(long = "dataset-name", short = 'd', default_value = "main_dataset")]
    pub dataset_name: String,

    #[arg(long = "chunk-size")]
    pub chunk_size: Option<u32>,

    #[arg(long = "ontology-file")]
    pub ontology_file: Option<String>,

    #[arg(long = "chunker", default_value = "TextChunker")]
    pub chunker: ChunkerArg,

    #[arg(long = "llm-max-retries", value_parser = clap::value_parser!(u32).range(1..))]
    pub llm_max_retries: Option<u32>,

    #[arg(long = "llm-max-parallel-requests", value_parser = clap::value_parser!(u32).range(1..))]
    pub llm_max_parallel_requests: Option<u32>,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum QueryTypeArg {
    #[value(name = "GRAPH_COMPLETION")]
    GraphCompletion,
    #[value(name = "RAG_COMPLETION")]
    RagCompletion,
    #[value(name = "CHUNKS")]
    Chunks,
    #[value(name = "SUMMARIES")]
    Summaries,
    #[value(name = "CODE")]
    Code,
    #[value(name = "CYPHER")]
    Cypher,
    #[value(name = "TEMPORAL")]
    Temporal,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum OutputFormatArg {
    #[value(name = "json")]
    Json,
    #[value(name = "pretty")]
    Pretty,
    #[value(name = "simple")]
    Simple,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    pub query_text: String,

    #[arg(long = "query-type", short = 't', default_value = "GRAPH_COMPLETION")]
    pub query_type: QueryTypeArg,

    #[arg(long = "datasets", short = 'd')]
    pub datasets: Vec<String>,

    #[arg(long = "top-k", short = 'k', default_value_t = 10)]
    pub top_k: usize,

    /// Inline system prompt text. Takes precedence over --system-prompt-path.
    #[arg(long = "system-prompt")]
    pub system_prompt: Option<String>,

    /// Path to a file containing the system prompt. Defaults to the configured
    /// `default_system_prompt_path` when neither this nor --system-prompt is set.
    #[arg(long = "system-prompt-path")]
    pub system_prompt_path: Option<String>,

    #[arg(long = "session-id")]
    pub session_id: Option<String>,

    #[arg(long = "output-format", short = 'f', default_value = "pretty")]
    pub output_format: OutputFormatArg,

    #[arg(long = "llm-max-retries", value_parser = clap::value_parser!(u32).range(1..))]
    pub llm_max_retries: Option<u32>,
}

/// Arguments for `cognee-cli remember` — one-call store (add + cognify + improve).
///
/// Mirrors the Python `cognee.remember()` SDK function. Accepts inline text
/// and/or file paths (same input handling as `add`). When `--session-id` is
/// supplied the data is stored in the session cache (session memory mode)
/// instead of the permanent knowledge graph.
#[derive(Debug, Args)]
pub struct RememberArgs {
    /// Inline text and/or file paths to remember.
    #[arg(required = true)]
    pub data: Vec<String>,

    #[arg(long = "dataset-name", short = 'd', default_value = "main_dataset")]
    pub dataset_name: String,

    /// Store in the given session cache instead of the permanent graph.
    #[arg(long = "session-id")]
    pub session_id: Option<String>,

    /// Skip the self-improvement (memify) pass that normally runs after
    /// cognify. By default self-improvement is ON (Python parity:
    /// `self_improvement=True`).
    #[arg(long = "no-improve", default_value_t = false)]
    pub no_improve: bool,

    #[arg(long = "tenant-id")]
    pub tenant_id: Option<String>,
}

/// Arguments for `cognee-cli recall` — smart memory query with auto-routing.
///
/// Mirrors the Python `cognee.recall()` SDK function. When `--query-type` is
/// omitted the search type is auto-routed based on the query text.
#[derive(Debug, Args)]
pub struct RecallArgs {
    pub query: String,

    /// Search type to use. When omitted, recall auto-routes the search type.
    #[arg(long = "query-type", short = 't')]
    pub query_type: Option<QueryTypeArg>,

    #[arg(long = "datasets", short = 'd')]
    pub datasets: Vec<String>,

    #[arg(long = "top-k", short = 'k', default_value_t = 10)]
    pub top_k: usize,

    #[arg(long = "session-id")]
    pub session_id: Option<String>,

    #[arg(long = "output-format", short = 'f', default_value = "pretty")]
    pub output_format: OutputFormatArg,
}

/// Arguments for `cognee-cli forget` — remove memory.
///
/// Mirrors the Python `cognee.forget()` SDK function. Exactly one target must
/// be selected:
///   * `--all` — delete everything the user owns.
///   * `--data-id` (+ `--dataset-name`) — delete one data item from a dataset.
///   * `--dataset-name` — delete an entire dataset.
#[derive(Debug, Args)]
pub struct ForgetArgs {
    /// Dataset to forget (or to scope a `--data-id` deletion).
    #[arg(long = "dataset-name", short = 'd')]
    pub dataset_name: Option<String>,

    /// Forget a single data item (UUID). Requires `--dataset-name`.
    #[arg(long = "data-id", conflicts_with = "all")]
    pub data_id: Option<String>,

    /// Forget everything the current owner owns.
    #[arg(long = "all", default_value_t = false)]
    pub all: bool,

    #[arg(long = "tenant-id")]
    pub tenant_id: Option<String>,
}

/// Arguments for `cognee-cli improve` — enrich existing memory / bridge sessions.
///
/// Mirrors the Python `cognee.improve()` SDK function. Runs the four-stage
/// session-graph bridge: apply feedback weights, persist session Q&A to the
/// graph, default enrichment (memify), and sync graph edges back into sessions.
#[derive(Debug, Args)]
pub struct ImproveArgs {
    #[arg(long = "dataset-name", short = 'd', default_value = "main_dataset")]
    pub dataset_name: String,

    /// Session id(s) to bridge into the permanent graph. Repeatable.
    #[arg(long = "session-id")]
    pub session_id: Vec<String>,

    /// Restrict the enrichment (memify) pass to these graph node names.
    /// Repeatable.
    #[arg(long = "node-name")]
    pub node_name: Vec<String>,

    /// Mixing factor for feedback weight propagation (Stage 1).
    #[arg(long = "feedback-alpha", default_value_t = 0.1)]
    pub feedback_alpha: f64,

    #[arg(long = "tenant-id")]
    pub tenant_id: Option<String>,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum DeleteModeArg {
    #[value(name = "soft")]
    Soft,
    #[value(name = "hard")]
    Hard,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    #[arg(long = "dataset-name", short = 'd', conflicts_with = "dataset_id")]
    pub dataset_name: Option<String>,

    /// Target a dataset by UUID instead of by name. Mutually exclusive with
    /// `--dataset-name`.
    #[arg(long = "dataset-id", conflicts_with = "dataset_name")]
    pub dataset_id: Option<String>,

    #[arg(long = "user-id", short = 'u')]
    pub user_id: Option<String>,

    #[arg(long = "data-id")]
    pub data_id: Option<String>,

    #[arg(long = "all", default_value_t = false)]
    pub all: bool,

    #[arg(long = "mode", default_value = "soft")]
    pub mode: DeleteModeArg,

    #[arg(long = "dry-run", default_value_t = false)]
    pub dry_run: bool,

    #[arg(long = "force", short = 'f', default_value_t = false)]
    pub force: bool,

    /// Auto-delete the owning dataset if it becomes empty after data removal.
    /// Only applies with --data-id.
    #[arg(long = "delete-dataset-if-empty", default_value_t = false)]
    pub delete_dataset_if_empty: bool,

    /// Enforce ACL permission checks before deletion.
    ///
    /// When enabled, the delete operation verifies that the requesting
    /// principal (--user-id) holds "delete" permission on each target
    /// dataset via the ACL table.
    #[arg(long = "enforce-acl", default_value_t = false)]
    pub enforce_acl: bool,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    Get(ConfigGetArgs),
    Set(ConfigSetArgs),
    List,
    Unset(ConfigUnsetArgs),
    Reset(ConfigResetArgs),
}

#[derive(Debug, Args)]
pub struct ConfigGetArgs {
    pub key: Option<String>,
}

#[derive(Debug, Args)]
pub struct ConfigSetArgs {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Args)]
pub struct ConfigUnsetArgs {
    pub key: String,

    #[arg(long = "force", short = 'f', default_value_t = false)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct ConfigResetArgs {
    #[arg(long = "force", short = 'f', default_value_t = false)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct RunSequenceArgs {
    /// Path(s) to JSON file(s) containing the command sequence
    #[arg(required = true)]
    pub sequence_files: Vec<String>,
}
