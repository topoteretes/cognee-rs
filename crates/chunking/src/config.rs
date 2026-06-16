//! Chunking configuration — tokenizer selection via environment variables.
//!
//! [`TokenCounterKind`] selects which token counting implementation to use based on
//! environment variables and the active embedding provider. Call [`TokenCounterKind::from_env`]
//! at pipeline construction time to pick the best available counter automatically, then
//! call [`TokenCounterKind::build`] to construct the counter.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::ChunkingError;
use crate::token_counter::{TokenCounter, WordCounter};

/// Selects which token counting implementation to use.
///
/// `from_env()` picks the best available counter based on env vars and the current
/// embedding provider setting. `WordCounter` is the last-resort fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TokenCounterKind {
    /// Accurate BPE/WordPiece via a HuggingFace tokenizer model ID (requires network or cache).
    HuggingFace { model_id: String },
    /// Accurate BPE/WordPiece from a local tokenizer.json file.
    HuggingFaceFile { path: PathBuf },
    /// TikToken cl100k_base BPE (for OpenAI models).
    TikToken,
    /// Whitespace word count. Last-resort fallback.
    Word,
}

impl TokenCounterKind {
    /// Determine the best available token counter from the environment.
    ///
    /// Mirrors Python's `LiteLLMEmbeddingEngine.get_tokenizer()` logic, which selects a
    /// tokenizer based on the provider and stores it on the engine instance. Python's
    /// `chunk_by_sentence()` calls `embedding_engine.tokenizer.count_tokens()` directly —
    /// the tokenizer is a property of the engine, not a separate config. The Rust design
    /// decouples them (`TokenCounterKind` is independent of the engine), but the selection
    /// logic below preserves the same provider → tokenizer mapping.
    ///
    /// Priority order (highest wins):
    /// 1. `COGNEE_TOKEN_COUNTER=tiktoken` → TikToken
    /// 2. `COGNEE_TOKEN_COUNTER=huggingface` or `COGNEE_TOKEN_COUNTER=hf` → check
    ///    `HUGGINGFACE_TOKENIZER`
    /// 3. `HUGGINGFACE_TOKENIZER` env var is set → HuggingFace { model_id }
    /// 4. `EMBEDDING_PROVIDER=onnx` or `fastembed` and `EMBEDDING_TOKENIZER_PATH` is set
    ///    and the file exists → HuggingFaceFile
    /// 5. `EMBEDDING_PROVIDER=openai` or `openai_compatible` → TikToken
    /// 6. `EMBEDDING_PROVIDER=ollama` and `HUGGINGFACE_TOKENIZER` set → HuggingFace
    /// 7. Fallback → Word
    pub fn from_env() -> Self {
        // Priority 1 & 2: explicit COGNEE_TOKEN_COUNTER override
        if let Ok(counter) = std::env::var("COGNEE_TOKEN_COUNTER") {
            match counter.to_lowercase().as_str() {
                "tiktoken" => return TokenCounterKind::TikToken,
                "word" => return TokenCounterKind::Word,
                "huggingface" | "hf" => {
                    if let Ok(model_id) = std::env::var("HUGGINGFACE_TOKENIZER")
                        && !model_id.trim().is_empty()
                    {
                        return TokenCounterKind::HuggingFace { model_id };
                    }
                    // explicit hf requested but no model id — fall through to other priorities
                }
                _ => {}
            }
        }

        // Priority 3: HUGGINGFACE_TOKENIZER set (any provider)
        if let Ok(model_id) = std::env::var("HUGGINGFACE_TOKENIZER")
            && !model_id.trim().is_empty()
        {
            return TokenCounterKind::HuggingFace { model_id };
        }

        // Priority 4–6: based on EMBEDDING_PROVIDER
        // Python's default embedding provider is `openai`, whose default tokenizer is
        // tiktoken cl100k_base. Match that when EMBEDDING_PROVIDER is unset so an
        // out-of-box OpenAI-family setup counts BPE tokens, not whitespace.
        // Users who explicitly set EMBEDDING_PROVIDER=onnx (or point to a tokenizer
        // file via EMBEDDING_TOKENIZER_PATH) get the HuggingFaceFile path as before.
        let provider = std::env::var("EMBEDDING_PROVIDER")
            .unwrap_or_else(|_| "openai".to_string())
            .to_lowercase();

        match provider.as_str() {
            "onnx" | "fastembed" => {
                // Try to reuse the ONNX engine's tokenizer file
                if let Ok(path) = std::env::var("EMBEDDING_TOKENIZER_PATH") {
                    let p = PathBuf::from(&path);
                    if p.exists() {
                        return TokenCounterKind::HuggingFaceFile { path: p };
                    }
                }
                // No tokenizer file available — fall through to Word
                TokenCounterKind::Word
            }
            "openai" | "openai_compatible" => TokenCounterKind::TikToken,
            "ollama" => {
                if let Ok(model_id) = std::env::var("HUGGINGFACE_TOKENIZER")
                    && !model_id.trim().is_empty()
                {
                    return TokenCounterKind::HuggingFace { model_id };
                }
                TokenCounterKind::Word
            }
            _ => TokenCounterKind::Word,
        }
    }

    /// Construct a boxed `TokenCounter` from this kind.
    ///
    /// Returns an error if the selected kind cannot be constructed (e.g. file not found,
    /// model download failed). When the relevant Cargo feature is disabled, silently falls
    /// back to `WordCounter` and logs a warning — so the crate compiles without optional
    /// features but users get a visible signal that their configured tokenizer is inactive.
    pub fn build(self) -> Result<Box<dyn TokenCounter + Send + Sync>, ChunkingError> {
        match self {
            TokenCounterKind::Word => Ok(Box::new(WordCounter)),

            #[cfg(feature = "hf-tokenizer")]
            TokenCounterKind::HuggingFace { model_id } => {
                let counter =
                    crate::token_counter::HuggingFaceTokenCounter::from_pretrained(&model_id)?;
                Ok(Box::new(counter))
            }

            #[cfg(feature = "hf-tokenizer")]
            TokenCounterKind::HuggingFaceFile { path } => {
                let counter = crate::token_counter::HuggingFaceTokenCounter::from_file(path)?;
                Ok(Box::new(counter))
            }

            #[cfg(feature = "tiktoken")]
            TokenCounterKind::TikToken => {
                let counter = crate::token_counter::TikTokenCounter::cl100k_base()?;
                Ok(Box::new(counter))
            }

            // When the relevant feature is disabled, fall back to Word with a warning.
            // This keeps the crate usable without optional features while signalling to
            // the user that their configured tokenizer is not active.
            #[cfg(not(feature = "hf-tokenizer"))]
            TokenCounterKind::HuggingFace { model_id: _ } => {
                eprintln!(
                    "cognee-chunking: HuggingFace tokenizer requested but `hf-tokenizer` \
                     feature is not enabled — falling back to WordCounter"
                );
                Ok(Box::new(WordCounter))
            }

            #[cfg(not(feature = "hf-tokenizer"))]
            TokenCounterKind::HuggingFaceFile { path: _ } => {
                eprintln!(
                    "cognee-chunking: HuggingFaceFile tokenizer requested but `hf-tokenizer` \
                     feature is not enabled — falling back to WordCounter"
                );
                Ok(Box::new(WordCounter))
            }

            #[cfg(not(feature = "tiktoken"))]
            TokenCounterKind::TikToken => {
                eprintln!(
                    "cognee-chunking: TikToken tokenizer requested but `tiktoken` feature is \
                     not enabled — falling back to WordCounter"
                );
                Ok(Box::new(WordCounter))
            }
        }
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

    /// When no env vars are set the default provider is treated as `openai`, which maps
    /// to `TikToken` — matching Python's out-of-box cl100k_base tokenizer.
    ///
    /// # Safety
    /// `std::env::remove_var` is marked `unsafe` in edition 2024.  Tests run
    /// single-threaded under the project harness (`--test-threads=1`), so there
    /// are no concurrent readers of the modified env vars.
    #[test]
    fn from_env_defaults_to_tiktoken_for_openai_family() {
        unsafe {
            std::env::remove_var("EMBEDDING_PROVIDER");
            std::env::remove_var("COGNEE_TOKEN_COUNTER");
            std::env::remove_var("HUGGINGFACE_TOKENIZER");
            std::env::remove_var("EMBEDDING_TOKENIZER_PATH");
        }
        assert!(matches!(
            TokenCounterKind::from_env(),
            TokenCounterKind::TikToken
        ));
    }

    /// Explicitly setting EMBEDDING_PROVIDER=onnx still falls back to Word when
    /// no tokenizer file is available (existing ONNX-user behaviour is unchanged).
    #[test]
    fn from_env_onnx_without_tokenizer_falls_back_to_word() {
        unsafe {
            std::env::set_var("EMBEDDING_PROVIDER", "onnx");
            std::env::remove_var("COGNEE_TOKEN_COUNTER");
            std::env::remove_var("HUGGINGFACE_TOKENIZER");
            std::env::remove_var("EMBEDDING_TOKENIZER_PATH");
        }
        assert!(matches!(
            TokenCounterKind::from_env(),
            TokenCounterKind::Word
        ));
        // Restore
        unsafe { std::env::remove_var("EMBEDDING_PROVIDER") };
    }

    #[test]
    fn word_variant_builds() {
        let counter = TokenCounterKind::Word.build();
        assert!(counter.is_ok());
        let counter = counter.unwrap();
        assert_eq!(counter.count_tokens("hello world"), 2);
    }

    #[test]
    fn word_variant_builds_empty() {
        let counter = TokenCounterKind::Word.build().unwrap();
        assert_eq!(counter.count_tokens(""), 0);
    }

    #[test]
    #[cfg(feature = "tiktoken")]
    fn tiktoken_variant_builds() {
        let counter = TokenCounterKind::TikToken.build();
        assert!(counter.is_ok());
    }

    #[test]
    #[cfg(not(feature = "hf-tokenizer"))]
    fn hf_falls_back_without_feature() {
        let counter = TokenCounterKind::HuggingFace {
            model_id: "bert-base-uncased".to_string(),
        }
        .build();
        assert!(counter.is_ok(), "should fall back to WordCounter");
        let counter = counter.unwrap();
        assert_eq!(counter.count_tokens("hello world"), 2);
    }

    #[test]
    #[cfg(not(feature = "tiktoken"))]
    fn tiktoken_falls_back_without_feature() {
        let counter = TokenCounterKind::TikToken.build();
        assert!(counter.is_ok(), "should fall back to WordCounter");
        let counter = counter.unwrap();
        assert_eq!(counter.count_tokens("hello world"), 2);
    }
}
