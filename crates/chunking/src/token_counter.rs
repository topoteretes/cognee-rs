use serde::{Deserialize, Serialize};

/// How a chunk's token size is measured.
///
/// Sub-word tokenizers are not additive over a partition of the text: cl100k
/// encodes `" the"` as a single token, but the two pieces `"the"` and `" "` as
/// two. `chunk_by_word` hands back words with their trailing space attached, so
/// asking the tokenizer for each word *in isolation* and summing systematically
/// over-counts the span those words compose — MEASURED at 1.61-2.07x across the
/// committed corpora and 1.78x over Alice in Wonderland, with cl100k. The
/// fullest chunk of an 8191-token budget then held 4,669 real tokens where the
/// span count fills it to 8,188, so a run made ~1.8x the LLM calls it was
/// configured for. Reproduce with the `measure_token_overcount` example.
///
/// [`TokenCountMode::Span`] is the default: the accumulated span is counted
/// once, so the size a chunk reports is the size the tokenizer agrees it has.
///
/// [`TokenCountMode::PerWord`] is the historical behaviour, kept because it is
/// what Python 1.5.x still does (`get_word_size(word)` per word, from
/// `cognee/tasks/chunks/chunk_by_sentence.py`). Selecting it restores byte-level
/// chunk-boundary parity with Python at the cost of the over-count. It is
/// opt-in and off by default; see [`TokenCountMode::from_env`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenCountMode {
    /// Count the accumulated span once. Correct, and the default.
    #[default]
    Span,
    /// Sum the tokenizer's count of every word in isolation. Legacy; over-counts.
    PerWord,
}

impl TokenCountMode {
    /// Environment variable that opts back in to the legacy per-word count.
    pub const ENV_VAR: &'static str = "COGNEE_LEGACY_PER_WORD_TOKEN_COUNT";

    /// Read the mode from the environment.
    ///
    /// Returns [`TokenCountMode::PerWord`] only when `COGNEE_LEGACY_PER_WORD_TOKEN_COUNT`
    /// is set to a truthy value (`1`, `true`, `yes`, `on`, case-insensitive).
    /// Anything else — unset, empty, `0`, or unrecognised — is
    /// [`TokenCountMode::Span`], so the correct count is what an unconfigured
    /// deployment gets.
    #[must_use]
    #[allow(
        clippy::disallowed_methods,
        reason = "config-unification: legacy direct env read, to be moved behind a config boundary"
    )]
    pub fn from_env() -> Self {
        match std::env::var(Self::ENV_VAR) {
            Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => TokenCountMode::PerWord,
                _ => TokenCountMode::Span,
            },
            Err(_) => TokenCountMode::Span,
        }
    }
}

/// Trait for counting tokens in text. Allows swapping word count for a real
/// tokenizer (e.g. HuggingFace tokenizers) later.
pub trait TokenCounter {
    fn count_tokens(&self, text: &str) -> usize;
}

/// Blanket implementation so `Box<dyn TokenCounter + Send + Sync>` can be passed
/// to functions that accept `impl TokenCounter` (like `chunk_text`).
impl<T: TokenCounter + ?Sized> TokenCounter for Box<T> {
    fn count_tokens(&self, text: &str) -> usize {
        (**self).count_tokens(text)
    }
}

/// Blanket implementation so `&dyn TokenCounter` can be used anywhere `TokenCounter` is required.
impl<T: TokenCounter + ?Sized> TokenCounter for &T {
    fn count_tokens(&self, text: &str) -> usize {
        (*self).count_tokens(text)
    }
}

/// Simple token counter that splits on whitespace and counts words.
#[derive(Debug, Clone, Default)]
pub struct WordCounter;

impl TokenCounter for WordCounter {
    fn count_tokens(&self, text: &str) -> usize {
        text.split_whitespace().count()
    }
}

#[cfg(any(feature = "hf-tokenizer", feature = "tiktoken"))]
use crate::error::ChunkingError;
#[cfg(feature = "hf-tokenizer")]
use std::{path::Path, sync::Arc};

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

/// Token counter using TikToken BPE encoding (cl100k_base).
///
/// Use when chunking for OpenAI models (text-embedding-3-large, GPT-4, etc.).
/// Matches Python's TikTokenTokenizer with cl100k_base encoding.
#[cfg(feature = "tiktoken")]
pub struct TikTokenCounter {
    bpe: tiktoken_rs::CoreBPE,
}

#[cfg(feature = "tiktoken")]
impl TikTokenCounter {
    /// Create with cl100k_base encoding (matches GPT-4, text-embedding-3-large).
    pub fn cl100k_base() -> Result<Self, ChunkingError> {
        let bpe =
            tiktoken_rs::cl100k_base().map_err(|e| ChunkingError::TokenizerError(e.to_string()))?;
        Ok(Self { bpe })
    }
}

#[cfg(feature = "tiktoken")]
impl TokenCounter for TikTokenCounter {
    fn count_tokens(&self, text: &str) -> usize {
        self.bpe.encode_with_special_tokens(text).len()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
#[allow(
    clippy::disallowed_methods,
    reason = "tests drive configuration through the process env"
)]
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

    /// The correct count is what an unconfigured deployment gets. Only an
    /// explicit, truthy opt-in brings the legacy over-count back.
    ///
    /// # Safety
    /// `std::env::set_var` / `remove_var` are `unsafe` in edition 2024. Tests
    /// run single-threaded under the project harness (`--test-threads=1`), so
    /// there are no concurrent readers of the modified variable.
    #[test]
    fn legacy_per_word_counting_is_opt_in() {
        unsafe { std::env::remove_var(TokenCountMode::ENV_VAR) };
        assert_eq!(TokenCountMode::from_env(), TokenCountMode::Span);
        assert_eq!(TokenCountMode::default(), TokenCountMode::Span);

        for truthy in ["1", "true", "TRUE", "Yes", " on "] {
            unsafe { std::env::set_var(TokenCountMode::ENV_VAR, truthy) };
            assert_eq!(
                TokenCountMode::from_env(),
                TokenCountMode::PerWord,
                "{truthy:?} should opt in to the legacy count",
            );
        }

        // Anything unrecognised means the default, not the legacy behaviour:
        // a typo must not silently halve the chunk size.
        for falsy in ["0", "false", "no", "", "off", "maybe"] {
            unsafe { std::env::set_var(TokenCountMode::ENV_VAR, falsy) };
            assert_eq!(
                TokenCountMode::from_env(),
                TokenCountMode::Span,
                "{falsy:?} should leave the default in place",
            );
        }

        unsafe { std::env::remove_var(TokenCountMode::ENV_VAR) };
    }
}

#[cfg(all(test, feature = "hf-tokenizer"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod hf_tests {
    use super::*;

    #[test]
    fn test_from_file_nonexistent() {
        let result = HuggingFaceTokenCounter::from_file("/nonexistent/tokenizer.json");
        assert!(result.is_err());
    }
}

#[cfg(all(test, feature = "tiktoken"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tiktoken_tests {
    use super::*;

    #[test]
    fn cl100k_base_constructs() {
        let counter = TikTokenCounter::cl100k_base();
        assert!(counter.is_ok());
    }

    #[test]
    fn counts_known_text() {
        let counter = TikTokenCounter::cl100k_base().expect("cl100k_base should load");
        // "Hello, world!" is 4 tokens in cl100k_base
        let count = counter.count_tokens("Hello, world!");
        assert!(count > 0);
        // verify it's in reasonable range (3-6 tokens for this string)
        assert!((3..=6).contains(&count), "Expected 3-6 tokens, got {count}");
    }

    #[test]
    fn empty_string_is_zero_tokens() {
        let counter = TikTokenCounter::cl100k_base().expect("cl100k_base should load");
        assert_eq!(counter.count_tokens(""), 0);
    }
}
