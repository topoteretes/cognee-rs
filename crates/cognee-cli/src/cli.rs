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
    Search(SearchArgs),
    Config(ConfigArgs),
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[arg(required = true)]
    pub data: Vec<String>,

    #[arg(long = "dataset-name", short = 'd', default_value = "main_dataset")]
    pub dataset_name: String,
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

    #[arg(long = "verbose", short = 'v', default_value_t = false)]
    pub verbose: bool,
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

    #[arg(long = "output-format", short = 'f', default_value = "pretty")]
    pub output_format: OutputFormatArg,
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
