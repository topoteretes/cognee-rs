# COG-4454: Cognee Core — Implementation Index & Status

**Linear issue:** [COG-4454](https://linear.app/cognee/issue/COG-4454/cognee-core)
**Branch:** `feature/cog-4454-cognee-core`
**Crate:** `cognee-core` (`crates/core/`)

This is the index/status document. Each remaining gap has its own detailed,
step-by-step implementation document under [`cog-4454-core/`](./cog-4454-core/).
Work the gaps **in the order listed below** — the first two share a new
`sentinels.rs` module and are purely additive; rate limiting comes last because
it touches the executor's retry loop and the `ExecEnv` plumbing.

## Already implemented (no action)

The three core pillars from the issue are done:

- **Thread pool** — `CpuPool` + `RayonThreadPool` (`thread_pool.rs`)
- **Object-safe traits** — `Llm`, `GraphDBTrait`, `VectorDB`, `DatabaseConnection` (all `Send + Sync`, `async_trait`)
- **Pipeline execution** — all 8 task variants (4 single + 4 batch), batch processing, progress tracking (`ProgressToken`), cancellation (`CancellationToken`), DB status persistence (`ExecStatusManager` + `DbPipelineWatcher`), retry/backoff (`RetryPolicy`/`RetryDelay`), three execution modes, telemetry

## Remaining work

Ready-to-run, per-gap implementation prompts (3-step: implement → review → land)
live in [cog-4454-core/IMPLEMENTATION-PROMPTS.md](./cog-4454-core/IMPLEMENTATION-PROMPTS.md).

| # | Gap | Effort | Detailed doc | Status |
|---|-----|--------|--------------|--------|
| 1 | **Drop / filter sentinel** — tasks can signal "discard this item" | Small | [01-drop-sentinel.md](./cog-4454-core/01-drop-sentinel.md) | ☑ Implemented (d1a7967) |
| 2 | **Enrichment mode (`enriches` flag)** — enriching task returns input unchanged | Small | [02-enrichment-mode.md](./cog-4454-core/02-enrichment-mode.md) | ☑ Implemented (1c87685) |
| 3 | **Rate limiting** — token-bucket / concurrency limiter for LLM & HTTP tasks | Medium | [03-rate-limiting.md](./cog-4454-core/03-rate-limiting.md) | ☑ Implemented (b709efd) |

### Why this order

1. **Drop sentinel first** — establishes the new `crates/core/src/sentinels.rs`
   module and the `&dyn Value` downcast helper pattern. Smallest, self-contained,
   no signature changes to the executor.
2. **Enrichment mode second** — adds a second sentinel (`PassthroughSentinel`) to
   the same module plus an `enriches` flag on `TaskInfo`. Reuses the exact
   `execute_from` insertion point established in #1.
3. **Rate limiting last** — the only medium-effort item. Introduces a new
   `rate_limiter.rs` module, threads an `Option<Arc<dyn RateLimiter>>` through
   `Pipeline → ExecEnv → call_with_retry`, and acquires inside the retry loop.

## Not in scope for this issue

- **Deferred parameter binding (TaskSpec/BoundTask)** — listed as low priority in
  the issue; Rust's `TaskInfo` builder already covers the practical need. Deferred.

## Verification

After each gap, per the project conventions:

```bash
cargo check --all-targets          # compiles
cargo test -p cognee-core          # unit + integration tests for the gap
scripts/check_all.sh               # fmt + clippy -D warnings + binding checks (run after all gaps)
```
