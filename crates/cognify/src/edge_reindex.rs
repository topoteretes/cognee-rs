//! Whole-graph repair for `EdgeType_relationship_name` vector rows that the
//! graph has an edge for but the vector store does not.
//!
//! # The hole this fills
//!
//! Cognify commits extracted edges to the graph in `extract_graph_from_data`
//! (`tasks.rs`, `graph_db.add_edges`) but writes their `EdgeType` vector points
//! a whole pipeline stage later, in `add_data_points` → `index_data_points`. A
//! process **killed** between the two leaves graph edges with no vector row.
//!
//! Those orphans are permanent under every reachable configuration, because the
//! retry does not re-emit them:
//!
//! - `retrieve_existing_edges` finds the edge already in the graph, so
//!   `expand_with_nodes_and_edges` routes it into `claimed_existing_edge_map`
//!   rather than the write bucket (`graph_integration/expansion.rs:844,867-870`).
//! - `extract_graph_from_data` returns only `dedup_result.unique_edges`, and
//!   that is what feeds `edge_type_counts`, `edge_types` and the `index_points`
//!   call for the `EdgeType` collection. The claimed-existing bucket is written
//!   to the ownership ledger and nowhere else.
//!
//! The rollback sweeper does not cover it either: `RunSweeper` is reached only
//! from `rollback::on_run_failed`, whose production callers are both in-process
//! error paths — a SIGKILL runs neither, so every `RollbackScope` collapses to
//! the same end state. `reset_orphans` only rewrites `pipeline_runs` rows, and
//! a library/CLI process never calls it. The ownership-ledger row written
//! before the graph write makes an orphan *findable*, not repaired.
//!
//! # Relationship to Python
//!
//! This is a **robustness improvement over Python, not a parity fix**. Python
//! has the identical dead end — `attach_new_edges_to_data_points` skips edges in
//! `existing_edge_identities` (`expand_with_nodes_and_edges.py:245`) so they
//! never reach `index_graph_edges` via `add_data_points.py:345` — and Python's
//! own comment at `add_data_points.py:316-327` calls the graph-then-vector order
//! "self-healing" on the same false in-process premise.
//!
//! What Python *does* ship, and this module is modelled on, is a whole-graph
//! repair: `index_graph_edges(edges_data=None)`
//! (`cognee/tasks/storage/index_graph_edges.py:77-87`) pulls
//! `graph_engine.get_graph_data()`, counts distinct edge texts, and re-embeds
//! one `EdgeType` per text for the entire graph. It is still called bare from
//! `migrate_relational_database.py:71` and `web_scraper_task.py:350`.
//!
//! Two deliberate differences from that Python routine:
//!
//! 1. **It re-embeds unconditionally; this does not.** Python rebuilds every
//!    `EdgeType` row in the graph whether or not it was missing. At the scale
//!    this module targets that is a bill, not a repair, so the orphan set is
//!    established first with [`VectorDB::retrieve`] — a pure existence probe
//!    that embeds nothing — and only the genuinely missing rows are embedded.
//! 2. **Reporting is the default.** Python's is fire-and-forget; see
//!    [`EdgeReindexOptions::apply`].
//!
//! # Orphan-ness is per retrieval text, not per edge
//!
//! An `EdgeType` point id is [`EdgeType::deterministic_id`] of the edge's
//! *retrieval text* (the nonblank `edge_text` property, else the bare
//! `relationship_name`). Many edges share one text and therefore one row, so a
//! single write heals every edge carrying that text, and an orphan self-heals
//! the moment any later run writes a new edge with the same text. Counting per
//! edge would overstate the damage by the average edge-per-text fan-out.
//!
//! # The orphan set is wider than the crash orphans, and says so
//!
//! "Every edge type the graph implies but the collection lacks" is what this
//! measures, and a killed run is only one way to get there. Cognify builds its
//! `EdgeType` rows from the edges `add_data_points` writes — the extracted
//! `input.edges` and the structural edges `get_graph_from_model` discovers
//! (`is_part_of`, `contains`, `made_from`, `is_a`), both stamped with Python's
//! default `edge_text` sentence first (`graph_extraction::edge_text`). Three
//! families reach the graph without a row:
//!
//! - **Edges written before that stamping.** Structural edges then carried no
//!   `edge_text` and got no row, so their retrieval text is the bare relation
//!   name. Re-cognifying rewrites them with text; until then they are the
//!   same small, roughly constant floor of one text per relation name.
//! - **Edges with an endpoint outside the batch.** The stamp needs both
//!   endpoints' labels and leaves such an edge bare (its text is the relation
//!   name). A structural one gets no row.
//! - **DLT foreign-key edges** — `extract_dlt_fk_edges` runs after
//!   `add_data_points` entirely and writes its own edges with an `edge_text`.
//!
//! So a graph that never crashed can still report a nonzero orphan count.
//! **Do not read the count as crash damage.** Crash orphans are
//! sentence-shaped texts — LLM edge descriptions and stamped defaults — and
//! they sit on top of that floor.
//!
//! Applying therefore also writes rows for those families, which makes those
//! edges take a real vector distance in the retrieval lanes instead of the
//! `triplet_distance_penalty` they take today. That is a ranking change, not
//! only a repair, and it is deliberate: it is exactly what Python's
//! `index_graph_edges(edges_data=None)` does, since it too counts whatever
//! `get_graph_data()` returns. Excluding them would be a rule this module
//! invented, and it would make the repair's end state differ from Python's.
//!
//! # Scale: the graph read is not streamed
//!
//! Say this plainly rather than let the resume cursor imply otherwise. The scan
//! opens with `GraphDBTrait::get_graph_data()`, which materialises **every node
//! and every edge in the graph** into memory at once — the trait exposes no
//! paginated or streaming edge accessor, on any of the three adapters, so there
//! is nothing else to call. Peak memory is therefore a function of graph size,
//! not of `limit`: a corpus with millions of edges needs room for all of them
//! before the first probe is issued.
//!
//! [`EdgeReindexOptions::limit`] and [`EdgeReindexOptions::resume_after`] bound
//! the *embedding* work — the part that is billed — and nothing else. A
//! `--limit 1` run reads exactly as much of the graph as an unlimited one. An
//! operator sizing a run on a large store should budget for the read, and a
//! streaming variant is a new trait method across all three adapters, which is
//! deliberately out of scope here.

use std::collections::{BTreeMap, HashMap, HashSet};

use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_models::EdgeType;
use cognee_vector::{VectorDB, VectorPoint};
use serde_json::json;
use tracing::{info, warn};
use uuid::Uuid;

use crate::error::CognifyError;

/// Vector collection this module repairs.
const EDGE_TYPE_DATA_TYPE: &str = "EdgeType";
/// Indexed field within [`EDGE_TYPE_DATA_TYPE`].
const EDGE_TYPE_FIELD: &str = "relationship_name";

/// How many point ids to probe per [`VectorDB::retrieve`] round trip.
///
/// Independent of the embedding batch size: this is a key lookup with no model
/// behind it, so it is bounded by how large a parameter list the adapter will
/// accept rather than by a token budget.
const PROBE_BATCH: usize = 1_000;

/// What to repair, and whether to actually repair it.
#[derive(Debug, Clone, Default)]
pub struct EdgeReindexOptions {
    /// Write the missing points. Defaults to `false` — reporting only.
    ///
    /// Reporting is the default because the count is the valuable half and it
    /// is free: orphan-ness is decided by an id lookup, so a report costs a few
    /// key probes, while applying embeds every missing text and is billed per
    /// token. An operator who has not yet seen the number has no basis for
    /// choosing to pay it.
    pub apply: bool,

    /// Dataset stamped on the points this run writes, or `None` to write them
    /// dataset-less.
    ///
    /// This does **not** narrow which edges are scanned, and cannot: the scan is
    /// necessarily whole-graph. Nothing in the data supports narrowing it —
    /// `graph_db()` hands back one shared store with no dataset partition,
    /// cognify's edge properties (`build_edge_props`: `relationship_name`,
    /// `source_node_id`, `target_node_id`, `ontology_valid`, `edge_text`) carry
    /// no dataset, and the point id is hashed from the retrieval text alone, so
    /// one row is shared by every dataset holding an edge with that text. A
    /// `--dataset` that silently scanned a subset would report an orphan count
    /// that is not the orphan count.
    ///
    /// `None` matches Python's whole-graph repair, which constructs
    /// `EdgeType(relationship_name=text, number_of_edges=count)` with no dataset
    /// at all (`index_graph_edges.py:49-52`).
    pub dataset_id: Option<Uuid>,

    /// Owner stamped on written points. Metadata only, like `dataset_id`.
    pub user_id: Option<Uuid>,

    /// Tenant stamped on written points. Metadata only, like `dataset_id`.
    pub tenant_id: Option<Uuid>,

    /// Resume point: skip every retrieval text less than or equal to this one.
    ///
    /// The work is ordered by retrieval text — the unit of work itself, and a
    /// content-derived key, so the order is identical on every run regardless of
    /// the order the graph hands edges back. That is what makes a cursor
    /// produced by one pass still address the same position in the next one.
    ///
    /// It does not make a resumed pass equivalent to a fresh whole-graph pass.
    /// "Skip everything at or before the cursor" is literal: a retrieval text
    /// that first appeared *since* the cursor was issued and sorts before it is
    /// skipped too, and stays orphaned until a pass runs without a cursor. So
    /// use the cursor to finish the run it came from, and finish with one
    /// cursorless pass — which is cheap, because by then the probe finds every
    /// row the earlier passes wrote and embeds nothing again.
    pub resume_after: Option<String>,

    /// Stop after writing this many points, reporting the cursor to resume from.
    pub limit: Option<usize>,
}

/// Outcome of one [`reindex_edge_types`] pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EdgeReindexReport {
    /// Graph edges examined.
    pub edges_scanned: usize,
    /// Edges dropped for having neither `edge_text` nor `relationship_name` —
    /// cognify writes no `EdgeType` row for these, so they cannot be orphaned.
    pub edges_without_text: usize,
    /// Distinct `EdgeType` points implied by the scanned edges, after the
    /// cursor. Counted per point id, not per text: retrieval texts differing
    /// only in case, spacing or apostrophes normalise to one id and are counted
    /// once, with their edge counts summed.
    pub distinct_texts: usize,
    /// Distinct retrieval texts whose point is absent from the collection.
    /// **This is the orphan count**, and it is per text, not per edge.
    ///
    /// Not a crash-damage count: it also includes the structural and DLT edge
    /// families cognify never vector-indexes at all, so a healthy graph reports
    /// a small nonzero floor. See the module docs.
    pub orphaned_texts: usize,
    /// Points actually written. Always `0` when [`EdgeReindexOptions::apply`]
    /// is `false`.
    pub points_written: usize,
    /// Embedding batches issued. Always `0` in a report-only run.
    pub batches: usize,
    /// Retrieval text to pass as [`EdgeReindexOptions::resume_after`] to
    /// continue, or `None` when the pass completed the whole orphan set.
    pub resume_cursor: Option<String>,
    /// Whether this pass wrote anything, echoing the option back.
    pub applied: bool,
    /// Set when [`EdgeReindexOptions::resume_after`] skipped every edge type,
    /// so the all-zero counts above mean "nothing was examined", not "nothing
    /// is wrong". A mistyped cursor lands here.
    pub cursor_consumed_everything: bool,
}

/// Find — and with [`EdgeReindexOptions::apply`], repair — `EdgeType` vector
/// rows the graph implies but the vector store lacks.
///
/// Report-only by default: it embeds nothing and writes nothing, so it costs
/// nothing to run against production to size the damage — with one caveat that
/// is not about money. The scan materialises the whole graph in memory (see the
/// module docs' *Scale* section); neither `limit` nor `resume_after` bounds
/// that, in either mode.
pub async fn reindex_edge_types(
    graph_db: &dyn GraphDBTrait,
    vector_db: &dyn VectorDB,
    embedding_engine: Option<&dyn EmbeddingEngine>,
    options: &EdgeReindexOptions,
) -> Result<EdgeReindexReport, CognifyError> {
    let mut report = EdgeReindexReport {
        applied: options.apply,
        ..Default::default()
    };

    // The whole-graph read, exactly as Python's repair does it
    // (`index_graph_edges.py:79-80`) and as memify's `extract_triplets`
    // already does in this crate. `GraphDBTrait` exposes no paginated or
    // streaming edge accessor, so this materialises every edge; the resume
    // cursor below bounds the *embedding* work, which is the part that costs
    // money, but not this read. Streaming it needs a new trait method on all
    // three adapters and is out of scope here.
    let (_, edges) = graph_db.get_graph_data().await?;
    report.edges_scanned = edges.len();

    // BTreeMap, not HashMap: its iteration order is the sorted retrieval-text
    // order the cursor is defined against, so the resume key is stable without
    // a separate sort. Counting here is what makes orphan-ness per distinct
    // text rather than per edge.
    let mut counts_by_text: BTreeMap<String, (Uuid, i32)> = BTreeMap::new();
    for (_, _, relationship_name, properties) in &edges {
        let edge_text = properties.get("edge_text").and_then(|v| v.as_str());
        // One call, both halves: the map key is the text the writer embedded and
        // the id is the point the writer stored it under, derived by the single
        // shared rule in `cognee-models` (SDK-699) rather than restated here.
        let Some(point_id) = EdgeType::point_id_for(edge_text, relationship_name) else {
            report.edges_without_text += 1;
            continue;
        };
        let text = EdgeType::retrieval_text(edge_text, relationship_name);
        counts_by_text.entry(text).or_insert((point_id, 0)).1 += 1;
    }

    // Drop everything an earlier pass already covered. `split_off` keeps the
    // keys strictly greater than the cursor, which is the "after" in
    // `resume_after`.
    if let Some(cursor) = options.resume_after.as_deref() {
        counts_by_text = counts_by_text.split_off(cursor);
        counts_by_text.remove(cursor);
    }
    if counts_by_text.is_empty() {
        // Distinguish "the graph has nothing to check" from "the cursor
        // skipped everything". A mistyped `--resume-after`, or one that sorts
        // after every text in the graph, otherwise returns all-zeroes and the
        // CLI reports the graph fully repaired without having probed a single
        // point.
        if let Some(cursor) = options.resume_after.as_deref() {
            warn!(
                edges_scanned = report.edges_scanned,
                cursor,
                "Edge re-index: the resume cursor skipped every edge type, so nothing was                  checked. If this was not the end of a previous pass, the cursor is wrong —                  re-run without `--resume-after` to scan the whole graph."
            );
            report.cursor_consumed_everything = true;
        } else {
            info!(
                edges_scanned = report.edges_scanned,
                "Edge re-index: no edge types to check"
            );
        }
        return Ok(report);
    }

    // Establish the orphan set without embedding anything. `retrieve` omits
    // absent ids from its result rather than erroring, and answers a missing
    // collection with an empty vec, so "absent from the response" is exactly
    // "needs writing" in both cases.
    // Two retrieval texts that differ only in case, spacing or apostrophes
    // collapse to a single point id: `point_id_for` runs the text through
    // `normalize_identifier`, and nothing upstream normalises
    // `relationship_name` before it reaches the graph
    // (`graph_integration/expansion.rs:893-898` stores the raw LLM string). The
    // The cognify writer has the same hazard and is **not** fixed: its
    // `edge_type_counts` (`tasks.rs`) is keyed on the raw text too, and one run
    // spans many chunks, so two chunks spelling a relation differently collide
    // there as well. Tracked as SDK-708; do not read this collapse as evidence
    // the writer is safe.
    //
    // Collapse by id — keeping the first text in cursor order and summing the
    // counts — so that no batch can carry the same id twice. pgvector writes a
    // batch as one multi-row `INSERT ... ON CONFLICT (id) DO UPDATE`, which
    // Postgres rejects outright when a row repeats (SQLSTATE 21000); the other
    // backends would not error but would silently keep only one of them, with
    // `number_of_edges` holding just that text's share of the count.
    let mut first_seen: HashMap<Uuid, usize> = HashMap::new();
    let mut keyed: Vec<(String, Uuid, i32)> = Vec::with_capacity(counts_by_text.len());
    for (text, (id, count)) in counts_by_text {
        match first_seen.get(&id) {
            Some(&idx) => keyed[idx].2 += count,
            None => {
                first_seen.insert(id, keyed.len());
                keyed.push((text, id, count));
            }
        }
    }
    report.distinct_texts = keyed.len();

    let mut present: HashSet<Uuid> = HashSet::new();
    for probe in keyed.chunks(PROBE_BATCH) {
        let ids: Vec<Uuid> = probe.iter().map(|(_, id, _)| *id).collect();
        let found = vector_db
            .retrieve(EDGE_TYPE_DATA_TYPE, EDGE_TYPE_FIELD, &ids)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
        present.extend(found.into_iter().map(|hit| hit.id));
    }

    let orphans: Vec<(String, Uuid, i32)> = keyed
        .into_iter()
        .filter(|(_, id, _)| !present.contains(id))
        .collect();
    report.orphaned_texts = orphans.len();

    if !options.apply {
        info!(
            edges_scanned = report.edges_scanned,
            distinct_texts = report.distinct_texts,
            orphaned_texts = report.orphaned_texts,
            "Edge re-index (report only): {} of {} edge types have no vector row. \
             Re-run with apply to write them.",
            report.orphaned_texts,
            report.distinct_texts
        );
        return Ok(report);
    }

    if orphans.is_empty() {
        info!("Edge re-index: every edge type already has a vector row");
        return Ok(report);
    }

    // Only the write path needs an engine, which is why the parameter is
    // optional: a report probes ids and embeds nothing, so a caller triaging a
    // crashed run need not have a working embedding backend to get one.
    let embedding_engine = embedding_engine.ok_or_else(|| {
        CognifyError::EmbeddingError(
            "edge re-index cannot apply without an embedding engine".to_string(),
        )
    })?;

    let dimension = embedding_engine.dimension();
    if !vector_db
        .has_collection(EDGE_TYPE_DATA_TYPE, EDGE_TYPE_FIELD)
        .await
        .map_err(|e| CognifyError::VectorDBError(e.to_string()))?
    {
        vector_db
            .create_collection(EDGE_TYPE_DATA_TYPE, EDGE_TYPE_FIELD, dimension)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;
    }

    // Python: `batch_size = vector_engine.embedding_engine.get_batch_size()`.
    let batch_size = embedding_engine.batch_size().max(1);
    let budget = options.limit.unwrap_or(usize::MAX);

    for batch in orphans.chunks(batch_size) {
        if report.points_written >= budget {
            break;
        }

        // Honour the limit mid-batch rather than overshooting a whole batch.
        let take = batch.len().min(budget - report.points_written);
        let batch = &batch[..take];

        let texts: Vec<&str> = batch.iter().map(|(text, _, _)| text.as_str()).collect();
        let vectors = match embedding_engine.embed(&texts).await {
            Ok(vectors) => vectors,
            Err(e) => {
                // The report — and with it the cursor — is discarded by `?`,
                // and a transient 429 partway through a large `--apply` is
                // exactly when an operator needs it. Log it before propagating
                // so the run is resumable rather than restartable.
                match &report.resume_cursor {
                    Some(cursor) => warn!(
                        written = report.points_written,
                        cursor,
                        "Edge re-index failed while embedding; re-run with                          `--resume-after` set to the reported cursor to continue from the                          last batch that landed"
                    ),
                    None => warn!(
                        "Edge re-index failed while embedding the first batch; nothing was                          written, so re-run without `--resume-after`"
                    ),
                }
                return Err(CognifyError::EmbeddingError(e.to_string()));
            }
        };

        let points: Vec<VectorPoint> = batch
            .iter()
            .zip(vectors)
            .map(|((text, id, count), vector)| {
                // Rebuild the DataPoint the writer would have built, so the
                // payload matches `index_data_points`' `EdgeType` branch rather
                // than being a reduced stand-in that later reads have to
                // special-case. `new_deterministic` re-derives the same id from
                // the same text; asserting that equality here would be
                // circular, so the test suite pins it against
                // `EdgeType::deterministic_id` instead.
                let mut edge_type = EdgeType::new_deterministic(text, options.dataset_id);
                edge_type.set_count(*count);

                let mut point = VectorPoint::new(*id, vector);
                for (k, v) in edge_type.base.vector_metadata() {
                    point = point.with_metadata(k, v);
                }
                point = point
                    .with_metadata("field", json!(EDGE_TYPE_FIELD))
                    .with_metadata("relationship_name", json!(edge_type.relationship_name))
                    .with_metadata("number_of_edges", json!(edge_type.number_of_edges));
                if let Some(did) = options.dataset_id {
                    point = point.with_metadata("dataset_id", json!(did.to_string()));
                }
                if let Some(uid) = options.user_id {
                    point = point.with_metadata("user_id", json!(uid.to_string()));
                }
                if let Some(tid) = options.tenant_id {
                    point = point.with_metadata("tenant_id", json!(tid.to_string()));
                }
                point
            })
            .collect();

        vector_db
            .index_points(EDGE_TYPE_DATA_TYPE, EDGE_TYPE_FIELD, &points)
            .await
            .map_err(|e| CognifyError::VectorDBError(e.to_string()))?;

        report.batches += 1;
        report.points_written += batch.len();

        // Advance only after the write lands, so an interrupted run resumes at
        // the last text that is actually in the store rather than skipping the
        // batch it died in.
        if let Some((last_text, _, _)) = batch.last() {
            report.resume_cursor = Some(last_text.clone());
        }
    }

    // A cursor is only worth reporting when there is something left after it.
    if report.points_written >= report.orphaned_texts {
        report.resume_cursor = None;
    } else {
        warn!(
            written = report.points_written,
            remaining = report.orphaned_texts - report.points_written,
            "Edge re-index stopped at the limit; re-run with the reported cursor to continue"
        );
    }

    info!(
        points_written = report.points_written,
        batches = report.batches,
        "Edge re-index wrote {} missing edge-type vector rows",
        report.points_written
    );

    Ok(report)
}
