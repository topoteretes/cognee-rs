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
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>>;

    /// Embed a single **query**, applying whatever asymmetric query handling the
    /// engine's model needs.
    ///
    /// Retrieval models in the BGE/E5 families are trained asymmetrically: the
    /// query is prefixed with a short task instruction and the passage never is.
    /// Embedding a query through [`EmbeddingEngine::embed`] therefore projects it
    /// as if it were a passage, which costs ranking quality on exactly the
    /// queries that share no vocabulary with their answer.
    ///
    /// The default implementation is [`EmbeddingEngine::embed`], so an engine
    /// whose model is symmetric (OpenAI, Ollama, the mock) needs no override and
    /// behaves exactly as before.
    ///
    /// # Errors
    /// * Returns whatever [`EmbeddingEngine::embed`] would return.
    async fn embed_query(&self, query: &str) -> EmbeddingResult<Vec<Vec<f32>>> {
        self.embed(&[query]).await
    }

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
