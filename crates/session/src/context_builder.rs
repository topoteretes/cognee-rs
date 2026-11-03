//! The deterministic builder, ranker, and candidate applier for the active
//! session-context layer.
//!
//! Port of Python's
//! `cognee/infrastructure/session/session_context_builder.py`. The builder
//! half loads the stored active [`SessionContextEntry`] rows, ranks them with
//! the lexical [`DeterministicRanker`] (no LLM), applies per-section and total
//! character budgets and renders the compact "Active session guidance" block
//! ([`build_active_context_block`]); durable preference lines share the
//! `preferences` section's budgets ([`fit_preference_lines`]). The candidate
//! half ([`apply_candidate_updates`]) turns proposed guidance into stored
//! entries: validate → confidence ≥ 0.75 → normalize → link an exact
//! duplicate or create a new entry. No LLM, no fuzzy matching, no
//! auto-delete.
//!
//! The `*_safe` builders are fail-open so they can never block answer
//! generation; the pure helpers are strict.

use std::collections::HashSet;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context_entry::{
    CONTEXT_ENTRY_KIND, CONTEXT_PROFILE_AGENT, CONTEXT_PROFILE_QA, MAX_CONTEXT_CONTENT_CHARS,
    SessionContextEntry, SessionContextRow, coerce_active_context_entries, normalize_content,
    valid_sections_for,
};
use crate::error::SessionError;
use crate::session_manager::SessionManager;

/// Per-section character budget of the rendered block.
pub const DEFAULT_PER_SECTION_CHAR_BUDGET: usize = 400;
/// Total character budget of the rendered block.
pub const DEFAULT_TOTAL_CHAR_BUDGET: usize = 1200;

/// `(section key, heading label)` of a [`CONTEXT_PROFILE_QA`] block, in
/// render order.
pub const SECTION_HEADINGS: &[(&str, &str)] = &[
    ("goals", "Goals"),
    ("rules", "Rules"),
    ("preferences", "Preferences"),
    ("lessons_learned", "Lessons learned"),
];
/// `(section key, heading label)` of a [`CONTEXT_PROFILE_AGENT`] block.
pub const AGENT_SECTION_HEADINGS: &[(&str, &str)] = &[
    ("tool_rules", "Tool rules"),
    ("workflow_state", "Workflow state"),
    ("success_patterns", "Success patterns"),
    ("failure_lessons", "Failure lessons"),
    ("environment_facts", "Environment facts"),
];

/// The section whose budgets durable preference lines share.
const PREFERENCES_SECTION: &str = "preferences";

/// First line of every rendered block.
pub const BLOCK_TITLE: &str = "## Active session guidance";
/// Second line of every rendered block.
pub const CONFLICT_INSTRUCTION: &str = "Items are listed oldest to newest within each section. \
     When guidance conflicts, prefer the later item.";

/// Net helpfulness is clamped so a run of ratings in one direction cannot
/// dominate a score.
pub const NET_HELP_CLAMP: i64 = 3;

/// Ordered `(section key, heading label)` pairs of a profile; unknown
/// profiles fall back to QA.
pub fn headings_for_profile(profile: &str) -> &'static [(&'static str, &'static str)] {
    match profile {
        CONTEXT_PROFILE_AGENT => AGENT_SECTION_HEADINGS,
        _ => SECTION_HEADINGS,
    }
}

/// `helpful_count - harmful_count`, clamped to `[-NET_HELP_CLAMP,
/// NET_HELP_CLAMP]`. The ranker scores with it; the distillation gate reads
/// its sign.
pub fn clamped_net_helpfulness(entry: &SessionContextEntry) -> i64 {
    let net = i64::try_from(entry.helpful_count).unwrap_or(i64::MAX)
        - i64::try_from(entry.harmful_count).unwrap_or(i64::MAX);
    net.clamp(-NET_HELP_CLAMP, NET_HELP_CLAMP)
}

/// Scores an active session-context entry against a query; higher sorts first.
pub trait ContextRanker: Send + Sync {
    fn score(&self, entry: &SessionContextEntry, query: &str) -> f64;
}

/// Pure-arithmetic ranker (Python's `DeterministicRanker`): a weighted sum of
/// section priority (rules > goals > preferences/lessons), confidence, query
/// token overlap, clamped net helpfulness, recency (newer `last_served_at` /
/// `created_at` sorts slightly higher) and the explicit priority.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeterministicRanker;

impl DeterministicRanker {
    // Weights chosen so section priority dominates, then confidence and
    // relevance, then signals.
    const W_SECTION: f64 = 10.0;
    const W_CONFIDENCE: f64 = 5.0;
    const W_OVERLAP: f64 = 4.0;
    const W_NET_HELP: f64 = 2.0;
    const W_PRIORITY: f64 = 1.0;
    const W_RECENCY: f64 = 0.5;

    fn section_priority(profile: &str, section: &str) -> f64 {
        let priority = match (profile, section) {
            (CONTEXT_PROFILE_AGENT, "failure_lessons" | "tool_rules") => 3,
            (CONTEXT_PROFILE_AGENT, "environment_facts" | "workflow_state") => 2,
            (CONTEXT_PROFILE_AGENT, "success_patterns") => 1,
            (CONTEXT_PROFILE_AGENT, _) => 0,
            (_, "rules") => 3,
            (_, "goals") => 2,
            (_, "preferences" | "lessons_learned") => 1,
            _ => 0,
        };
        f64::from(priority)
    }

    /// Share of the content's tokens that also occur in the query.
    fn query_overlap(content: &str, query: &str) -> f64 {
        let normalized = normalize_content(content);
        let content_tokens: HashSet<&str> =
            normalized.split(' ').filter(|t| !t.is_empty()).collect();
        if content_tokens.is_empty() {
            return 0.0;
        }
        let normalized_query = normalize_content(query);
        let query_tokens: HashSet<&str> = normalized_query
            .split(' ')
            .filter(|t| !t.is_empty())
            .collect();
        if query_tokens.is_empty() {
            return 0.0;
        }
        content_tokens.intersection(&query_tokens).count() as f64 / content_tokens.len() as f64
    }

    /// A small monotonic float for an ISO timestamp; missing or unparseable
    /// reads as 0.
    fn recency(entry: &SessionContextEntry) -> f64 {
        let stamp = entry
            .last_served_at
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&entry.created_at);
        match parse_iso(stamp) {
            // Scaled down so recency only tie-breaks.
            Some((_, unix_seconds)) => unix_seconds / 1.0e12,
            None => 0.0,
        }
    }
}

impl ContextRanker for DeterministicRanker {
    fn score(&self, entry: &SessionContextEntry, query: &str) -> f64 {
        Self::W_SECTION * Self::section_priority(&entry.context_profile, &entry.section)
            + Self::W_CONFIDENCE * entry.confidence
            + Self::W_OVERLAP * Self::query_overlap(&entry.content, query)
            + Self::W_NET_HELP * clamped_net_helpfulness(entry) as f64
            + Self::W_PRIORITY * entry.priority as f64
            + Self::W_RECENCY * Self::recency(entry)
    }
}

/// Parse what Python's `datetime.fromisoformat` accepts for our stamps: the
/// wall-clock time in the stamp's own offset (naive stamps as written) and
/// the Unix seconds (naive stamps read as UTC).
fn parse_iso(stamp: &str) -> Option<(NaiveDateTime, f64)> {
    let stamp = stamp.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(stamp) {
        let seconds =
            parsed.timestamp() as f64 + f64::from(parsed.timestamp_subsec_nanos()) / 1.0e9;
        return Some((parsed.naive_local(), seconds));
    }
    let naive = ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(stamp, format).ok())
        .or_else(|| {
            NaiveDate::parse_from_str(stamp, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
        })?;
    let seconds = naive.and_utc().timestamp() as f64
        + f64::from(naive.and_utc().timestamp_subsec_nanos()) / 1.0e9;
    Some((naive, seconds))
}

/// A compact `HH:MM:SS` label of an ISO timestamp; invalid or missing reads
/// as `unknown`.
fn time_label(stamp: &str) -> String {
    if stamp.is_empty() {
        return "unknown".to_string();
    }
    match parse_iso(stamp) {
        Some((local, _)) => local.format("%H:%M:%S").to_string(),
        None => "unknown".to_string(),
    }
}

/// Select the highest-scoring entries of one profile within the total and
/// per-section character budgets (Python's `select_context_entries`).
pub fn select_context_entries(
    entries: &[SessionContextEntry],
    query: &str,
    ranker: &dyn ContextRanker,
    per_section_char_budget: usize,
    total_char_budget: usize,
    context_profile: &str,
) -> Vec<SessionContextEntry> {
    let mut section_usage: Vec<(&str, usize)> = headings_for_profile(context_profile)
        .iter()
        .map(|(key, _)| (*key, 0))
        .collect();
    let mut total_used = 0;
    let mut scored: Vec<(f64, &SessionContextEntry)> = entries
        .iter()
        .map(|entry| (ranker.score(entry, query), entry))
        .collect();
    // Stable, highest score first: equal scores keep their stored order.
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut selected = Vec::new();
    for (_, entry) in scored {
        if entry.context_profile != context_profile {
            continue;
        }
        let Some((_, used)) = section_usage
            .iter_mut()
            .find(|(key, _)| *key == entry.section)
        else {
            continue;
        };
        let content = entry.content.trim();
        if content.is_empty() {
            continue;
        }
        let cost = content.chars().count();
        if *used + cost > per_section_char_budget || total_used + cost > total_char_budget {
            continue;
        }
        *used += cost;
        total_used += cost;
        selected.push(entry.clone());
    }
    selected
}

/// Fit durable preference lines (newest first) into the leftover
/// `preferences` budget (Python's `fit_preference_lines`).
///
/// The guidance block has one owner and one size limit, so durable preference
/// lines from the graph node share the `preferences` section's budgets with
/// this session's entries instead of forming a second block. Lines
/// duplicating an already-selected entry (or an earlier line) are skipped,
/// only whole lines are kept, and the kept lines are returned oldest first —
/// ready to render ahead of this session's entries so later items win
/// conflicts.
pub fn fit_preference_lines<S: AsRef<str>>(
    preference_lines: &[S],
    mut preferences_used: usize,
    mut total_used: usize,
    per_section_char_budget: usize,
    total_char_budget: usize,
    existing_normalized: &HashSet<String>,
) -> Vec<String> {
    let mut kept = Vec::new();
    let mut seen = existing_normalized.clone();
    for line in preference_lines {
        let content = line.as_ref().trim();
        if content.is_empty() {
            continue;
        }
        let normalized = normalize_content(content);
        if seen.contains(&normalized) {
            continue;
        }
        let cost = content.chars().count();
        if preferences_used + cost > per_section_char_budget
            || total_used + cost > total_char_budget
        {
            continue;
        }
        kept.push(content.to_string());
        seen.insert(normalized);
        preferences_used += cost;
        total_used += cost;
    }
    kept.reverse();
    kept
}

/// Assemble the block from `(heading label, bullet lines)` groups.
fn render_block(groups: &[(&str, Vec<String>)]) -> String {
    let mut lines = vec![BLOCK_TITLE.to_string(), CONFLICT_INSTRUCTION.to_string()];
    for (heading, bullets) in groups {
        if bullets.is_empty() {
            continue;
        }
        lines.push(format!("### {heading}"));
        lines.extend(bullets.iter().map(|bullet| format!("- {bullet}")));
    }
    lines.join("\n")
}

/// Render durable preference lines as a preferences-only guidance block, or
/// `""` when none fit (Python's `render_preference_block`).
///
/// Used when the stored-entry guidance layer is off (auto feedback disabled)
/// but preference personalization still has lines to serve: same owner, same
/// budgets, same block shape as [`build_active_context_block`].
pub fn render_preference_block<S: AsRef<str>>(preference_lines: &[S]) -> String {
    ActiveContextSelection::preferences_only().render(preference_lines)
}

/// The stored entries selected for one turn's guidance block, before durable
/// preference lines join the `Preferences` section.
///
/// The search retrievers load preference lines (per user and dataset) and
/// render the block themselves with [`Self::render`]; the entries are chosen
/// once per turn, so the served ids do not depend on the preference lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveContextSelection {
    /// Per heading: `(heading label, section key, bullets oldest first)`.
    sections: Vec<(&'static str, &'static str, Vec<String>)>,
    preferences_used: usize,
    total_used: usize,
    existing_normalized: HashSet<String>,
    per_section_char_budget: usize,
    total_char_budget: usize,
}

impl Default for ActiveContextSelection {
    /// No stored entries, default budgets: renders the preference lines alone.
    fn default() -> Self {
        Self::with_budgets(
            headings_for_profile(CONTEXT_PROFILE_QA),
            DEFAULT_PER_SECTION_CHAR_BUDGET,
            DEFAULT_TOTAL_CHAR_BUDGET,
        )
    }
}

impl ActiveContextSelection {
    fn with_budgets(
        headings: &'static [(&'static str, &'static str)],
        per_section_char_budget: usize,
        total_char_budget: usize,
    ) -> Self {
        Self {
            sections: headings
                .iter()
                .map(|(key, label)| (*label, *key, Vec::new()))
                .collect(),
            preferences_used: 0,
            total_used: 0,
            existing_normalized: HashSet::new(),
            per_section_char_budget,
            total_char_budget,
        }
    }

    /// The selection of a session whose stored-entry layer is off: only
    /// durable preference lines render.
    pub fn preferences_only() -> Self {
        Self::default()
    }

    /// Whether neither stored entries nor preference lines could render.
    pub fn is_empty_without_preferences(&self) -> bool {
        self.sections
            .iter()
            .all(|(_, _, bullets)| bullets.is_empty())
    }

    /// The block with `preference_lines` (newest first) merged into its
    /// `Preferences` section under the shared budgets, or `""` when there is
    /// nothing to render.
    pub fn render<S: AsRef<str>>(&self, preference_lines: &[S]) -> String {
        let fitted = if self
            .sections
            .iter()
            .any(|(_, key, _)| *key == PREFERENCES_SECTION)
        {
            fit_preference_lines(
                preference_lines,
                self.preferences_used,
                self.total_used,
                self.per_section_char_budget,
                self.total_char_budget,
                &self.existing_normalized,
            )
        } else {
            Vec::new()
        };
        if self.is_empty_without_preferences() && fitted.is_empty() {
            return String::new();
        }
        let groups: Vec<(&str, Vec<String>)> = self
            .sections
            .iter()
            .map(|(label, key, bullets)| {
                let mut bullets = bullets.clone();
                if *key == PREFERENCES_SECTION && !fitted.is_empty() {
                    // Durable lines lead the section: this session's entries
                    // come later, so under the conflict instruction they win.
                    bullets.splice(0..0, fitted.iter().cloned());
                }
                (*label, bullets)
            })
            .collect();
        render_block(&groups)
    }
}

/// Load, rank and budget the session's active entries for one profile and
/// stamp `last_served_at` on the selected ones (`stamp_served`). Returns the
/// selection and the served entry ids. Strict: a failed read is an error.
#[allow(clippy::too_many_arguments)]
pub async fn build_active_context_selection(
    manager: &SessionManager,
    session_id: &str,
    user_id: Option<&str>,
    query: &str,
    ranker: &dyn ContextRanker,
    per_section_char_budget: usize,
    total_char_budget: usize,
    context_profile: &str,
    stamp_served: bool,
) -> Result<(ActiveContextSelection, Vec<String>), SessionError> {
    let rows = manager
        .get_session_context_entries(Some(session_id), user_id)
        .await?;
    let entries = coerce_active_context_entries(&rows)?;
    let headings = headings_for_profile(context_profile);
    let selected = select_context_entries(
        &entries,
        query,
        ranker,
        per_section_char_budget,
        total_char_budget,
        context_profile,
    );

    let mut selection =
        ActiveContextSelection::with_budgets(headings, per_section_char_budget, total_char_budget);
    let mut by_section: Vec<Vec<&SessionContextEntry>> = vec![Vec::new(); headings.len()];
    for entry in &selected {
        if let Some(index) = headings.iter().position(|(key, _)| *key == entry.section) {
            by_section[index].push(entry);
        }
    }
    for (index, mut section_entries) in by_section.into_iter().enumerate() {
        // Oldest first; stable on equal stamps.
        section_entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        selection.sections[index].2 = section_entries
            .iter()
            .map(|entry| {
                format!(
                    "[{}] {}",
                    time_label(&entry.created_at),
                    entry.content.trim()
                )
            })
            .collect();
    }
    for entry in &selected {
        let cost = entry.content.trim().chars().count();
        selection.total_used += cost;
        if entry.section == PREFERENCES_SECTION {
            selection.preferences_used += cost;
        }
        selection
            .existing_normalized
            .insert(normalize_content(&entry.content));
    }

    let served_ids: Vec<String> = selected.iter().map(|entry| entry.id.clone()).collect();
    if stamp_served {
        stamp_served_entries(manager, session_id, user_id, &served_ids).await;
    }
    Ok((selection, served_ids))
}

/// [`build_active_context_selection`] with the default ranker, budgets and
/// QA profile, fail-open: any error yields an empty selection and no ids,
/// so guidance can never block answer generation.
pub async fn build_active_context_selection_safe(
    manager: &SessionManager,
    session_id: &str,
    user_id: Option<&str>,
    query: &str,
    stamp_served: bool,
) -> (ActiveContextSelection, Vec<String>) {
    match build_active_context_selection(
        manager,
        session_id,
        user_id,
        query,
        &DeterministicRanker,
        DEFAULT_PER_SECTION_CHAR_BUDGET,
        DEFAULT_TOTAL_CHAR_BUDGET,
        CONTEXT_PROFILE_QA,
        stamp_served,
    )
    .await
    {
        Ok(built) => built,
        Err(error) => {
            tracing::debug!(%error, "falling back after an error building the active context");
            (ActiveContextSelection::default(), Vec::new())
        }
    }
}

/// Load active context entries, rank them, budget-cap and render a compact
/// block (Python's `build_active_context_block`).
///
/// Renders only `context_profile` entries. `preference_lines` (durable stated
/// preferences, newest first) are merged into the `Preferences` section as
/// one more source, under the same budgets and with duplicate lines skipped.
/// With `stamp_served` false the call performs no writes (read-only recall
/// and previews); otherwise `last_served_at` is stamped on the rendered
/// entries. Returns `(block, served_ids)`; the block is `""` when nothing
/// renders. Strict: a failed read is an error, see
/// [`build_active_context_block_safe`] for the fail-open form.
#[allow(clippy::too_many_arguments)]
pub async fn build_active_context_block<S: AsRef<str>>(
    manager: &SessionManager,
    session_id: &str,
    user_id: Option<&str>,
    query: &str,
    ranker: &dyn ContextRanker,
    per_section_char_budget: usize,
    total_char_budget: usize,
    context_profile: &str,
    stamp_served: bool,
    preference_lines: &[S],
) -> Result<(String, Vec<String>), SessionError> {
    let (selection, served_ids) = build_active_context_selection(
        manager,
        session_id,
        user_id,
        query,
        ranker,
        per_section_char_budget,
        total_char_budget,
        context_profile,
        stamp_served,
    )
    .await?;
    Ok((selection.render(preference_lines), served_ids))
}

/// [`build_active_context_block`] with the default ranker, budgets and QA
/// profile, fail-open: any error yields `("", [])`.
pub async fn build_active_context_block_safe<S: AsRef<str>>(
    manager: &SessionManager,
    session_id: &str,
    user_id: Option<&str>,
    query: &str,
    stamp_served: bool,
    preference_lines: &[S],
) -> (String, Vec<String>) {
    match build_active_context_block(
        manager,
        session_id,
        user_id,
        query,
        &DeterministicRanker,
        DEFAULT_PER_SECTION_CHAR_BUDGET,
        DEFAULT_TOTAL_CHAR_BUDGET,
        CONTEXT_PROFILE_QA,
        stamp_served,
        preference_lines,
    )
    .await
    {
        Ok(built) => built,
        Err(error) => {
            tracing::debug!(%error, "falling back after an error building the active context");
            (String::new(), Vec::new())
        }
    }
}

/// Best-effort `last_served_at` stamp on the entries rendered into a block.
async fn stamp_served_entries(
    manager: &SessionManager,
    session_id: &str,
    user_id: Option<&str>,
    entry_ids: &[String],
) {
    if entry_ids.is_empty() {
        return;
    }
    let served_at = Utc::now().to_rfc3339();
    for entry_id in entry_ids {
        let mut merge = SessionContextRow::new();
        merge.insert(
            "last_served_at".to_string(),
            Value::String(served_at.clone()),
        );
        if let Err(error) = manager
            .update_session_context_entry(Some(session_id), user_id, entry_id, merge)
            .await
        {
            tracing::debug!(%error, "skipping an entry after an error stamping last_served_at");
        }
    }
}

/// The one confidence threshold: a candidate below it is not stored
/// (Python's `GATE_CONFIDENCE` / `MIN_CANDIDATE_CONFIDENCE`).
pub const MIN_CANDIDATE_CONFIDENCE: f64 = 0.75;

/// A proposed new guidance entry (Python's `CandidateContextUpdate` and its
/// agent variant). Built by an extractor; [`apply_candidate_updates`] decides
/// whether it is stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateContextUpdate {
    /// [`CONTEXT_PROFILE_QA`] or [`CONTEXT_PROFILE_AGENT`].
    #[serde(default = "qa_profile")]
    pub context_profile: String,
    pub section: String,
    pub content: String,
    #[serde(default)]
    pub confidence: f64,
}

fn qa_profile() -> String {
    CONTEXT_PROFILE_QA.to_string()
}

impl CandidateContextUpdate {
    /// A candidate for `profile`, with Python's field validators applied:
    /// the section is trimmed and lowercased, the content stripped and cut to
    /// [`MAX_CONTEXT_CONTENT_CHARS`], the confidence clamped to `[0, 1]`
    /// (`NaN` reads as 0).
    pub fn new(profile: &str, section: &str, content: &str, confidence: f64) -> Self {
        Self {
            context_profile: profile.to_string(),
            section: section.trim().to_lowercase(),
            content: content
                .trim()
                .chars()
                .take(MAX_CONTEXT_CONTENT_CHARS)
                .collect(),
            confidence: if confidence.is_nan() {
                0.0
            } else {
                confidence.clamp(0.0, 1.0)
            },
        }
    }
}

/// Apply candidates to the session's context entries; returns the ids of the
/// entries touched or created, in candidate order.
///
/// For each candidate: its section must be valid for its profile, its
/// content non-empty and at most [`MAX_CONTEXT_CONTENT_CHARS`], its
/// confidence at least [`MIN_CANDIDATE_CONFIDENCE`]. An entry of the same
/// profile and section with identical normalized content is a duplicate: it
/// gains `source_id` (a feedback id for `qa`, a trace id for `agent`; an
/// empty id links nothing) and is returned. Otherwise a new entry is created.
///
/// Fail-open per candidate, as in Python: a store error skips that candidate
/// (logged) and the rest still apply.
pub async fn apply_candidate_updates(
    manager: &SessionManager,
    session_id: &str,
    user_id: &str,
    source_id: &str,
    candidates: &[CandidateContextUpdate],
) -> Vec<String> {
    apply_candidate_updates_for_user(manager, session_id, Some(user_id), source_id, candidates)
        .await
}

/// Variant for SDK callers whose session key has no explicit user id.
pub async fn apply_candidate_updates_for_user(
    manager: &SessionManager,
    session_id: &str,
    user_id: Option<&str>,
    source_id: &str,
    candidates: &[CandidateContextUpdate],
) -> Vec<String> {
    let mut touched = Vec::new();
    for candidate in candidates {
        match apply_single_candidate(manager, session_id, user_id, source_id, candidate).await {
            Ok(Some(id)) => touched.push(id),
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(%error, "skipping a candidate context update after an error");
            }
        }
    }
    touched
}

async fn apply_single_candidate(
    manager: &SessionManager,
    session_id: &str,
    user_id: Option<&str>,
    source_id: &str,
    candidate: &CandidateContextUpdate,
) -> Result<Option<String>, SessionError> {
    let profile = candidate.context_profile.as_str();
    let section = candidate.section.as_str();
    if !valid_sections_for(profile).contains(&section) {
        return Ok(None);
    }
    let content = candidate.content.trim();
    if content.is_empty() || content.chars().count() > MAX_CONTEXT_CONTENT_CHARS {
        return Ok(None);
    }
    if candidate.confidence < MIN_CANDIDATE_CONFIDENCE {
        return Ok(None);
    }

    let normalized = normalize_content(content);
    let source_field = if profile == CONTEXT_PROFILE_AGENT {
        "source_trace_ids"
    } else {
        "source_feedback_ids"
    };

    let rows = manager
        .get_session_context_entries(Some(session_id), user_id)
        .await?;
    if let Some(duplicate) = rows
        .iter()
        .find(|row| is_duplicate(row, profile, section, &normalized))
    {
        let entry_id = duplicate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut source_ids: Vec<Value> = duplicate
            .get(source_field)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !source_id.is_empty() && !source_ids.iter().any(|id| id.as_str() == Some(source_id)) {
            source_ids.push(Value::String(source_id.to_string()));
            let mut merge = SessionContextRow::new();
            merge.insert(source_field.to_string(), Value::Array(source_ids));
            manager
                .update_session_context_entry(Some(session_id), user_id, &entry_id, merge)
                .await?;
        }
        return Ok(Some(entry_id));
    }

    let linked: Vec<String> = if source_id.is_empty() {
        Vec::new()
    } else {
        vec![source_id.to_string()]
    };
    let is_agent = profile == CONTEXT_PROFILE_AGENT;
    let entry = SessionContextEntry {
        id: uuid::Uuid::new_v4().to_string(),
        section: section.to_string(),
        context_profile: profile.to_string(),
        content: content.to_string(),
        normalized_content: normalized,
        confidence: candidate.confidence,
        created_at: Utc::now().to_rfc3339(),
        source_feedback_ids: if is_agent { Vec::new() } else { linked.clone() },
        source_trace_ids: if is_agent { linked } else { Vec::new() },
        helpful_count: 0,
        harmful_count: 0,
        priority: 0,
        last_served_at: None,
        embedding: None,
        kind: CONTEXT_ENTRY_KIND.to_string(),
    };
    manager
        .create_session_context_entry(Some(session_id), user_id, entry.to_row()?)
        .await?;
    Ok(Some(entry.id))
}

/// A stored `"context"` row of `profile`/`section` with this normalized
/// content (rows written by Python may omit `kind` and `context_profile`).
fn is_duplicate(row: &SessionContextRow, profile: &str, section: &str, normalized: &str) -> bool {
    let field = |key: &str, default: &str| -> bool {
        match row.get(key) {
            None => true,
            Some(Value::String(value)) => value == default,
            Some(_) => false,
        }
    };
    let text = |key: &str| row.get(key).and_then(Value::as_str);
    field("kind", CONTEXT_ENTRY_KIND)
        && match row.get("context_profile") {
            None => profile == CONTEXT_PROFILE_QA,
            Some(_) => field("context_profile", profile),
        }
        && text("section") == Some(section)
        && text("normalized_content") == Some(normalized)
}

#[cfg(all(test, feature = "fs"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::FsSessionStore;

    fn manager(dir: &tempfile::TempDir) -> SessionManager {
        SessionManager::new(Arc::new(FsSessionStore::new(dir.path())))
    }

    async fn rows(manager: &SessionManager) -> Vec<SessionContextRow> {
        manager
            .get_session_context_entries(Some("s"), Some("u"))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_confident_candidate_is_created_with_its_source() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        let candidate =
            CandidateContextUpdate::new(CONTEXT_PROFILE_AGENT, "Tool_Rules", "Run uv sync", 0.9);
        let touched = apply_candidate_updates(&manager, "s", "u", "t1", &[candidate]).await;
        assert_eq!(touched.len(), 1);
        let rows = rows(&manager).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["section"], "tool_rules");
        assert_eq!(rows[0]["context_profile"], "agent");
        assert_eq!(rows[0]["source_trace_ids"], serde_json::json!(["t1"]));
        assert_eq!(rows[0]["source_feedback_ids"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn low_confidence_bad_section_and_empty_content_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        let candidates = [
            CandidateContextUpdate::new(CONTEXT_PROFILE_AGENT, "tool_rules", "Weak", 0.74),
            CandidateContextUpdate::new(CONTEXT_PROFILE_AGENT, "goals", "Wrong section", 0.9),
            CandidateContextUpdate::new(CONTEXT_PROFILE_QA, "rules", "   ", 0.9),
        ];
        assert!(
            apply_candidate_updates(&manager, "s", "u", "", &candidates)
                .await
                .is_empty()
        );
        assert!(rows(&manager).await.is_empty());
    }

    #[tokio::test]
    async fn an_exact_duplicate_links_the_new_source_instead_of_a_new_entry() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        let first = CandidateContextUpdate::new(CONTEXT_PROFILE_QA, "rules", "Cite sources", 0.8);
        let again =
            CandidateContextUpdate::new(CONTEXT_PROFILE_QA, "rules", "  CITE   sources", 0.9);
        let a = apply_candidate_updates(&manager, "s", "u", "f1", &[first]).await;
        let b =
            apply_candidate_updates(&manager, "s", "u", "f2", std::slice::from_ref(&again)).await;
        assert_eq!(a, b);
        // The same source again links nothing new.
        apply_candidate_updates(&manager, "s", "u", "f2", &[again]).await;
        let rows = rows(&manager).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["source_feedback_ids"],
            serde_json::json!(["f1", "f2"])
        );
    }

    fn entry(section: &str, content: &str, id: &str) -> SessionContextEntry {
        SessionContextEntry {
            id: id.to_string(),
            section: section.to_string(),
            context_profile: CONTEXT_PROFILE_QA.to_string(),
            content: content.to_string(),
            normalized_content: normalize_content(content),
            confidence: 0.8,
            created_at: Utc::now().to_rfc3339(),
            source_feedback_ids: Vec::new(),
            source_trace_ids: Vec::new(),
            helpful_count: 0,
            harmful_count: 0,
            priority: 0,
            last_served_at: None,
            embedding: None,
            kind: CONTEXT_ENTRY_KIND.to_string(),
        }
    }

    async fn seed(manager: &SessionManager, entries: &[SessionContextEntry]) {
        for entry in entries {
            manager
                .create_session_context_entry(Some("s"), Some("u"), entry.to_row().unwrap())
                .await
                .unwrap();
        }
    }

    async fn build(
        manager: &SessionManager,
        query: &str,
        preference_lines: &[&str],
    ) -> (String, Vec<String>) {
        build_active_context_block_safe(manager, "s", Some("u"), query, true, preference_lines)
            .await
    }

    // ---- ranker (Python `test_ranker_*`)

    #[test]
    fn the_ranker_orders_by_section_confidence_overlap_and_helpfulness() {
        let ranker = DeterministicRanker;
        let rule = entry("rules", "always use tabs", "r");
        let goal = entry("goals", "ship the feature", "g");
        assert!(ranker.score(&rule, "anything") > ranker.score(&goal, "anything"));

        let mut high = entry("goals", "high confidence goal", "h");
        high.confidence = 0.95;
        let mut low = entry("goals", "low confidence goal", "l");
        low.confidence = 0.1;
        assert!(ranker.score(&high, "unrelated") > ranker.score(&low, "unrelated"));

        let preference = entry("preferences", "use postgres database", "p");
        assert!(
            ranker.score(&preference, "how do I configure postgres database")
                > ranker.score(&preference, "completely different topic")
        );

        let mut helpful = entry("goals", "g", "a");
        helpful.helpful_count = 5;
        let mut harmful = entry("goals", "g", "b");
        harmful.harmful_count = 5;
        assert!(ranker.score(&helpful, "x") > ranker.score(&harmful, "x"));
    }

    #[test]
    fn clamped_helpfulness_keeps_section_priority_dominant() {
        let ranker = DeterministicRanker;
        let mut rule = entry("rules", "rule", "r");
        rule.confidence = 0.0;
        let mut preference = entry("preferences", "preference", "p");
        preference.confidence = 0.0;
        preference.helpful_count = 100;
        assert_eq!(clamped_net_helpfulness(&preference), NET_HELP_CLAMP);
        assert!(ranker.score(&rule, "x") > ranker.score(&preference, "x"));
    }

    // ---- builder (Python `test_build_*`)

    #[tokio::test]
    async fn an_empty_session_renders_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        assert_eq!(build(&manager, "q", &[]).await, (String::new(), vec![]));
    }

    #[tokio::test]
    async fn the_block_has_the_exact_python_shape_and_stamps_served_entries() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        let mut goal = entry("goals", "Ship the MVP", "g1");
        goal.created_at = "2026-06-10T10:00:01".to_string();
        let mut rule = entry("rules", "Always run ruff", "r1");
        rule.created_at = "2026-06-10T10:00:02+00:00".to_string();
        let mut newer = entry("preferences", "Prefer 4 concise bullet points.", "p-new");
        newer.created_at = "2026-06-10T10:19:08".to_string();
        let mut older = entry(
            "preferences",
            "Prefer 2 informative bullet points.",
            "p-old",
        );
        older.created_at = "2026-06-10T10:14:22".to_string();
        let mut lesson = entry("lessons_learned", "Redis unavailable locally", "l1");
        lesson.created_at = "not a timestamp".to_string();
        seed(&manager, &[goal, rule, newer, older, lesson]).await;

        let (block, served) = build(&manager, "bullet points", &[]).await;

        assert_eq!(
            block,
            "## Active session guidance\n\
             Items are listed oldest to newest within each section. \
             When guidance conflicts, prefer the later item.\n\
             ### Goals\n- [10:00:01] Ship the MVP\n\
             ### Rules\n- [10:00:02] Always run ruff\n\
             ### Preferences\n\
             - [10:14:22] Prefer 2 informative bullet points.\n\
             - [10:19:08] Prefer 4 concise bullet points.\n\
             ### Lessons learned\n- [unknown] Redis unavailable locally"
        );
        let mut served_sorted = served.clone();
        served_sorted.sort();
        assert_eq!(served_sorted, ["g1", "l1", "p-new", "p-old", "r1"]);
        for row in rows(&manager).await {
            assert!(row["last_served_at"].is_string(), "{row:?}");
        }
    }

    #[tokio::test]
    async fn stamp_served_false_performs_no_writes() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        seed(&manager, &[entry("goals", "Ship the MVP", "g1")]).await;
        let (block, served) =
            build_active_context_block_safe(&manager, "s", Some("u"), "q", false, &[] as &[&str])
                .await;
        assert!(block.contains("Ship the MVP"));
        assert_eq!(served, ["g1"]);
        assert!(rows(&manager).await[0]["last_served_at"].is_null());
    }

    #[tokio::test]
    async fn the_total_and_per_section_budgets_cap_the_selection() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        let entries: Vec<_> = (0..20)
            .map(|i| entry("goals", &"x".repeat(100), &format!("g{i}")))
            .collect();
        seed(&manager, &entries).await;
        let manager = &manager;
        let selected = async |per, total| {
            build_active_context_block(
                manager,
                "s",
                Some("u"),
                "q",
                &DeterministicRanker,
                per,
                total,
                CONTEXT_PROFILE_QA,
                false,
                &[] as &[&str],
            )
            .await
            .unwrap()
            .1
            .len()
        };
        // 300 / 100 chars: at most three fit overall; 250 per section: two.
        assert_eq!(selected(10_000, 300).await, 3);
        assert_eq!(selected(250, 10_000).await, 2);
        // The defaults: 400 chars per section.
        assert_eq!(
            selected(DEFAULT_PER_SECTION_CHAR_BUDGET, DEFAULT_TOTAL_CHAR_BUDGET).await,
            4
        );
    }

    #[tokio::test]
    async fn feedback_rows_and_other_profiles_are_never_rendered() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        seed(&manager, &[entry("goals", "real goal", "g1")]).await;
        let mut agent = entry("tool_rules", "agent only", "a1");
        agent.context_profile = CONTEXT_PROFILE_AGENT.to_string();
        seed(&manager, &[agent]).await;
        let mut feedback = SessionContextRow::new();
        feedback.insert("id".into(), "f1".into());
        feedback.insert("kind".into(), "feedback".into());
        feedback.insert("section".into(), "goals".into());
        feedback.insert("content".into(), "fb".into());
        manager
            .create_session_context_entry(Some("s"), Some("u"), feedback)
            .await
            .unwrap();
        let (_, served) = build(&manager, "q", &[]).await;
        assert_eq!(served, ["g1"]);
    }

    #[tokio::test]
    async fn a_malformed_stored_entry_fails_open_to_no_guidance() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        let mut bad = entry("goals", "ok", "g1").to_row().unwrap();
        bad.insert("section".into(), "nonsense".into());
        manager
            .create_session_context_entry(Some("s"), Some("u"), bad)
            .await
            .unwrap();
        assert_eq!(build(&manager, "q", &["Keep it short."]).await.1.len(), 0);
        assert!(
            build_active_context_block(
                &manager,
                "s",
                Some("u"),
                "q",
                &DeterministicRanker,
                400,
                1200,
                CONTEXT_PROFILE_QA,
                false,
                &[] as &[&str],
            )
            .await
            .is_err()
        );
    }

    // ---- preference lines (Python `test_build_*preference*`)

    #[tokio::test]
    async fn preference_lines_lead_the_preferences_section_of_one_block() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        seed(
            &manager,
            &[entry("preferences", "Prefer bullet points.", "p1")],
        )
        .await;
        let (block, served) = build(&manager, "q", &["Keep answers concise."]).await;
        assert_eq!(block.matches("### Preferences").count(), 1);
        assert!(
            block.find("Keep answers concise.").unwrap()
                < block.find("Prefer bullet points.").unwrap()
        );
        assert_eq!(served, ["p1"]);
    }

    #[tokio::test]
    async fn preference_lines_duplicating_selected_entries_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        seed(
            &manager,
            &[entry("preferences", "Keep answers concise.", "p1")],
        )
        .await;
        let (block, _) = build(&manager, "q", &["keep  answers CONCISE.", "Prefer tables."]).await;
        assert_eq!(
            block
                .to_lowercase()
                .matches("keep answers concise.")
                .count(),
            1
        );
        assert!(block.contains("- Prefer tables."));
    }

    #[tokio::test]
    async fn preference_lines_share_the_section_budget() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        seed(&manager, &[entry("preferences", &"a".repeat(90), "p1")]).await;
        let (selection, _) = build_active_context_selection(
            &manager,
            "s",
            Some("u"),
            "q",
            &DeterministicRanker,
            100,
            10_000,
            CONTEXT_PROFILE_QA,
            false,
        )
        .await
        .unwrap();
        let block = selection.render(&["b".repeat(20), "c".repeat(5)]);
        assert!(block.contains(&"c".repeat(5)));
        assert!(!block.contains(&"b".repeat(20)));
    }

    #[tokio::test]
    async fn preference_lines_render_without_stored_entries() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager(&dir);
        let (block, served) = build(&manager, "q", &["Keep answers concise."]).await;
        assert!(block.contains("### Preferences\n- Keep answers concise."));
        assert!(served.is_empty());
    }

    #[test]
    fn fit_preference_lines_keeps_newest_within_budget_oldest_first() {
        let kept = fit_preference_lines(
            &["newest", "middle", "oldest"],
            0,
            0,
            12,
            10_000,
            &HashSet::new(),
        );
        assert_eq!(kept, ["middle", "newest"]);
    }

    #[test]
    fn the_preference_block_is_the_same_block_shape() {
        assert_eq!(
            render_preference_block(&["Keep answers concise."]),
            "## Active session guidance\n\
             Items are listed oldest to newest within each section. \
             When guidance conflicts, prefer the later item.\n\
             ### Preferences\n- Keep answers concise."
        );
        assert_eq!(render_preference_block::<&str>(&[]), "");
    }

    #[test]
    fn the_candidate_validators_match_python() {
        let long = "x".repeat(400);
        let candidate =
            CandidateContextUpdate::new(CONTEXT_PROFILE_AGENT, " Failure_Lessons ", &long, 7.0);
        assert_eq!(candidate.section, "failure_lessons");
        assert_eq!(candidate.content.chars().count(), MAX_CONTEXT_CONTENT_CHARS);
        assert_eq!(candidate.confidence, 1.0);
        assert_eq!(
            CandidateContextUpdate::new(CONTEXT_PROFILE_QA, "rules", "x", f64::NAN).confidence,
            0.0
        );
    }
}
