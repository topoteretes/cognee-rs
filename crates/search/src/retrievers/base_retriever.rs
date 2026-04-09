use std::sync::Arc;

use async_trait::async_trait;
use cognee_session::SessionContext;

use crate::types::{SearchContext, SearchError, SearchOutput, SearchType};

pub type SearchRetrieverRef = Arc<dyn SearchRetriever>;

#[async_trait]
pub trait SearchRetriever: Send + Sync {
    fn search_type(&self) -> SearchType;

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError>;

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
    ) -> Result<SearchOutput, SearchError>;
}
