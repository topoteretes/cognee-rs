use serde::{Deserialize, Serialize};

/// Selects the backend used to generate embeddings.
///
/// Matches the Python SDK `EMBEDDING_PROVIDER` env var values so that a shared
/// `.env` file works without modification across both SDKs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingProvider {
    /// Local ONNX inference (existing engine). Equivalent to Python's "fastembed" provider.
    #[default]
    Onnx,
    /// Alias for Onnx — accepts Python env files using EMBEDDING_PROVIDER=fastembed unchanged.
    /// Deserialized as Onnx internally.
    #[serde(alias = "fastembed")]
    Fastembed,
    /// OpenAI API or any OpenAI-compatible server (llama.cpp, vLLM, TEI, etc.).
    /// In Python, EMBEDDING_PROVIDER=openai uses LiteLLMEmbeddingEngine; in Rust we use a
    /// direct-HTTP engine. Both "openai" and "openai_compatible" map to the same engine.
    #[serde(rename = "openai")]
    OpenAi,
    /// Explicit alias for self-hosted OpenAI-compatible servers.
    /// Identical engine to OpenAi; exists so Python "openai_compatible" env files work unchanged.
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    /// Ollama /api/embed endpoint (not OpenAI-compatible format).
    Ollama,
    /// Zero vectors; for testing. Activated by EMBEDDING_PROVIDER=mock or MOCK_EMBEDDING=true.
    Mock,
}
