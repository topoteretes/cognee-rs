//! Criterion benchmark: full pipeline (add → cognify → search) against the live Rust HTTP server.
//!
//! Ports `cognee/tests/performance/batch_add_cognify_test.py` and extends it with search.
//!
//! # Criterion parameters
//!
//! `sample_size(10)` with `measurement_time(30s)` forces exactly **one pipeline execution per
//! sample**: because a single add+cognify+search run takes far longer than 30s, Criterion
//! schedules the minimum of 1 iteration per sample.  Total measurements = 10.
//!
//! # Environment variables
//!
//! | Variable                 | Default                     | Purpose                           |
//! |--------------------------|-----------------------------|-----------------------------------|
//! | `COGNEE_HTTP_SERVER_BIN` | `cargo run` fallback        | Path to pre-built server binary   |
//! | `COGNEE_BENCH_NUM_FILES` | `10`                        | Documents per pipeline run        |
//! | `LLM_API_KEY`            | (required for cognify)      | Forwarded to server process       |
//! | `OPENAI_URL`             | (required for cognify)      | Forwarded to server process       |
//!
//! # Running
//!
//! ```sh
//! LLM_API_KEY=sk-... OPENAI_URL=https://api.openai.com/v1 \
//!     cargo bench -p cognee-bench --bench batch_add_cognify
//!
//! # Python-equivalent run (200 files)
//! COGNEE_BENCH_NUM_FILES=200 LLM_API_KEY=sk-... \
//!     cargo bench -p cognee-bench --bench batch_add_cognify
//! ```

use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, SamplingMode, criterion_group, criterion_main};
use rand::Rng;
use rand::seq::SliceRandom;
use reqwest::blocking::{Client, multipart};
use tempfile::TempDir;
use uuid::Uuid;

// ── Document-generation constants (verbatim from batch_add_cognify_test.py) ──

const TOPICS: &[&str] = &[
    "quantum computing",
    "machine learning",
    "climate change",
    "renewable energy",
    "space exploration",
    "genetic engineering",
    "blockchain technology",
    "artificial intelligence",
    "ocean conservation",
    "urban planning",
    "medieval history",
    "philosophy of mind",
    "distributed systems",
    "neuroscience",
    "economic theory",
];

const SUBTOPICS: &[&str] = &[
    "data analysis",
    "pattern recognition",
    "resource allocation",
    "risk assessment",
    "optimization algorithms",
    "predictive modeling",
    "system integration",
    "scalability",
    "error correction",
    "signal processing",
    "network topology",
    "feedback loops",
    "energy efficiency",
    "material science",
    "behavioral adaptation",
    "information theory",
];

const SENTENCE_TEMPLATES: &[&str] = &[
    "The field of {topic} has seen remarkable advances in recent years, particularly in the area of {subtopic}.",
    "Researchers studying {topic} have discovered that {subtopic} plays a crucial role in understanding the broader implications.",
    "A comprehensive review of {topic} literature reveals that {subtopic} remains one of the most debated aspects.",
    "Recent experiments in {topic} demonstrate a strong correlation between {subtopic} and observed outcomes.",
    "The intersection of {topic} and {subtopic} opens new possibilities for practical applications.",
    "Experts in {topic} argue that {subtopic} will be the defining challenge of the next decade.",
    "Historical analysis shows that {topic} has always been influenced by developments in {subtopic}.",
    "New computational models for {topic} suggest that {subtopic} can be optimized through iterative approaches.",
    "The economic impact of {topic} is closely tied to advancements in {subtopic}, according to recent studies.",
    "Collaborative efforts in {topic} have led to breakthroughs in {subtopic} that were previously thought impossible.",
    "Understanding {topic} requires a deep appreciation of how {subtopic} interacts with existing frameworks.",
    "Policy makers are increasingly turning to {topic} research to inform decisions about {subtopic}.",
];

// ── Document generation ───────────────────────────────────────────────────────

fn generate_paragraph(topic: &str, num_sentences: usize, rng: &mut impl Rng) -> String {
    (0..num_sentences)
        .map(|_| {
            let tmpl = SENTENCE_TEMPLATES.choose(rng).expect("non-empty templates");
            let subtopic = SUBTOPICS.choose(rng).expect("non-empty subtopics");
            tmpl.replace("{topic}", topic)
                .replace("{subtopic}", subtopic)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn generate_document(rng: &mut impl Rng) -> String {
    let topic = TOPICS.choose(rng).expect("non-empty topics");
    let num_paragraphs = rng.gen_range(2..=5);
    let mut paragraphs: Vec<String> = (0..num_paragraphs)
        .map(|_| generate_paragraph(topic, rng.gen_range(50..=100), rng))
        .collect();
    // Trailing UUID paragraph ensures content uniqueness across iterations (matches Python).
    paragraphs.push(Uuid::new_v4().to_string());
    paragraphs.join("\n\n")
}

// ── ServerGuard — subprocess lifecycle ───────────────────────────────────────

/// Holds a running `cognee-http-server` subprocess.
/// The server is killed and state cleaned up when this guard drops.
struct ServerGuard {
    child: Child,
    pub base_url: String,
    _state_dir: TempDir,
}

/// LLM-related env vars forwarded from the caller to the server subprocess.
const LLM_ENV_VARS: &[&str] = &[
    "LLM_API_KEY",
    "OPENAI_API_KEY",
    "OPENAI_TOKEN",
    "OPENAI_URL",
    "OPENAI_MODEL",
    "LLM_ENDPOINT",
    "LLM_MODEL",
    "ANTHROPIC_API_KEY",
    "EMBEDDING_PROVIDER",
    "EMBEDDING_MODEL",
    "EMBEDDING_ENDPOINT",
    "EMBEDDING_API_KEY",
    "MOCK_EMBEDDING",
];

impl ServerGuard {
    /// Start `cognee-http-server` on an ephemeral port with isolated state.
    /// Blocks until `/health` returns 200 or panics after 240 s.
    fn start() -> Self {
        // Grab an OS-assigned port then release it so the server can bind it.
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
            l.local_addr().expect("local_addr").port()
        };
        let host = "127.0.0.1";
        let base_url = format!("http://{host}:{port}");

        // Isolated state directory — wiped on drop.
        let state_dir = tempfile::tempdir().expect("create temp state dir");
        let data_dir = state_dir.path().join("data");
        let system_dir = state_dir.path().join("system");
        let session_dir = state_dir.path().join("sessions");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        std::fs::create_dir_all(&system_dir).expect("create system dir");
        std::fs::create_dir_all(&session_dir).expect("create sessions dir");

        // Resolve binary: COGNEE_HTTP_SERVER_BIN env var or fall back to cargo run.
        let bin_override = std::env::var("COGNEE_HTTP_SERVER_BIN").ok();
        let use_binary = bin_override
            .as_deref()
            .map(|p| Path::new(p).exists())
            .unwrap_or(false);

        let mut cmd = if use_binary {
            let mut c = Command::new(bin_override.as_deref().expect("checked above"));
            c.args(["--host", host, "--port", &port.to_string()]);
            c
        } else {
            let mut c = Command::new("cargo");
            c.args([
                "run",
                "-p",
                "cognee-http-server",
                "--features",
                "bin",
                "--bin",
                "cognee-http-server",
                "--",
                "--host",
                host,
                "--port",
                &port.to_string(),
            ]);
            c
        };

        cmd.env("REQUIRE_AUTHENTICATION", "false")
            .env("DATA_ROOT_DIRECTORY", &data_dir)
            .env("SYSTEM_ROOT_DIRECTORY", &system_dir)
            .env("COGNEE_SESSION_DIR", &session_dir)
            .env("HTTP_API_HOST", host)
            .env("HTTP_API_PORT", port.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        for var in LLM_ENV_VARS {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }

        let child = cmd.spawn().expect("spawn cognee-http-server");

        // Wait for /health to become ready.
        let health_url = format!("{base_url}/health");
        let probe = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("health probe client");
        let deadline = Instant::now() + Duration::from_secs(240);
        loop {
            if Instant::now() > deadline {
                panic!("cognee-http-server at {health_url} did not become ready in 240s");
            }
            if probe
                .get(&health_url)
                .send()
                .map(|r| r.status().is_success())
                .unwrap_or(false)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }

        Self {
            child,
            base_url,
            _state_dir: state_dir,
        }
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // _state_dir drops here, removing the temp directory
    }
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────

/// POST all documents in one multipart request to `/api/v1/add`.
/// Returns the wall-clock duration of the HTTP call only (not document generation).
fn add_documents(client: &Client, base_url: &str, dataset_name: &str, docs: &[String]) -> Duration {
    let mut form = multipart::Form::new().text("datasetName", dataset_name.to_string());
    for (i, doc) in docs.iter().enumerate() {
        let part = multipart::Part::bytes(doc.as_bytes().to_vec())
            .file_name(format!("document_{}.txt", i + 1))
            .mime_str("text/plain")
            .expect("valid mime type");
        form = form.part("data", part);
    }

    let start = Instant::now();
    let resp = client
        .post(format!("{base_url}/api/v1/add"))
        .multipart(form)
        .timeout(Duration::from_secs(1800))
        .send()
        .expect("add request");
    let elapsed = start.elapsed();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "POST /api/v1/add failed: {}",
        resp.text().unwrap_or_default()
    );
    elapsed
}

/// POST `/api/v1/cognify` for a dataset.
/// Returns the wall-clock duration of the HTTP call only.
fn cognify(client: &Client, base_url: &str, dataset_name: &str) -> Duration {
    // camelCase field names per CognifyPayloadDTO (#[serde(rename_all = "camelCase")])
    let payload = serde_json::json!({
        "datasets": [dataset_name],
        "runInBackground": false
    });
    let start = Instant::now();
    let resp = client
        .post(format!("{base_url}/api/v1/cognify"))
        .json(&payload)
        .timeout(Duration::from_secs(36_000))
        .send()
        .expect("cognify request");
    let elapsed = start.elapsed();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "POST /api/v1/cognify failed: {}",
        resp.text().unwrap_or_default()
    );
    elapsed
}

/// POST `/api/v1/search` for a dataset.
/// Returns the wall-clock duration of the HTTP call only.
fn search_documents(client: &Client, base_url: &str, dataset_name: &str, query: &str) -> Duration {
    // camelCase field names per SearchPayloadDTO (#[serde(rename_all = "camelCase")])
    let payload = serde_json::json!({
        "searchType": "GRAPH_COMPLETION",
        "query": query,
        "datasets": [dataset_name],
        "onlyContext": false,
        "verbose": false
    });
    let start = Instant::now();
    let resp = client
        .post(format!("{base_url}/api/v1/search"))
        .json(&payload)
        .timeout(Duration::from_secs(3_600))
        .send()
        .expect("search request");
    let elapsed = start.elapsed();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "POST /api/v1/search failed: {}",
        resp.text().unwrap_or_default()
    );
    elapsed
}

// ── Criterion benchmark ───────────────────────────────────────────────────────

fn num_files() -> usize {
    std::env::var("COGNEE_BENCH_NUM_FILES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

/// Benchmark: full pipeline — add → cognify → search.
///
/// Each Criterion sample runs the three operations in sequence against a single
/// shared server instance.  Per-phase durations are printed to stderr so they
/// remain visible even when Criterion redirects stdout.
///
/// # Criterion parameters
///
/// `measurement_time(30s)` is intentionally shorter than a single pipeline run
/// (~minutes with a real LLM).  This forces Criterion to schedule exactly **1
/// pipeline execution per sample**, giving 10 clean, independent measurements
/// without the iteration explosion seen when the budget exceeds the per-run time.
fn bench_pipeline(c: &mut Criterion) {
    let n = num_files();

    let mut group = c.benchmark_group("pipeline");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    // Shorter than one full pipeline run → Criterion schedules 1 iter/sample.
    group.measurement_time(Duration::from_secs(30));

    group.bench_function(BenchmarkId::new("files", n), |b| {
        let server = ServerGuard::start();
        let client = Client::builder()
            .timeout(Duration::from_secs(36_000))
            .build()
            .expect("reqwest client");

        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let dataset = format!("bench_{}", Uuid::new_v4().simple());
                let mut rng = rand::thread_rng();
                let docs: Vec<String> = (0..n).map(|_| generate_document(&mut rng)).collect();

                let t_add = add_documents(&client, &server.base_url, &dataset, &docs);
                let t_cognify = cognify(&client, &server.base_url, &dataset);
                let t_search = search_documents(
                    &client,
                    &server.base_url,
                    &dataset,
                    "What is in the document?",
                );

                eprintln!(
                    "[pipeline] add={t_add:.2?}  cognify={t_cognify:.2?}  search={t_search:.2?}"
                );

                total += t_add + t_cognify + t_search;
            }
            total
        })
    });

    group.finish();
}

criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
