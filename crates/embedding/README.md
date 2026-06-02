# Cognee-Embedding

Multi-provider text embedding engine for Cognee-Rust. Supports local ONNX
inference (BGE-Small-v1.5) plus OpenAI-compatible and Ollama HTTP backends,
selected at runtime via `EmbeddingConfig`.

## Providers

Selected via `EmbeddingProvider` (or the `EMBEDDING_PROVIDER` env var):

- **`OnnxEmbeddingEngine`** (`onnx` feature) — local ONNX Runtime inference via
  `ort`, with HuggingFace tokenizers; auto-downloads models from HuggingFace Hub
- **`OpenAICompatibleEmbeddingEngine`** — OpenAI/Azure/vLLM/llama.cpp/TEI via HTTP
  (retry + input sanitization)
- **`OllamaEmbeddingEngine`** — Ollama `/api/embed`
- **`MockEmbeddingEngine`** — zero vectors for testing (`MOCK_EMBEDDING=true`)

The default provider is **OpenAI `text-embedding-3-small`** (1536-d) on host
platforms and local **ONNX** on Android (when the `onnx` feature is enabled).

## Features

- **ONNX Runtime:** Efficient local inference via `ort` crate (behind the `onnx` feature)
- **HuggingFace Tokenizers:** Proper BPE/WordPiece tokenization matching Python fastembed
- **Batch Processing:** Process multiple texts in single inference call
- **L2 Normalization:** Unit vectors for cosine similarity
- **Async API:** Non-blocking via `spawn_blocking`

## Quick Start

### From environment (Recommended)

`EmbeddingConfig::from_env()` reads the same env vars as the Python SDK and
`create_engine()` returns the appropriate provider as `Arc<dyn EmbeddingEngine>`:

```rust
use cognee_embedding::EmbeddingConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Reads EMBEDDING_PROVIDER, EMBEDDING_MODEL, EMBEDDING_ENDPOINT, etc.
    let config = EmbeddingConfig::from_env();
    let engine = config.create_engine().await?;

    let texts = ["Cognee transforms documents into AI memory"];
    let embeddings = engine.embed(&texts).await?;

    println!("Dimension: {}", embeddings[0].len());
    Ok(())
}
```

### Local ONNX with automatic download

With the `onnx` feature, `OnnxEmbeddingEngine` auto-downloads the model and
tokenizer from HuggingFace Hub if not found locally. It is configured with an
`OnnxEmbeddingConfig`:

```rust
use cognee_embedding::{EmbeddingEngine, OnnxEmbeddingConfig, OnnxEmbeddingEngine};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Configure the ONNX engine (BGE-Small-v1.5 by default)
    let config = OnnxEmbeddingConfig::bge_small("./target/models");

    // 2. Create engine (auto-downloads model and tokenizer if missing)
    let engine = OnnxEmbeddingEngine::with_auto_download(config).await?;

    // 3. Embed texts (note: embed() takes &[&str])
    let texts = [
        "Cognee transforms documents into AI memory",
        "Knowledge graphs enable semantic search",
    ];

    let embeddings = engine.embed(&texts).await?;

    // 4. Use embeddings (each is a 384-dim L2-normalized vector)
    for (text, embedding) in texts.iter().zip(embeddings) {
        println!("Text: {}", text);
        println!("Dimension: {}", embedding.len());
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        println!("L2 Norm: {:.6}", norm);  // Should be ~1.0
    }

    Ok(())
}
```

### Manual model placement (Advanced)

If you prefer to download models manually, use the synchronous constructor
`OnnxEmbeddingEngine::new(config)` instead of `with_auto_download`. It expects
the files referenced by the config to already exist:

- Model: `./target/models/BGE-Small-v1.5-model_quantized.onnx`
- Tokenizer: `./target/models/bge-small-tokenizer.json`

## Models Supported

### BGE-Small-v1.5 (default)

- **Model:** BAAI/bge-small-en-v1.5
- **Dimensions:** 384
- **Size:** ~90MB (quantized)
- **Tokenizer:** `BAAI/bge-small-en-v1.5`
- **Max sequence:** 512 tokens

```rust
let config = OnnxEmbeddingConfig::bge_small("./target/models");
```

### all-MiniLM-L6-v2

- **Model:** sentence-transformers/all-MiniLM-L6-v2
- **Dimensions:** 384
- **Size:** ~22MB (quantized)
- **Tokenizer:** `sentence-transformers/all-MiniLM-L6-v2`
- **Max sequence:** 256 tokens

```rust
let config = OnnxEmbeddingConfig::minilm_l6("./target/models");
```

## Download API

For advanced use cases, you can use the download utilities directly:

```rust
use cognee_embedding::download::{download_model, ensure_model_exists, ModelUrls};
use std::path::Path;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Download a known model by name
    let model_dir = Path::new("./target/models");
    let (model_path, tokenizer_path) = download_model("bge-small-en-v1.5", model_dir).await?;
    
    // Or download from custom URLs
    let model_url = "https://huggingface.co/...";
    let model_path = Path::new("./models/model.onnx");
    ensure_model_exists(model_path, model_url).await?;
    
    Ok(())
}
```

Supported model names: `"bge-small-en-v1.5"`, `"all-MiniLM-L6-v2"`

## Running Examples

```bash
# Basic usage example (downloads the BGE-Small model on first run)
cargo run --example embedding_engine_example
```

## Running Tests

```bash
# Unit tests (no model required)
cargo test --package cognee-embedding

# Integration tests (requires model + tokenizer)
cargo test --package cognee-embedding --test integration -- --ignored
```

## API Reference

### EmbeddingEngine Trait

```rust
#[async_trait]
pub trait EmbeddingEngine: Send + Sync {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;
    fn batch_size(&self) -> usize;
    fn max_sequence_length(&self) -> usize;
}
```

### Configuration

`EmbeddingConfig` is the provider-agnostic top-level config (use
`EmbeddingConfig::from_env()` or `EmbeddingConfig::default()`):

```rust
pub struct EmbeddingConfig {
    pub provider: EmbeddingProvider,        // Onnx / Fastembed / OpenAi / OpenAiCompatible / Ollama / Mock
    pub model: String,                      // Model identifier
    pub dimensions: usize,                  // Output dimensions
    pub endpoint: Option<String>,           // API endpoint (HTTP providers)
    pub api_key: Option<String>,            // EMBEDDING_API_KEY / LLM_API_KEY
    pub api_version: Option<String>,        // e.g. Azure API version
    pub max_completion_tokens: usize,       // default 8191
    pub batch_size: usize,                  // default 36
    pub mock: bool,                         // force mock zero vectors
    #[cfg(feature = "onnx")]
    pub onnx: OnnxEmbeddingConfig,          // ONNX-only settings
    pub huggingface_tokenizer: Option<String>,
}
```

`OnnxEmbeddingConfig` (behind the `onnx` feature) holds the ONNX-only fields:

```rust
pub struct OnnxEmbeddingConfig {
    pub model_path: PathBuf,         // Path to .onnx file
    pub tokenizer_path: PathBuf,     // Path to tokenizer.json
    pub model_name: String,          // Display name / auto-download selector
    pub dimensions: usize,           // Output dimensions
    pub max_sequence_length: usize,  // Max tokens
    pub batch_size: usize,           // Batch size
}
```

### Environment variables

`EmbeddingConfig::from_env()` reads (Python-SDK-compatible names):
`EMBEDDING_PROVIDER`, `MOCK_EMBEDDING`, `EMBEDDING_MODEL`, `EMBEDDING_DIMENSIONS`,
`EMBEDDING_ENDPOINT`, `EMBEDDING_API_KEY` (fallback `LLM_API_KEY`),
`EMBEDDING_API_VERSION`, `EMBEDDING_MAX_COMPLETION_TOKENS`, `EMBEDDING_BATCH_SIZE`,
`HUGGINGFACE_TOKENIZER`.

## Architecture

The implementation follows these key patterns:

1. **HuggingFace Tokenization:** Uses `tokenizers` crate to load tokenizer.json files, ensuring exact match with Python fastembed
2. **ONNX Inference:** Runs model via `ort` crate with Level3 graph optimization
3. **Mean Pooling:** Averages token embeddings (respecting attention mask) over sequence dimension
4. **L2 Normalization:** All output vectors normalized to unit length
5. **Async Wrapper:** Synchronous ONNX calls wrapped in `tokio::task::spawn_blocking`
6. **Thread Safety:** Session and tokenizer wrapped in `Arc<Mutex<T>>`

## Python Parity

This implementation matches Python's `FastembedEmbeddingEngine` by:

- Using the same HuggingFace tokenizers (exact token IDs)
- Same ONNX models from HuggingFace Hub
- Same pooling and normalization strategies
- Results should match within floating-point precision (< 0.01 cosine distance)

## Troubleshooting

### "Model file not found"

Download the model first:
```bash
cargo run --example embedding_engine_example
```

### "Failed to load tokenizer" / "Tokenizer.json not found"

Use `OnnxEmbeddingEngine::with_auto_download(...)` (or the example above) to
fetch the model and tokenizer from HuggingFace Hub automatically. If you place
files manually, the tokenizer must be at:
`./target/models/bge-small-tokenizer.json`

## License

Same as cognee-rust (check root LICENSE file)
