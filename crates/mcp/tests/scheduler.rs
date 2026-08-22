#![cfg(feature = "runtime")]

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use cognee_mcp::scheduler::{ScheduledEvent, select_batch};
use cognee_mcp::spool::{Priority, SpoolFile};

fn candidate(priority: Priority, sequence: i64, not_before_offset: Option<i64>) -> ScheduledEvent {
    let event_id = format!("{sequence:064x}");
    ScheduledEvent {
        file: SpoolFile {
            path: PathBuf::from(format!("{sequence}.json")),
            priority,
            source_unix_nanos: sequence,
            event_id,
        },
        not_before: not_before_offset.map(|offset| {
            Utc.timestamp_opt(1_777_200_000 + offset, 0)
                .single()
                .expect("fixture not-before")
        }),
    }
}

#[test]
fn weighted_cycle_repeats_and_preserves_fifo_per_priority() {
    let now = Utc
        .timestamp_opt(1_777_200_000, 0)
        .single()
        .expect("fixture now");
    let mut candidates = Vec::new();
    for index in 0..20 {
        candidates.push(candidate(Priority::High, 1_000 + index, None));
        candidates.push(candidate(Priority::Low, 3_000 + index, None));
    }
    for index in 0..60 {
        candidates.push(candidate(Priority::Normal, 2_000 + index, None));
    }

    let selected = select_batch(candidates, now, 50);
    assert_eq!(selected.len(), 50);
    let priorities: Vec<_> = selected.iter().map(|event| event.priority).collect();
    assert_eq!(
        &priorities[..10],
        &[
            Priority::High,
            Priority::Normal,
            Priority::Normal,
            Priority::Normal,
            Priority::Low,
            Priority::High,
            Priority::Normal,
            Priority::Normal,
            Priority::Normal,
            Priority::Low,
        ]
    );
    assert_eq!(
        priorities
            .iter()
            .filter(|priority| **priority == Priority::High)
            .count(),
        10
    );
    assert_eq!(
        priorities
            .iter()
            .filter(|priority| **priority == Priority::Normal)
            .count(),
        30
    );
    assert_eq!(
        priorities
            .iter()
            .filter(|priority| **priority == Priority::Low)
            .count(),
        10
    );

    for priority in [Priority::High, Priority::Normal, Priority::Low] {
        let sequences: Vec<_> = selected
            .iter()
            .filter(|event| event.priority == priority)
            .map(|event| event.source_unix_nanos)
            .collect();
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    }
}

#[test]
fn not_before_is_skipped_without_reordering_eligible_peers() {
    let now = Utc
        .timestamp_opt(1_777_200_000, 0)
        .single()
        .expect("fixture now");
    let candidates = vec![
        candidate(Priority::Normal, 1, Some(60)),
        candidate(Priority::Normal, 2, None),
        candidate(Priority::Normal, 3, Some(0)),
        candidate(Priority::Normal, 4, Some(-1)),
    ];

    let selected = select_batch(candidates, now, 50);
    let sequences: Vec<_> = selected
        .iter()
        .map(|event| event.source_unix_nanos)
        .collect();
    assert_eq!(sequences, [2, 3, 4]);
    assert!(select_batch(Vec::new(), now, 0).is_empty());
}
