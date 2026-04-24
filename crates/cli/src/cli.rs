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
    Delete(DeleteArgs),
    Config(ConfigArgs),
    #[command(name = "run-sequence")]
    RunSequence(RunSequenceArgs),
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

    #[arg(long = "system-prompt")]
    pub system_prompt: Option<String>,

    #[arg(long = "session-id")]
    pub session_id: Option<String>,

    #[arg(long = "output-format", short = 'f', default_value = "pretty")]
    pub output_format: OutputFormatArg,

    #[arg(long = "llm-max-retries", value_parser = clap::value_parser!(u32).range(1..))]
    pub llm_max_retries: Option<u32>,
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
