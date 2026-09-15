//! Operator entry point for the whole-graph `EdgeType` vector backfill.
//!
//! The repair itself lives in `cognee_cognify::edge_reindex`, which documents
//! the hole it fills. This module is the console surface: argument parsing, and
//! a report an operator can act on.
//!
//! Report-first, unlike its neighbour `vector-reindex`. That command does the
//! work immediately because its build is free of ambiguity and idempotent, so a
//! report-only mode would only cost a second invocation. Here the two modes are
//! genuinely different transactions: the report is a handful of key lookups, and
//! applying embeds every missing row. An operator who has not seen the count has
//! no basis for authorising the spend.

use std::sync::Arc;

use cognee::cognify::{EdgeReindexOptions, reindex_edge_types};
use cognee::{ComponentManager, PipelineContext};
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::EdgeReindexArgs;
use crate::error::CliError;

/// Report — and with `--apply`, repair — missing `EdgeType` vector rows.
pub fn run(args: EdgeReindexArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    // Parse before opening any connection, so a typo fails immediately rather
    // than after the graph load.
    let dataset_id = args
        .dataset_id
        .as_deref()
        .map(|raw| {
            Uuid::parse_str(raw).map_err(|e| {
                CliError::Runtime(format!("--dataset-id '{raw}' is not a valid UUID: {e}"))
            })
        })
        .transpose()?;

    crate::teardown::run_command(Arc::clone(&cm), async move {
        let graph_db = cm
            .graph_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let vector_db = cm
            .vector_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let embedding_engine = cm
            .embedding_engine()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        if args.apply {
            info!(
                "Scanning the whole graph for edge types with no vector row, and \
                 writing the missing ones. Each missing row costs one embedding."
            );
        } else {
            info!(
                "Scanning the whole graph for edge types with no vector row. This \
                 is a report only — nothing is embedded and nothing is written."
            );
        }

        // Say it at the moment it could mislead, not only in `--help`. An
        // operator who passed a dataset is about to read a count; without this
        // line they read the whole graph's orphan count as their dataset's.
        if dataset_id.is_some() {
            info!(
                "--dataset-id only labels the points this run writes. The scan \
                 below covers every edge in the graph, not just that dataset's, \
                 and the counts are whole-graph counts."
            );
        }
        if args.limit.is_some() && !args.apply {
            warn!(
                "--limit bounds writes, and a report writes nothing, so it has no \
                 effect here. The counts below are for the whole scan."
            );
        }
        if args.resume_after.is_some() {
            info!(
                "--resume-after narrows this run to the retrieval texts sorting \
                 after the cursor, so the counts below exclude everything before \
                 it. Re-run without it for the graph's total."
            );
        }

        let options = EdgeReindexOptions {
            apply: args.apply,
            dataset_id,
            user_id: None,
            tenant_id: None,
            resume_after: args.resume_after.clone(),
            limit: args.limit,
        };

        let report = reindex_edge_types(
            graph_db.as_ref(),
            vector_db.as_ref(),
            embedding_engine.as_ref(),
            &options,
        )
        .await
        .map_err(|error| CliError::Runtime(format!("Edge re-index failed: {error}")))?;

        // Spell out that the orphan count is per distinct retrieval text. Many
        // edges share one text and therefore one row, so an operator reading
        // "3 orphaned" against a 6-edge graph would otherwise assume the tool
        // had missed half of them.
        info!(
            "Scanned {} graph edge(s) -> {} distinct edge type(s). {} edge(s) carried \
             no usable text and have no row by design.",
            report.edges_scanned, report.distinct_texts, report.edges_without_text
        );

        if report.orphaned_texts == 0 {
            info!("Every edge type already has a vector row — nothing to repair.");
            return Ok(());
        }

        if report.applied {
            info!(
                "Wrote {} missing edge-type vector row(s) in {} embedding batch(es).",
                report.points_written, report.batches
            );
            if let Some(cursor) = report.resume_cursor.as_deref() {
                info!(
                    "Stopped at the --limit with {} row(s) still missing. Continue with: \
                     --apply --resume-after {cursor:?}",
                    report.orphaned_texts - report.points_written
                );
            }
        } else {
            info!(
                "{} of {} edge type(s) have NO vector row. These edges are invisible to \
                 the retrieval lanes that read EdgeType_relationship_name. Re-run with \
                 --apply to write them (one embedding each).",
                report.orphaned_texts, report.distinct_texts
            );
            // Do not let the number read as a damage report. Cognify builds
            // EdgeType rows only from LLM-extracted edges, so the structural
            // (`is_part_of`, `contains`, `made_from`) and DLT edge families are
            // missing on a graph that never crashed — a small constant floor
            // that is here every time, on top of which real crash orphans show
            // up as long, sentence-shaped descriptions.
            info!(
                "Not all of that is crash damage: cognify never writes a row for the \
                 structural (is_part_of / contains / made_from) or DLT edge families, \
                 so a healthy graph still reports one per such relation name. Writing \
                 them is what Python's whole-graph repair does too, and it makes those \
                 edges rankable rather than always taking the distance penalty."
            );
        }

        Ok(())
    })
}
