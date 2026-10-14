//! `ComponentRegistry::build_embedding` installs the process-wide embedding
//! pacer before it picks a factory, so a factory added with
//! `register_embedding` is paced like the built-ins.
//!
//! This lives in its own test binary because the pacer is a process global
//! that the first install wins: in a shared binary another test's embedding
//! build could install it first and make the assertion vacuous.

use std::sync::Arc;

use async_trait::async_trait;
use cognee_components::{
    BackendBuildContext, ComponentError, ComponentRegistry, EmbeddingFactory, EmbeddingInputs,
    LlmInputs,
};
use cognee_embedding::EmbeddingEngine;

/// A custom factory that never calls into the built-in embedding code.
struct CustomEmbedding;

#[async_trait]
impl EmbeddingFactory for CustomEmbedding {
    fn provider(&self) -> &str {
        "my_tei"
    }
    async fn build(
        &self,
        _ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        Err(ComponentError::EmbeddingEngine(
            "custom factory".to_string(),
        ))
    }
}

#[tokio::test]
async fn build_embedding_installs_the_pacer_for_a_custom_factory() {
    assert!(
        cognee_utils::pacing::embedding_pacer().is_none(),
        "nothing in this binary may install the pacer before the test runs"
    );

    let mut reg = ComponentRegistry::empty();
    reg.register_embedding(Arc::new(CustomEmbedding));
    let ctx = BackendBuildContext {
        sections: Default::default(),
        data_root_directory: std::path::PathBuf::from("/tmp/cognee-test"),
        system_root_directory: std::path::PathBuf::from("/tmp/cognee-test"),
        cache_root_directory: std::path::PathBuf::from("/tmp/cognee-test-cache"),
        relational_db_url: "sqlite::memory:".to_string(),
        graph_provider: String::new(),
        graph_file_path: String::new(),
        graph_postgres_url: None,
        vector_provider: String::new(),
        vector_db_url: String::new(),
        vector_postgres_url: None,
        embedding_dimensions: 384,
        embedding: EmbeddingInputs {
            provider: "my_tei".to_string(),
            rate_limit_enabled: true,
            rate_limit_requests: 7,
            rate_limit_interval: 1,
            ..Default::default()
        },
        llm: LlmInputs::default(),
    };

    let err = match reg.build_embedding(&ctx).await {
        Ok(_) => panic!("the stub factory always fails"),
        Err(e) => e,
    };
    assert!(
        matches!(&err, ComponentError::EmbeddingEngine(m) if m == "custom factory"),
        "build_embedding must dispatch to the custom factory, got: {err:?}"
    );
    assert!(
        cognee_utils::pacing::embedding_pacer().is_some(),
        "build_embedding must install the embedding pacer before dispatching to a custom factory"
    );
}
