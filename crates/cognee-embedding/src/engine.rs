use crate::error::EmbeddingResult;
use async_trait::async_trait;

/// Core trait for text embedding engines
///
/// Provides async interface for embedding generation while allowing
/// synchronous implementations (e.g., ONNX) to be wrapped via spawn_blocking.
///
/// All returned embeddings are L2-normalized to unit vectors for cosine similarity.
#[async_trait]
pub trait EmbeddingEngine: Send + Sync {
    /// Embed a batch of text strings into normalized vectors
    ///
    /// # Arguments
    /// * `texts` - Slice of strings to embed
    ///
    /// # Returns
    /// * Vector of embeddings, one per input text. Each embedding is L2-normalized.
    ///
    /// # Errors
    /// * Returns error if tokenization, inference, or normalization fails
    ///
    /// # Example
    /// ```ignore
    /// let texts = vec!["Hello world".to_string()];
    /// let embeddings = engine.embed(&texts).await?;
    /// assert_eq!(embeddings.len(), 1);
    /// assert_eq!(embeddings[0].len(), engine.dimension());
    /// ```
    async fn embed(&self, texts: &[String]) -> EmbeddingResult<Vec<Vec<f32>>>;

    /// Get the dimensionality of embeddings produced by this engine
    ///
    /// # Returns
    /// * Number of dimensions in output vectors (e.g., 384 for BGE-Small)
    fn dimension(&self) -> usize;

    /// Get the optimal batch size for this engine
    ///
    /// Batches larger than this should be chunked by the caller.
    ///
    /// # Returns
    /// * Maximum number of texts to process in a single embed() call
    fn batch_size(&self) -> usize;

    /// Get the maximum sequence length (in tokens) supported
    ///
    /// Input texts will be truncated to this length during tokenization.
    ///
    /// # Returns
    /// * Maximum token count (e.g., 512 for BGE-Small-v1.5)
    fn max_sequence_length(&self) -> usize;
}
