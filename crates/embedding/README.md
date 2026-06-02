# Cognee-Embedding

Embedding generation for Cognee-Rust using local ONNX models with HuggingFace tokenizers.

## Features

- **ONNX Runtime:** Efficient local inference via `ort` crate
- **HuggingFace Tokenizers:** Proper BPE/WordPiece tokenization matching Python fastembed
- **Batch Processing:** Process multiple texts in single inference call
- **L2 Normalization:** Unit vectors for cosine similarity
- **Python Parity:** Embeddings match Python's FastembedEmbeddingEngine
- **Async API:** Non-blocking via `spawn_blocking`

## Quick Start

### Automatic Download (Recommended)

The embedding engine will automatically download models and tokenizers from HuggingFace Hub if not found locally:

```rust
use cognee_embedding::{
    config::EmbeddingConfig,
    engine::EmbeddingEngine,
    onnx::OnnxEmbeddingEngine,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Configure engine
    let config = EmbeddingConfig::bge_small("./target/models");
    
    // 2. Create engine (auto-downloads model and tokenizer if missing)
    let engine = OnnxEmbeddingEngine::with_auto_download(config).await?;
    
    // 3. Embed texts
    let texts = vec![
        "Cognee transforms documents into AI memory".to_string(),
        "Knowledge graphs enable semantic search".to_string(),
    ];
    
    let embeddings = engine.embed(&texts).await?;
    
    // 4. Use embeddings (each is 384-dim L2-normalized vector)
    for (text, embedding) in texts.iter().zip(embeddings) {
        println!("Text: {}", text);
        println!("Dimension: {}", embedding.len());
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        println!("L2 Norm: {:.6}", norm);  // Should be ~1.0
    }
    
    Ok(())
}
```

### Manual Download (Advanced)

If you prefer to download models manually or use pre-existing models, you can use the standard constructor:

```rust
use cognee_embedding::{
    config::EmbeddingConfig,
    engine::EmbeddingEngine,
    onnx::OnnxEmbeddingEngine,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Manually ensure models exist in target directory
    // - Model: ./target/models/BGE-Small-v1.5-model_quantized.onnx
    // - Tokenizer: ./target/models/bge-small-tokenizer.json
    
    // 2. Configure engine
    let config = EmbeddingConfig::bge_small("./target/models");
    
    // 2. Create engine
    let engine = OnnxEmbeddingEngine::new(config)?;
    
    // 3. Embed texts
    let texts = vec![
        "Cognee transforms documents into AI memory".to_string(),
        "Knowledge graphs enable semantic search".to_string(),
    ];
    
    let embeddings = engine.embed(&texts).await?;
    
    // 4. Use embeddings (each is 384-dim L2-normalized vector)
    for (text, embedding) in texts.iter().zip(embeddings) {
        println!("Text: {}", text);
        println!("Dimension: {}", embedding.len());
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        println!("L2 Norm: {:.6}", norm);  // Should be ~1.0
    }
    
    Ok(())
}
```

## Models Supported

### BGE-Small-v1.5 (default)

- **Model:** BAAI/bge-small-en-v1.5
- **Dimensions:** 384
- **Size:** ~90MB (quantized)
- **Tokenizer:** `BAAI/bge-small-en-v1.5`
- **Max sequence:** 512 tokens

```rust
let config = EmbeddingConfig::bge_small("./target/models");
```

### all-MiniLM-L6-v2

- **Model:** sentence-transformers/all-MiniLM-L6-v2
- **Dimensions:** 384
- **Size:** ~22MB (quantized)
- **Tokenizer:** `sentence-transformers/all-MiniLM-L6-v2`
- **Max sequence:** 256 tokens

```rust
let config = EmbeddingConfig::minilm_l6("./target/models");
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
    async fn embed(&self, texts: &[String]) -> EmbeddingResult<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;
    fn batch_size(&self) -> usize;
    fn max_sequence_length(&self) -> usize;
}
```

### Configuration

```rust
pub struct EmbeddingConfig {
    pub model_path: PathBuf,        // Path to .onnx file
    pub tokenizer_path: PathBuf,    // Path to tokenizer.json
    pub model_name: String,          // Display name
    pub dimensions: usize,           // Output dimensions
    pub max_sequence_length: usize,  // Max tokens
    pub batch_size: usize,           // Batch size
}
```

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

### "Failed to load tokenizer"

Download the tokenizer:
```bash
python scripts/download-tokenizer.py
```

### "Tokenizer.json not found"

Make sure you've run the download script. The tokenizer must be at:
`./target/models/bge-small-tokenizer.json`

## License

Same as cognee-rust (check root LICENSE file)
