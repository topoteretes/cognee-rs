//! Tests for `collect_terms` / `OntologyResolver::terms()`.
//!
//! Two layers:
//!
//! 1. **The raw contract** this crate actually ships: IRI-only subjects, URI
//!    ordering, and `rdfs:label` / `rdfs:comment` returned as raw lexical
//!    forms with `None` meaning "predicate absent".
//! 2. **A reproduction of Python's `_collect_ontology_terms` fold**
//!    (`cognee/tasks/graph/gliner/schema.py`) built on test-local mirrors of
//!    `_local_name` and `to_snake_case`.
//!
//! Layer 2 tests that the **ordering and raw-value contract of layer 1 is
//! sufficient** to reproduce Python's result — it does *not* cover any shipped
//! normalisation, because this crate deliberately ships none: `OntologyTerm`
//! returns raw lexical forms and normalising them is the caller's job, since
//! different callers normalise differently. A later reader should not mistake
//! these assertions for coverage of an implementation in this crate: they
//! guard a contract.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable failures"
)]

use std::path::PathBuf;

use cognee_ontology::builder::collect_terms;
use cognee_ontology::{OntologyResolver, OntologyTerm, RdfLibOntologyResolver};
use sophia_inmem::graph::FastGraph;

const EX: &str = "http://example.org#";
/// Second namespace, present so that full-IRI ordering and local-name ordering
/// disagree. `http://zz.example.org#Alpha` sorts *last* by full IRI but would
/// sort *first* by local name.
const ZZ_ALPHA: &str = "http://zz.example.org#Alpha";

fn fixture_path(file_name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(file_name)
}

fn fixture_resolver() -> RdfLibOntologyResolver {
    RdfLibOntologyResolver::new(fixture_path("owl_terms.ttl")).expect("fixture should load")
}

fn fixture_terms() -> cognee_ontology::OntologyTerms {
    fixture_resolver().terms().expect("terms should collect")
}

fn find<'a>(terms: &'a [OntologyTerm], local: &str) -> &'a OntologyTerm {
    let uri = format!("{EX}{local}");
    terms
        .iter()
        .find(|term| term.uri == uri)
        .unwrap_or_else(|| panic!("expected a term for {uri}"))
}

// ---------------------------------------------------------------------------
// Layer 1 — the raw contract
// ---------------------------------------------------------------------------

#[test]
fn terms_are_sorted_by_uri() {
    let terms = fixture_terms();

    let class_uris: Vec<&str> = terms.classes.iter().map(|t| t.uri.as_str()).collect();
    assert_eq!(
        class_uris,
        vec![
            "http://example.org#Organization",
            "http://example.org#Person",
            "http://example.org#Unnamed",
            "http://example.org#memberOf",
            "http://example.org#zPerson",
            ZZ_ALPHA,
        ],
        "sorted by FULL IRI, not by local name: `Alpha` is last because \
         `http://e…` < `http://z…`, even though `Alpha` is the \
         alphabetically first local name"
    );

    // Pin the sort key explicitly: by local name, `Alpha` would come first.
    let local_name_order = {
        let mut sorted = class_uris.clone();
        sorted.sort_by_key(|uri| local_name(uri));
        sorted
    };
    assert_ne!(
        class_uris, local_name_order,
        "fixture must keep a case where the two orderings differ, or this \
         test cannot tell the full-IRI sort from a local-name sort"
    );
    assert_eq!(
        local_name_order.first(),
        Some(&ZZ_ALPHA),
        "local-name ordering would put Alpha first; full-IRI ordering must not"
    );

    let property_uris: Vec<&str> = terms
        .object_properties
        .iter()
        .map(|t| t.uri.as_str())
        .collect();
    assert_eq!(
        property_uris,
        vec![
            "http://example.org#memberOf",
            "http://example.org#works-at",
            "http://example.org#worksAt",
            "http://example.org#works_at",
            "http://example.org#zzzEmployment",
        ]
    );

    // Strictly ascending, which is what the fold in layer 2 relies on.
    assert!(class_uris.windows(2).all(|w| w[0] < w[1]));
    assert!(property_uris.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn blank_node_subjects_are_dropped() {
    let terms = fixture_terms();

    // The fixture declares seven `owl:Class` subjects; the blank node is dropped.
    assert_eq!(terms.classes.len(), 6);
    assert!(terms.classes.iter().all(|term| !term.uri.is_empty()));
    assert!(
        terms
            .classes
            .iter()
            .all(|term| term.uri.starts_with("http"))
    );
}

#[test]
fn missing_label_is_none_empty_label_is_some_empty() {
    let terms = fixture_terms();

    // No `rdfs:label` triple at all -> None -> caller falls back to the local name.
    assert_eq!(find(&terms.classes, "Person").label, None);

    // `rdfs:label ""` is NOT absent -> Some("") -> caller must skip the term.
    assert_eq!(
        find(&terms.classes, "Unnamed").label,
        Some(String::new()),
        "an explicitly empty label must never collapse to None"
    );
}

#[test]
fn label_and_comment_are_raw() {
    let terms = fixture_terms();

    assert_eq!(
        find(&terms.classes, "Organization").comment.as_deref(),
        Some("  A company or institution.  "),
        "comments must not be trimmed by this crate"
    );
    assert_eq!(
        find(&terms.object_properties, "worksAt").comment.as_deref(),
        Some("   "),
        "a whitespace-only comment is returned verbatim, not as None"
    );
    assert_eq!(
        find(&terms.object_properties, "zzzEmployment")
            .label
            .as_deref(),
        Some("works at"),
        "labels are not normalised by this crate"
    );
}

#[test]
fn classes_and_properties_are_separate_pools() {
    let terms = fixture_terms();

    assert_eq!(terms.object_properties.len(), 5);

    // `ex:memberOf` is typed both `owl:Class` and `owl:ObjectProperty`, so it
    // must appear in both pools — exactly once each, with the same raw values.
    let member_of = format!("{EX}memberOf");
    let in_classes: Vec<&OntologyTerm> = terms
        .classes
        .iter()
        .filter(|term| term.uri == member_of)
        .collect();
    let in_properties: Vec<&OntologyTerm> = terms
        .object_properties
        .iter()
        .filter(|term| term.uri == member_of)
        .collect();
    assert_eq!(in_classes.len(), 1, "once in the class pool");
    assert_eq!(in_properties.len(), 1, "once in the object-property pool");
    assert_eq!(
        in_classes[0], in_properties[0],
        "the two scans are independent: neither pool's entry is degraded by \
         the other"
    );
    assert_eq!(
        in_classes[0].comment.as_deref(),
        Some("Membership relation.")
    );

    // …and the dual typing is the ONLY overlap: nothing else leaks across.
    let overlap: Vec<&str> = terms
        .classes
        .iter()
        .filter(|class| {
            terms
                .object_properties
                .iter()
                .any(|property| property.uri == class.uri)
        })
        .map(|class| class.uri.as_str())
        .collect();
    assert_eq!(
        overlap,
        vec![member_of.as_str()],
        "only the deliberately dual-typed subject may appear in both pools"
    );
}

#[test]
fn empty_graph_yields_empty_terms() {
    let graph = FastGraph::new();
    assert!(collect_terms(&graph).unwrap().is_empty());
}

#[test]
fn resolver_terms_matches_collect_terms() {
    let resolver = fixture_resolver();
    let graph = resolver.graph().expect("fixture graph should be loaded");

    assert_eq!(resolver.terms().unwrap(), collect_terms(graph).unwrap());
}

#[test]
fn resolver_without_graph_yields_empty_terms() {
    let resolver = RdfLibOntologyResolver::new(fixture_path("definitely-not-here.ttl"))
        .expect("a missing ontology file is tolerated, matching Python");
    assert!(!resolver.is_loaded());

    let terms = resolver
        .terms()
        .expect("terms() must not error without a graph");
    assert!(terms.is_empty());
}

// ---------------------------------------------------------------------------
// Layer 2 — Python's fold, reproduced from the contract above
//
// The helpers below are *test-local mirrors* of Python. Normalisation is out
// of scope for this crate by design and belongs to whoever consumes
// `OntologyTerm`, so nothing here is exercised by production code. What these
// tests prove is that the ordering and raw-value contract of layer 1 is
// sufficient to reproduce Python's output — not that any shipped
// normalisation is correct.
// ---------------------------------------------------------------------------

/// Mirrors Python `_local_name` (`schema.py`): rstrip `"/#"`, then
/// successively rsplit on `'#'`, `'/'`, `':'`.
fn local_name(uri: &str) -> String {
    let mut value = uri.trim_end_matches(['/', '#']).to_string();
    for separator in ['#', '/', ':'] {
        if let Some((_, tail)) = value.rsplit_once(separator) {
            value = tail.to_string();
        }
    }
    value
}

/// Mirrors Python `to_snake_case` (`schema.py`): camel boundaries become `_`,
/// runs of non-alphanumerics become `_`, `_+` collapses, `_` is trimmed, then
/// lowercase. Hand-rolled — this crate has no `regex` dependency and needs
/// none. The Python regexes are ASCII-only classes, so are these tests.
fn to_snake_case(name: &str) -> String {
    let chars: Vec<char> = name.trim().chars().collect();
    let mut marked = String::with_capacity(chars.len() * 2);

    for (index, &current) in chars.iter().enumerate() {
        if index > 0 {
            let previous = chars[index - 1];
            // (?<=[a-z0-9])(?=[A-Z])
            let lower_to_upper = (previous.is_ascii_lowercase() || previous.is_ascii_digit())
                && current.is_ascii_uppercase();
            // (?<=[A-Z])(?=[A-Z][a-z])
            let acronym_end = previous.is_ascii_uppercase()
                && current.is_ascii_uppercase()
                && chars.get(index + 1).is_some_and(char::is_ascii_lowercase);
            if lower_to_upper || acronym_end {
                marked.push('_');
            }
        }
        if current.is_ascii_alphanumeric() {
            marked.push(current);
        } else {
            marked.push('_');
        }
    }

    let mut collapsed = String::with_capacity(marked.len());
    let mut previous_underscore = false;
    for character in marked.chars() {
        if character == '_' {
            if !previous_underscore {
                collapsed.push('_');
            }
            previous_underscore = true;
        } else {
            collapsed.push(character);
            previous_underscore = false;
        }
    }

    collapsed.trim_matches('_').to_lowercase()
}

/// Mirrors the fold in Python `_collect_ontology_terms`: first subject to
/// produce a name owns the slot, and the first non-empty description wins.
/// Insertion-ordered, hence a `Vec` of pairs rather than a `HashMap`.
fn fold(terms: &[OntologyTerm]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();

    for term in terms {
        let name = match &term.label {
            Some(label) => to_snake_case(label),
            None => to_snake_case(&local_name(&term.uri)),
        };
        if name.is_empty() {
            continue;
        }
        let description = term.comment.as_deref().unwrap_or("").trim().to_string();
        match out.iter_mut().find(|(existing, _)| *existing == name) {
            None => out.push((name, description)),
            Some((_, stored)) if !description.is_empty() && stored.is_empty() => {
                *stored = description;
            }
            Some(_) => {}
        }
    }

    out
}

fn pairs(folded: &[(String, String)]) -> Vec<(&str, &str)> {
    folded
        .iter()
        .map(|(name, description)| (name.as_str(), description.as_str()))
        .collect()
}

#[test]
fn fold_reproduces_python_entity_types() {
    let terms = fixture_terms();
    let folded = fold(&terms.classes);

    assert_eq!(
        pairs(&folded),
        vec![
            ("organization", "A company or institution."),
            ("person", "A human being."),
            ("member_of", "Membership relation."),
            ("alpha", ""),
        ],
        "covers the local-name fallback (ex:Person), comment trimming \
         (ex:Organization), the empty-label skip (ex:Unnamed), \
         first-non-empty-description-wins (ex:zPerson) and the full-IRI sort \
         (zz:Alpha folds LAST despite the alphabetically first local name)"
    );
}

#[test]
fn fold_reproduces_python_relation_types() {
    let terms = fixture_terms();
    let folded = fold(&terms.object_properties);

    assert_eq!(
        pairs(&folded),
        vec![
            ("member_of", "Membership relation."),
            ("works_at", "Employment relation."),
        ],
        "four subjects collapse to one name; the whitespace-only comment counts \
         as absent, and the later non-empty comment must not override the first"
    );
}
