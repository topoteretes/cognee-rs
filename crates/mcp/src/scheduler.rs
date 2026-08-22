//! Deterministic weighted FIFO selection for bounded drain batches.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};

use crate::spool::{Priority, SpoolFile};

const PRIORITY_CYCLE: [Priority; 5] = [
    Priority::High,
    Priority::Normal,
    Priority::Normal,
    Priority::Normal,
    Priority::Low,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledEvent {
    pub file: SpoolFile,
    pub not_before: Option<DateTime<Utc>>,
}

pub fn select_batch(
    candidates: Vec<ScheduledEvent>,
    now: DateTime<Utc>,
    limit: usize,
) -> Vec<SpoolFile> {
    if limit == 0 {
        return Vec::new();
    }

    let mut eligible: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.not_before.is_none_or(|time| time <= now))
        .collect();
    eligible.sort_by(|left, right| {
        left.file
            .source_unix_nanos
            .cmp(&right.file.source_unix_nanos)
            .then_with(|| left.file.event_id.cmp(&right.file.event_id))
    });

    let mut high = VecDeque::new();
    let mut normal = VecDeque::new();
    let mut low = VecDeque::new();
    for candidate in eligible {
        match candidate.file.priority {
            Priority::High => high.push_back(candidate.file),
            Priority::Normal => normal.push_back(candidate.file),
            Priority::Low => low.push_back(candidate.file),
        }
    }

    let mut selected = Vec::with_capacity(limit.min(high.len() + normal.len() + low.len()));
    let mut cycle_index = 0usize;
    while selected.len() < limit && !(high.is_empty() && normal.is_empty() && low.is_empty()) {
        let priority = PRIORITY_CYCLE[cycle_index];
        cycle_index = (cycle_index + 1) % PRIORITY_CYCLE.len();
        let next = match priority {
            Priority::High => high.pop_front(),
            Priority::Normal => normal.pop_front(),
            Priority::Low => low.pop_front(),
        };
        if let Some(next) = next {
            selected.push(next);
        }
    }
    selected
}
