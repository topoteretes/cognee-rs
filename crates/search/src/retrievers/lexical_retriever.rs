//! Lexical (token-overlap) chunk retrieval over the graph's `DocumentChunk`
//! nodes: Okapi BM25 for `CHUNKS_LEXICAL`, set/multiset Jaccard on request.
//!
//! Port of Python's `LexicalRetriever` family (`lexical_retriever.py`,
//! `bm25_retriever.py`, `jaccard_retrival.py`).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use cognee_graph::GraphDBTrait;
use cognee_session::SessionContext;
use regex::Regex;
use serde_json::{Value, json};

use crate::retrievers::SearchRetriever;
use crate::types::{
    SearchContext, SearchError, SearchItem, SearchOutput, SearchParams, SearchType,
};
use crate::utils::DEFAULT_STOP_WORDS;

const DEFAULT_TOP_K: usize = 10;
const DOCUMENT_CHUNK_TYPE: &str = "DocumentChunk";

/// BM25 term-frequency saturation (`bm25_retriever.py:23`).
pub const BM25_DEFAULT_K1: f32 = 1.5;
/// BM25 length normalization (`bm25_retriever.py:24`).
pub const BM25_DEFAULT_B: f32 = 0.75;

/// How a [`LexicalRetriever`] scores a chunk against the query.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LexicalScorer {
    /// Okapi BM25 with corpus IDF and average chunk length — Python's
    /// `BM25ChunksRetriever`, which backs `CHUNKS_LEXICAL`.
    Bm25 { k1: f32, b: f32 },
    /// Set Jaccard, or frequency-aware multiset Jaccard — Python's
    /// `JaccardChunksRetriever`.
    Jaccard { multiset: bool },
}

/// A `DocumentChunk` node as one search loaded it.
struct LoadedChunk {
    id: Option<uuid::Uuid>,
    payload: serde_json::Value,
    text: String,
}

/// The tokenized corpus, with the BM25 statistics computed once from it
/// (`bm25_retriever.py:_build_corpus_stats`).
///
/// Built from one load's chunk list and reused while a later load fingerprints
/// the same: tokenizing every chunk and rebuilding the IDF table is the
/// expensive part of a lexical search, and it only changes when the chunks do.
struct Corpus {
    /// Indices into the load's chunk list of the chunks that produced tokens.
    kept: Vec<usize>,
    /// Tokens of each kept chunk, aligned with `kept`.
    tokens: Vec<Vec<String>>,
    /// BM25 statistics; empty for a Jaccard scorer, which never reads them.
    idf: HashMap<String, f32>,
    avg_chunk_length: f32,
}

impl Corpus {
    fn new(kept: Vec<usize>, tokens: Vec<Vec<String>>, with_bm25_stats: bool) -> Self {
        let mut corpus = Self {
            kept,
            tokens,
            idf: HashMap::new(),
            avg_chunk_length: 0.0,
        };
        if with_bm25_stats {
            corpus.build_bm25_stats();
        }
        corpus
    }

    fn build_bm25_stats(&mut self) {
        let total_chunks = self.tokens.len() as f32;
        let mut document_frequency: HashMap<&str, usize> = HashMap::new();
        let mut total_length = 0usize;
        for tokens in &self.tokens {
            total_length += tokens.len();
            let unique: HashSet<&str> = tokens.iter().map(String::as_str).collect();
            for token in unique {
                *document_frequency.entry(token).or_default() += 1;
            }
        }
        self.idf = document_frequency
            .into_iter()
            .map(|(token, df)| {
                let df = df as f32;
                (
                    token.to_string(),
                    (1.0 + (total_chunks - df + 0.5) / (df + 0.5)).ln(),
                )
            })
            .collect();
        self.avg_chunk_length = if self.tokens.is_empty() {
            0.0
        } else {
            total_length as f32 / total_chunks
        };
    }

    /// Okapi BM25 of one chunk, summed over the unique query terms
    /// (`bm25_retriever.py:_scorer`).
    fn bm25(&self, query_tokens: &[String], chunk_tokens: &[String], k1: f32, b: f32) -> f32 {
        if query_tokens.is_empty() || chunk_tokens.is_empty() || self.avg_chunk_length == 0.0 {
            return 0.0;
        }

        let mut term_frequencies: HashMap<&str, usize> = HashMap::new();
        for token in chunk_tokens {
            *term_frequencies.entry(token.as_str()).or_default() += 1;
        }
        let length_norm = k1 * (1.0 - b + b * chunk_tokens.len() as f32 / self.avg_chunk_length);

        let unique_query: HashSet<&str> = query_tokens.iter().map(String::as_str).collect();
        unique_query
            .into_iter()
            .filter_map(|token| {
                let tf = *term_frequencies.get(token)? as f32;
                let idf = self.idf.get(token).copied().unwrap_or(0.0);
                Some(idf * (tf * (k1 + 1.0)) / (tf + length_norm))
            })
            .sum()
    }
}

/// Fingerprint of a chunk list: ids and texts, in load order. Equal
/// fingerprints mean the cached tokens and statistics still describe it.
fn fingerprint(chunks: &[LoadedChunk]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    chunks.len().hash(&mut hasher);
    for chunk in chunks {
        chunk.id.hash(&mut hasher);
        chunk.text.hash(&mut hasher);
    }
    hasher.finish()
}

/// Python's `\w` over `str`: a character `str.isalnum()` accepts — letters
/// (`L*`) and numbers (`N*`) — or the underscore. Unlike Rust's
/// `char::is_alphanumeric`, this excludes combining marks such as Devanagari
/// vowel signs, which Python's `\w` does not match.
#[allow(
    clippy::expect_used,
    reason = "a constant pattern that compiles; the word-pattern test exercises it"
)]
static WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}_]+").expect("the word pattern is a valid regex"));

pub struct LexicalRetriever {
    graph_db: Arc<dyn GraphDBTrait>,
    top_k: usize,
    with_scores: bool,
    stop_words: HashSet<String>,
    scorer: LexicalScorer,
    /// The last corpus built, keyed by the fingerprint of the chunk list it
    /// was built from.
    corpus_cache: Mutex<Option<(u64, Arc<Corpus>)>>,
}

impl LexicalRetriever {
    /// A Jaccard retriever (Python's `JaccardChunksRetriever`): no stop words
    /// unless given, set or multiset Jaccard.
    pub fn new(
        graph_db: Arc<dyn GraphDBTrait>,
        top_k: Option<usize>,
        with_scores: bool,
        stop_words: Option<Vec<String>>,
        multiset_jaccard: bool,
    ) -> Self {
        Self {
            graph_db,
            top_k: top_k.unwrap_or(DEFAULT_TOP_K),
            with_scores,
            stop_words: lowercase_set(stop_words.unwrap_or_default()),
            scorer: LexicalScorer::Jaccard {
                multiset: multiset_jaccard,
            },
            corpus_cache: Mutex::new(None),
        }
    }

    /// A BM25 retriever (Python's `BM25ChunksRetriever`, `CHUNKS_LEXICAL`):
    /// `k1 = 1.5`, `b = 0.75`, and the default English stop words unless
    /// `stop_words` is given (an empty list disables filtering).
    pub fn bm25(
        graph_db: Arc<dyn GraphDBTrait>,
        top_k: Option<usize>,
        with_scores: bool,
        stop_words: Option<Vec<String>>,
    ) -> Self {
        Self {
            graph_db,
            top_k: top_k.unwrap_or(DEFAULT_TOP_K),
            with_scores,
            stop_words: match stop_words {
                Some(words) => lowercase_set(words),
                None => DEFAULT_STOP_WORDS.iter().map(|w| w.to_string()).collect(),
            },
            scorer: LexicalScorer::Bm25 {
                k1: BM25_DEFAULT_K1,
                b: BM25_DEFAULT_B,
            },
            corpus_cache: Mutex::new(None),
        }
    }

    /// The scoring function this retriever ranks with.
    pub fn scorer(&self) -> LexicalScorer {
        self.scorer
    }

    /// Lowercase, split on word characters, drop stop words — Python's
    /// `tokenize_words`: `re.findall(r"\w+", text.lower())`. The whole text is
    /// lowercased first, as `str.lower()` does, so context-dependent mappings
    /// (a word-final `Σ` becomes `ς`) match Python.
    fn tokenize(&self, text: &str) -> Vec<String> {
        let lowered = text.to_lowercase();
        WORD.find_iter(&lowered)
            .map(|word| word.as_str())
            .filter(|word| !self.stop_words.contains(*word))
            .map(str::to_string)
            .collect()
    }

    fn score(&self, corpus: &Corpus, query_tokens: &[String], chunk_tokens: &[String]) -> f32 {
        match self.scorer {
            LexicalScorer::Bm25 { k1, b } => corpus.bm25(query_tokens, chunk_tokens, k1, b),
            LexicalScorer::Jaccard { multiset } => jaccard(query_tokens, chunk_tokens, multiset),
        }
    }

    async fn load_document_chunks(&self) -> Result<Vec<LoadedChunk>, SearchError> {
        let filters = HashMap::from([(Cow::Borrowed("type"), vec![json!(DOCUMENT_CHUNK_TYPE)])]);

        let (nodes, _) = self.graph_db.get_filtered_graph_data(&filters).await?;
        let mut chunks = Vec::new();

        for (node_id, node_data) in nodes {
            let node_type = node_data
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if node_type != DOCUMENT_CHUNK_TYPE {
                continue;
            }

            let Some(text) = node_data.get("text").and_then(Value::as_str) else {
                continue;
            };

            let payload = serde_json::to_value(&node_data)?;

            let id = node_data
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .or_else(|| uuid::Uuid::parse_str(&node_id).ok());

            chunks.push(LoadedChunk {
                id,
                payload,
                text: text.to_string(),
            });
        }

        Ok(chunks)
    }

    /// Load the chunks and the tokenized corpus over them.
    ///
    /// The chunks are read on every search: Python builds a fresh retriever
    /// per search (`get_search_type_retriever_instance.py`), while this one
    /// lives as long as its orchestrator — a binding handle's lifetime — and
    /// the graph exposes no change signal, so a cached chunk list would keep
    /// serving the chunks from before a later `add` + `cognify`. The expensive
    /// part — tokenizing and the BM25 statistics — is cached and reused while
    /// the chunk list's fingerprint is unchanged.
    ///
    /// No usable chunk is Python's `NoDataError` (`lexical_retriever.py:90-92`).
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    async fn load_corpus(&self) -> Result<(Vec<LoadedChunk>, Arc<Corpus>), SearchError> {
        let chunks = self.load_document_chunks().await?;
        let key = fingerprint(&chunks);

        let cached = self
            .corpus_cache
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(cached_key, _)| *cached_key == key)
            .map(|(_, corpus)| Arc::clone(corpus));
        let corpus = match cached {
            Some(corpus) => corpus,
            None => {
                let (kept, tokens): (Vec<usize>, Vec<Vec<String>>) = chunks
                    .iter()
                    .enumerate()
                    .filter_map(|(index, chunk)| {
                        let tokens = self.tokenize(&chunk.text);
                        (!tokens.is_empty()).then_some((index, tokens))
                    })
                    .unzip();
                let corpus = Arc::new(Corpus::new(
                    kept,
                    tokens,
                    matches!(self.scorer, LexicalScorer::Bm25 { .. }),
                ));
                *self.corpus_cache.lock().unwrap() = Some((key, Arc::clone(&corpus)));
                corpus
            }
        };

        if corpus.kept.is_empty() {
            return Err(SearchError::NotFound(
                "No valid chunks loaded during initialization.".to_string(),
            ));
        }
        Ok((chunks, corpus))
    }
}

fn lowercase_set(words: Vec<String>) -> HashSet<String> {
    words.into_iter().map(|word| word.to_lowercase()).collect()
}

/// Set or multiset Jaccard (`jaccard_retrival.py:_scorer`).
fn jaccard(query_tokens: &[String], chunk_tokens: &[String], multiset: bool) -> f32 {
    if query_tokens.is_empty() || chunk_tokens.is_empty() {
        return 0.0;
    }

    if multiset {
        let mut query_counts = HashMap::new();
        for token in query_tokens {
            *query_counts.entry(token).or_insert(0usize) += 1;
        }

        let mut chunk_counts = HashMap::new();
        for token in chunk_tokens {
            *chunk_counts.entry(token).or_insert(0usize) += 1;
        }

        let universe: HashSet<&String> = query_counts
            .keys()
            .chain(chunk_counts.keys())
            .copied()
            .collect();

        let (numerator, denominator) =
            universe.iter().fold((0usize, 0usize), |(num, den), token| {
                let q = query_counts.get(*token).copied().unwrap_or_default();
                let c = chunk_counts.get(*token).copied().unwrap_or_default();
                (num + q.min(c), den + q.max(c))
            });

        if denominator == 0 {
            0.0
        } else {
            numerator as f32 / denominator as f32
        }
    } else {
        let query_set: HashSet<&String> = query_tokens.iter().collect();
        let chunk_set: HashSet<&String> = chunk_tokens.iter().collect();
        let intersection_size = query_set.intersection(&chunk_set).count();
        let union_size = query_set.union(&chunk_set).count();

        if union_size == 0 {
            0.0
        } else {
            intersection_size as f32 / union_size as f32
        }
    }
}

#[async_trait]
impl SearchRetriever for LexicalRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::ChunksLexical
    }

    fn supports_session_turn_preparation(&self) -> bool {
        false
    }

    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        let (chunks, corpus) = self.load_corpus().await?;

        let query_tokens = self.tokenize(query);
        if query_tokens.is_empty() {
            return Ok(vec![]);
        }

        let mut items_with_score = corpus
            .kept
            .iter()
            .zip(&corpus.tokens)
            .map(|(&index, tokens)| {
                let chunk = &chunks[index];
                SearchItem {
                    id: chunk.id,
                    score: Some(self.score(&corpus, &query_tokens, tokens)),
                    payload: chunk.payload.clone(),
                }
            })
            .collect::<Vec<_>>();

        // Descending score. Python's `heapq.nlargest` keeps ties in the order
        // the graph engine returned the chunks, which no backend guarantees;
        // ties break on the chunk id instead, so the top-k is the same on every
        // run and every backend.
        items_with_score.sort_by(|left, right| {
            right
                .score
                .unwrap_or_default()
                .total_cmp(&left.score.unwrap_or_default())
                .then_with(|| left.id.cmp(&right.id))
        });
        items_with_score.truncate(params.top_k_or(self.top_k));

        if !self.with_scores {
            for item in &mut items_with_score {
                item.score = None;
            }
        }

        Ok(items_with_score)
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        _session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        let output_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query, params).await?,
        };

        Ok(SearchOutput::Items(output_context))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::Arc;

    use cognee_graph::{GraphDBTrait, GraphDBTraitExt, MockGraphDB};
    use serde::Serialize;
    use uuid::Uuid;

    use cognee_session::SessionContext;

    use crate::retrievers::{LexicalRetriever, SearchRetriever};
    use crate::types::{SearchError, SearchOutput, SearchParams};

    #[derive(Serialize)]
    struct DocumentChunkNode {
        id: String,
        #[serde(rename = "type")]
        kind: String,
        text: String,
    }

    async fn add_chunk(graph_db: &MockGraphDB, text: &str) {
        let node = DocumentChunkNode {
            id: Uuid::new_v4().to_string(),
            kind: "DocumentChunk".to_string(),
            text: text.to_string(),
        };

        graph_db.add_node(&node).await.unwrap();
    }

    #[tokio::test]
    async fn ranks_chunks_with_set_jaccard() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        add_chunk(&mock_graph_db, "rust memory safety and ownership").await;
        add_chunk(&mock_graph_db, "python async search orchestration").await;
        add_chunk(&mock_graph_db, "ownership ownership ownership model").await;
        let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;

        let retriever = LexicalRetriever::new(
            Arc::clone(&graph_db),
            Some(2),
            true,
            Some(vec!["and".to_string()]),
            false,
        );

        let context = retriever
            .get_context("ownership and safety", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(context.len(), 2);
        assert!(
            context[0]
                .payload
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap()
                .contains("ownership")
        );
        assert!(context[0].score.unwrap() >= context[1].score.unwrap());
    }

    #[tokio::test]
    async fn multiset_jaccard_accounts_for_frequency() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        add_chunk(&mock_graph_db, "rust rust rust memory").await;
        add_chunk(&mock_graph_db, "rust memory").await;
        let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;

        let retriever = LexicalRetriever::new(Arc::clone(&graph_db), Some(2), true, None, true);

        let context = retriever
            .get_context("rust rust memory", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(context.len(), 2);
        assert!(context[0].score.unwrap() > context[1].score.unwrap());
    }

    #[tokio::test]
    async fn ranks_correctly_when_with_scores_is_false() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        // The query will be "ownership safety". The first chunk matches both tokens,
        // the second matches neither, the third matches "ownership" only.
        add_chunk(
            &mock_graph_db,
            "ownership and safety are core rust features",
        )
        .await;
        add_chunk(&mock_graph_db, "python async search orchestration").await;
        add_chunk(&mock_graph_db, "ownership model in rust").await;
        let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;

        let retriever = LexicalRetriever::new(
            Arc::clone(&graph_db),
            Some(2),
            false, // scores NOT included in output
            Some(vec!["and".to_string(), "are".to_string(), "in".to_string()]),
            false,
        );

        let context = retriever
            .get_context("ownership safety", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(context.len(), 2);

        // Scores should be None (with_scores=false)
        assert!(context[0].score.is_none());
        assert!(context[1].score.is_none());

        // But ranking must still be correct: the chunk with both "ownership" and
        // "safety" should come first.
        let first_text = context[0]
            .payload
            .get("text")
            .and_then(|v| v.as_str())
            .expect("first item should have text");
        assert!(
            first_text.contains("ownership") && first_text.contains("safety"),
            "highest-ranked chunk should contain both query terms, got: {first_text}"
        );
    }

    #[test]
    fn tokenize_lowercases_unicode_characters() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        let graph_db: Arc<dyn cognee_graph::GraphDBTrait> = mock_graph_db;
        let retriever = LexicalRetriever::new(Arc::clone(&graph_db), None, false, None, false);

        assert_eq!(retriever.tokenize("Über"), vec!["über"]);
        assert_eq!(retriever.tokenize("Ñoño"), vec!["ñoño"]);
        assert_eq!(retriever.tokenize("ДМИТРО"), vec!["дмитро"]);
        assert_eq!(retriever.tokenize("Hello World"), vec!["hello", "world"]);
    }

    /// Chunks cognified after the first search must be searchable: the
    /// corpus is loaded per search, as Python builds a retriever per search.
    #[tokio::test]
    async fn sees_chunks_added_after_the_first_search() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        add_chunk(&mock_graph_db, "first chunk about rust").await;
        let graph_db: Arc<dyn GraphDBTrait> = Arc::clone(&mock_graph_db) as _;
        let retriever = LexicalRetriever::bm25(graph_db, Some(5), false, None);

        let before = retriever
            .get_context("ownership", &SearchParams::default())
            .await
            .unwrap();
        assert_eq!(before.len(), 1);

        add_chunk(&mock_graph_db, "second chunk about ownership").await;
        let after = retriever
            .get_context("ownership", &SearchParams::default())
            .await
            .unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].payload["text"], "second chunk about ownership");
    }

    /// No chunk to search is Python's `NoDataError` (`lexical_retriever.py:
    /// 90-92`), not an empty result.
    #[tokio::test]
    async fn no_chunks_is_not_found() {
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
        let retriever = LexicalRetriever::bm25(graph_db, None, false, None);
        let err = retriever
            .get_context("anything", &SearchParams::default())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::NotFound(_)), "got {err:?}");
    }

    async fn bm25_scores(
        chunks: &[&str],
        query: &str,
        stop_words: Option<Vec<String>>,
    ) -> Vec<(String, f32)> {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        for chunk in chunks {
            add_chunk(&mock_graph_db, chunk).await;
        }
        let retriever = LexicalRetriever::bm25(mock_graph_db, Some(chunks.len()), true, stop_words);
        retriever
            .get_context(query, &SearchParams::default())
            .await
            .unwrap()
            .into_iter()
            .map(|item| {
                (
                    item.payload["text"].as_str().unwrap().to_string(),
                    item.score.unwrap(),
                )
            })
            .collect()
    }

    /// Golden values from Python's `BM25ChunksRetriever._scorer` on the same
    /// corpus (k1 = 1.5, b = 0.75, stop words disabled).
    #[tokio::test]
    async fn bm25_matches_python_scores() {
        let ranked = bm25_scores(
            &[
                "rust memory safety ownership",
                "python async search orchestration",
                "ownership ownership ownership model rust",
                "rust compiler",
            ],
            "ownership safety",
            Some(vec![]),
        )
        .await;

        assert_eq!(ranked[0].0, "rust memory safety ownership");
        assert!((ranked[0].1 - 1.841_864_1).abs() < 1e-5, "{ranked:?}");
        assert_eq!(ranked[1].0, "ownership ownership ownership model rust");
        assert!((ranked[1].1 - 1.066_380_3).abs() < 1e-5, "{ranked:?}");
        assert_eq!(ranked[2].1, 0.0);
        assert_eq!(ranked[3].1, 0.0);
    }

    /// IDF: a match on a rare term outranks a match on a common one.
    #[tokio::test]
    async fn rare_term_outranks_common_term() {
        let ranked = bm25_scores(
            &[
                "common rare",
                "common filler",
                "common other",
                "common more",
            ],
            "common rare",
            None,
        )
        .await;
        assert_eq!(ranked[0].0, "common rare");
        assert!(ranked[0].1 > ranked[1].1);
    }

    /// Length normalization: the same single match scores higher in a short
    /// chunk than in a long one.
    #[tokio::test]
    async fn shorter_chunk_wins_on_equal_term_frequency() {
        let ranked = bm25_scores(
            &[
                "target word padded with many other unrelated filler tokens here",
                "target word",
                "nothing relevant",
            ],
            "target",
            None,
        )
        .await;
        assert_eq!(ranked[0].0, "target word");
        assert!(ranked[0].1 > ranked[1].1);
    }

    /// k1 saturation: repeating a term helps, but with diminishing returns —
    /// unlike multiset Jaccard, which is linear in the count.
    #[tokio::test]
    async fn term_frequency_saturates() {
        let ranked = bm25_scores(
            &[
                "apple x x x",
                "apple apple x x",
                "apple apple apple apple",
                "pear x x x",
            ],
            "apple",
            Some(vec![]),
        )
        .await;
        let score = |text: &str| ranked.iter().find(|(t, _)| t == text).unwrap().1;
        let (one, two, four) = (
            score("apple x x x"),
            score("apple apple x x"),
            score("apple apple apple apple"),
        );
        assert!(one < two && two < four);
        assert!(
            two - one > (four - two) / 2.0,
            "gain per repeat must shrink"
        );
    }

    /// BM25 defaults to the English stop words, so a query made only of them
    /// matches nothing.
    #[tokio::test]
    async fn bm25_filters_default_stop_words() {
        let ranked = bm25_scores(&["the cat", "a dog"], "the a", None).await;
        assert!(
            ranked.is_empty(),
            "stop-word-only query has no tokens: {ranked:?}"
        );
    }

    #[tokio::test]
    async fn get_completion_returns_items_output() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        add_chunk(&mock_graph_db, "exact term matching with jaccard").await;
        let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;

        let retriever = LexicalRetriever::new(Arc::clone(&graph_db), Some(5), false, None, false);

        let output = retriever
            .get_completion(
                "exact term",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::Items(items) => {
                assert_eq!(items.len(), 1);
                assert!(items[0].score.is_none());
            }
            _ => panic!("expected items output"),
        }
    }

    /// Golden tokens from Python's `re.findall(r"\w+", text.lower())`:
    /// Devanagari vowel signs split words (they are not `\w`), a word-final
    /// `Σ` lowercases to `ς`, and number characters are word characters.
    #[test]
    fn tokenize_matches_python_word_characters() {
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
        let retriever = LexicalRetriever::new(graph_db, None, false, None, false);

        assert_eq!(
            retriever.tokenize("किताब ΟΔΟΣ ab_c Ⅻ ½"),
            vec!["क", "त", "ब", "οδος", "ab_c", "ⅻ", "½"]
        );
    }

    /// The tokenized corpus is reused while the chunks are unchanged, and
    /// rebuilt as soon as one is added.
    #[tokio::test]
    async fn corpus_is_reused_until_the_chunks_change() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        add_chunk(&mock_graph_db, "rust ownership").await;
        let graph_db: Arc<dyn GraphDBTrait> = Arc::clone(&mock_graph_db) as _;
        let retriever = LexicalRetriever::bm25(graph_db, Some(5), false, None);

        let (_, first) = retriever.load_corpus().await.unwrap();
        let (_, second) = retriever.load_corpus().await.unwrap();
        assert!(
            Arc::ptr_eq(&first, &second),
            "unchanged chunks reuse the corpus"
        );

        add_chunk(&mock_graph_db, "python async").await;
        let (_, third) = retriever.load_corpus().await.unwrap();
        assert!(!Arc::ptr_eq(&second, &third), "a new chunk rebuilds it");
        assert_eq!(third.tokens.len(), 2);
    }

    /// Jaccard never reads the BM25 statistics, so they are not built for it.
    #[tokio::test]
    async fn jaccard_builds_no_bm25_statistics() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        add_chunk(&mock_graph_db, "rust ownership").await;
        let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;
        let retriever = LexicalRetriever::new(graph_db, Some(5), false, None, false);

        let (_, corpus) = retriever.load_corpus().await.unwrap();
        assert!(corpus.idf.is_empty());
    }
}
