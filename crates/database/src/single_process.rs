//! Is exactly one process using this relational database?
//!
//! The question has one consumer today — whether startup may clear the
//! recovery state a killed run left behind (an orphaned `pipeline_runs` row
//! and its exclusive-run claim) — and both of those clears are unscoped, so
//! both are sound only when no peer process can be holding what they drop.
//!
//! It lives in `cognee-database` rather than in `cognee-utils` because the
//! derivation must agree with [`crate::sqlite_url_is_in_memory`], which is the
//! canonical answer to "does this URL name a database only this process can
//! see". `cognee-utils` is a dependency *of* this crate, so it cannot reach
//! that predicate; every caller that needs this one — `cognee`,
//! `cognee-bindings-common`, and `cognee-http-server` from the closed
//! `cognee-cloud-rs` repo — already depends on `cognee-database`.

use crate::connection::sqlite_url_is_in_memory;

/// Env var asserting (or denying) that exactly one process uses the relational
/// database.
///
/// Truthy per [`cognee_utils::parse_env_bool`]; any other non-whitespace value
/// is an explicit `false`, so an operator can switch the assertion off as well
/// as on. Unset, empty or whitespace-only means "derive it" — see
/// [`single_process_default`].
pub const SINGLE_PROCESS_ENV: &str = "COGNEE_SINGLE_PROCESS";

/// The single-process assertion derived from the relational DB URL alone.
///
/// **Only an in-memory SQLite database derives `true`.** Such a database is
/// private to the process that opened it: no peer can see it, so there is
/// provably nobody whose state a startup sweep could destroy.
///
/// Everything else derives `false`, **file-backed SQLite included**. This is
/// the case that is easy to get wrong: the shipped default relational URL is
/// `sqlite:./cognee.db?mode=rwc`, a *file*, and every cognee process started
/// in that directory opens the same one. Two Python or C-API embedders, or an
/// HTTP server restarting while `cognee-cli cognify` runs, would each be
/// "single-process" under a naive `starts_with("sqlite")` test and would then
/// delete a live sibling's claim — re-admitting exactly the concurrent run the
/// claim exists to prevent.
///
/// # The `true` branch is inert, and that is fine
///
/// An in-memory database dies with its process, so it can never *hold* a
/// leftover from a dead run: after a kill the next process opens an empty one.
/// The derived `true` therefore never gives the sweep anything to do, and in
/// practice **the sweep runs only under an explicit opt-in**
/// (`COGNEE_SINGLE_PROCESS=1` / `single_process = true`).
///
/// The value here is the `false` half. It is the H2 regression — "SQLite
/// implies single process" — in executable form, guarded by
/// `file_backed_sqlite_does_not_derive_single_process` below, and it is the
/// predicate any future caller asking "may I clear shared state at startup?"
/// should reach for rather than re-deriving badly. (The HTTP server's
/// unconditional `reset_orphans`, which still runs for multi-replica Postgres,
/// is the next candidate.)
pub fn single_process_default(relational_db_url: &str) -> bool {
    sqlite_url_is_in_memory(relational_db_url)
}

/// Resolve the assertion: the operator's explicit `configured` answer when
/// there is one, else [`single_process_default`].
pub fn resolve_single_process(relational_db_url: &str, configured: Option<bool>) -> bool {
    configured.unwrap_or_else(|| single_process_default(relational_db_url))
}

/// Interpret a raw [`SINGLE_PROCESS_ENV`] value.
///
/// `None` means "no answer given, derive it": the variable is unset, empty, or
/// whitespace only. Anything else is an explicit answer.
///
/// This is the single place the raw value is interpreted, and every caller
/// must route through it. Two call sites parsing it separately is how the
/// `cognee` settings path and the `cognee-http-server` path came to disagree
/// on `COGNEE_SINGLE_PROCESS="   "` — one read it as an explicit `false`, the
/// other as "derive" — and disagreeing about this particular flag means one of
/// them sweeps a database the other is protecting.
pub fn parse_single_process_override(raw: &str) -> Option<bool> {
    if raw.trim().is_empty() {
        return None;
    }
    Some(cognee_utils::parse_env_bool(raw))
}

/// Read [`SINGLE_PROCESS_ENV`] as an explicit override, if one was given.
pub fn single_process_override_from_env() -> Option<bool> {
    parse_single_process_override(&std::env::var(SINGLE_PROCESS_ENV).ok()?)
}

/// [`resolve_single_process`] with the override taken straight from the
/// environment, for callers that carry no settings object of their own.
pub fn single_process_from_env(relational_db_url: &str) -> bool {
    resolve_single_process(relational_db_url, single_process_override_from_env())
}

#[cfg(test)]
mod tests {
    use super::{parse_single_process_override, resolve_single_process, single_process_default};

    #[test]
    fn only_in_memory_sqlite_derives_single_process() {
        for private in [
            "sqlite::memory:",
            "sqlite://:memory:",
            "sqlite:file::memory:?cache=shared",
            "sqlite://cognee.db?mode=memory",
        ] {
            assert!(
                single_process_default(private),
                "{private:?} is private to this process and must derive single-process"
            );
        }
    }

    #[test]
    fn file_backed_sqlite_does_not_derive_single_process() {
        // The regression this guards: `sqlite:./cognee.db?mode=rwc` is the
        // shipped default relational URL, and every process started in that
        // directory opens the same file. Deriving `true` here would have the
        // startup sweep delete a live sibling's claim.
        for shared in [
            "sqlite:./cognee.db?mode=rwc",
            "sqlite:///home/u/.cognee_system/cognee.db",
            "sqlite://cognee.db",
            // A path that merely *contains* the words must not be misread —
            // `classify_sqlite_url` matches query parameters exactly, and this
            // test is what keeps that guarantee visible from here.
            "sqlite:///home/u/mode=memory/cognee.db",
        ] {
            assert!(
                !single_process_default(shared),
                "{shared:?} is a shared file and must require an explicit opt-in"
            );
        }
    }

    #[test]
    fn server_backends_never_derive_single_process() {
        for shared in [
            "postgres://u:p@host:5432/cognee",
            "postgresql://u:p@host/cognee?sslmode=require",
            "mysql://u:p@host/cognee",
            "",
        ] {
            assert!(!single_process_default(shared), "{shared:?}");
        }
    }

    #[test]
    fn an_explicit_answer_wins_over_the_derivation_in_both_directions() {
        // The opt-in that file-backed SQLite now needs: one process per
        // device, asserted by the embedder because the SDK cannot see it.
        assert!(resolve_single_process(
            "sqlite:./cognee.db?mode=rwc",
            Some(true)
        ));
        // And the opt-out, which must work even on a URL the derivation would
        // have said `true` to: an explicit answer is always the last word, so
        // an operator can switch startup recovery off without changing the
        // database they point at.
        assert!(!resolve_single_process("sqlite::memory:", Some(false)));
    }

    #[test]
    fn a_blank_override_is_no_answer_at_all() {
        // The whitespace case both resolution paths must agree on.
        for blank in ["", " ", "\t", "  \n "] {
            assert_eq!(
                parse_single_process_override(blank),
                None,
                "{blank:?} must mean 'derive it', not 'explicitly false'"
            );
        }
        assert_eq!(parse_single_process_override(" 1 "), Some(true));
        assert_eq!(parse_single_process_override("no"), Some(false));
        // Anything unrecognised is still an *answer* — a deliberate `false` —
        // because the operator did set the variable.
        assert_eq!(parse_single_process_override("maybe"), Some(false));
    }
}
