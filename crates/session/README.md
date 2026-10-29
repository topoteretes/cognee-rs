# cognee-session

Session management and QA-history storage for cognee — tracks search/answer history, feedback, agent trace steps and session-context rows (guidance entries and the improve stages' watermark state rows) per session with pluggable backends (filesystem, Redis, SeaORM).

It also holds the deterministic guidance applier (`apply_candidate_updates`) and the agent-context extraction (`extract_pending_agent_context`, `extract_live_agent_context`) that turns agent trace steps into `agent`-profile lessons, with Python's prompt and redaction.

Part of [cognee-rs](https://github.com/topoteretes/cognee-rs) — see the [project README](../../README.md) for an architecture overview and how the pieces fit together.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
