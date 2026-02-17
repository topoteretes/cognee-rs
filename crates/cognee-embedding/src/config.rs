use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for ONNX embedding engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Path to ONNX model file (.onnx)
    pub model_path: PathBuf,

    /// Path to tokenizer.json file
    pub tokenizer_path: PathBuf,

    /// Model name for logging/identification
    pub model_name: String,

    /// Embedding dimensions (must match model output)
    pub dimensions: usize,

    /// Maximum sequence length in tokens (truncate if longer)
    pub max_sequence_length: usize,

    /// Batch size for inference (max texts per inference call)
    pub batch_size: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::from("./target/models/BGE-Small-v1.5-model_quantized.onnx"),
            tokenizer_path: PathBuf::from("./target/models/bge-small-tokenizer.json"),
            model_name: "BGE-Small-v1.5".to_string(),
            dimensions: 384,
            max_sequence_length: 512,
            batch_size: 32,
        }
    }
}

impl EmbeddingConfig {
    /// Create config for BGE-Small-v1.5 model
    pub fn bge_small(model_dir: impl Into<PathBuf>) -> Self {
        let mut model_path = model_dir.into();
        let tokenizer_path = model_path.join("bge-small-tokenizer.json");
        model_path.push("BGE-Small-v1.5-model_quantized.onnx");
        Self {
            model_path,
            tokenizer_path,
            model_name: "BGE-Small-v1.5".to_string(),
            dimensions: 384,
            max_sequence_length: 512,
            batch_size: 32,
        }
    }

    /// Create config for all-MiniLM-L6-v2 model  
    pub fn minilm_l6(model_dir: impl Into<PathBuf>) -> Self {
        let mut model_path = model_dir.into();
        let tokenizer_path = model_path.join("minilm-l6-tokenizer.json");
        model_path.push("all-MiniLM-L6-v2.onnx");
        Self {
            model_path,
            tokenizer_path,
            model_name: "all-MiniLM-L6-v2".to_string(),
            dimensions: 384,
            max_sequence_length: 256,
            batch_size: 32,
        }
    }
}
