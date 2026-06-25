use async_trait::async_trait;
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::Tensor;
use std::sync::{Arc, Mutex};
use tokenizers::Tokenizer;
use tracing::info;

use crate::{
    config::OnnxEmbeddingConfig,
    download::{ModelUrls, ensure_model_exists, ensure_tokenizer_exists},
    engine::EmbeddingEngine,
    error::{EmbeddingError, EmbeddingResult},
    utils::{l2_normalize, mean_pool},
};
/// Type alias for tokenization batch results
type TokenizationBatch = (Vec<Vec<i64>>, Vec<Vec<i64>>);
/// ONNX-based embedding engine for local inference
///
/// Wraps ONNX Runtime session and HuggingFace tokenizer.
/// Based on examples/embeddings.rs with proper tokenization for Python parity.
pub struct OnnxEmbeddingEngine {
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Mutex<Tokenizer>>,
    config: OnnxEmbeddingConfig,
}

impl OnnxEmbeddingEngine {
    /// Create a new ONNX embedding engine
    ///
    /// Initializes ONNX Runtime, loads the model, and downloads/caches the tokenizer.
    ///
    /// # Arguments
    /// * `config` - Engine configuration with model path and tokenizer model ID
    ///
    /// # Returns
    /// * Initialized engine or error
    ///
    /// # Errors
    /// * Returns error if model file not found, ONNX Runtime init fails, or tokenizer download fails
    ///
    /// # Example
    /// ```ignore
    /// let config = OnnxEmbeddingConfig::bge_small("./target/models");
    /// let engine = OnnxEmbeddingEngine::new(config)?;
    /// ```
    pub fn new(config: OnnxEmbeddingConfig) -> EmbeddingResult<Self> {
        ort::init().commit();

        if !config.model_path.exists() {
            return Err(EmbeddingError::ModelLoadError(format!(
                "Model file not found: {:?}",
                config.model_path
            )));
        }

        info!("Loading tokenizer: {:?}", config.tokenizer_path);
        let tokenizer = Tokenizer::from_file(&config.tokenizer_path).map_err(|e| {
            EmbeddingError::TokenizerError(format!(
                "Failed to load tokenizer from {:?}: {}. Please ensure tokenizer.json file exists.",
                config.tokenizer_path, e
            ))
        })?;

        info!("Loading ONNX model: {:?}", config.model_path);
        let session = Session::builder()
            .map_err(|e| EmbeddingError::ModelLoadError(e.to_string()))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| EmbeddingError::ModelLoadError(e.to_string()))?
            .commit_from_file(&config.model_path)
            .map_err(|e| EmbeddingError::ModelLoadError(e.to_string()))?;

        info!(
            "✓ Loaded {} (dims: {}, max_seq_len: {})",
            config.model_name, config.dimensions, config.max_sequence_length
        );

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(Mutex::new(tokenizer)),
            config,
        })
    }

    /// Create a new ONNX embedding engine with automatic model downloading
    ///
    /// Downloads model and tokenizer from HuggingFace Hub if not found locally.
    /// This is the recommended constructor for most use cases.
    ///
    /// # Arguments
    /// * `config` - Engine configuration with model path and tokenizer model ID
    ///
    /// # Returns
    /// * Initialized engine or error
    ///
    /// # Errors
    /// * Returns error if download fails, ONNX Runtime init fails, or tokenizer load fails
    ///
    /// # Example
    /// ```ignore
    /// let config = OnnxEmbeddingConfig::bge_small("./target/models");
    /// let engine = OnnxEmbeddingEngine::with_auto_download(config).await?;
    /// ```
    pub async fn with_auto_download(config: OnnxEmbeddingConfig) -> EmbeddingResult<Self> {
        let (model_url, tokenizer_url) = match config.model_name.to_lowercase().as_str() {
            "bge-small-en-v1.5" | "bge-small-v1.5" => (
                ModelUrls::BGE_SMALL.model_url,
                ModelUrls::BGE_SMALL.tokenizer_url,
            ),
            "all-minilm-l6-v2" => (
                ModelUrls::MINILM_L6.model_url,
                ModelUrls::MINILM_L6.tokenizer_url,
            ),
            _ => {
                return Err(EmbeddingError::ModelLoadError(format!(
                    "Unknown model name '{}'. Supported: 'bge-small-en-v1.5', 'all-MiniLM-L6-v2'",
                    config.model_name
                )));
            }
        };

        let model_downloaded = ensure_model_exists(&config.model_path, model_url).await?;
        if model_downloaded {
            info!("✓ Downloaded model to {:?}", config.model_path);
        }

        let tokenizer_downloaded =
            ensure_tokenizer_exists(&config.tokenizer_path, tokenizer_url).await?;
        if tokenizer_downloaded {
            info!("✓ Downloaded tokenizer to {:?}", config.tokenizer_path);
        }

        Self::new(config)
    }

    /// Tokenize a batch of texts using HuggingFace tokenizer
    ///
    /// Uses proper BPE/WordPiece tokenization matching Python fastembed.
    ///
    /// # Arguments
    /// * `texts` - Texts to tokenize
    ///
    /// # Returns
    /// * Tuple of (input_ids, attention_mask) tensors, both with shape [batch_size, max_seq_len]
    fn tokenize_batch(&self, texts: &[&str]) -> EmbeddingResult<TokenizationBatch> {
        #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
        let tokenizer = self.tokenizer.lock().unwrap(); // lock poison is unrecoverable
        let max_len = self.config.max_sequence_length;

        let mut input_ids_batch = Vec::new();
        let mut attention_mask_batch = Vec::new();

        for text in texts {
            let encoding = tokenizer
                .encode(*text, true)
                .map_err(|e| EmbeddingError::TokenizerError(e.to_string()))?;

            let mut ids = encoding
                .get_ids()
                .iter()
                .map(|&id| id as i64)
                .collect::<Vec<_>>();
            let mut mask = encoding
                .get_attention_mask()
                .iter()
                .map(|&m| m as i64)
                .collect::<Vec<_>>();

            if ids.len() > max_len {
                ids.truncate(max_len);
                mask.truncate(max_len);
            }

            while ids.len() < max_len {
                ids.push(0); // [PAD] token
                mask.push(0); // Padding mask
            }

            input_ids_batch.push(ids);
            attention_mask_batch.push(mask);
        }

        Ok((input_ids_batch, attention_mask_batch))
    }

    /// Extract embedding from ONNX output tensor
    ///
    /// Handles both 2D [seq_len, hidden_dim] and 3D [batch_size, seq_len, hidden_dim] outputs.
    fn extract_embedding(
        &self,
        output_data: &[f32],
        output_shape: &[usize],
        attention_mask: &[i64],
    ) -> EmbeddingResult<Vec<f32>> {
        let output_dim = self.config.dimensions;

        if output_shape.len() == 3 {
            let seq_len = output_shape[1];
            let hidden_dim = output_shape[2];

            let pooled = mean_pool(output_data, seq_len, hidden_dim, attention_mask, output_dim);
            Ok(l2_normalize(&pooled))
        } else if output_shape.len() == 2 {
            let embedding: Vec<f32> = output_data.iter().take(output_dim).copied().collect();
            Ok(l2_normalize(&embedding))
        } else {
            Err(EmbeddingError::InferenceError(format!(
                "Unexpected output shape: {output_shape:?}"
            )))
        }
    }
}

impl OnnxEmbeddingEngine {
    /// Run ONNX inference over a SINGLE bounded batch of `texts`.
    ///
    /// A transformer's activation memory scales with `batch × seq_len`
    /// (attention is `batch × heads × seq_len²`), so the batch must stay small.
    /// The public [`OnnxEmbeddingEngine::embed`] splits large inputs into
    /// `config.batch_size` chunks before calling this — never pass an unbounded
    /// slice here.
    async fn embed_batch(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let batch_size = texts.len();
        let seq_len = self.config.max_sequence_length;

        let (input_ids_batch, attention_mask_batch) = self.tokenize_batch(texts)?;

        let input_ids_flat: Vec<i64> = input_ids_batch.iter().flatten().copied().collect();
        let attention_mask_flat: Vec<i64> =
            attention_mask_batch.iter().flatten().copied().collect();

        let input_ids_tensor = Tensor::from_array((vec![batch_size, seq_len], input_ids_flat))
            .map_err(|e| EmbeddingError::InferenceError(e.to_string()))?;
        let attention_mask_tensor =
            Tensor::from_array((vec![batch_size, seq_len], attention_mask_flat))
                .map_err(|e| EmbeddingError::InferenceError(e.to_string()))?;
        let token_type_ids_tensor =
            Tensor::from_array((vec![batch_size, seq_len], vec![0i64; batch_size * seq_len]))
                .map_err(|e| EmbeddingError::InferenceError(e.to_string()))?;

        let session = Arc::clone(&self.session);
        let attention_masks = attention_mask_batch.clone();

        let (output_shape, output_data) = tokio::task::spawn_blocking(move || {
            #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
            let mut session = session.lock().unwrap(); // lock poison is unrecoverable
            let outputs = session.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            })?;

            let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
            let shape_usize: Vec<usize> = shape.iter().map(|&d| d as usize).collect();
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>((shape_usize, data.to_vec()))
        })
        .await
        .map_err(|e| EmbeddingError::InferenceError(e.to_string()))?
        .map_err(|e| EmbeddingError::InferenceError(e.to_string()))?;

        let mut embeddings = Vec::with_capacity(batch_size);

        if output_shape.len() == 3 {
            let seq_len = output_shape[1];
            let hidden_dim = output_shape[2];
            let sample_size = seq_len * hidden_dim;

            for (i, mask) in attention_masks.iter().enumerate().take(batch_size) {
                let start = i * sample_size;
                let end = start + sample_size;
                let sample_data = &output_data[start..end];

                let embedding =
                    self.extract_embedding(sample_data, &[1, seq_len, hidden_dim], mask)?;

                embeddings.push(embedding);
            }
        } else if output_shape.len() == 2 && batch_size == 1 {
            let embedding =
                self.extract_embedding(&output_data, &output_shape, &attention_masks[0])?;
            embeddings.push(embedding);
        } else {
            return Err(EmbeddingError::InferenceError(format!(
                "Unexpected output shape: {output_shape:?} for batch_size {batch_size}"
            )));
        }

        Ok(embeddings)
    }
}

#[async_trait]
impl EmbeddingEngine for OnnxEmbeddingEngine {
    /// Embed `texts`, splitting into `config.batch_size` sub-batches so the ONNX
    /// session never receives an unbounded batch. A transformer's activation
    /// memory scales with `batch × seq_len²`; embedding a whole corpus in one
    /// call (several thousand chunks) would allocate tens of GB and OOM.
    /// Sub-batching keeps peak memory flat regardless of how many texts are
    /// passed.
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let batch = self.config.batch_size.max(1);
        if texts.len() <= batch {
            return self.embed_batch(texts).await;
        }
        let mut embeddings = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(batch) {
            embeddings.extend(self.embed_batch(chunk).await?);
        }
        Ok(embeddings)
    }

    fn dimension(&self) -> usize {
        self.config.dimensions
    }

    fn batch_size(&self) -> usize {
        self.config.batch_size
    }

    fn max_sequence_length(&self) -> usize {
        self.config.max_sequence_length
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tokenization() {
        // Test HuggingFace tokenizer loading from file
        // This test will be skipped if tokenizer file doesn't exist
        let tokenizer_path = "../../target/models/bge-small-tokenizer.json";
        if std::path::Path::new(tokenizer_path).exists() {
            let tokenizer = Tokenizer::from_file(tokenizer_path).expect("Failed to load tokenizer");

            let encoding = tokenizer.encode("Hello world", true).unwrap();
            let ids = encoding.get_ids();

            assert!(!ids.is_empty());
            assert_eq!(ids[0], 101); // [CLS] for BERT-based models
        }
    }

    #[test]
    fn test_l2_normalization() {
        use crate::utils::{compute_norm, l2_normalize};

        let vec = vec![3.0, 4.0];
        let normalized = l2_normalize(&vec);
        let norm = compute_norm(&normalized);

        assert!((norm - 1.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_engine_creation() {
        let config = OnnxEmbeddingConfig::default();
        // Will fail if model not present - that's expected
        let result = OnnxEmbeddingEngine::new(config);

        // Test passes if error is clear about missing model
        if let Err(e) = result {
            assert!(
                e.to_string().contains("Model file not found")
                    || e.to_string().contains("tokenizer")
            );
        }
    }

    /// Regression test for the unbounded-batch OOM: `embed` must split inputs
    /// larger than `config.batch_size` into sub-batches (so ONNX never sees a
    /// giant `[N, seq_len]` tensor), while returning one embedding per input
    /// that matches the single-batch result. Skips when the model artifacts
    /// have not been downloaded.
    #[tokio::test]
    async fn embed_sub_batches_large_inputs() {
        let model = "../../target/models/BGE-Small-v1.5-model_quantized.onnx";
        let tok = "../../target/models/bge-small-tokenizer.json";
        if !std::path::Path::new(model).exists() || !std::path::Path::new(tok).exists() {
            return; // model not available in this environment — skip
        }

        let config = OnnxEmbeddingConfig {
            model_path: model.into(),
            tokenizer_path: tok.into(),
            batch_size: 4, // force several sub-batches
            ..Default::default()
        };

        let engine = OnnxEmbeddingEngine::new(config).expect("engine creation");

        // 10 inputs with batch_size 4 → 3 sub-batches (4 + 4 + 2).
        let texts: Vec<String> = (0..10).map(|i| format!("sentence number {i}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();

        let chunked = engine.embed(&refs).await.expect("embed");
        assert_eq!(
            chunked.len(),
            10,
            "one embedding per input across sub-batches"
        );
        assert_eq!(chunked[0].len(), engine.dimension());

        // Sub-batching must not change an embedding's meaning. (Exact equality
        // can't be required: the quantized model selects batch-size-dependent
        // kernels, so values differ by tiny numerical noise.) The L2-normalized
        // vectors must stay essentially parallel — cosine similarity ≈ 1.
        let single = engine.embed_batch(&refs).await.expect("embed_batch");
        assert_eq!(single.len(), chunked.len());
        for (a, b) in chunked.iter().zip(single.iter()) {
            assert_eq!(a.len(), b.len());
            let cos: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
            assert!(cos > 0.999, "chunked vs single-batch diverged: cos={cos}");
        }
    }
}
