//! Rule-based query router for recall.
//!
//! Picks a [`SearchType`] for a query without calling an LLM. Rules are
//! checked in order and the first match wins; a query no rule matches goes
//! to [`ROUTER_FALLBACK_TYPE`].

use std::sync::LazyLock;

use regex::Regex;
use tracing::debug;

use crate::types::SearchType;

/// The search type for a query no rule matched.
pub const ROUTER_FALLBACK_TYPE: SearchType = SearchType::HybridCompletion;

/// A routing decision: the chosen search type, the rule that picked it, and
/// whether the search must run with `only_context`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteDecision {
    pub search_type: SearchType,
    /// The matched rule's name, `"default"` when no rule matched, or
    /// `"pinned"` for a caller's explicit type.
    pub rule: &'static str,
    /// Set when no LLM can write the answer: the search returns the context
    /// it would have answered from.
    pub only_context: bool,
}

/// What the deployment can run. The default allows everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingConstraints {
    /// An LLM can make calls (see `ComponentManager::llm_available`).
    pub llm_available: bool,
    /// Cypher queries are allowed (`search.allow_cypher_query`).
    pub cypher_allowed: bool,
}

impl Default for RoutingConstraints {
    fn default() -> Self {
        Self {
            llm_available: true,
            cypher_allowed: true,
        }
    }
}

/// Routed types that never call an LLM: with no LLM they run as they are. A
/// new LLM-free rule target belongs here, or a keyless install sends it to
/// the fallback.
pub const LLM_FREE_TYPES: [SearchType; 2] = [SearchType::ChunksLexical, SearchType::CodingRules];

/// Types whose only LLM call writes the final answer: with no LLM they run
/// with `only_context` and return the context instead.
///
/// Python's set has no `TripletCompletion`: there it is a deprecated alias of
/// `GraphCompletion`. Rust still has the triplet retriever, whose context
/// needs no LLM either.
pub const COMPLETION_ONLY_TYPES: [SearchType; 4] = [
    SearchType::HybridCompletion,
    SearchType::GraphCompletion,
    SearchType::RagCompletion,
    SearchType::TripletCompletion,
];

/// Types that run the query as Cypher, directly or after an LLM writes it.
pub const CYPHER_TYPES: [SearchType; 2] = [SearchType::Cypher, SearchType::NaturalLanguage];

/// Route `query` by the first rule whose pattern matches it, then resolve that
/// pick against what the deployment can run.
pub fn route_query(query: &str, constraints: &RoutingConstraints) -> RouteDecision {
    let decision = constrain(match_rules(query), constraints);
    debug!(rule = decision.rule, search_type = ?decision.search_type, "query_router: routed");
    decision
}

/// The decision for a query no rule matched.
pub fn default_route(constraints: &RoutingConstraints) -> RouteDecision {
    constrain(unconstrained(ROUTER_FALLBACK_TYPE, "default"), constraints)
}

/// The decision for a caller's explicit type: never rerouted, but with no LLM
/// a completion type runs with `only_context` instead of failing.
pub fn pinned_route(search_type: SearchType, constraints: &RoutingConstraints) -> RouteDecision {
    RouteDecision {
        search_type,
        rule: "pinned",
        only_context: !constraints.llm_available && COMPLETION_ONLY_TYPES.contains(&search_type),
    }
}

/// Resolve a rule's pick against what the deployment can run. Applied after
/// the rules, so the rules look at the query text only.
pub fn constrain(decision: RouteDecision, constraints: &RoutingConstraints) -> RouteDecision {
    let mut search_type = decision.search_type;
    if !constraints.cypher_allowed && CYPHER_TYPES.contains(&search_type) {
        debug!(
            ?search_type,
            "query_router: Cypher queries are disabled; using the fallback"
        );
        search_type = ROUTER_FALLBACK_TYPE;
    }
    if constraints.llm_available || LLM_FREE_TYPES.contains(&search_type) {
        return unconstrained(search_type, decision.rule);
    }
    if !COMPLETION_ONLY_TYPES.contains(&search_type) {
        debug!(
            ?search_type,
            "query_router: no LLM for retrieval; using the fallback"
        );
        search_type = ROUTER_FALLBACK_TYPE;
    }
    RouteDecision {
        search_type,
        rule: decision.rule,
        only_context: true,
    }
}

fn match_rules(query: &str) -> RouteDecision {
    let query = query.trim();
    RULES
        .iter()
        .find(|rule| rule.pattern.is_match(query))
        .map_or(unconstrained(ROUTER_FALLBACK_TYPE, "default"), |rule| {
            unconstrained(rule.search_type, rule.name)
        })
}

fn unconstrained(search_type: SearchType, rule: &'static str) -> RouteDecision {
    RouteDecision {
        search_type,
        rule,
        only_context: false,
    }
}

struct Rule {
    name: &'static str,
    pattern: Regex,
    search_type: SearchType,
}

/// The rules in match order. A rule on the query's shape comes before one on
/// its wording, so `"coding rules"` in quotes is a lexical search. CYPHER has
/// no rule: auto-routing would run any request text as a graph query, which
/// can mutate the graph.
static RULES: LazyLock<[Rule; 3]> = LazyLock::new(|| {
    [
        Rule {
            name: "quoted_phrase",
            pattern: compile(r#"^"[^"]+"$"#),
            search_type: SearchType::ChunksLexical,
        },
        Rule {
            name: "coding_rules_intent",
            pattern: compile(concat!(
                r"(?i)\b(?:coding (?:rules?|standards?|conventions?|guidelines?)",
                r"|code review (?:guidelines?|rules?|standards?|checklist|conventions?))\b",
            )),
            search_type: SearchType::CodingRules,
        },
        Rule {
            name: "time_scoped_question",
            pattern: compile(&time_scoped_question_pattern()),
            search_type: SearchType::Temporal,
        },
    ]
});

#[allow(clippy::expect_used, reason = "the patterns are fixed and unit-tested")]
fn compile(pattern: &str) -> Regex {
    Regex::new(pattern)
        .expect("a router pattern should compile, as the unit tests compile all of them")
}

/// A time preposition followed directly by an absolute date: `in 2019`,
/// `between 1910 and 1920`, `on 7 November 1867`, `in the 1990s`,
/// `scheduled for 2031`. `the 2019 report`, `Q4 2024` or `since monday` do not
/// match: the temporal retriever would spend an LLM call to find no window.
fn time_scoped_question_pattern() -> String {
    format!(
        r"(?i)(?:\b{TIME_PREPOSITION}\s+(?:{absolute_date}|the\s+{YEAR}s)\b|{for_date})",
        absolute_date = absolute_date(),
        for_date = for_date(),
    )
}

const TIME_PREPOSITION: &str = concat!(
    r"(?:in|during|before|after|since|until|till|between|from|by|on|around|circa",
    r"|as of|prior to|through|throughout)",
);
const YEAR: &str = r"(?:1[0-9]{3}|20[0-9]{2})";
const DAY: &str = r"\d{1,2}(?:st|nd|rd|th)?";
const MONTH: &str = concat!(
    r"(?:january|february|march|april|may|june|july|august|september|october|november",
    r"|december|jan|feb|mar|apr|jun|jul|aug|sep|sept|oct|nov|dec)\.?",
);

/// `2024-03-01`, `7 November 1867`, `July 1969`, `March 1, 2024`, `1915`,
/// `early 2024`, `1990s`.
fn absolute_date() -> String {
    format!(
        r"(?:{YEAR}-\d{{2}}-\d{{2}}|{day_month_year}|(?:(?:early|late|mid)[-\s]+)?{YEAR}s?)",
        day_month_year = day_month_year(),
    )
}

/// `7 November 1867`, `July 1969`, `March 1, 2024`.
fn day_month_year() -> String {
    format!(r"(?:{DAY}\s+)?{MONTH}\s+(?:{DAY},?\s+)?{YEAR}")
}

/// `for` followed by a date. `for` takes a quantity as readily as a date
/// (`for 2000 users`), so after it a bare year must end the phrase.
fn for_date() -> String {
    format!(
        r"\bfor\s+(?:(?:{YEAR}-\d{{2}}-\d{{2}}|{day_month_year})\b|(?:(?:early|late|mid)[-\s]+)?{year_as_object}|the\s+{year_as_object})",
        day_month_year = day_month_year(),
        year_as_object = year_as_object(),
    )
}

/// A year that ends its phrase: followed by nothing, punctuation, a non-letter
/// or a conjunction joining another date, never by a noun.
///
/// Python writes the follow-up as a negative lookahead, which the `regex`
/// crate does not support; these alternatives match the same queries.
fn year_as_object() -> String {
    format!(r"{YEAR}s?\b(?:$|[^\s]|\s+(?:$|[^\sa-z]|(?:and|or|to|through|until|till)\b))")
}

#[cfg(test)]
mod tests {
    use super::*;

    use SearchType::{ChunksLexical, CodingRules, HybridCompletion, Temporal};

    fn route(query: &str) -> RouteDecision {
        route_query(query, &RoutingConstraints::default())
    }

    /// Python's golden table (`test_query_router.py`).
    const GOLDEN: &[(&str, SearchType)] = &[
        // quoted_phrase
        ("\"polonium and radium\"", ChunksLexical),
        // coding_rules_intent
        ("What coding rules apply to error handling?", CodingRules),
        ("Show me the code review guidelines", CodingRules),
        ("What coding rules apply to gate.py?", CodingRules),
        // default
        ("Who won Nobel Prizes?", HybridCompletion),
        ("What did Einstein discover?", HybridCompletion),
        ("List all scientists", HybridCompletion),
        ("Tell me something interesting", HybridCompletion),
        ("What is the return policy?", HybridCompletion),
        ("How do I reset my password?", HybridCompletion),
        // a bare Cypher expression looks like an all-caps heading
        ("RETURN 1", HybridCompletion),
        // "exact"/"verbatim" wording is not a lexical-search signal
        ("Find the exact phrase in the documents", HybridCompletion),
        (
            "Find the exact error message from the logs",
            HybridCompletion,
        ),
        ("Quote the paragraph verbatim", HybridCompletion),
        // summary intent stays on the default
        ("Summarize everything about Marie Curie", HybridCompletion),
        ("Give me an overview of the project", HybridCompletion),
        ("tldr of the report", HybridCompletion),
        ("Give me the main points of the meeting", HybridCompletion),
        (
            "Summarize the timeline of Einstein's work",
            HybridCompletion,
        ),
        ("Summarize why the migration stalled", HybridCompletion),
        // time_scoped_question: a time preposition right before an absolute date
        ("what happened in 2019", Temporal),
        ("What happened between 1910 and 1920?", Temporal),
        ("What was discovered in 1915?", Temporal),
        ("What did we decide in 2024?", Temporal),
        ("Incidents from 2019 to 2021", Temporal),
        ("What shipped on 2024-03-01?", Temporal),
        ("What happened in July 1969?", Temporal),
        ("Who was born on 7 November 1867?", Temporal),
        ("What happened on March 1, 2024?", Temporal),
        ("What did Marie Curie do before 1900?", Temporal),
        ("What changed after 2010?", Temporal),
        ("What happened in the 1990s?", Temporal),
        ("What happened in early 2024?", Temporal),
        ("What was the plan as of Jan 2025?", Temporal),
        ("What did Pierre say about Napoleon in 1805?", Temporal),
        // `for` only when the year is the object of the phrase, never a quantity
        ("What is scheduled for 2031?", Temporal),
        ("What is planned for March 2026?", Temporal),
        ("What are the goals for 2027 and 2028?", Temporal),
        ("What was the plan for the 1990s?", Temporal),
        // a year naming a thing, or a bare temporal word, stays on the default
        ("Summarise the 2019 report", HybridCompletion),
        ("Who wrote the 2019 annual report?", HybridCompletion),
        ("What's in the 2019 report?", HybridCompletion),
        (
            "What was the 1990s policy on remote work?",
            HybridCompletion,
        ),
        ("Open ticket 2048", HybridCompletion),
        ("Error 1045 in the logs", HybridCompletion),
        ("How many rows are in table 2024_sales?", HybridCompletion),
        ("Which version shipped in Q4 2024?", HybridCompletion),
        ("What did Alice say in 2 meetings?", HybridCompletion),
        ("What is the budget for 2000 users?", HybridCompletion),
        ("Order chairs for 1500 guests", HybridCompletion),
        ("Reserve the hall for 2024_sales", HybridCompletion),
        ("What is the plan for 2000 new hires?", HybridCompletion),
        ("Show the timeline of discoveries", HybridCompletion),
        ("When was the company founded?", HybridCompletion),
        ("What happened after the merger?", HybridCompletion),
        ("Since when has Alice been on the team?", HybridCompletion),
        ("list the open tickets since monday", HybridCompletion),
        ("When did Einstein publish?", HybridCompletion),
        ("What happened last year?", HybridCompletion),
        // reasoning / relationship intent stays on the default
        ("Why did Curie win two Nobel Prizes?", HybridCompletion),
        ("Explain how the auth module works", HybridCompletion),
        ("Why is the deploy failing?", HybridCompletion),
        (
            "How is Einstein connected to the Sorbonne?",
            HybridCompletion,
        ),
        (
            "What is the relationship between supply and demand?",
            HybridCompletion,
        ),
        ("What entities are related to physics?", HybridCompletion),
        // incidental code / engineering words are not coding-rules intent
        ("Refactor plan for the billing service", HybridCompletion),
        (
            "What are the best practices for onboarding new hires?",
            HybridCompletion,
        ),
        ("Who did the code review for PR 12?", HybridCompletion),
    ];

    #[test]
    fn golden_routes() {
        for &(query, expected) in GOLDEN {
            assert_eq!(route(query).search_type, expected, "{query:?}");
        }
    }

    #[test]
    fn no_golden_query_matches_two_rules() {
        for &(query, _) in GOLDEN {
            let matched: Vec<_> = RULES
                .iter()
                .filter(|rule| rule.pattern.is_match(query.trim()))
                .map(|rule| rule.name)
                .collect();
            assert!(matched.len() <= 1, "{query:?} matches {matched:?}");
        }
    }

    #[test]
    fn shape_rules_win_over_intent_rules() {
        assert_eq!(route("\"coding rules\"").rule, "quoted_phrase");
    }

    #[test]
    fn rules_only_target_routable_types() {
        let routable = [HybridCompletion, ChunksLexical, CodingRules, Temporal];
        let targets = RULES.iter().map(|rule| rule.search_type);
        for target in targets.chain([ROUTER_FALLBACK_TYPE]) {
            assert!(routable.contains(&target), "{target:?} is not routable");
        }
    }

    #[test]
    fn a_year_after_for_routes_only_when_it_ends_the_phrase() {
        let cases = [
            ("What is planned for 2031", Temporal),
            ("What is planned for 2031, and why?", Temporal),
            ("What is planned for 2031 or later?", Temporal),
            ("What is planned for 2031 - draft", Temporal),
            ("What was the budget for 1990s users?", HybridCompletion),
            ("What is the budget for 2000 Users?", HybridCompletion),
            ("What is the plan for 2000 andrew?", HybridCompletion),
        ];
        for (query, expected) in cases {
            assert_eq!(route(query).search_type, expected, "{query:?}");
        }
    }

    #[test]
    fn an_unmatched_query_gets_the_default_decision() {
        let expected = RouteDecision {
            search_type: ROUTER_FALLBACK_TYPE,
            rule: "default",
            only_context: false,
        };
        assert_eq!(route("Tell me something"), expected);
    }

    #[test]
    fn a_matched_query_names_its_rule() {
        assert_eq!(route("\"polonium and radium\"").rule, "quoted_phrase");
        assert_eq!(
            route("Show me the coding rules").rule,
            "coding_rules_intent"
        );
        assert_eq!(route("What happened in 1898?").rule, "time_scoped_question");
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        let decision = route("   \"polonium and radium\"  ");
        assert_eq!(decision.search_type, ChunksLexical);
    }

    #[test]
    fn no_coding_rules_without_an_explicit_phrase() {
        let queries = [
            "hook PreToolUse gate.py emergency exit",
            "Where is def recover() mentioned in the incident notes?",
            "Describe the import process for customer records",
            "What does the return policy say?",
            "Find the async workflow in the operations guide",
            "Find the await keyword mentioned in the incident notes",
            "Which runbook mentions class Parser(",
            "Which runbook mentions function restore_state(",
            "What class of animal is a dolphin?",
            "What is the function of the liver?",
            "Run the linter on the billing module",
            "Refactor the payment flow",
        ];
        for query in queries {
            assert_ne!(route(query).search_type, CodingRules, "{query:?}");
        }
    }

    #[test]
    fn cypher_is_never_auto_routed() {
        let queries = [
            // Valid Cypher, including statements that mutate or destroy.
            "MATCH (n) DETACH DELETE n",
            "MATCH (n:Person) RETURN n.name",
            "CREATE (x:Note {t: 1}) RETURN x",
            "MERGE (a:Tag {name: 'x'}) RETURN a",
            "MATCH (n:User) SET n.admin = true RETURN n",
            "MATCH (n:Doc) REMOVE n.secret RETURN n",
            "OPTIONAL MATCH (n:Person) RETURN n",
            "UNWIND [1, 2, 3] AS x RETURN x",
            "MATCH (a)--(b) RETURN a",
            // Prose that merely looks Cypher-ish.
            "RETURN POLICY FOR DAMAGED GOODS",
            "MERGE CONFLICT in the deploy branch",
            "Run diff --( format on the file",
        ];
        for query in queries {
            assert_ne!(route(query).search_type, SearchType::Cypher, "{query:?}");
        }
    }

    /// A known limitation: a preposition before a four-digit count reads as a
    /// year. TEMPORAL then finds no time window and answers as plain hybrid.
    #[test]
    fn a_four_digit_quantity_after_a_preposition_reads_as_a_year() {
        assert_eq!(route("What is in 2048 bytes?").search_type, Temporal);
    }

    const NO_LLM: RoutingConstraints = RoutingConstraints {
        llm_available: false,
        cypher_allowed: true,
    };

    fn decision(search_type: SearchType, rule: &'static str, only_context: bool) -> RouteDecision {
        RouteDecision {
            search_type,
            rule,
            only_context,
        }
    }

    #[test]
    fn with_no_llm_the_default_is_hybrid_as_only_context() {
        let expected = decision(HybridCompletion, "default", true);
        assert_eq!(
            route_query("Where was Marie Curie born?", &NO_LLM),
            expected
        );
        assert_eq!(default_route(&NO_LLM), expected);
    }

    #[test]
    fn with_no_llm_llm_free_rules_keep_their_types() {
        assert_eq!(
            route_query("\"polonium and radium\"", &NO_LLM),
            decision(ChunksLexical, "quoted_phrase", false)
        );
        assert_eq!(
            route_query("show me the coding rules", &NO_LLM),
            decision(CodingRules, "coding_rules_intent", false)
        );
    }

    #[test]
    fn with_no_llm_completion_only_types_run_as_only_context() {
        for search_type in COMPLETION_ONLY_TYPES {
            let picked = constrain(decision(search_type, "r", false), &NO_LLM);
            assert_eq!(picked, decision(search_type, "r", true));
            let pinned = pinned_route(search_type, &NO_LLM);
            assert_eq!(pinned, decision(search_type, "pinned", true));
        }
    }

    #[test]
    fn with_no_llm_a_temporal_pick_goes_to_the_fallback() {
        assert_eq!(
            route_query("What happened in 1898?", &NO_LLM),
            decision(ROUTER_FALLBACK_TYPE, "time_scoped_question", true)
        );
    }

    #[test]
    fn with_cypher_disabled_no_cypher_type_is_emitted() {
        let no_cypher = RoutingConstraints {
            llm_available: true,
            cypher_allowed: false,
        };
        for search_type in CYPHER_TYPES {
            let picked = constrain(decision(search_type, "r", false), &no_cypher);
            assert_eq!(picked, decision(ROUTER_FALLBACK_TYPE, "r", false));
        }
        let neither = RoutingConstraints {
            llm_available: false,
            cypher_allowed: false,
        };
        let picked = constrain(decision(SearchType::Cypher, "r", false), &neither);
        assert_eq!(picked, decision(ROUTER_FALLBACK_TYPE, "r", true));
    }

    #[test]
    fn a_pinned_type_is_never_rerouted() {
        assert_eq!(
            pinned_route(Temporal, &NO_LLM),
            decision(Temporal, "pinned", false)
        );
        assert_eq!(
            pinned_route(HybridCompletion, &RoutingConstraints::default()),
            decision(HybridCompletion, "pinned", false)
        );
    }
}
