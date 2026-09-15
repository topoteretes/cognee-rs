//! Report and clear what a killed run left behind on a dataset (SDK-616).
//!
//! A cognify or memify run is refused with "already running" by **two**
//! independent gates, and a killed process leaves both of them set:
//!
//! 1. `check_pipeline_run_qualification` reads the latest `pipeline_runs` row
//!    for the pair and rejects a `Started` one. This is checked *first*, and it
//!    **never expires** — the only sweep that retires such a row runs at
//!    HTTP-server startup, so a CLI-only deployment never reaches it.
//! 2. The exclusive-run claim, which does expire, a day later.
//!
//! Clearing only the claim leaves the run still refused by the first gate, so
//! this command reports and clears both or it is not worth having.
//!
//! Reporting is the default and clearing is opt-in, because the distinction
//! that matters cannot be made from here: a young claim is either a live run or
//! a holder that died minutes ago, and clearing a live one re-admits the
//! concurrent run the claim exists to prevent. Only the operator knows which.
//!
//! For the same reason the two modes have different default scopes — see
//! [`resolve_pipelines`]. Reporting covers every pipeline; clearing covers only
//! the one named.

use std::sync::Arc;

use cognee::cognify::CLAIM_STALE_AFTER;
use cognee::database::{
    PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository, ops,
};
use cognee::{ComponentManager, PipelineContext};
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::PipelineUnblockArgs;
use crate::error::CliError;

/// Pipeline names that take a claim. An unrecognised `--pipeline` would
/// otherwise look up a pair that cannot exist and report, confidently, that
/// nothing is blocking.
const KNOWN_PIPELINES: &[&str] = &["cognify_pipeline", "temporal-cognify", "memify_pipeline"];

/// Recorded on the `Errored` row this command writes, so the audit trail says
/// who retired the run rather than leaving it looking like a real failure.
const RESET_REASON: &str = "operator_unblock";

pub fn run(args: PipelineUnblockArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let pipelines = resolve_pipelines(args.pipeline.as_deref(), args.clear)?;

    // Scoped so the settings read guard is dropped before the async block below
    // captures `cm` — holding it across an await would keep a lock alive for
    // the whole command.
    let owner_id = {
        let settings = cm.settings();
        Uuid::parse_str(&settings.default_user_id).map_err(|error| {
            CliError::Validation(format!(
                "Invalid default_user_id '{}': {error}",
                settings.default_user_id
            ))
        })?
    };

    crate::teardown::run_command(Arc::clone(&cm), async move {
        let database = cm
            .database()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        let dataset = ops::datasets::get_dataset_by_name(&database, &args.dataset, owner_id, None)
            .await
            .map_err(|error| {
                CliError::Runtime(format!(
                    "Failed to resolve dataset '{}': {error}",
                    args.dataset
                ))
            })?
            .ok_or_else(|| CliError::Validation(format!("Dataset '{}' not found", args.dataset)))?;

        let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&database));
        let ds = dataset.id;
        let mut found_any = false;

        for pipe in &pipelines {
            let pipe = pipe.as_str();

            // Gate 1: the orphaned run row. Checked first because that is the
            // order cognify checks them in, so it is the order the operator
            // hits them.
            //
            // `Started` only. `check_pipeline_run_qualification` maps
            // `Initiated` to `Proceed`, so an `Initiated` row refuses nothing —
            // reporting it as a blocker, and then "clearing" it, would be a
            // confident wrong answer about a problem that does not exist.
            let orphan = repo
                .get_pipeline_run_by_dataset(ds, pipe)
                .await
                .map_err(|e| CliError::Runtime(format!("{e}")))?
                .filter(|run| matches!(run.status, PipelineRunStatus::Started));

            // Gate 2: the claim.
            let claim = repo
                .get_pipeline_run_claim(ds, pipe)
                .await
                .map_err(|e| CliError::Runtime(format!("{e}")))?;

            // A claim past the staleness window is reclaimed by the next run on
            // its own, so it refuses nothing. Counting it as a blocker would
            // send the operator to clear something that was never the problem.
            let claim_blocks = claim.as_ref().is_some_and(|held| !is_reclaimable(held));
            if orphan.is_none() && !claim_blocks {
                if claim.is_some() {
                    for line in describe(orphan.as_ref(), claim.as_ref(), &args.dataset, ds, pipe) {
                        info!("{line}");
                    }
                }
                continue;
            }

            found_any = true;
            for line in describe(orphan.as_ref(), claim.as_ref(), &args.dataset, ds, pipe) {
                info!("{line}");
            }

            if !args.clear {
                info!(
                    "Reporting only. If the process that started this run is gone, clear it \
                     with: cognee-cli pipeline-unblock -d {} --pipeline {pipe} --clear",
                    args.dataset
                );
                continue;
            }

            let mut cleared = 0usize;
            let mut contradicted = false;

            if let Some(stuck) = &orphan {
                warn!(
                    "Retiring the orphaned '{pipe}' run {} on dataset '{}'.",
                    stuck.pipeline_run_id, args.dataset
                );
                // Scoped to the *row* reported above: a real run started between
                // the report and this clear must not be marked failed. It has
                // to be `stuck.id` — `pipeline_run_id` is derived from
                // `(pipeline_id, dataset_id)` alone, so a newcomer dispatched
                // over HTTP carries the same one and would slip past the guard.
                if repo
                    .reset_orphan_run(ds, pipe, stuck.id, RESET_REASON)
                    .await
                    .map_err(|e| CliError::Runtime(format!("{e}")))?
                {
                    cleared += 1;
                } else {
                    contradicted = true;
                    info!(
                        "The run reported above is no longer the latest for this pair — \
                         nothing retired. A new run may have started since."
                    );
                }
            }

            if let Some(held) = &claim
                && claim_blocks
            {
                warn!(
                    "Releasing the '{pipe}' claim on dataset '{}', holder {}.",
                    args.dataset, held.claim_id
                );
                // Scoped to the holder read above: if it finished and a new run
                // took the pair in between, this removes nothing rather than
                // killing the newcomer.
                if repo
                    .try_release_pipeline_run_claim(ds, pipe, held.claim_id)
                    .await
                    .map_err(|e| CliError::Runtime(format!("{e}")))?
                {
                    cleared += 1;
                } else {
                    contradicted = true;
                    info!(
                        "The claim was given up while this command ran — nothing released, \
                         and a new run may now hold it."
                    );
                }
            }

            if cleared > 0 && !contradicted {
                info!(
                    "Cleared. '{pipe}' can run on dataset '{}' again.",
                    args.dataset
                );
            } else if contradicted {
                // Something moved under us, so "can run again" would contradict
                // the line printed immediately above it. Say what is known.
                info!(
                    "Partly cleared. State changed while this command ran, so re-run without \
                     --clear to see where '{pipe}' on dataset '{}' now stands.",
                    args.dataset
                );
            } else {
                info!(
                    "Nothing left to clear on '{pipe}' for dataset '{}'.",
                    args.dataset
                );
            }
        }

        if !found_any {
            info!(
                "Nothing is blocking a run on dataset '{}' ({ds}). If one is still being \
                 refused, it is being refused by something else.",
                args.dataset
            );
        }
        Ok(())
    })
}

/// Which pipelines this invocation acts on, and whether it is allowed to act
/// on them at all.
///
/// The two modes want opposite defaults, so they do not share one.
///
/// *Reporting* is broad on purpose. Naming a pipeline is an optimisation, not a
/// requirement: defaulting to cognify would answer "nothing is blocking" for a
/// wedged `temporal-cognify`, which is the confident-wrong-answer
/// `KNOWN_PIPELINES` exists to prevent — and nothing in the failing cognify
/// output tells the operator which pipeline name to pass. So an omitted
/// `--pipeline` sweeps all three and tells them which one is wedged.
///
/// *Clearing* must not inherit that breadth. The three pipelines are
/// independent — each claims a dataset under its own name — so the operator who
/// diagnosed a wedged `cognify_pipeline` and reaches for `--clear` would also
/// retire a `memify_pipeline` run that is alive and mid-flight, marking it
/// failed and releasing its fresh claim. Nothing in the report they just read
/// warned them, because the report is *supposed* to cover all three. Naming the
/// pipeline is therefore required for the destructive path: the operator has
/// already read which one is wedged, so it costs them a word, and it is the
/// same word the reporting output tells them to pass.
fn resolve_pipelines(pipeline: Option<&str>, clear: bool) -> Result<Vec<String>, CliError> {
    match pipeline {
        Some(named) => {
            if !KNOWN_PIPELINES.contains(&named) {
                return Err(CliError::Validation(format!(
                    "Unknown pipeline '{named}'. Expected one of: {}. A name that takes no \
                     claim would report nothing blocking, which is the wrong answer rather \
                     than no answer.",
                    KNOWN_PIPELINES.join(", ")
                )));
            }
            Ok(vec![named.to_string()])
        }
        None if clear => Err(CliError::Validation(format!(
            "--clear needs an explicit --pipeline. Without one this command covers {}, and \
             clearing all of them would retire a run on another pipeline that is alive and \
             mid-flight — they claim the dataset independently, so a wedged one says nothing \
             about the others. Re-run without --clear to see which pipeline is blocked, then \
             pass that name: cognee-cli pipeline-unblock -d <dataset> --pipeline <name> --clear",
            KNOWN_PIPELINES.join(", ")
        ))),
        None => Ok(KNOWN_PIPELINES.iter().map(|p| (*p).to_string()).collect()),
    }
}

/// Whether a claim is already past the staleness window, so the next run
/// reclaims it without anyone asking.
///
/// `get_pipeline_run_claim` applies no staleness filter — it reports the row as
/// it stands — so this is the caller's job. Mirrors `try_claim_pipeline_run`'s
/// rule, which reclaims when `age >= stale_after`.
fn is_reclaimable(claim: &cognee::database::PipelineRunClaim) -> bool {
    chrono::Utc::now()
        .signed_duration_since(claim.claimed_at)
        .to_std()
        .is_ok_and(|age| age >= CLAIM_STALE_AFTER)
}

/// The report shown before any action, one line per blocker.
///
/// Returned rather than logged so the wording is testable without a database:
/// the judgement an operator makes from these lines is the whole point of the
/// command, and getting "already expired" backwards would send them to release
/// a claim that was blocking nothing.
fn describe(
    orphan: Option<&cognee::database::PipelineRun>,
    claim: Option<&cognee::database::PipelineRunClaim>,
    dataset_name: &str,
    dataset_id: Uuid,
    pipeline: &str,
) -> Vec<String> {
    let mut lines = vec![format!(
        "Dataset '{dataset_name}' ({dataset_id}), pipeline '{pipeline}':"
    )];

    match orphan {
        Some(run) => lines.push(format!(
            "  - run {} is recorded as {:?}, started {}. This is checked before the claim and \
             never expires on its own, so it blocks every re-run until retired.",
            run.pipeline_run_id,
            run.status,
            run.created_at.to_rfc3339()
        )),
        None => lines.push("  - no unfinished run is recorded.".to_string()),
    }

    match claim {
        Some(held) => {
            let age = chrono::Utc::now().signed_duration_since(held.claimed_at);
            let expiry = if is_reclaimable(held) {
                " It is already past the staleness window, so the next run would reclaim it \
                 automatically — it is not what is blocking you."
                    .to_string()
            } else {
                String::new()
            };
            lines.push(format!(
                "  - a claim is held by {}, {} (since {}).{expiry}",
                held.claim_id,
                humanise(age),
                held.claimed_at.to_rfc3339()
            ));
        }
        None => lines.push("  - no claim is held.".to_string()),
    }

    lines
}

/// Render a claim age at the coarsest unit that still says something useful.
fn humanise(age: chrono::Duration) -> String {
    let seconds = age.num_seconds();
    // A clock skew between writer and reader can put `claimed_at` in the
    // future. Report it plainly rather than printing a negative duration.
    if seconds < 0 {
        return "held for an unknown time (its timestamp is in the future)".to_string();
    }
    let rendered = match seconds {
        s if s < 120 => format!("{s}s"),
        s if s < 7200 => format!("{}m", s / 60),
        s => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    };
    format!("held for {rendered}")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use cognee::database::{PipelineRun, PipelineRunClaim};

    fn claim_aged(seconds: i64) -> PipelineRunClaim {
        PipelineRunClaim {
            claim_id: Uuid::nil(),
            claimed_at: chrono::Utc::now() - chrono::Duration::seconds(seconds),
        }
    }

    fn started_run() -> PipelineRun {
        PipelineRun {
            id: Uuid::nil(),
            created_at: chrono::Utc::now(),
            status: PipelineRunStatus::Started,
            pipeline_run_id: Uuid::nil(),
            pipeline_name: "cognify_pipeline".to_string(),
            pipeline_id: Uuid::nil(),
            dataset_id: None,
            run_info: None,
        }
    }

    fn validation_message(result: Result<Vec<String>, CliError>) -> String {
        match result {
            Err(CliError::Validation(message)) => message,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// Reporting keeps the broad default: a dataset wedged on `temporal-cognify`
    /// must be found by an operator who only knows cognify failed.
    #[test]
    fn reporting_without_a_pipeline_covers_every_known_pipeline() {
        let resolved = resolve_pipelines(None, false).expect("reporting needs no pipeline");
        assert_eq!(resolved, KNOWN_PIPELINES);
    }

    /// The destructive path does not inherit that breadth. `--clear` on all
    /// three retires a live `memify_pipeline` run while the operator is
    /// unblocking cognify.
    #[test]
    fn clearing_without_a_pipeline_is_refused() {
        let message = validation_message(resolve_pipelines(None, true));
        assert!(
            message.contains("--pipeline"),
            "the error must name the flag to pass; got: {message}"
        );
        for known in KNOWN_PIPELINES {
            assert!(
                message.contains(known),
                "the error must list the names that are valid, missing {known}; got: {message}"
            );
        }
    }

    #[test]
    fn clearing_one_named_pipeline_is_allowed() {
        let resolved =
            resolve_pipelines(Some("memify_pipeline"), true).expect("a named pipeline clears");
        assert_eq!(resolved, vec!["memify_pipeline".to_string()]);
    }

    #[test]
    fn an_unknown_pipeline_is_refused_in_either_mode() {
        for clear in [false, true] {
            let message = validation_message(resolve_pipelines(Some("cognify"), clear));
            assert!(
                message.contains("Unknown pipeline"),
                "clear={clear}; got: {message}"
            );
        }
    }

    #[test]
    fn age_renders_at_a_useful_scale() {
        assert_eq!(humanise(chrono::Duration::seconds(45)), "held for 45s");
        assert_eq!(humanise(chrono::Duration::seconds(600)), "held for 10m");
        assert_eq!(
            humanise(chrono::Duration::seconds(3 * 3600 + 300)),
            "held for 3h 5m"
        );
    }

    /// The boundaries, because an off-by-one here reads as a wrong diagnosis:
    /// "119s" and "2m" are the same instant described two ways.
    #[test]
    fn age_switches_units_at_the_stated_boundaries() {
        assert_eq!(humanise(chrono::Duration::seconds(119)), "held for 119s");
        assert_eq!(humanise(chrono::Duration::seconds(120)), "held for 2m");
        assert_eq!(humanise(chrono::Duration::seconds(7199)), "held for 119m");
        assert_eq!(humanise(chrono::Duration::seconds(7200)), "held for 2h 0m");
    }

    #[test]
    fn a_future_timestamp_is_reported_rather_than_negated() {
        let rendered = humanise(chrono::Duration::seconds(-30));
        assert!(
            rendered.contains("future"),
            "expected the skew to be named, got {rendered:?}"
        );
    }

    /// The orphaned run must be named as the blocker that does not expire.
    /// Reporting only the claim is what made the first version of this command
    /// useless: the operator clears it and the next run fails identically.
    #[test]
    fn an_orphaned_run_is_reported_as_never_expiring() {
        let lines = describe(
            Some(&started_run()),
            None,
            "ds",
            Uuid::nil(),
            "cognify_pipeline",
        );
        let report = lines.join("\n");
        assert!(report.contains("never expires"), "got: {report}");
        assert!(report.contains("no claim is held"), "got: {report}");
    }

    /// A claim past the staleness window blocks nothing — the next run reclaims
    /// it. Telling an operator to release that one sends them to do work that
    /// changes nothing, and leaves them believing the real blocker is gone.
    #[test]
    fn a_stale_claim_is_reported_as_already_reclaimable() {
        let stale = CLAIM_STALE_AFTER.as_secs() as i64 + 60;
        let lines = describe(None, Some(&claim_aged(stale)), "ds", Uuid::nil(), "p");
        let report = lines.join("\n");
        assert!(report.contains("not what is blocking you"), "got: {report}");
    }

    #[test]
    fn a_fresh_claim_is_not_reported_as_reclaimable() {
        let lines = describe(None, Some(&claim_aged(30)), "ds", Uuid::nil(), "p");
        let report = lines.join("\n");
        assert!(
            !report.contains("not what is blocking you"),
            "a claim inside the window is a real blocker; got: {report}"
        );
    }
}
