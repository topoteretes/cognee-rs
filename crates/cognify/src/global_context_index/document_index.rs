//! Task 2 of the `from_entities` method: compute the index and write it back
//! (Python's `build_document_index.py`).
//!
//! A bucket holds the entities its document is distinctively about (tf·idf
//! over the dataset's chunks) and the relations among them; the root holds
//! the dataset's most widespread names (document frequency). The two
//! rankings are opposite on purpose: a bucket leads with what sets its
//! document apart, the root with what the dataset shares.
//!
//! No LLM call anywhere on this path: every run computes the full desired
//! index, writes the nodes whose text changed, and deletes the index nodes it
//! did not compute. That one rule covers deleted documents, incremental
//! additions and switching over from the LLM method, so there is no rebuild
//! flag. [`build_context_summaries`] and its helpers are pure.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use cognee_models::GlobalContextSummary;

use super::content::DatasetContent;
use super::persist::IndexStore;
use super::{
    DOCUMENT_BUCKET_MAX_ENTITIES, DOCUMENT_BUCKET_MAX_RELATIONS, ROOT_MAX_DOCUMENTS,
    ROOT_MAX_ENTITIES, ROOT_MAX_TYPES, SUMMARIZED_IN, SUMMARIZES, create_document_bucket_id,
    create_root_summary_id,
};
use crate::error::CognifyError;

/// Entity id and score, in rank order.
type RankedEntities<'a> = Vec<(&'a str, f64)>;

/// Entity IDF weights over a chunk population (Python's
/// `entity_scoring.compute_idf_from_counts`): `ln(chunks / entity chunks)`.
///
/// An entity in every chunk weighs 0. Empty for an empty population; an
/// entity with no chunks is left out. An entity in more chunks than there
/// are is an error.
pub fn compute_idf_from_counts<'a>(
    chunk_count: usize,
    entity_chunk_counts: impl IntoIterator<Item = (&'a str, usize)>,
) -> Result<HashMap<&'a str, f64>, String> {
    if chunk_count == 0 {
        return Ok(HashMap::new());
    }
    let mut weights = HashMap::new();
    for (entity_id, count) in entity_chunk_counts {
        if count == 0 {
            continue;
        }
        if count > chunk_count {
            return Err(format!(
                "entity_chunk_count cannot be greater than chunk_count for entity '{entity_id}'."
            ));
        }
        weights.insert(entity_id, (chunk_count as f64 / count as f64).ln());
    }
    Ok(weights)
}

/// Write the computed index and drop everything else (Python's
/// `build_document_index`). Returns the nodes it wrote.
///
/// `link_documents` writes the `summarizes` edge from each bucket to its
/// document; turning it off only stops new writes.
pub async fn build_document_index(
    content: &DatasetContent,
    store: &IndexStore<'_>,
    link_documents: bool,
) -> Result<Vec<GlobalContextSummary>, CognifyError> {
    let desired = build_context_summaries(content);
    let desired_ids: HashSet<String> = desired.iter().map(|s| s.base.id.to_string()).collect();

    let stale: Vec<String> = content
        .existing_summary_texts
        .keys()
        .filter(|id| !desired_ids.contains(*id))
        .cloned()
        .collect();
    store.delete_nodes(&stale).await?;

    let changed: Vec<GlobalContextSummary> = desired
        .into_iter()
        .filter(|summary| {
            content
                .existing_summary_texts
                .get(&summary.base.id.to_string())
                != Some(&summary.text)
        })
        .collect();
    store.persist_summaries(&changed).await?;

    // The root id derives from the dataset id alone, so an unchanged
    // bucket's edges stay correct.
    let buckets: Vec<&GlobalContextSummary> = changed.iter().filter(|s| !s.is_root).collect();
    if !buckets.is_empty() {
        let root_id = create_root_summary_id(&content.dataset_id).to_string();
        let mut edges = vec![(
            SUMMARIZED_IN,
            buckets
                .iter()
                .map(|b| (b.base.id.to_string(), root_id.clone()))
                .collect(),
        )];
        if link_documents {
            edges.push((
                SUMMARIZES,
                buckets
                    .iter()
                    .filter_map(|b| Some((b.base.id.to_string(), b.document_id.clone()?)))
                    .collect(),
            ));
        }
        store.persist_edges(&edges).await?;
    }
    Ok(changed)
}

/// The full desired index: one bucket per document that has entities, then
/// the root (Python's `build_context_summaries`).
///
/// A dataset with any chunk always gets its root, even when no document has
/// an entity to name; an empty dataset gets nothing.
pub fn build_context_summaries(content: &DatasetContent) -> Vec<GlobalContextSummary> {
    if content.document_by_chunk.is_empty() {
        return Vec::new();
    }
    let ranked = rank_document_entities(content);
    let mut document_ids: Vec<&str> = ranked.keys().copied().collect();
    document_ids.sort_by(|a, b| {
        content
            .document_name(a)
            .cmp(content.document_name(b))
            .then_with(|| a.cmp(b))
    });

    let mut summaries: Vec<GlobalContextSummary> = document_ids
        .into_iter()
        .map(|document_id| {
            GlobalContextSummary::new(
                create_document_bucket_id(&content.dataset_id, document_id),
                render_bucket_text(document_id, &ranked[document_id], content),
                content.dataset_id.clone(),
                0,
                false,
                Some(document_id.to_string()),
            )
        })
        .collect();
    summaries.push(GlobalContextSummary::new(
        create_root_summary_id(&content.dataset_id),
        render_root_text(content),
        content.dataset_id.clone(),
        1,
        true,
        None,
    ));
    summaries
}

/// Descending by score; equal scores compare equal.
fn by_score_desc(a: f64, b: f64) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

/// Each document's top entities by tf·idf, in rank order. Documents with
/// none are absent.
///
/// IDF is over the dataset's chunks. Term frequency is an integer count
/// multiplied once by the IDF. Ties break on the entity name, then its id.
fn rank_document_entities(content: &DatasetContent) -> HashMap<&str, RankedEntities<'_>> {
    let idf = compute_idf_from_counts(
        content.document_by_chunk.len(),
        content
            .entities
            .iter()
            .map(|(id, entity)| (id.as_str(), entity.chunk_ids.len())),
    )
    // Cannot fail: an entity's chunks are a subset of the dataset's chunks.
    .unwrap_or_default();

    let mut term_frequencies: HashMap<&str, HashMap<&str, u32>> = HashMap::new();
    for (entity_id, entity) in &content.entities {
        for chunk_id in &entity.chunk_ids {
            if let Some(document_id) = content.document_by_chunk.get(chunk_id) {
                *term_frequencies
                    .entry(document_id.as_str())
                    .or_default()
                    .entry(entity_id.as_str())
                    .or_default() += 1;
            }
        }
    }

    term_frequencies
        .into_iter()
        .map(|(document_id, counts)| {
            let mut scored: RankedEntities<'_> = counts
                .into_iter()
                .map(|(entity_id, count)| {
                    let weight = idf.get(entity_id).copied().unwrap_or(0.0);
                    (entity_id, f64::from(count) * weight)
                })
                .collect();
            scored.sort_by(|(a_id, a_score), (b_id, b_score)| {
                by_score_desc(*a_score, *b_score)
                    .then_with(|| {
                        content.entities[*a_id]
                            .name
                            .cmp(&content.entities[*b_id].name)
                    })
                    .then_with(|| a_id.cmp(b_id))
            });
            scored.truncate(DOCUMENT_BUCKET_MAX_ENTITIES);
            (document_id, scored)
        })
        .collect()
}

/// Relation texts between the bucket's own entities, best first: ranked by
/// the weaker endpoint's score, then the text; each text once.
fn rank_document_relations<'a>(
    ranked: &RankedEntities<'_>,
    content: &'a DatasetContent,
) -> Vec<&'a str> {
    let mut candidates: Vec<(f64, &str)> = Vec::new();
    for (source_id, source_score) in ranked {
        for (target_id, target_score) in ranked {
            if let Some(texts) = content
                .relations
                .get(&((*source_id).to_string(), (*target_id).to_string()))
            {
                let score = source_score.min(*target_score);
                candidates.extend(texts.iter().map(|text| (score, text.as_str())));
            }
        }
    }
    candidates.sort_by(|(a_score, a_text), (b_score, b_text)| {
        by_score_desc(*a_score, *b_score).then_with(|| a_text.cmp(b_text))
    });
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .map(|(_, text)| text)
        .filter(|text| seen.insert(*text))
        .take(DOCUMENT_BUCKET_MAX_RELATIONS)
        .collect()
}

/// Document name, then one `Type: name, name` line per type (ordered by its
/// best entity), then relations.
fn render_bucket_text(
    document_id: &str,
    ranked: &RankedEntities<'_>,
    content: &DatasetContent,
) -> String {
    let mut names_by_type: Vec<(&str, Vec<&str>)> = Vec::new();
    for (entity_id, _) in ranked {
        let entity = &content.entities[*entity_id];
        match names_by_type
            .iter_mut()
            .find(|(type_name, _)| *type_name == entity.type_name)
        {
            Some((_, names)) => names.push(&entity.name),
            None => names_by_type.push((&entity.type_name, vec![&entity.name])),
        }
    }
    let mut lines = vec![content.document_name(document_id).to_string()];
    lines.extend(
        names_by_type
            .into_iter()
            .map(|(type_name, names)| format!("{type_name}: {}", names.join(", "))),
    );
    lines.extend(
        rank_document_relations(ranked, content)
            .into_iter()
            .map(str::to_string),
    );
    lines.join("\n")
}

/// The dataset's vocabulary: one name-dense line per label, leaving out a
/// label with nothing to say.
fn render_root_text(content: &DatasetContent) -> String {
    [
        ("Topics", topics_line(content)),
        ("Entities", entities_line(content)),
        ("Documents", documents_line(content)),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(label, value)| format!("{label}: {value}"))
    .collect::<Vec<_>>()
    .join("\n")
}

/// Entity types, by how many entities carry them.
fn topics_line(content: &DatasetContent) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for entity in content.entities.values() {
        *counts.entry(entity.type_name.as_str()).or_default() += 1;
    }
    let mut topics: Vec<(&str, usize)> = counts.into_iter().collect();
    topics.sort_by(|(a_name, a_count), (b_name, b_count)| {
        b_count.cmp(a_count).then_with(|| a_name.cmp(b_name))
    });
    topics
        .into_iter()
        .take(ROOT_MAX_TYPES)
        .map(|(name, count)| format!("{name} ({count})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The dataset's most widespread entity names, by how many documents
/// mention them.
fn entities_line(content: &DatasetContent) -> String {
    let mut frequencies: Vec<(&str, usize)> = content
        .entities
        .iter()
        .map(|(entity_id, entity)| {
            let documents: HashSet<&String> = entity
                .chunk_ids
                .iter()
                .filter_map(|chunk_id| content.document_by_chunk.get(chunk_id))
                .collect();
            (entity_id.as_str(), documents.len())
        })
        .collect();
    frequencies.sort_by(|(a_id, a_df), (b_id, b_df)| {
        b_df.cmp(a_df)
            .then_with(|| {
                content.entities[*a_id]
                    .name
                    .cmp(&content.entities[*b_id].name)
            })
            .then_with(|| a_id.cmp(b_id))
    });
    frequencies
        .into_iter()
        .take(ROOT_MAX_ENTITIES)
        .map(|(entity_id, _)| content.entities[entity_id].name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Document names, by how many chunks they have; never hides how many it
/// left out.
fn documents_line(content: &DatasetContent) -> String {
    let mut chunk_counts: HashMap<&str, usize> = HashMap::new();
    for document_id in content.document_by_chunk.values() {
        *chunk_counts.entry(document_id.as_str()).or_default() += 1;
    }
    let mut documents: Vec<(&str, usize)> = chunk_counts.into_iter().collect();
    documents.sort_by(|(a_id, a_count), (b_id, b_count)| {
        b_count
            .cmp(a_count)
            .then_with(|| content.document_name(a_id).cmp(content.document_name(b_id)))
            .then_with(|| a_id.cmp(b_id))
    });
    let mut line = documents
        .iter()
        .take(ROOT_MAX_DOCUMENTS)
        .map(|(id, _)| content.document_name(id))
        .collect::<Vec<_>>()
        .join(", ");
    if documents.len() > ROOT_MAX_DOCUMENTS {
        line.push_str(&format!(
            " (+{} more)",
            documents.len() - ROOT_MAX_DOCUMENTS
        ));
    }
    line
}

/// Ports of Python's `test_build_document_index.py` and the
/// `compute_idf_from_counts` cases of `test_global_context_index_idf.py`.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::global_context_index::content::DatasetEntity;

    type EntitySpec<'a> = (&'a str, (&'a str, &'a str, &'a [&'a str]));

    /// `entities` are id → (name, type name, chunk ids); `relations` are triples.
    fn make_content(
        documents: &[(&str, &str)],
        document_by_chunk: &[(&str, &str)],
        entities: &[EntitySpec<'_>],
        relations: &[(&str, &str, &str)],
    ) -> DatasetContent {
        let owned = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        DatasetContent {
            dataset_id: "ds".to_string(),
            document_names: owned(documents),
            document_by_chunk: owned(document_by_chunk),
            entities: entities
                .iter()
                .map(|(id, (name, type_name, chunks))| {
                    (
                        id.to_string(),
                        DatasetEntity {
                            name: name.to_string(),
                            type_name: type_name.to_string(),
                            chunk_ids: chunks.iter().map(|c| c.to_string()).collect(),
                        },
                    )
                })
                .collect(),
            relations: relations
                .iter()
                .map(|(s, t, text)| ((s.to_string(), t.to_string()), vec![text.to_string()]))
                .collect(),
            existing_summary_texts: BTreeMap::new(),
        }
    }

    /// Two documents sharing one dataset-wide entity, which therefore has idf 0.
    fn two_document_content() -> DatasetContent {
        make_content(
            &[("d1", "alpha.md"), ("d2", "beta.md")],
            &[("c1", "d1"), ("c2", "d1"), ("c3", "d2")],
            &[
                ("e_shared", ("Shared", "Thing", &["c1", "c2", "c3"])),
                ("e_acme", ("Acme", "Organization", &["c1", "c2"])),
                ("e_bob", ("Bob", "Person", &["c1"])),
                ("e_cara", ("Cara", "Person", &["c3"])),
            ],
            &[
                ("e_bob", "e_acme", "Bob works at Acme"),
                ("e_shared", "e_acme", "Shared relates to Acme"),
                ("e_acme", "e_cara", "Acme knows Cara"),
            ],
        )
    }

    /// Each bucket's text by document id, and the root's under `"root"`.
    fn index_texts(content: &DatasetContent) -> HashMap<String, String> {
        build_context_summaries(content)
            .into_iter()
            .map(|s| (s.document_id.unwrap_or_else(|| "root".to_string()), s.text))
            .collect()
    }

    #[test]
    fn entity_in_every_chunk_ranks_last_in_each_bucket_and_tops_the_root() {
        let texts = index_texts(&two_document_content());
        assert!(
            texts["d1"].starts_with("alpha.md\nPerson: Bob\nOrganization: Acme\nThing: Shared\n")
        );
        assert_eq!(texts["d2"], "beta.md\nPerson: Cara\nThing: Shared");
        assert!(texts["root"].contains("\nEntities: Shared, "));
    }

    #[test]
    fn bucket_entities_are_ranked_by_term_frequency_times_idf() {
        let content = make_content(
            &[("d1", "alpha.md"), ("d2", "beta.md")],
            &[
                ("c1", "d1"),
                ("c2", "d1"),
                ("c3", "d1"),
                ("c4", "d2"),
                ("c5", "d2"),
                ("c6", "d2"),
                ("c7", "d2"),
            ],
            &[
                ("e_often", ("Often", "Thing", &["c1", "c2", "c3"])),
                ("e_rare", ("Rare", "Thing", &["c1"])),
                (
                    "e_common",
                    ("Common", "Thing", &["c1", "c2", "c4", "c5", "c6"]),
                ),
            ],
            &[],
        );
        assert_eq!(
            index_texts(&content)["d1"],
            "alpha.md\nThing: Often, Rare, Common"
        );
    }

    #[test]
    fn equal_scores_always_render_in_the_same_order() {
        let content = make_content(
            &[("d1", "alpha.md"), ("d2", "beta.md")],
            &[("c1", "d1"), ("c2", "d2")],
            &[
                ("a-id", ("Bruno", "Person", &["c1"])),
                ("z-id", ("Alice", "Person", &["c1"])),
            ],
            &[],
        );
        assert_eq!(
            index_texts(&content)["d1"],
            "alpha.md\nPerson: Alice, Bruno"
        );
    }

    #[test]
    fn document_without_any_entities_gets_no_bucket() {
        let content = make_content(
            &[("d1", "alpha.md"), ("d2", "beta.md")],
            &[("c1", "d1"), ("c2", "d2")],
            &[("e_acme", ("Acme", "Organization", &["c1"]))],
            &[],
        );
        let documents: Vec<Option<String>> = build_context_summaries(&content)
            .into_iter()
            .map(|s| s.document_id)
            .collect();
        assert_eq!(documents, [Some("d1".to_string()), None]);
    }

    #[test]
    fn empty_dataset_produces_no_summaries_at_all_not_even_a_root() {
        let mut content = make_content(&[], &[], &[], &[]);
        content.existing_summary_texts =
            BTreeMap::from([("stale-bucket".to_string(), "old".to_string())]);
        assert!(build_context_summaries(&content).is_empty());
    }

    #[test]
    fn one_chunk_dataset_still_names_every_entity() {
        let names: Vec<String> = (0..8).map(|i| format!("Name{i}")).collect();
        let mut content = make_content(&[("d1", "only.md")], &[("c1", "d1")], &[], &[]);
        for (i, name) in names.iter().enumerate() {
            content.entities.insert(
                format!("e{i}"),
                DatasetEntity {
                    name: name.clone(),
                    type_name: "Person".to_string(),
                    chunk_ids: BTreeSet::from(["c1".to_string()]),
                },
            );
        }
        let summaries = build_context_summaries(&content);
        assert_eq!(
            summaries[0].text,
            format!("only.md\nPerson: {}", names.join(", "))
        );
        assert!(summaries[1].text.starts_with("Topics: Person (8)\n"));
    }

    #[test]
    fn relation_is_dropped_when_an_endpoint_is_not_in_this_bucket() {
        let texts = index_texts(&two_document_content());
        assert!(texts["d1"].ends_with("Thing: Shared\nBob works at Acme\nShared relates to Acme"));
    }

    #[test]
    fn relations_are_ranked_by_their_weaker_endpoint() {
        let content = make_content(
            &[("d1", "alpha.md"), ("d2", "beta.md")],
            &[("c1", "d1"), ("c2", "d2"), ("c3", "d2"), ("c4", "d2")],
            &[
                ("e_ann", ("Ann", "Person", &["c1"])),
                ("e_bea", ("Bea", "Person", &["c1", "c2"])),
                ("e_cy", ("Cy", "Person", &["c1", "c2"])),
                ("e_dan", ("Dan", "Person", &["c1", "c2", "c3"])),
            ],
            &[
                ("e_ann", "e_dan", "Ann met Dan"),
                ("e_bea", "e_cy", "Bea met Cy"),
            ],
        );
        assert!(index_texts(&content)["d1"].ends_with("\nBea met Cy\nAnn met Dan"));
    }

    #[test]
    fn bucket_text_leads_with_the_document_name_and_orders_types_by_best_entity() {
        assert_eq!(
            index_texts(&two_document_content())["d1"],
            "alpha.md\nPerson: Bob\nOrganization: Acme\nThing: Shared\n\
             Bob works at Acme\nShared relates to Acme"
        );
    }

    #[test]
    fn root_text_is_three_name_dense_lines() {
        assert_eq!(
            index_texts(&two_document_content())["root"],
            "Topics: Person (2), Organization (1), Thing (1)\n\
             Entities: Shared, Acme, Bob, Cara\n\
             Documents: alpha.md, beta.md"
        );
    }

    #[test]
    fn root_document_line_reports_how_many_documents_it_left_out() {
        let count = ROOT_MAX_DOCUMENTS + 3;
        let mut content = make_content(&[], &[], &[], &[]);
        for i in 0..count {
            content
                .document_names
                .insert(format!("d{i}"), format!("doc{i:03}.md"));
            content
                .document_by_chunk
                .insert(format!("c{i}"), format!("d{i}"));
        }
        assert!(index_texts(&content)["root"].ends_with(" (+3 more)"));
    }

    #[test]
    fn root_document_line_falls_back_to_the_id_of_a_document_with_no_name() {
        let content = make_content(&[], &[("c1", "d1")], &[], &[]);
        assert_eq!(index_texts(&content)["root"], "Documents: d1");
    }

    #[test]
    fn compute_idf_from_counts_uses_expected_math() {
        let idf = compute_idf_from_counts(4, [("a", 1), ("b", 2), ("c", 4)]).unwrap();
        assert!((idf["a"] - 4f64.ln()).abs() < 1e-12);
        assert!((idf["b"] - 2f64.ln()).abs() < 1e-12);
        // Ubiquitous entities get zero weight.
        assert_eq!(idf["c"], 0.0);
    }

    #[test]
    fn compute_idf_from_counts_returns_empty_for_empty_population() {
        assert!(compute_idf_from_counts(0, [("a", 1)]).unwrap().is_empty());
    }

    #[test]
    fn compute_idf_from_counts_ignores_non_positive_entity_counts() {
        let idf = compute_idf_from_counts(3, [("a", 0), ("b", 1)]).unwrap();
        assert_eq!(idf.keys().copied().collect::<Vec<_>>(), ["b"]);
    }

    #[test]
    fn compute_idf_from_counts_rejects_entity_count_greater_than_chunk_count() {
        assert!(compute_idf_from_counts(1, [("a", 2)]).is_err());
    }
}
