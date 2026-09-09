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
            // Titan's tokenizer is not published, so no counter here is exact.
            // cl100k BPE is the closest available proxy: both are byte-level BPE
            // over a ~100k vocabulary and agree within a few percent on prose,
            // where whitespace counting is off by ~35% and in the wrong unit
            // entirely. `fit_token_budget` absorbs the residual difference.
            "bedrock" => TokenCounterKind::TikToken,
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

    /// What this kind will ACTUALLY be at runtime, given the compiled features.
    ///
    /// `build()` falls back to `WordCounter` when the feature backing the
    /// requested counter is not compiled in, and says so only on stderr. Callers
    /// that size a budget by unit therefore cannot trust the requested kind:
    /// asking for `TikToken` in an image built without the `tiktoken` feature
    /// yields whitespace counting, and any token budget handed to it is spent in
    /// the wrong unit.
    ///
    /// That is not hypothetical — it shipped. A build requested TikToken for
    /// Bedrock, degraded silently to `WordCounter`, and the resulting 8191-word
    /// chunks measured ~11149 real tokens against an 8192-token embedder limit,
    /// failing every embedding call with HTTP 400.
    fn effective(&self) -> TokenCounterKind {
        match self {
            TokenCounterKind::Word => TokenCounterKind::Word,
            TokenCounterKind::HuggingFace { .. } | TokenCounterKind::HuggingFaceFile { .. } => {
                #[cfg(feature = "hf-tokenizer")]
                {
                    self.clone()
                }
                #[cfg(not(feature = "hf-tokenizer"))]
                {
                    TokenCounterKind::Word
                }
            }
            TokenCounterKind::TikToken => {
                #[cfg(feature = "tiktoken")]
                {
                    TokenCounterKind::TikToken
                }
                #[cfg(not(feature = "tiktoken"))]
                {
                    TokenCounterKind::Word
                }
            }
        }
    }

    /// Convert a TOKEN budget into whatever unit this counter actually measures.
    ///
    /// The single entry point callers need: it resolves the counter that will
    /// really run (a BPE counter degrades to `WordCounter` when its cargo
    /// feature is absent) and converts the budget for it. There is deliberately
    /// no way to size a budget against a counter that will not run.
    ///
    /// `auto_chunk_size`-style budgets are derived from an embedding model's
    /// TOKEN limit, so handing one to a word counter overshoots that limit by
    /// construction: 8191 spent as words measured ~11100 real tokens and every
    /// Bedrock embedding call failed with "Too many input tokens. Max input
    /// tokens: 8192".
    ///
    /// `WORD_TOKENS_UPPER_BOUND` is deliberately pessimistic. English prose runs
    /// ~1.3 tokens/word, the observed ratio on the corpus that produced the bug
    /// was 1.36, and dense or punctuation-heavy text goes higher. Undershooting
    /// costs a few more chunks; overshooting costs the whole run.
    #[must_use]
    pub fn fit_token_budget(&self, token_budget: usize) -> usize {
        /// Pessimistic tokens-per-word, scaled by 100 to stay in integer maths.
        const WORD_TOKENS_UPPER_BOUND: usize = 150;
        match self.effective() {
            // Already BPE/WordPiece: the budget is in the right unit. Exactness
            // against a *different* BPE vocabulary is covered by the caller's own
            // headroom, not by this conversion.
            TokenCounterKind::HuggingFace { .. }
            | TokenCounterKind::HuggingFaceFile { .. }
            | TokenCounterKind::TikToken => token_budget,
            TokenCounterKind::Word => (token_budget * 100 / WORD_TOKENS_UPPER_BOUND).max(1),
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
                tracing::warn!(
                    "cognee-chunking: HuggingFace tokenizer requested but `hf-tokenizer` \
                     feature is not enabled — falling back to WordCounter. A token budget \
                     spent in words undercounts real tokens by ~35%, which can \
                     exceed an embedding model's input cap."
                );
                Ok(Box::new(WordCounter))
            }

            #[cfg(not(feature = "hf-tokenizer"))]
            TokenCounterKind::HuggingFaceFile { path: _ } => {
                tracing::warn!(
                    "cognee-chunking: HuggingFaceFile tokenizer requested but `hf-tokenizer` \
                     feature is not enabled — falling back to WordCounter. A token budget \
                     spent in words undercounts real tokens by ~35%, which can \
                     exceed an embedding model's input cap."
                );
                Ok(Box::new(WordCounter))
            }

            #[cfg(not(feature = "tiktoken"))]
            TokenCounterKind::TikToken => {
                tracing::warn!(
                    "cognee-chunking: TikToken tokenizer requested but `tiktoken` feature is \
                     not enabled — falling back to WordCounter. A token budget spent \
                     in words undercounts real tokens by ~35%, which can exceed an \
                     embedding model's input cap."
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
    /// Bedrock must not fall through to whitespace counting. `auto_chunk_size`
    /// derives its budget from the embedding model's TOKEN limit, so a word
    /// counter spends it in the wrong unit — 8191 words measured ~11100 Titan
    /// tokens and every embed call 400d against Titan v2's 8192 cap.
    ///
    /// # Safety
    /// Same as the sibling tests: env mutation under a single-threaded harness.
    #[test]
    fn bedrock_gets_a_bpe_counter_not_whitespace() {
        unsafe {
            std::env::remove_var("COGNEE_TOKEN_COUNTER");
            std::env::remove_var("HUGGINGFACE_TOKENIZER");
            std::env::remove_var("EMBEDDING_TOKENIZER_PATH");
            std::env::set_var("EMBEDDING_PROVIDER", "bedrock");
        }
        let kind = TokenCounterKind::from_env();
        unsafe { std::env::remove_var("EMBEDDING_PROVIDER") };
        assert!(
            matches!(kind, TokenCounterKind::TikToken),
            "bedrock must count BPE tokens, not whitespace words: {kind:?}",
        );
    }

    /// A token budget must be converted before a word counter spends it.
    ///
    /// This is the bug that produced `Too many input tokens. Max input tokens:
    /// 8192, request input token count: 11100` on Bedrock: the budget is the
    /// embedding model's TOKEN limit, the chunker measured whitespace, and 8191
    /// words is ~11100 tokens. An auto-derived budget could only ever exceed the
    /// limit it was derived from.
    #[test]
    fn a_token_budget_is_converted_before_a_word_counter_spends_it() {
        // Word counting must be scaled down, and far enough that the observed
        // 1.36 tokens/word on real prose still lands under the limit.
        let words = TokenCounterKind::Word.fit_token_budget(8191);
        assert!(
            words < 8191,
            "a word budget must be smaller than the token budget, got {words}",
        );
        let worst_case_tokens = (words as f64 * 1.36) as usize;
        assert!(
            worst_case_tokens <= 8191,
            "{words} words is ~{worst_case_tokens} tokens at the observed ratio, \
             which must not exceed the 8191-token budget it came from",
        );

        // Never zero, however small the budget.
        assert!(TokenCounterKind::Word.fit_token_budget(1) >= 1);
    }

    /// Sizing must follow the counter that will ACTUALLY run, not the one that
    /// was requested. `build()` degrades a BPE counter to `WordCounter` when its
    /// cargo feature is absent, so a request for TikToken in a build without the
    /// `tiktoken` feature means whitespace counting.
    ///
    /// This shipped: an image requested TikToken for Bedrock, silently got
    /// WordCounter, and 8191 "tokens" became 8191 words ~= 11149 real tokens
    /// against an 8192-token embedder cap — HTTP 400 on every embedding call.
    #[test]
    fn sizing_follows_the_effective_counter_not_the_requested_one() {
        let sized = TokenCounterKind::TikToken.fit_token_budget(8191);
        if cfg!(feature = "tiktoken") {
            assert_eq!(sized, 8191, "a real BPE counter spends the budget as-is");
        } else {
            assert!(
                sized < 8191,
                "a counter that degraded to whitespace must still get a converted \
                 budget, got {sized}",
            );
        }
    }

    /// A budget already in BPE units passes through untouched.
    #[test]
    fn a_bpe_counter_receives_the_token_budget_unchanged() {
        let hf = TokenCounterKind::HuggingFace {
            model_id: "bert-base-uncased".to_string(),
        };
        assert_eq!(
            hf.fit_token_budget(8191),
            if cfg!(feature = "hf-tokenizer") {
                8191
            } else {
                5460
            },
            "a HuggingFace request keeps the token budget only when compiled in",
        );
    }
}
