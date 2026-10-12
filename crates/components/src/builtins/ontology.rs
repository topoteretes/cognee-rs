//! Built-in ontology resolver factory: RDF (via the rdflib-compatible loader).

use std::sync::Arc;

use async_trait::async_trait;
use cognee_ontology::{FuzzyMatchingStrategy, OntologyResolver, RdfLibOntologyResolver};

use crate::context::BackendBuildContext;
use crate::error::ComponentError;
use crate::sections::OntologyConfig;
use crate::traits::OntologyResolverFactory;

/// Loads OWL/Turtle/RDF ontology files (Python's `RDFLibOntologyResolver`).
pub struct RdfLibOntologyFactory;

#[async_trait]
impl OntologyResolverFactory for RdfLibOntologyFactory {
    fn provider(&self) -> &str {
        "rdflib"
    }

    async fn build(
        &self,
        ctx: &BackendBuildContext,
        file_path: &str,
    ) -> Result<Arc<dyn OntologyResolver>, ComponentError> {
        let strategy = ctx.section::<OntologyConfig>().matching_strategy;
        if !strategy.trim().eq_ignore_ascii_case("fuzzy") {
            tracing::warn!(
                strategy,
                "ontology.matching_strategy: only `fuzzy` is implemented; using it"
            );
        }
        let resolver = RdfLibOntologyResolver::with_strategy(
            file_path,
            Box::new(FuzzyMatchingStrategy::default()),
        )
        .map_err(|e| ComponentError::Ontology(format!("initialization failed: {e}")))?;
        Ok(Arc::new(resolver))
    }
}
