//! Inspect and clear the exclusive-run claim on a dataset (SDK-616).
//!
//! The claim is what stops two cognify runs colliding on one dataset. It is
//! released on every graceful exit path, errors included — but a killed process
//! cannot release, so its row outlives it and refuses every later run on the
//! pair until it ages out a day later. Before this command there was no way to
//! clear one: the existing release is scoped to the holder's `claim_id`, which
//! is precisely what an operator staring at a dead process does not have.
//!
//! Reporting is the default and releasing is opt-in, because the distinction
//! that matters cannot be made from here. A claim older than the staleness
//! window is already reclaimed automatically, so anything this command finds is
//! *young* — either a live run, or a holder that died minutes ago. Only the
//! operator knows which.

use std::sync::Arc;

use cognee::database::{PipelineRunRepository, SeaOrmPipelineRunRepository, ops};
use cognee::{ComponentManager, PipelineContext};
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::PipelineClaimArgs;
use crate::error::CliError;

pub fn run(args: PipelineClaimArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    // Scoped so the settings read guard is dropped before the async block below
    // captures `cm` — holding it across the await would keep a lock alive for
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

        let Some(claim) = repo
            .get_pipeline_run_claim(dataset.id, &args.pipeline)
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?
        else {
            info!(
                "No '{}' claim is held on dataset '{}' ({}). A run refused as already-running is \
                 not being refused by this claim.",
                args.pipeline, args.dataset, dataset.id
            );
            return Ok(());
        };

        // Whole seconds: the value of this number is its order of magnitude —
        // "two minutes" versus "nine hours" is what tells an operator whether
        // to suspect a crash or a long run still going.
        let age = chrono::Utc::now().signed_duration_since(claim.claimed_at);
        let age_note = format!(
            "held for {} (since {})",
            humanise(age),
            claim.claimed_at.to_rfc3339()
        );

        if !args.release {
            info!(
                "Dataset '{}' ({}) has a '{}' claim, {age_note}, holder {}.\n\
                 If that run is still going, leave it alone — releasing would let a second run \
                 start on the same dataset.\n\
                 If its process is gone, clear it with: \
                 cognee-cli pipeline-claim -d {} --pipeline {} --release",
                args.dataset,
                dataset.id,
                args.pipeline,
                claim.claim_id,
                args.dataset,
                args.pipeline,
            );
            return Ok(());
        }

        warn!(
            "Releasing the '{}' claim on dataset '{}' ({}), {age_note}, holder {}.",
            args.pipeline, args.dataset, dataset.id, claim.claim_id
        );

        let released = repo
            .force_release_pipeline_run_claim(dataset.id, &args.pipeline)
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        if released {
            info!(
                "Released. '{}' can run on dataset '{}' again.",
                args.pipeline, args.dataset
            );
        } else {
            // Between the read and the delete the holder finished and released
            // it. Nothing is wrong, and saying "released" would be a lie.
            info!(
                "Nothing to release — the '{}' claim on dataset '{}' was given up while this \
                 command ran.",
                args.pipeline, args.dataset
            );
        }
        Ok(())
    })
}

/// Render a claim age at the coarsest unit that still says something useful.
fn humanise(age: chrono::Duration) -> String {
    let seconds = age.num_seconds();
    // A clock skew between writer and reader can put `claimed_at` in the
    // future. Report it plainly rather than printing a negative duration.
    if seconds < 0 {
        return "an unknown time (its timestamp is in the future)".to_string();
    }
    match seconds {
        s if s < 120 => format!("{s}s"),
        s if s < 7200 => format!("{}m", s / 60),
        s => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_renders_at_a_useful_scale() {
        assert_eq!(humanise(chrono::Duration::seconds(45)), "45s");
        assert_eq!(humanise(chrono::Duration::seconds(600)), "10m");
        assert_eq!(humanise(chrono::Duration::seconds(3 * 3600 + 300)), "3h 5m");
    }

    /// The boundaries, because an off-by-one here reads as a wrong diagnosis:
    /// "119s" and "2m" are the same instant described two ways.
    #[test]
    fn age_switches_units_at_the_stated_boundaries() {
        assert_eq!(humanise(chrono::Duration::seconds(119)), "119s");
        assert_eq!(humanise(chrono::Duration::seconds(120)), "2m");
        assert_eq!(humanise(chrono::Duration::seconds(7199)), "119m");
        assert_eq!(humanise(chrono::Duration::seconds(7200)), "2h 0m");
    }

    /// Clock skew must not print a negative age, which would read as nonsense
    /// at exactly the moment an operator is trying to judge whether to release.
    #[test]
    fn a_future_timestamp_is_reported_rather_than_negated() {
        let rendered = humanise(chrono::Duration::seconds(-30));
        assert!(
            rendered.contains("future"),
            "expected the skew to be named, got {rendered:?}"
        );
    }
}
