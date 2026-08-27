//! Process-wide ceiling on concurrent LLM HTTP requests.
//!
//! This is the Rust equivalent of the connection-pool bound every Python HTTP
//! client applies. Python's `asyncio.gather` over a batch looks unbounded, but
//! the coroutines contend for a pool that admits a fixed number at a time:
//!
//! | layer | `max_connections` |
//! |---|---|
//! | bare `httpx` | 100 (`httpx._config.DEFAULT_LIMITS`) |
//! | `openai-python` | 1000 (`openai._constants.DEFAULT_CONNECTION_LIMITS`) |
//! | `litellm` | 1000 (`HTTPHandler(concurrent_limit=1000)`) |
//!
//! `reqwest`/`hyper` have no equivalent default. `pool_max_idle_per_host` bounds
//! *idle kept-alive* connections, not requests in flight, so nothing stops a
//! caller from opening as many sockets as it has futures. Before this gate the
//! only ceiling was whatever semaphore the calling stage happened to hold, which
//! left every caller that does not thread its config through — the HTTP routers
//! build `CognifyConfig::default()` — effectively unbounded.
//!
//! ## Relationship to the pacer
//!
//! [`cognee_utils::pacing`] bounds the *rate* of dispatch (tokens per interval,
//! plus a cooldown once the provider signals overload). This bounds *concurrency*
//! — permits are held for the whole request, not just its start. They are not
//! substitutes: a rate limiter cannot stop 1000 simultaneous sockets, and a
//! semaphore cannot stop a steady stream that exceeds a quota. `crates/core`'s
//! own `rate_limiter` documentation makes the same distinction.
//!
//! Order matters at the call site: take the in-flight permit *first*, then wait
//! on the pacer. Reversed, a paced caller would hold a permit while sleeping,
//! and an overload episode would idle the pool instead of draining it.

use std::sync::{Arc, OnceLock};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Concurrent-request ceiling applied when nothing configures one.
///
/// Matches `openai-python` and `litellm`, both of which cap their connection
/// pool at 1000, so a Rust deployment behaves like a Python one rather than
/// opening an unbounded number of sockets.
pub const DEFAULT_MAX_IN_FLIGHT: usize = 1000;

static LLM_IN_FLIGHT: OnceLock<Arc<Semaphore>> = OnceLock::new();

/// Install the process-wide in-flight ceiling. First call wins.
///
/// Deliberately first-call-wins rather than adjustable: permits already held by
/// in-flight requests belong to the existing semaphore, so swapping it would
/// under-count and let the ceiling be exceeded during the changeover. Same
/// tradeoff, and same reason, as [`cognee_utils::pacing::init_llm_pacer`].
///
/// `0` is treated as 1 rather than as "closed" — see [`permits_for`].
pub fn init_llm_in_flight(max_in_flight: usize) -> Arc<Semaphore> {
    Arc::clone(LLM_IN_FLIGHT.get_or_init(|| Arc::new(Semaphore::new(permits_for(max_in_flight)))))
}

/// Permits to open the semaphore with, for a configured ceiling.
///
/// Clamps `0` up to 1 rather than honouring it: a zero-permit semaphore parks
/// every request forever, so a misconfigured `LLM_MAX_PARALLEL_REQUESTS=0` would
/// hang the pipeline rather than slow it. Serialising is the nearest useful
/// reading of "as few as possible", and it is what the CLI already does with the
/// same setting (`commands/cognify.rs` applies `.max(1)`).
///
/// A named function rather than an inline `.max(1)` so the clamp is reachable
/// from tests — the semaphore itself lives behind a `OnceLock` that only the
/// first caller in a process can set.
fn permits_for(max_in_flight: usize) -> usize {
    max_in_flight.max(1)
}

/// The process-wide in-flight semaphore, if one has been installed.
///
/// Adapters used as a plain library — with no component factory to install a
/// ceiling — get `None` and keep their previous unbounded behaviour, matching how
/// [`cognee_utils::pacing::llm_pacer`] degrades.
pub fn llm_in_flight() -> Option<Arc<Semaphore>> {
    LLM_IN_FLIGHT.get().map(Arc::clone)
}

/// Acquire a permit for one LLM request, or `None` when no ceiling is installed.
///
/// Held for the lifetime of the request — including its retries, so a request
/// that backs off does not release its slot to a competitor and then have to
/// re-queue behind it.
pub async fn acquire_in_flight() -> Option<OwnedSemaphorePermit> {
    match llm_in_flight() {
        // `acquire_owned` only errors when the semaphore is closed, and nothing
        // closes this one; treating that as "ungated" keeps a would-be panic
        // out of the request path.
        Some(semaphore) => semaphore.acquire_owned().await.ok(),
        None => None,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_the_python_client_pool_bound() {
        // openai-python's DEFAULT_CONNECTION_LIMITS and litellm's HTTPHandler
        // both cap at 1000; drifting from that silently changes how much
        // concurrency a Rust deployment offers relative to a Python one.
        assert_eq!(DEFAULT_MAX_IN_FLIGHT, 1000);
    }

    #[tokio::test]
    async fn no_installed_ceiling_leaves_requests_ungated() {
        // A fresh process installs nothing until a factory does, and the
        // adapters must stay usable as a plain library in that state.
        if llm_in_flight().is_none() {
            assert!(acquire_in_flight().await.is_none());
        }
    }

    #[test]
    fn a_zero_ceiling_serialises_rather_than_deadlocks() {
        // 0 must not reach the semaphore: `Semaphore::new(0)` parks every
        // acquirer forever, turning a misconfigured ceiling into a hang.
        assert_eq!(permits_for(0), 1);
        // Everything else passes through untouched.
        assert_eq!(permits_for(1), 1);
        assert_eq!(permits_for(DEFAULT_MAX_IN_FLIGHT), DEFAULT_MAX_IN_FLIGHT);
    }

    #[tokio::test]
    async fn a_single_permit_serialises_acquirers() {
        // The behaviour the clamp above buys: one at a time, and the slot comes
        // back. Asserted on a local semaphore because the process-global one is
        // a `OnceLock` shared with every other test in this binary.
        let semaphore = Arc::new(Semaphore::new(permits_for(0)));
        let held = Arc::clone(&semaphore).acquire_owned().await.unwrap();
        assert_eq!(semaphore.available_permits(), 0);
        drop(held);
        assert_eq!(semaphore.available_permits(), 1);
    }
}
