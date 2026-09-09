use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::config_store::Settings;

#[derive(Debug, Parser)]
#[command(name = "cognee-cli", version)]
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
    Export(ExportArgs),
    #[command(name = "pipeline-unblock")]
    PipelineUnblock(PipelineUnblockArgs),
    Config(ConfigArgs),
    #[command(name = "run-sequence")]
    RunSequence(RunSequenceArgs),
    #[cfg(feature = "visualization")]
    Visualize(VisualizeArgs),
    #[cfg(feature = "bench")]
    Bench(BenchArgs),
}

impl Commands {
    /// The `--llm-max-retries` value carried by whichever subcommand was parsed.
    ///
    /// The flag exists on the three subcommands that drive LLM work. It was
    /// declared on all three from the start but read by nothing, so passing it
    /// silently did nothing while `LLM_MAX_RETRIES` worked — the sibling
    /// `--llm-max-parallel-requests` was wired, this one was missed (SDK-511).
    ///
    /// Returned rather than applied, so each entry point can fold it in at the
    /// point that suits it: `main::run` writes it into `Settings` before
    /// `ConfigManager` exists, while `run_sequence` — whose steps never reach
    /// `main::run` — goes through `ConfigManager::set_llm_max_retries` per step.
    ///
    /// Folding it in up front is preferred where possible. Not because a later
    /// change could not take effect (it can: the setter bumps the config version
    /// and `ComponentManager` rebuilds affected components on next access), but
    /// because that rebuild discards every warm component, which is wasteful
    /// when the value was known before anything was built.
    pub fn llm_max_retries_override(&self) -> Option<u32> {
        match self {
            Commands::Cognify(args) => args.llm_max_retries,
            Commands::AddAndCognify(args) => args.llm_max_retries,
            Commands::Search(args) => args.llm_max_retries,
            // `memify` / `remember` / `recall` / `improve` also drive LLM work
            // but do not declare the flag. Adding it to them is a separate
            // change — this one wires the flag that already exists. `RunSequence`
            // is `None` by design: its steps carry their own flags and are
            // folded in per step by `run_sequence::run`.
            _ => None,
        }
    }

    /// Fold this invocation's settings-valued flags over `settings`, so a flag
    /// outranks config and env for the run it was passed on.
    ///
    /// Currently one field. `--llm-max-parallel-requests` is deliberately *not*
    /// here: it is resolved per command into `CognifyConfig` rather than into
    /// `Settings`, so it never needed to reach the shared config at all. Flags
    /// that do belong in `Settings` should be added here rather than growing a
    /// second mechanism.
    ///
    /// Lives here rather than in `main` so it is reachable from tests: the
    /// binary's own `run()` cannot be called from the test harness, and the bug
    /// this fixes was precisely a flag that parsed correctly and then reached
    /// nothing. A test that only checks parsing would have passed against it.
    ///
    /// Call before building `ConfigManager` — see `llm_max_retries_override`.
    pub fn apply_overrides(&self, settings: &mut Settings) {
        if let Some(retries) = self.llm_max_retries_override() {
            settings.llm_max_retries = retries;
        }
    }
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

    /// Directory to write per-phase profiles into (a flamegraph SVG plus a
    /// span-timing JSON per phase). Only honoured when built with
    /// `--features profiling`. Ignored otherwise.
    #[arg(long = "profile-dir")]
    pub profile_dir: Option<String>,

    /// Minimum node count cognify must produce (stale-cassette guard). A run
    /// over a non-empty corpus always requires a non-empty graph. Pass a higher
    /// value to assert the recorded baseline. The default of 0 only checks that
    /// the graph is non-empty.
    #[arg(long = "min-graph-nodes", default_value_t = 0)]
    pub min_graph_nodes: u64,
}

/// Arguments for `cognee-cli export` — write the graph as a COGX archive.
///
/// The archive is what Python cognee re-imports, either locally via
/// `cognee.remember(COGXArchiveSource(path))` or over HTTP by POSTing the
/// packed tarball to `/api/v1/remember` with `content_type=cogx-archive`.
#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Destination directory for the archive. Defaults to
    /// `<dataset>_cogx` in the working directory.
    #[arg(long = "output", short = 'o')]
    pub output: Option<String>,

    /// Dataset name recorded in the manifest note and used for the default
    /// output path. Does not filter the graph: cognee-rs has no per-dataset
    /// graph partition, so the whole store is exported either way.
    #[arg(long = "dataset", short = 'd')]
    pub dataset: Option<String>,

    /// Also write `<output>.cogx.tar.gz`, the form Python's remember
    /// endpoint accepts as an upload.
    #[arg(long = "pack")]
    pub pack: bool,

    /// Export format. Only `cogx` round-trips into Python.
    #[arg(long = "format", default_value = "cogx")]
    pub format: String,
}

#[cfg(feature = "visualization")]
#[derive(Debug, Args)]
pub struct VisualizeArgs {
    /// Destination HTML file. If omitted, writes to `~/graph_visualization.html`.
    #[arg(long = "output", short = 'o')]
    pub output: Option<String>,
}

/// Report, and optionally clear, what a killed run left behind on a dataset.
///
/// A run is refused as "already running" by two independent gates, and a killed
/// process leaves both set: the latest `pipeline_runs` row stuck at `Started`,
/// which is checked first and **never expires** outside an HTTP-server restart;
/// and the exclusive-run claim, which expires after a day. Clearing only the
/// claim leaves the run still refused, so this handles both.
///
/// Reporting is the default. Clearing is opt-in because nothing here can tell a
/// dead holder from a live run, and clearing a live one re-admits the
/// concurrent run the claim exists to prevent.
#[derive(Debug, Args)]
pub struct PipelineUnblockArgs {
    /// Dataset to inspect, by name.
    #[arg(long = "dataset", short = 'd')]
    pub dataset: String,

    /// Pipeline to inspect: `cognify_pipeline`, `temporal-cognify` or
    /// `memify_pipeline`. Each claims a dataset under its own name, so they
    /// never exclude each other.
    #[arg(long = "pipeline", default_value = "cognify_pipeline")]
    pub pipeline: String,

    /// Actually clear what is reported.
    ///
    /// Only do this when the process that started the run is known to be gone.
    #[arg(long = "clear", default_value_t = false)]
    pub clear: bool,
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

    /// Overrides `LLM_MAX_RETRIES` for this invocation.
    ///
    /// `0` is accepted so the flag and the env var agree, but what it means is
    /// provider-dependent: OpenAI-compatible, Azure and Anthropic floor it to 1,
    /// while Bedrock takes it literally as a single attempt with no retry. On
    /// the providers that floor it, `LLM_MIN_RETRY_SECONDS=0` is what shortens a
    /// retry that would otherwise keep waiting.
    #[arg(long = "llm-max-retries", value_parser = clap::value_parser!(u32).range(0..))]
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

    /// Overrides `LLM_MAX_RETRIES` for this invocation.
    ///
    /// `0` is accepted so the flag and the env var agree, but what it means is
    /// provider-dependent: OpenAI-compatible, Azure and Anthropic floor it to 1,
    /// while Bedrock takes it literally as a single attempt with no retry. On
    /// the providers that floor it, `LLM_MIN_RETRY_SECONDS=0` is what shortens a
    /// retry that would otherwise keep waiting.
    #[arg(long = "llm-max-retries", value_parser = clap::value_parser!(u32).range(0..))]
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

/// Which stream the console log layer should use for this invocation.
///
/// Any invocation whose stdout is a **contract with a program** gets stderr,
/// because the console layer writing to stdout corrupts that contract: the
/// payload ends up preceded by timestamped log lines, so `serde_json::from_str`
/// reads the `2026` of the first timestamp as a number and fails with "trailing
/// characters at line 1 column 5".
///
/// That covers more than `--output-format json`:
///
/// - `json` — the obvious one, a single JSON document.
/// - `simple` — also machine output, not prose. `search`'s `Simple` arm prints
///   `item.payload`, a `serde_json::Value`, one per line (NDJSON). This repo's
///   own cross-SDK harness consumes it programmatically and had to work around
///   the pollution with `RUST_LOG=error` plus a log-prefix-stripping regex
///   (`e2e-cross-sdk/harness/helpers.py`).
/// - `export` / `visualize` — `println!` a bare path, for `$(cognee-cli …)`.
/// - `bench` — `println!` a JSON result, with every progress line deliberately
///   on `eprintln!` already; `docs/performance/mock-benchmark.md` documents the
///   stdout contract.
///
/// `pretty` and every other command keep stdout, deliberately. This CLI routes
/// its whole human-facing surface through `tracing` — not only diagnostics but
/// progress lines like "Cognify completed." — so switching those too would
/// relocate all of it to stderr. That is a UX decision well beyond fixing a
/// broken machine contract, and ten `cli_e2e`/`cli_memify` assertions pin the
/// current placement as intended behaviour. The faithful end state is Python's
/// (`cognee/cli/logging_utils.py` puts its handler on stderr in every mode,
/// with data via `echo`), and getting there means routing CLI messages through
/// something other than `tracing` — a separate change.
///
/// Known gap: `run-sequence` parses its steps internally
/// (`commands/run_sequence.rs`), so a step's `-f json` is invisible here and
/// that invocation keeps stdout.
pub fn console_stream_for(command: &Commands) -> cognee_logging::ConsoleStream {
    use cognee_logging::ConsoleStream;
    let format = match command {
        Commands::Search(args) => &args.output_format,
        Commands::Recall(args) => &args.output_format,
        // stdout is a path or a JSON document for these, regardless of flags.
        // `Visualize` and `Bench` are feature-gated on the enum, so their arms
        // must be too — a slim CLI build has no such variants.
        Commands::Export(_) => return ConsoleStream::Stderr,
        #[cfg(feature = "visualization")]
        Commands::Visualize(_) => return ConsoleStream::Stderr,
        #[cfg(feature = "bench")]
        Commands::Bench(_) => return ConsoleStream::Stderr,
        _ => return ConsoleStream::Stdout,
    };
    match format {
        OutputFormatArg::Json | OutputFormatArg::Simple => ConsoleStream::Stderr,
        OutputFormatArg::Pretty => ConsoleStream::Stdout,
    }
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

    /// Overrides `LLM_MAX_RETRIES` for this invocation.
    ///
    /// `0` is accepted so the flag and the env var agree, but what it means is
    /// provider-dependent: OpenAI-compatible, Azure and Anthropic floor it to 1,
    /// while Bedrock takes it literally as a single attempt with no retry. On
    /// the providers that floor it, `LLM_MIN_RETRY_SECONDS=0` is what shortens a
    /// retry that would otherwise keep waiting.
    #[arg(long = "llm-max-retries", value_parser = clap::value_parser!(u32).range(0..))]
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Cli {
        Cli::try_parse_from(argv).expect("argv should parse")
    }

    /// The regression this guards: the flag parsed fine before SDK-511 and was
    /// then dropped on the floor, so a parse-only assertion would have passed
    /// against the bug. These assert the value is *reachable* from `Commands`,
    /// which is what `run()` folds into `Settings`.
    #[test]
    fn llm_max_retries_is_reachable_from_every_subcommand_that_declares_it() {
        for argv in [
            vec!["cognee-cli", "cognify", "--llm-max-retries", "10"],
            vec![
                "cognee-cli",
                "add-and-cognify",
                "--llm-max-retries",
                "10",
                "data.txt",
            ],
            vec!["cognee-cli", "search", "--llm-max-retries", "10", "q"],
        ] {
            let cli = parse(&argv);
            assert_eq!(
                cli.command.llm_max_retries_override(),
                Some(10),
                "flag was not reachable from {:?}",
                argv[1],
            );
        }
    }

    #[test]
    fn absent_flag_yields_no_override_so_config_and_env_still_win() {
        let cli = parse(&["cognee-cli", "cognify"]);
        assert_eq!(cli.command.llm_max_retries_override(), None);
    }

    #[test]
    fn subcommand_without_the_flag_yields_no_override() {
        let cli = parse(&["cognee-cli", "add", "data.txt"]);
        assert_eq!(cli.command.llm_max_retries_override(), None);
    }

    /// `LLM_MAX_RETRIES=0` parses and is stored, so the flag accepting it keeps
    /// the two surfaces consistent. What `0` then *means* is provider-dependent
    /// — see the flag's doc comment — so this asserts only that it survives
    /// parsing rather than asserting an equivalence that does not hold.
    #[test]
    fn zero_is_accepted_to_match_the_env_var() {
        let cli = parse(&["cognee-cli", "cognify", "--llm-max-retries", "0"]);
        assert_eq!(cli.command.llm_max_retries_override(), Some(0));
    }

    /// A `run-sequence` step is parsed by `run_sequence::run`, not `main::run`,
    /// so it needs its own fold. This pins the half of the contract that lives
    /// here: a step command parsed out of a sequence file still exposes its
    /// override. `demo/sequences/demo_pipeline.json` passes this flag.
    #[test]
    fn a_sequence_step_command_still_exposes_its_override() {
        // Copied from `demo/sequences/demo_pipeline.json`, shaped as
        // `run_sequence::run` builds it: argv0 + the step's own command array.
        let cli = parse(&[
            "cognee-cli",
            "cognify",
            "--datasets",
            "demo",
            "--chunk-size",
            "700",
            "--llm-max-retries",
            "3",
            "--llm-max-parallel-requests",
            "4",
        ]);
        assert_eq!(cli.command.llm_max_retries_override(), Some(3));
    }

    #[test]
    fn apply_overrides_writes_the_flag_into_settings() {
        let cli = parse(&["cognee-cli", "cognify", "--llm-max-retries", "10"]);
        let mut settings = Settings::default();
        assert_ne!(
            settings.llm_max_retries, 10,
            "default must differ from the test value"
        );

        cli.command.apply_overrides(&mut settings);

        assert_eq!(settings.llm_max_retries, 10);
    }

    #[test]
    fn apply_overrides_leaves_settings_alone_when_the_flag_is_absent() {
        let cli = parse(&["cognee-cli", "cognify"]);
        // 7 stands in for a value that came from config.json or the env.
        let mut settings = Settings {
            llm_max_retries: 7,
            ..Default::default()
        };

        cli.command.apply_overrides(&mut settings);

        assert_eq!(
            settings.llm_max_retries, 7,
            "an absent flag must not clobber config or env",
        );
    }
}

#[cfg(test)]
mod console_stream_tests {
    use super::*;
    use clap::Parser;
    use cognee_logging::ConsoleStream;

    fn stream_for(args: &[&str]) -> ConsoleStream {
        match Cli::try_parse_from(args) {
            Ok(cli) => console_stream_for(&cli.command),
            Err(e) => panic!("fixture args {args:?} must parse: {e}"),
        }
    }

    #[test]
    fn machine_readable_output_moves_console_logs_off_stdout() {
        for args in [
            // json: a single JSON document.
            vec!["cognee-cli", "search", "q", "--output-format", "json"],
            // Short flag and `=` form are clap's problem, not ours — assert
            // they reach the same decision so the spelling cannot drift.
            vec!["cognee-cli", "search", "q", "-f", "json"],
            vec!["cognee-cli", "recall", "q", "--output-format=json"],
            // simple: NDJSON, not prose — `Simple` prints `item.payload`,
            // which is a `serde_json::Value`. The cross-SDK harness parses it.
            vec!["cognee-cli", "search", "q", "--output-format", "simple"],
            vec!["cognee-cli", "recall", "q", "-f", "simple"],
            // These `println!` a bare path or a JSON document regardless of
            // flags, so their stdout is a contract too. `visualize`/`bench` are
            // feature-gated subcommands; only assert what this build has.
            vec!["cognee-cli", "export", "-o", "out"],
            #[cfg(feature = "visualization")]
            vec!["cognee-cli", "visualize"],
        ] {
            assert_eq!(
                stream_for(&args),
                ConsoleStream::Stderr,
                "expected stderr for {args:?}"
            );
        }
    }

    #[test]
    fn human_output_and_other_commands_keep_stdout() {
        // Not a fallback. The CLI routes progress lines like "Cognify
        // completed." through tracing too, and ten cli_e2e/cli_memify
        // assertions pin those on stdout. Moving them is a UX change, not part
        // of fixing a broken machine contract.
        for args in [
            vec!["cognee-cli", "search", "q"],
            vec!["cognee-cli", "search", "q", "--output-format", "pretty"],
            vec!["cognee-cli", "recall", "q", "-f", "pretty"],
            vec!["cognee-cli", "cognify", "--datasets", "d"],
            vec!["cognee-cli", "config", "get", "default_user_id"],
        ] {
            assert_eq!(
                stream_for(&args),
                ConsoleStream::Stdout,
                "expected stdout for {args:?}"
            );
        }
    }
}
