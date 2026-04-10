use std::sync::Arc;

use async_trait::async_trait;
use cognee_session::SessionContext;

use crate::types::{SearchContext, SearchError, SearchOutput, SearchParams, SearchType};

pub type SearchRetrieverRef = Arc<dyn SearchRetriever>;

#[async_trait]
pub trait SearchRetriever: Send + Sync {
    fn search_type(&self) -> SearchType;

    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError>;

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError>;

    /// Process multiple queries in sequence and return their contexts.
    ///
    /// Default: loops over [`get_context`]. Override for efficient batching.
    async fn get_context_batch(
        &self,
        queries: &[String],
        params: &SearchParams,
    ) -> Result<Vec<SearchContext>, SearchError> {
        let mut results = Vec::with_capacity(queries.len());
        for query in queries {
            results.push(self.get_context(query, params).await?);
        }
        Ok(results)
    }

    /// Process multiple queries and return their completions.
    ///
    /// Default: loops over [`get_completion`]. Override for efficient batching.
    async fn get_completion_batch(
        &self,
        queries: &[String],
        contexts: Option<Vec<SearchContext>>,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Vec<SearchOutput>, SearchError> {
        let mut results = Vec::with_capacity(queries.len());
        for (i, query) in queries.iter().enumerate() {
            let ctx = contexts.as_ref().and_then(|cs| cs.get(i).cloned());
            results.push(self.get_completion(query, ctx, session, params).await?);
        }
        Ok(results)
    }
}
