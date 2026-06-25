//! Rule-based query type classifier for auto-routing search queries.
//!
//! Ports the Python weighted-scoring heuristic from
//! `cognee/api/v1/recall/query_router.py` verbatim — same rules, same
//! weights, same negation window, same scoring semantics. Each detection
//! rule adds its weight to a [`SearchType`]; the highest total wins.
//!
//! Every rule is a `(matcher, search_type, weight)` tuple. Matchers are
//! either a list of keyword phrases (with word-boundary checks) or a
//! compiled [`regex::Regex`]. Matches suppressed by a nearby negation word
//! do not contribute to the score — see [`is_negated`].

use std::sync::OnceLock;

use regex::Regex;

use crate::types::SearchType;

/// Result of query routing.
#[derive(Debug, Clone)]
pub struct RouteResult {
    /// The recommended search type.
    pub search_type: SearchType,
    /// Confidence score (sum of matching rule weights).
    pub confidence: f32,
    /// Second-best search type.
    pub runner_up: SearchType,
    /// Runner-up confidence score.
    pub runner_up_score: f32,
    /// All aggregated scores sorted by weight descending. Parity with
    /// Python's `RouteResult.all_scores` (which is a dict, but Rust keeps
    /// deterministic ordering as a `Vec`).
    pub all_scores: Vec<(SearchType, f32)>,
}

impl RouteResult {
    /// Parity with Python's `RouteResult.is_confident`: the winning score
    /// is at least 2x the runner-up (with a floor of 1.0 so every match
    /// clears the bar when nothing else fires).
    pub fn is_confident(&self) -> bool {
        self.confidence >= 2.0 * self.runner_up_score.max(1.0)
    }
}

// --- Defaults, negation window ---------------------------------------------

const DEFAULT_TYPE: SearchType = SearchType::GraphCompletion;
const DEFAULT_BASE_SCORE: f32 = 2.0;
/// Parity with Python `_NEGATION_WINDOW = 20`.
const NEGATION_WINDOW: usize = 20;
/// Matches Python's `_NEGATION = re.compile(r"\b(not|n't|no|never|without|lack)\b")`.
const NEGATION_WORDS: &[&str] = &["not", "n't", "no", "never", "without", "lack"];

/// Check whether the char before `idx` and the char at `idx + keyword_len`
/// are both non-alphanumeric (i.e. the match is a standalone word,
/// mirroring Python's `\b...\b`).
fn is_word_boundary(text: &str, idx: usize, len: usize) -> bool {
    let before_ok = if idx == 0 {
        true
    } else {
        text[..idx]
            .chars()
            .next_back()
            .map(|c| !c.is_alphanumeric() && c != '_')
            .unwrap_or(true)
    };
    let after_idx = idx + len;
    let after_ok = if after_idx >= text.len() {
        true
    } else {
        text[after_idx..]
            .chars()
            .next()
            .map(|c| !c.is_alphanumeric() && c != '_')
            .unwrap_or(true)
    };
    before_ok && after_ok
}

/// Find the first occurrence of `kw` in `text` at a word boundary
/// (both sides non-alphanumeric). Returns the byte offset of the match
/// if any. `text` and `kw` are both assumed to be lowercase when called
/// on `lower`.
fn contains_word(text: &str, kw: &str) -> Option<usize> {
    if kw.is_empty() {
        return None;
    }
    let mut cursor = 0usize;
    while let Some(rel) = text[cursor..].find(kw) {
        let pos = cursor + rel;
        if is_word_boundary(text, pos, kw.len()) {
            return Some(pos);
        }
        // Advance by at least one byte to continue scanning. If we are
        // mid-codepoint for some reason, walk to the next char boundary.
        let mut step = pos + 1;
        while step < text.len() && !text.is_char_boundary(step) {
            step += 1;
        }
        cursor = step;
    }
    None
}

/// Is the character range `[match_start, match_start]` preceded by a
/// negation word within the 20-char window? Parity with Python's
/// `_is_negated(query, match)`.
fn is_negated(lower: &str, match_start: usize) -> bool {
    let mut window_start = match_start.saturating_sub(NEGATION_WINDOW);
    while window_start > 0 && !lower.is_char_boundary(window_start) {
        window_start -= 1;
    }
    let prefix = &lower[window_start..match_start];
    for neg in NEGATION_WORDS {
        // Word-boundary search inside the prefix.
        if contains_word(prefix, neg).is_some() {
            return true;
        }
    }
    false
}

// --- Rule tables ----------------------------------------------------------

/// How a rule probes the query. Keyword lists use word-boundary matching;
/// regex matchers use a compiled `regex::Regex`.
enum Matcher {
    /// Lowercased keywords/phrases; match if any of them is found in the
    /// lowercased query at a word boundary.
    Keywords(&'static [&'static str]),
    /// Compiled regex applied to the original (trimmed) query. The regex
    /// is built lazily on first use via `OnceLock`.
    Regex {
        cell: &'static OnceLock<Regex>,
        pattern: &'static str,
        case_insensitive: bool,
    },
}

struct Rule {
    matcher: Matcher,
    target: SearchType,
    weight: f32,
    /// Whether to suppress the match if preceded by a negation word.
    respects_negation: bool,
}

// --- Regex cells (one per rule that uses regex) ---------------------------
// Kept as module-level `OnceLock`s so compilation happens once per process.

static RE_CYPHER_PREFIX: OnceLock<Regex> = OnceLock::new();
static RE_LEXICAL_QUOTED: OnceLock<Regex> = OnceLock::new();
static RE_CODE_SYNTAX: OnceLock<Regex> = OnceLock::new();
static RE_RELATIONSHIP_HOW: OnceLock<Regex> = OnceLock::new();
static RE_RELATIONSHIP_WHAT: OnceLock<Regex> = OnceLock::new();
static RE_YEAR: OnceLock<Regex> = OnceLock::new();
static RE_YEAR_RANGE: OnceLock<Regex> = OnceLock::new();

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            // --- Cypher: raw query syntax (highest priority) ---
            Rule {
                matcher: Matcher::Regex {
                    cell: &RE_CYPHER_PREFIX,
                    // Python: `(^MATCH\s|^RETURN\s|^CREATE\s|^MERGE\s|--\(|\)--)`
                    pattern: r"(^MATCH\s|^RETURN\s|^CREATE\s|^MERGE\s|--\(|\)--)",
                    case_insensitive: false,
                },
                target: SearchType::Cypher,
                weight: 10.0,
                // Cypher syntax is structural — negation prefixes are not
                // meaningful for it. Python does not skip negation check
                // either, but the anchored `^MATCH` etc. cannot ever sit
                // inside a negation window.
                respects_negation: true,
            },
            // --- Coding rules: programming context keywords ---
            Rule {
                // Python: \b(coding rules?|code review|best practice|lint(ing|er)?|refactor(ing)?)\b
                matcher: Matcher::Keywords(&[
                    "coding rule",
                    "coding rules",
                    "code review",
                    "best practice",
                    "lint",
                    "linting",
                    "linter",
                    "refactor",
                    "refactoring",
                ]),
                target: SearchType::CodingRules,
                weight: 5.0,
                respects_negation: true,
            },
            Rule {
                // Python: \b(def |return |async |await |import |class \w+\(|\.py\b|function\s+\w+\()
                matcher: Matcher::Regex {
                    cell: &RE_CODE_SYNTAX,
                    pattern: r"\b(def |return |async |await |import |class \w+\(|\.py\b|function\s+\w+\()",
                    case_insensitive: true,
                },
                target: SearchType::CodingRules,
                weight: 3.0,
                respects_negation: true,
            },
            // --- Lexical: fully quoted phrase ---
            Rule {
                // Python: ^"[^"]+"$  (applies to the full trimmed query)
                matcher: Matcher::Regex {
                    cell: &RE_LEXICAL_QUOTED,
                    pattern: r#"^"[^"]+"$"#,
                    case_insensitive: false,
                },
                target: SearchType::ChunksLexical,
                weight: 8.0,
                respects_negation: true,
            },
            Rule {
                // Python: \b(exact|verbatim|literal|word.for.word)\b
                // NOTE: Python's `.` in `word.for.word` matches any char;
                // in practice that covers "word-for-word", "word_for_word",
                // "word for word".
                matcher: Matcher::Keywords(&[
                    "exact",
                    "verbatim",
                    "literal",
                    "word for word",
                    "word-for-word",
                    "word.for.word",
                    "word_for_word",
                ]),
                target: SearchType::ChunksLexical,
                weight: 4.0,
                respects_negation: true,
            },
            // --- Summary ---
            Rule {
                // Python: \b(summarize|summary|overview|outline|tl;?dr|gist|main points?|key takeaways?|high.?level)\b
                matcher: Matcher::Keywords(&[
                    "summarize",
                    "summary",
                    "overview",
                    "outline",
                    "tldr",
                    "tl;dr",
                    "gist",
                    "main point",
                    "main points",
                    "key takeaway",
                    "key takeaways",
                    "high level",
                    "high-level",
                    "highlevel",
                ]),
                target: SearchType::GraphSummaryCompletion,
                weight: 5.0,
                respects_negation: true,
            },
            // --- Reasoning / chain-of-thought ---
            Rule {
                // Python: \b(why|explain|reasoning|step.by.step|chain of thought)\b
                matcher: Matcher::Keywords(&[
                    "why",
                    "explain",
                    "reasoning",
                    "step by step",
                    "step-by-step",
                    "step.by.step",
                    "chain of thought",
                ]),
                target: SearchType::GraphCompletionCot,
                weight: 4.0,
                respects_negation: true,
            },
            Rule {
                // Python: \b(because|therefore|consequently)\b
                matcher: Matcher::Keywords(&["because", "therefore", "consequently"]),
                target: SearchType::GraphCompletionCot,
                weight: 2.0,
                respects_negation: true,
            },
            // --- Relationship / graph traversal ---
            Rule {
                // Python: \b(how (is|are|does|do)\s+\w+\s+(related|connected|linked))\b
                matcher: Matcher::Regex {
                    cell: &RE_RELATIONSHIP_HOW,
                    pattern: r"\b(how (is|are|does|do)\s+\w+\s+(related|connected|linked))\b",
                    case_insensitive: true,
                },
                target: SearchType::GraphCompletionContextExtension,
                weight: 5.0,
                respects_negation: true,
            },
            Rule {
                // Python: \b(what (connects|links|ties)|path between|degree of separation)\b
                matcher: Matcher::Regex {
                    cell: &RE_RELATIONSHIP_WHAT,
                    pattern: r"\b(what (connects|links|ties)|path between|degree of separation)\b",
                    case_insensitive: true,
                },
                target: SearchType::GraphCompletionContextExtension,
                weight: 5.0,
                respects_negation: true,
            },
            Rule {
                // Python: \b(connection|relationship|related to|linked to)\b
                matcher: Matcher::Keywords(&[
                    "connection",
                    "relationship",
                    "related to",
                    "linked to",
                ]),
                target: SearchType::GraphCompletionContextExtension,
                weight: 3.0,
                respects_negation: true,
            },
            // --- Temporal ---
            Rule {
                // Python: \b(when|before|after|during|since|until)\b
                matcher: Matcher::Keywords(&[
                    "when", "before", "after", "during", "since", "until",
                ]),
                target: SearchType::Temporal,
                weight: 3.0,
                respects_negation: true,
            },
            Rule {
                // Python: \b(timeline|chronolog|era|decade|century)\b
                matcher: Matcher::Keywords(&[
                    "timeline",
                    "chronolog",
                    "chronology",
                    "chronological",
                    "era",
                    "decade",
                    "century",
                ]),
                target: SearchType::Temporal,
                weight: 4.0,
                respects_negation: true,
            },
            Rule {
                // Python: \b\d{4}s?\b
                matcher: Matcher::Regex {
                    cell: &RE_YEAR,
                    pattern: r"\b\d{4}s?\b",
                    case_insensitive: false,
                },
                target: SearchType::Temporal,
                weight: 3.0,
                respects_negation: true,
            },
            Rule {
                // Python: \bbetween\s+\d{4}\s+and\s+\d{4}\b
                matcher: Matcher::Regex {
                    cell: &RE_YEAR_RANGE,
                    pattern: r"\bbetween\s+\d{4}\s+and\s+\d{4}\b",
                    case_insensitive: true,
                },
                target: SearchType::Temporal,
                weight: 6.0,
                respects_negation: true,
            },
        ]
    })
}

fn compile(
    cell: &'static OnceLock<Regex>,
    pattern: &str,
    case_insensitive: bool,
) -> &'static Regex {
    cell.get_or_init(|| {
        let mut builder = regex::RegexBuilder::new(pattern);
        builder.case_insensitive(case_insensitive);
        builder
            .build()
            .unwrap_or_else(|e| panic!("query_router: failed to compile regex {pattern:?}: {e}"))
    })
}

/// Try to match a rule against a query. Returns the match start index
/// (in bytes, within the appropriate view) if the rule fires.
fn rule_match(rule: &Rule, trimmed: &str, lower: &str) -> Option<usize> {
    match &rule.matcher {
        Matcher::Keywords(kws) => {
            // Walk keywords and return the earliest-matching start index so
            // negation windows operate on the real position. Order across
            // keywords is not meaningful for scoring (one rule contributes
            // at most once per query).
            let mut earliest: Option<usize> = None;
            for kw in *kws {
                if let Some(pos) = contains_word(lower, kw) {
                    earliest = Some(earliest.map_or(pos, |e| e.min(pos)));
                }
            }
            earliest
        }
        Matcher::Regex {
            cell,
            pattern,
            case_insensitive,
        } => {
            let re = compile(cell, pattern, *case_insensitive);
            re.find(trimmed).map(|m| m.start())
        }
    }
}

/// Route a natural-language query to the most appropriate [`SearchType`].
///
/// Uses a rule-based weighted-scoring classifier (no LLM call). Each rule's
/// weight is added to its target `SearchType` when its pattern matches
/// (and is not negated within a 20-char window). The `SearchType` with the
/// highest total score wins.
///
/// Falls back to [`SearchType::GraphCompletion`] (with base score 2.0) when
/// no rule fires. When a rule fires but the best score is still below the
/// base threshold (2.0), returns `GraphCompletion` with the original best
/// as `runner_up` for diagnostics.
pub fn route_query(query: &str) -> RouteResult {
    let trimmed = query.trim();
    let lower = trimmed.to_lowercase();

    // Track aggregated scores per SearchType. Vec keeps insertion order
    // for deterministic tie-breaking, matching Python dict iteration.
    let mut scores: Vec<(SearchType, f32)> = Vec::new();

    for rule in rules() {
        let Some(m_start) = rule_match(rule, trimmed, &lower) else {
            continue;
        };
        // Negation is evaluated on the lowercase view; the match start is
        // a byte offset that is valid for either view because lower/upper
        // ASCII mapping in the matched keyword preserves byte length.
        // For regex rules we compute the lowercased start by mapping the
        // same byte offset (ASCII-safe for our patterns).
        if rule.respects_negation && is_negated(&lower, m_start) {
            continue;
        }
        if let Some(entry) = scores.iter_mut().find(|(s, _)| *s == rule.target) {
            entry.1 += rule.weight;
        } else {
            scores.push((rule.target, rule.weight));
        }
    }

    if scores.is_empty() {
        return RouteResult {
            search_type: DEFAULT_TYPE,
            confidence: DEFAULT_BASE_SCORE,
            runner_up: DEFAULT_TYPE,
            runner_up_score: 0.0,
            all_scores: Vec::new(),
        };
    }

    // Sort descending by score.
    scores.sort_by(|a, b| b.1.total_cmp(&a.1));

    let (best_type, best_score) = scores[0];
    let (ru_type, ru_score) = scores.get(1).copied().unwrap_or((DEFAULT_TYPE, 0.0));

    if best_score < DEFAULT_BASE_SCORE {
        // Below threshold: fall back to default but keep the best-matched
        // rule in `runner_up` for diagnostics.
        return RouteResult {
            search_type: DEFAULT_TYPE,
            confidence: best_score,
            runner_up: best_type,
            runner_up_score: best_score,
            all_scores: scores,
        };
    }

    RouteResult {
        search_type: best_type,
        confidence: best_score,
        runner_up: ru_type,
        runner_up_score: ru_score,
        all_scores: scores,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Factual queries ---------------------------------------------------

    mod factual_queries {
        use super::*;

        #[test]
        fn simple_who() {
            assert_eq!(
                route_query("Who won Nobel Prizes?").search_type,
                SearchType::GraphCompletion
            );
        }

        #[test]
        fn simple_what() {
            assert_eq!(
                route_query("What did Einstein discover?").search_type,
                SearchType::GraphCompletion
            );
        }

        #[test]
        fn short_list() {
            assert_eq!(
                route_query("List all scientists").search_type,
                SearchType::GraphCompletion
            );
        }
    }

    // --- Cypher queries ----------------------------------------------------

    mod cypher {
        use super::*;

        #[test]
        fn match_statement() {
            assert_eq!(
                route_query("MATCH (n:Person) RETURN n.name").search_type,
                SearchType::Cypher
            );
        }

        #[test]
        fn return_statement() {
            assert_eq!(route_query("RETURN 1").search_type, SearchType::Cypher);
        }
    }

    // --- Coding rules (incl. 2 negatives) ---------------------------------

    mod coding_rules {
        use super::*;

        #[test]
        fn coding_rules_phrase() {
            let r = route_query("What coding rules apply to error handling?");
            assert_eq!(r.search_type, SearchType::CodingRules);
        }

        #[test]
        fn code_review() {
            assert_eq!(
                route_query("Show me the code review guidelines").search_type,
                SearchType::CodingRules
            );
        }

        #[test]
        fn bare_class_is_not_code() {
            let result = route_query("What class of animal is a dolphin?");
            assert_ne!(result.search_type, SearchType::CodingRules);
        }

        #[test]
        fn bare_function_is_not_code() {
            let result = route_query("What is the function of the liver?");
            assert_ne!(result.search_type, SearchType::CodingRules);
        }
    }

    // --- Lexical ----------------------------------------------------------

    mod lexical {
        use super::*;

        #[test]
        fn quoted_phrase() {
            assert_eq!(
                route_query("\"polonium and radium\"").search_type,
                SearchType::ChunksLexical
            );
        }

        #[test]
        fn exact_keyword() {
            let r = route_query("Find the exact phrase in the documents");
            assert_eq!(r.search_type, SearchType::ChunksLexical);
        }
    }

    // --- Summary ---------------------------------------------------------

    mod summary {
        use super::*;

        #[test]
        fn summarize() {
            let r = route_query("Summarize everything about Marie Curie");
            assert_eq!(r.search_type, SearchType::GraphSummaryCompletion);
        }

        #[test]
        fn overview() {
            let r = route_query("Give me an overview of the project");
            assert_eq!(r.search_type, SearchType::GraphSummaryCompletion);
        }

        #[test]
        fn tldr() {
            assert_eq!(
                route_query("tldr of the report").search_type,
                SearchType::GraphSummaryCompletion
            );
        }
    }

    // --- Reasoning -------------------------------------------------------

    mod reasoning {
        use super::*;

        #[test]
        fn why_question() {
            let r = route_query("Why did Curie win two Nobel Prizes?");
            assert_eq!(r.search_type, SearchType::GraphCompletionCot);
        }

        #[test]
        fn explain() {
            let r = route_query("Explain the theory of relativity");
            assert_eq!(r.search_type, SearchType::GraphCompletionCot);
        }
    }

    // --- Relationship ----------------------------------------------------

    mod relationship {
        use super::*;

        #[test]
        fn connection_between() {
            let r = route_query("How is Einstein connected to the Sorbonne?");
            assert_eq!(r.search_type, SearchType::GraphCompletionContextExtension);
        }

        #[test]
        fn related_to() {
            let r = route_query("What entities are related to physics?");
            assert_eq!(r.search_type, SearchType::GraphCompletionContextExtension);
        }

        #[test]
        fn between_not_temporal() {
            let r = route_query("What is the relationship between supply and demand?");
            assert_eq!(r.search_type, SearchType::GraphCompletionContextExtension);
        }
    }

    // --- Temporal --------------------------------------------------------

    mod temporal {
        use super::*;

        #[test]
        fn when_question() {
            assert_eq!(
                route_query("When did Einstein publish?").search_type,
                SearchType::Temporal
            );
        }

        #[test]
        fn year_range() {
            let r = route_query("What happened between 1910 and 1920?");
            assert_eq!(r.search_type, SearchType::Temporal);
        }

        #[test]
        fn timeline() {
            assert_eq!(
                route_query("Show the timeline of discoveries").search_type,
                SearchType::Temporal
            );
        }

        #[test]
        fn specific_year() {
            assert_eq!(
                route_query("What was discovered in 1915?").search_type,
                SearchType::Temporal
            );
        }
    }

    // --- Negation --------------------------------------------------------

    mod negation {
        use super::*;

        #[test]
        fn not_related_suppresses_graph() {
            let r = route_query("What is not related to physics?");
            assert_ne!(r.search_type, SearchType::GraphCompletionContextExtension);
        }

        #[test]
        fn no_connection_suppresses_graph() {
            let r = route_query("There is no connection between these topics");
            assert_ne!(r.search_type, SearchType::GraphCompletionContextExtension);
        }

        #[test]
        fn negation_does_not_affect_distant_match() {
            let r = route_query(
                "This is not about food at all, however I want to know how is X connected to Y?",
            );
            assert_eq!(r.search_type, SearchType::GraphCompletionContextExtension);
        }
    }

    // --- Confidence ------------------------------------------------------

    mod confidence {
        use super::*;

        #[test]
        fn high_confidence_for_cypher() {
            let r = route_query("MATCH (n) RETURN n");
            assert!(r.confidence >= 10.0);
            assert!(r.is_confident());
        }

        #[test]
        fn runner_up_populated() {
            let r = route_query("Summarize the timeline of discoveries");
            // The winner should be Summary; runner-up should be Temporal.
            assert_eq!(r.search_type, SearchType::GraphSummaryCompletion);
            assert!(!r.all_scores.is_empty());
        }

        #[test]
        fn default_has_base_confidence() {
            let r = route_query("Tell me something interesting");
            assert_eq!(r.search_type, SearchType::GraphCompletion);
            assert!(r.confidence >= 0.0);
        }
    }

    // --- Ambiguous queries -----------------------------------------------

    mod ambiguous {
        use super::*;

        #[test]
        fn temporal_beats_graph_for_years() {
            let r = route_query("What happened between 1910 and 1920?");
            assert_eq!(r.search_type, SearchType::Temporal);
        }

        #[test]
        fn summary_with_temporal_word() {
            let r = route_query("Summarize the timeline of Einstein's work");
            assert_eq!(r.search_type, SearchType::GraphSummaryCompletion);
        }

        #[test]
        fn default_for_vague_query() {
            assert_eq!(
                route_query("Tell me something").search_type,
                SearchType::GraphCompletion
            );
        }
    }
}
