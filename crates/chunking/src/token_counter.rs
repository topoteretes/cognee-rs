/// Trait for counting tokens in text. Allows swapping word count for a real
/// tokenizer (e.g. HuggingFace tokenizers) later.
pub trait TokenCounter {
    fn count_tokens(&self, text: &str) -> usize;
}

/// Simple token counter that splits on whitespace and counts words.
#[derive(Debug, Clone, Default)]
pub struct WordCounter;

impl TokenCounter for WordCounter {
    fn count_tokens(&self, text: &str) -> usize {
        text.split_whitespace().count()
    }
}

#[cfg(feature = "hf-tokenizer")]
use std::{path::Path, sync::Arc};
#[cfg(feature = "hf-tokenizer")]
use crate::error::ChunkingError;

/// Token counter backed by a HuggingFace `tokenizers` tokenizer.
///
/// Drop-in replacement for `WordCounter` when accurate BPE/WordPiece token counts are needed.
/// Use when chunking for models that use HuggingFace tokenizers (BGE, MiniLM, etc.).
#[cfg(feature = "hf-tokenizer")]
pub struct HuggingFaceTokenCounter {
    tokenizer: Arc<tokenizers::Tokenizer>,
}

#[cfg(feature = "hf-tokenizer")]
impl HuggingFaceTokenCounter {
    /// Load from a local `tokenizer.json` file.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ChunkingError> {
        let tokenizer = tokenizers::Tokenizer::from_file(path)
            .map_err(|e| ChunkingError::TokenizerError(e.to_string()))?;
        Ok(Self {
            tokenizer: Arc::new(tokenizer),
        })
    }

    /// Load from a HuggingFace model ID (requires network access).
    /// Caches locally in the HuggingFace cache directory.
    pub fn from_pretrained(model_id: &str) -> Result<Self, ChunkingError> {
        let tokenizer = tokenizers::Tokenizer::from_pretrained(model_id, None)
            .map_err(|e: tokenizers::Error| ChunkingError::TokenizerError(e.to_string()))?;
        Ok(Self {
            tokenizer: Arc::new(tokenizer),
        })
    }
}

#[cfg(feature = "hf-tokenizer")]
impl TokenCounter for HuggingFaceTokenCounter {
    fn count_tokens(&self, text: &str) -> usize {
        self.tokenizer
            .encode(text, false)
            .map(|enc| enc.len())
            .unwrap_or_else(|_| text.split_whitespace().count()) // fallback on encode error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_counter_empty() {
        assert_eq!(WordCounter.count_tokens(""), 0);
    }

    #[test]
    fn word_counter_whitespace_only() {
        assert_eq!(WordCounter.count_tokens("   \n\t  "), 0);
    }

    #[test]
    fn word_counter_simple() {
        assert_eq!(WordCounter.count_tokens("hello world"), 2);
    }

    #[test]
    fn word_counter_punctuation() {
        assert_eq!(WordCounter.count_tokens("Hello, world! How are you?"), 5);
    }
}

#[cfg(all(test, feature = "hf-tokenizer"))]
mod hf_tests {
    use super::*;

    #[test]
    fn test_from_file_nonexistent() {
        let result = HuggingFaceTokenCounter::from_file("/nonexistent/tokenizer.json");
        assert!(result.is_err());
    }
}
