//! Tokenizer, stemmer and BM25 index over code-graph text.
//!
//! Port of Python's `cognee/modules/retrieval/code_search_index.py`. Pure
//! text processing: the CODE operations feed it every fact's qualified name,
//! docstring and file path (plus each flow's summary) and ask which of them
//! best match some text. Queries and documents go through the same
//! tokenizer, so `validateToken`, `validate_token` and "validating tokens"
//! all meet on the terms `valid` and `token`.
//!
//! Python's character classes are Unicode ones (`[^\W_]+`, `str.isupper`,
//! `str.islower`, `str.isdigit`); here they are `char::is_alphanumeric`,
//! `is_uppercase`, `is_lowercase` and `is_numeric`, which agree on every
//! letter and digit a code identifier is made of.

use std::collections::{BTreeSet, HashMap};

/// English function words and question words, plus the filler a question
/// about a codebase carries. Matched before stemming, against the lowercased
/// token (`STOPWORDS`).
const STOPWORDS: &[&str] = &[
    "a",
    "an",
    "and",
    "are",
    "as",
    "at",
    "be",
    "by",
    "can",
    "could",
    "do",
    "does",
    "for",
    "from",
    "has",
    "have",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "of",
    "on",
    "or",
    "our",
    "should",
    "so",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "this",
    "to",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
    "work",
    "works",
    "happen",
    "happens",
    "deal",
    "deals",
    "code",
    "codebase",
    "repo",
    "repository",
];

/// The suffixes [`stem`] strips, longest first; equal lengths keep Python's
/// listing order (`sorted(..., key=len, reverse=True)` is stable).
const SUFFIXES: &[&str] = &[
    "izations", "ization", "ations", "ation", "ators", "ating", "izing", "izers", "ator", "ates",
    "ated", "izer", "izes", "ized", "ings", "ions", "ize", "ate", "ing", "ion", "ers", "ors",
    "ies", "ed", "es", "er", "or", "ly", "s", "e",
];
const MIN_STEM_LENGTH: usize = 4;
const MIN_TOKEN_LENGTH: usize = 2;

/// The text fields of a document and their BM25 weights (`FIELD_WEIGHTS`).
pub const FIELDS: [&str; 4] = ["name", "doc", "file", "summary"];
const WEIGHTS: [f64; 4] = [3.0, 2.0, 1.0, 1.0];
/// Per-field term frequencies are capped at eight bits, as Python packs them.
const TF_MAX: u32 = 255;

/// `split_identifier`: one alphanumeric run split at camelCase and acronym
/// boundaries. `validateToken` -> `validate`, `Token`; `HTTPServer` ->
/// `HTTP`, `Server`; digits stay with the letters before them.
pub fn split_identifier(word: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = word.char_indices().collect();
    let mut parts = Vec::new();
    let mut start = 0;
    for index in 1..chars.len() {
        let (offset, character) = chars[index];
        if !character.is_uppercase() {
            continue;
        }
        let previous = chars[index - 1].1;
        let follows_lower = previous.is_lowercase() || previous.is_numeric();
        let ends_acronym = previous.is_uppercase()
            && chars
                .get(index + 1)
                .is_some_and(|(_, next)| next.is_lowercase());
        if follows_lower || ends_acronym {
            parts.push(&word[start..offset]);
            start = offset;
        }
    }
    parts.push(&word[start..]);
    parts
}

/// `stem`: strip one inflectional suffix so related word forms share a
/// stem. `validate`, `validates`, `validated`, `validating`, `validation`
/// and `validator` all become `valid`; `class` and `status` stay whole.
pub fn stem(token: &str) -> String {
    for suffix in SUFFIXES {
        let Some(base) = token.strip_suffix(suffix) else {
            continue;
        };
        if *suffix == "s" && matches!(base.chars().last(), Some('s' | 'u' | 'i')) {
            continue;
        }
        let base = if *suffix == "ies" {
            format!("{base}y")
        } else {
            base.to_string()
        };
        if base.chars().count() >= MIN_STEM_LENGTH {
            return base;
        }
    }
    token.to_string()
}

fn run_terms(run: &str, terms: &mut Vec<String>) {
    for part in split_identifier(run) {
        let token = part.to_lowercase();
        if token.chars().count() < MIN_TOKEN_LENGTH || STOPWORDS.contains(&token.as_str()) {
            continue;
        }
        terms.push(stem(&token));
    }
}

/// `tokenize`: the searchable terms of a text, in order and with repeats.
/// Runs of letters and digits are words; everything else (`_ . / - :` and
/// whitespace included) separates them.
pub fn tokenize(text: Option<&str>) -> Vec<String> {
    let mut terms = Vec::new();
    let Some(text) = text else {
        return terms;
    };
    let mut start = None;
    for (offset, character) in text.char_indices() {
        if character.is_alphanumeric() {
            start.get_or_insert(offset);
        } else if let Some(begin) = start.take() {
            run_terms(&text[begin..offset], &mut terms);
        }
    }
    if let Some(begin) = start {
        run_terms(&text[begin..], &mut terms);
    }
    terms
}

/// `query_terms`: the distinct terms of a query, in the order they first
/// appear.
pub fn query_terms(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    tokenize(Some(text))
        .into_iter()
        .filter(|term| seen.insert(term.clone()))
        .collect()
}

/// One document's BM25 score and the fields a query term matched in
/// (`SearchMatch`).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchMatch {
    /// The weighted BM25 score.
    pub score: f64,
    /// The fields a term matched in, in [`FIELDS`] order.
    pub fields: Vec<&'static str>,
}

/// The text of one document: up to four fields, absent ones `None`.
#[derive(Debug, Clone, Default)]
pub struct DocumentFields {
    /// The fact's qualified name.
    pub name: Option<String>,
    /// Its docstring.
    pub doc: Option<String>,
    /// Its file path.
    pub file: Option<String>,
    /// A flow's summary.
    pub summary: Option<String>,
}

impl DocumentFields {
    fn get(&self, position: usize) -> Option<&str> {
        match position {
            0 => self.name.as_deref(),
            1 => self.doc.as_deref(),
            2 => self.file.as_deref(),
            _ => self.summary.as_deref(),
        }
    }
}

/// Field-weighted BM25 over code-graph documents (`CodeSearchIndex`).
///
/// A term scores per field with BM25 (k1, b), the field's length normalised
/// against that field's average over the documents that have it; field
/// scores are weighted (name 3, doc 2, file 1, summary 1) and summed.
/// Document frequency counts documents, whichever of their fields holds the
/// term, and idf = ln(1 + (N - df + 0.5) / (df + 0.5)).
#[derive(Debug, Clone)]
pub struct CodeSearchIndex {
    k1: f64,
    b: f64,
    keys: Vec<String>,
    lengths: [Vec<u32>; 4],
    /// term -> (document, per-field frequencies), in document order.
    postings: HashMap<String, Vec<(usize, [u32; 4])>>,
    average_lengths: [f64; 4],
}

impl CodeSearchIndex {
    /// Index `documents` with Python's defaults, k1 = 1.2 and b = 0.75.
    pub fn new(documents: impl IntoIterator<Item = (String, DocumentFields)>) -> Self {
        Self::with_parameters(documents, 1.2, 0.75)
    }

    /// Index `documents` with explicit BM25 parameters.
    pub fn with_parameters(
        documents: impl IntoIterator<Item = (String, DocumentFields)>,
        k1: f64,
        b: f64,
    ) -> Self {
        let mut keys = Vec::new();
        let mut lengths: [Vec<u32>; 4] = Default::default();
        let mut postings: HashMap<String, Vec<(usize, [u32; 4])>> = HashMap::new();
        for (key, fields) in documents {
            let document = keys.len();
            keys.push(key);
            let mut counts: [HashMap<String, u32>; 4] = Default::default();
            for (position, field_counts) in counts.iter_mut().enumerate() {
                for term in tokenize(fields.get(position)) {
                    *field_counts.entry(term).or_insert(0) += 1;
                }
            }
            for (position, field_counts) in counts.iter().enumerate() {
                let length: u32 = field_counts.values().sum();
                lengths[position].push(length);
            }
            let terms: BTreeSet<&String> = counts.iter().flat_map(HashMap::keys).collect();
            for term in terms {
                let mut frequencies = [0u32; 4];
                for (position, field_counts) in counts.iter().enumerate() {
                    frequencies[position] =
                        field_counts.get(term).copied().unwrap_or(0).min(TF_MAX);
                }
                postings
                    .entry(term.clone())
                    .or_default()
                    .push((document, frequencies));
            }
        }
        let average_lengths = [0, 1, 2, 3].map(|position| average(&lengths[position]));
        Self {
            k1,
            b,
            keys,
            lengths,
            postings,
            average_lengths,
        }
    }

    /// The number of documents.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether the index holds no document.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The inverse document frequency of `term`.
    pub fn idf(&self, term: &str) -> f64 {
        let frequency = self.postings.get(term).map_or(0, Vec::len) as f64;
        (1.0 + (self.keys.len() as f64 - frequency + 0.5) / (frequency + 0.5)).ln()
    }

    /// Score every document that holds at least one of `terms`. The sums run
    /// in Python's order (terms, then documents, then fields), so the scores
    /// are the same floats.
    pub fn search(&self, terms: &[String]) -> HashMap<String, SearchMatch> {
        let mut scores: HashMap<usize, f64> = HashMap::new();
        let mut matched: HashMap<usize, u8> = HashMap::new();
        let mut order: Vec<usize> = Vec::new();
        let mut seen_terms = std::collections::HashSet::new();
        for term in terms {
            if !seen_terms.insert(term) {
                continue;
            }
            let Some(postings) = self.postings.get(term) else {
                continue;
            };
            let idf = self.idf(term);
            for (document, frequencies) in postings {
                let mut score = 0.0;
                for (position, weight) in WEIGHTS.iter().enumerate() {
                    let frequency = f64::from(frequencies[position]);
                    if frequencies[position] == 0 {
                        continue;
                    }
                    let length = f64::from(self.lengths[position][*document]);
                    let norm =
                        self.k1 * (1.0 - self.b + self.b * length / self.average_lengths[position]);
                    score += weight * idf * frequency * (self.k1 + 1.0) / (frequency + norm);
                    *matched.entry(*document).or_insert(0) |= 1 << position;
                }
                let entry = scores.entry(*document).or_insert_with(|| {
                    order.push(*document);
                    0.0
                });
                *entry += score;
            }
        }
        order
            .into_iter()
            .map(|document| {
                let mask = matched.get(&document).copied().unwrap_or(0);
                let fields = FIELDS
                    .iter()
                    .enumerate()
                    .filter(|(position, _)| mask & (1 << position) != 0)
                    .map(|(_, field)| *field)
                    .collect();
                (
                    self.keys[document].clone(),
                    SearchMatch {
                        score: scores.get(&document).copied().unwrap_or(0.0),
                        fields,
                    },
                )
            })
            .collect()
    }
}

fn average(lengths: &[u32]) -> f64 {
    let present: Vec<u32> = lengths
        .iter()
        .copied()
        .filter(|length| *length > 0)
        .collect();
    if present.is_empty() {
        1.0
    } else {
        f64::from(present.iter().sum::<u32>()) / present.len() as f64
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

    #[test]
    fn identifiers_split_at_case_and_acronym_boundaries() {
        assert_eq!(split_identifier("validateToken"), vec!["validate", "Token"]);
        assert_eq!(split_identifier("HTTPServer"), vec!["HTTP", "Server"]);
        assert_eq!(split_identifier("sha256Hash"), vec!["sha256", "Hash"]);
        assert_eq!(split_identifier("plain"), vec!["plain"]);
    }

    #[test]
    fn stems_share_forms() {
        for word in [
            "validate",
            "validates",
            "validated",
            "validating",
            "validation",
            "validator",
        ] {
            assert_eq!(stem(word), "valid", "{word}");
        }
        assert_eq!(stem("class"), "class");
        assert_eq!(stem("status"), "status");
        assert_eq!(stem("entries"), "entry");
        assert_eq!(stem("tokens"), "token");
    }

    #[test]
    fn tokenizer_drops_stopwords_and_short_tokens() {
        assert_eq!(
            tokenize(Some("How does validateToken work in this repo?")),
            vec!["valid", "token"]
        );
        assert_eq!(
            tokenize(Some("auth/tokens.validate_token")),
            vec!["auth", "token", "valid", "token"]
        );
        assert_eq!(query_terms("token tokens validate"), vec!["token", "valid"]);
        assert!(tokenize(None).is_empty());
    }

    #[test]
    fn bm25_prefers_the_name_field() {
        let index = CodeSearchIndex::new([
            (
                "a".to_string(),
                DocumentFields {
                    name: Some("auth.validate_token".into()),
                    ..DocumentFields::default()
                },
            ),
            (
                "b".to_string(),
                DocumentFields {
                    name: Some("orders.create".into()),
                    doc: Some("Validates the token first.".into()),
                    ..DocumentFields::default()
                },
            ),
        ]);
        let matches = index.search(&query_terms("validate token"));
        assert!(matches["a"].score > matches["b"].score);
        assert_eq!(matches["a"].fields, vec!["name"]);
        assert_eq!(matches["b"].fields, vec!["doc"]);
        assert_eq!(index.len(), 2);
    }
}
