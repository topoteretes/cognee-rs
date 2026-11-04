use crate::error::{EmbeddingError, EmbeddingResult};
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

    /// Embed several **queries**: one vector per query, in order, each shaped
    /// exactly as [`EmbeddingEngine::embed_query`] shapes it.
    ///
    /// The default calls [`EmbeddingEngine::embed_query`] once per query, in
    /// sequence, so an engine that customises only `embed_query` keeps its
    /// query handling here. Engines override it to send the whole batch in one
    /// [`EmbeddingEngine::embed`] call — as Python's `batch_search` embeds its
    /// `query_texts` — applying their query handling to every query first.
    ///
    /// # Errors
    /// * Returns whatever [`EmbeddingEngine::embed_query`] would return, or
    ///   [`EmbeddingError::InferenceError`] when it yields no vector.
    async fn embed_queries(&self, queries: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        let mut vectors = Vec::with_capacity(queries.len());
        for query in queries {
            let vector = self
                .embed_query(query)
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    EmbeddingError::InferenceError("embed_query returned no vector".to_string())
                })?;
            vectors.push(vector);
        }
        Ok(vectors)
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

    /// How many [`batch_size`](Self::batch_size)-sized batches a caller may keep
    /// in flight at once.
    ///
    /// Python bounds indexing at `max(1, embedding_max_concurrent_data_points //
    /// batch_size)` batches (`index_data_points.py:57-71`); the default
    /// implementation is that formula with the default of
    /// [`DEFAULT_MAX_CONCURRENT_DATA_POINTS`](crate::config::DEFAULT_MAX_CONCURRENT_DATA_POINTS).
    /// Engines configured with `embedding.max_concurrent_data_points` override it.
    fn max_concurrent_batches(&self) -> usize {
        crate::config::concurrent_batches(
            crate::config::DEFAULT_MAX_CONCURRENT_DATA_POINTS,
            self.batch_size(),
        )
    }

    /// Get the maximum sequence length (in tokens) supported
    ///
    /// Input texts will be truncated to this length during tokenization.
    ///
    /// # Returns
    /// * Maximum token count (e.g., 512 for BGE-Small-v1.5)
    fn max_sequence_length(&self) -> usize;
}

/// Embed `texts` the way Python's `index_data_points` does: cut them into
/// batches of `engine.batch_size()` and keep at most
/// `engine.max_concurrent_batches()` of those batches in flight
/// (`index_data_points.py:57-71`). Vectors come back in input order; the first
/// failure aborts the call and cancels the batches still in flight.
///
/// # Errors
/// * Returns the first error any batch returns.
pub async fn embed_in_batches(
    engine: &dyn EmbeddingEngine,
    texts: &[&str],
) -> EmbeddingResult<Vec<Vec<f32>>> {
    use futures::stream::{self, StreamExt, TryStreamExt};

    if texts.is_empty() {
        return Ok(Vec::new());
    }
    // Futures are collected first (not mapped inside the stream) so the
    // returned future stays `Send` for every borrow lifetime.
    let futures: Vec<_> = texts
        .chunks(engine.batch_size().max(1))
        .map(|batch| engine.embed(batch))
        .collect();
    let batches: Vec<Vec<Vec<f32>>> = stream::iter(futures)
        .buffered(engine.max_concurrent_batches().max(1))
        .try_collect()
        .await?;
    Ok(batches.into_iter().flatten().collect())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    /// Shapes queries differently from passages, and overrides nothing else.
    struct ShapedQueries;

    #[async_trait]
    impl EmbeddingEngine for ShapedQueries {
        async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|text| vec![text.len() as f32]).collect())
        }
        async fn embed_query(&self, query: &str) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(vec![vec![-(query.len() as f32)]])
        }
        fn dimension(&self) -> usize {
            1
        }
        fn batch_size(&self) -> usize {
            8
        }
        fn max_sequence_length(&self) -> usize {
            128
        }
    }

    /// The default goes through `embed_query`, so a custom query shaping is
    /// kept for every query of a batch.
    #[tokio::test]
    async fn default_embed_queries_keeps_the_engines_query_shaping() {
        let vectors = ShapedQueries.embed_queries(&["a", "bcd"]).await.unwrap();
        assert_eq!(vectors, vec![vec![-1.0], vec![-3.0]]);
    }

    /// Counts concurrent `embed` calls to check `embed_in_batches`.
    struct Probe {
        batch: usize,
        concurrency: usize,
        in_flight: std::sync::atomic::AtomicUsize,
        peak: std::sync::atomic::AtomicUsize,
        sizes: std::sync::Mutex<Vec<usize>>,
    }

    #[async_trait]
    impl EmbeddingEngine for Probe {
        async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            use std::sync::atomic::Ordering::SeqCst;
            self.sizes.lock().unwrap().push(texts.len());
            let now = self.in_flight.fetch_add(1, SeqCst) + 1;
            self.peak.fetch_max(now, SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.in_flight.fetch_sub(1, SeqCst);
            Ok(texts.iter().map(|t| vec![t.len() as f32]).collect())
        }
        fn dimension(&self) -> usize {
            1
        }
        fn batch_size(&self) -> usize {
            self.batch
        }
        fn max_concurrent_batches(&self) -> usize {
            self.concurrency
        }
        fn max_sequence_length(&self) -> usize {
            1
        }
    }

    fn probe(batch: usize, concurrency: usize) -> Probe {
        Probe {
            batch,
            concurrency,
            in_flight: Default::default(),
            peak: Default::default(),
            sizes: Default::default(),
        }
    }

    #[tokio::test]
    async fn embed_in_batches_cuts_batches_and_keeps_input_order() {
        let engine = probe(32, 4);
        let owned: Vec<String> = (0..70).map(|i| "x".repeat(i + 1)).collect();
        let texts: Vec<&str> = owned.iter().map(String::as_str).collect();
        let vectors = embed_in_batches(&engine, &texts).await.unwrap();
        assert_eq!(vectors.len(), 70);
        for (i, v) in vectors.iter().enumerate() {
            assert_eq!(v[0], (i + 1) as f32, "vector {i} out of order");
        }
        let mut sizes = engine.sizes.lock().unwrap().clone();
        sizes.sort_unstable();
        assert_eq!(sizes, vec![6, 32, 32]);
    }

    #[tokio::test]
    async fn embed_in_batches_bounds_in_flight_batches() {
        use std::sync::atomic::Ordering::SeqCst;
        let engine = probe(2, 3);
        let texts = vec!["a"; 20];
        embed_in_batches(&engine, &texts).await.unwrap();
        assert_eq!(engine.peak.load(SeqCst), 3);

        let engine = probe(2, 1);
        embed_in_batches(&engine, &texts).await.unwrap();
        assert_eq!(engine.peak.load(SeqCst), 1);
    }

    #[tokio::test]
    async fn embed_in_batches_of_nothing_makes_no_call() {
        let engine = probe(32, 4);
        assert!(embed_in_batches(&engine, &[]).await.unwrap().is_empty());
        assert!(engine.sizes.lock().unwrap().is_empty());
    }

    #[test]
    fn default_max_concurrent_batches_is_150_over_batch_size() {
        assert_eq!(
            ShapedQueries.max_concurrent_batches(),
            150 / ShapedQueries.batch_size()
        );
    }
}
