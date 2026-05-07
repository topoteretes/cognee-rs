# Product-Analytics Telemetry (`send_telemetry`)

Cognee's Rust SDK ships an opt-out HTTP product-analytics client that
mirrors Python's `cognee.shared.utils.send_telemetry`. This document
covers what it sends, how to disable it, and how to configure it for
your deployment.

> **TL;DR — turn it off:**
> ```bash
> export TELEMETRY_DISABLED=1
> ```
> Or at compile time:
> ```bash
> cargo build -p cognee-cli --no-default-features
> ```

`send_telemetry` is a sibling of the OpenTelemetry / OTLP exporter
documented in [`opentelemetry.md`](opentelemetry.md). The two pillars
serve different audiences: OTLP traces are for *you* (process-level
observability for your own backend), `send_telemetry` is for the
cognee maintainers (anonymous product analytics on which features get
used). They share the `telemetry` cargo feature flag but are
otherwise independent.

## Why is it on by default?

Per locked decision 1 of the gap-02 design, telemetry is enabled in
both default builds (Python parity) so that the cognee maintainers
have an aggregate view of how the SDK is exercised in the wild.
Operators who need it off have two clearly documented escape hatches
(runtime env var, compile-time feature). The runtime hatch costs
zero — the disable check happens **before** any identity derivation,
filesystem read, or HTTP code path is touched.

## What it sends

Every public API call (e.g. `cognee.forget`, `cognee.recall`) emits a
single fire-and-forget HTTP POST to `https://test.prometh.ai` (locked
decision 2 — same proxy as Python so cross-SDK identity grouping
works).

The payload is a flat JSON object — see the
[Wire format reference](#wire-format-reference) below.

The body carries:

- `event_name` (e.g. `"cognee.forget"`).
- A three-layer identity tuple (`anonymous_id`, `persistent_id`,
  `api_key_tracking_id`).
- The cognee version + `sdk_runtime: "rust"` discriminator.
- A `time` field (`MM/DD/YYYY`, UTC).
- Caller-supplied `additional_properties`. Any string value under a
  `url` key is hashed via UUID v5 before transmission.

It does **NOT** carry:

- The raw `LLM_API_KEY`.
- A truncated tail of the `LLM_API_KEY`.
- Arbitrary URL strings (only sanitized via uuid5 if the caller
  passes them under a `url` key).
- File paths, SQL queries, or other content.
- The Cognee `User.email` or other user fields beyond `User.id`.

## Identity layers

| Layer | Stability | Source |
|---|---|---|
| `anonymous_id` | Project-local: `<project_root>/.anon_id`. Resets on git re-clone. | Override with `TRACKING_ID=<uuid>`. |
| `persistent_id` | Machine-local: `~/.cognee/.persistent_id`. Survives `forget(everything=True)`. | Created on first call. |
| `api_key_tracking_id` | Stable for the same `LLM_API_KEY` across machines. | PBKDF2-HMAC-SHA256, 100 000 iter, 16-byte output, prefix `ak_`. |

`anonymous_id` is **not** expected to match between Python and Rust
SDKs running in the same project — they may resolve different project
roots. `persistent_id` (machine-level) and `api_key_tracking_id`
(key-level) **are** byte-identical between SDKs sharing `~/.cognee/`
and `LLM_API_KEY`.

`api_key_tracking_id` is recomputed on every event-emission (locked
decision 11) — there is no in-process cache. Rotating
`LLM_API_KEY` mid-process is reflected on the very next call.

## Opt-out

### At runtime

```bash
export TELEMETRY_DISABLED=1   # any non-empty value
# or
export ENV=test               # also disables; intended for CI
export ENV=dev                # also disables; intended for dev shells
```

The check happens **before** any identity derivation, so disabling
costs zero: no file IO, no PBKDF2, no HTTP.

### At compile time

```bash
# Build cognee-cli without telemetry. The send_telemetry function
# still exists as a no-op; no HTTP code is linked.
cargo build -p cognee-cli --no-default-features
```

The `telemetry` cargo feature is ON by default for `cognee-cli` and
`cognee-lib`. With the feature off, `send_telemetry` and
`try_send_telemetry` remain present in the public surface but are
compiled to noop bodies — no `reqwest`, no `tokio` runtime fallback,
no PBKDF2 cost.

## Configuration

| Env var | Default | Effect |
|---|---|---|
| `TELEMETRY_DISABLED` | unset | Any non-empty value disables. Read on every call. |
| `ENV` | unset | If `test` or `dev`, disables. Read on every call. |
| `LLM_API_KEY` | unset | Source of `api_key_tracking_id` (locked decision 11 — read at every event-emission, never cached). When unset, `api_key_tracking_id` is the empty string. |
| `TRACKING_ID` | unset | Override `anonymous_id` (rarely used; intended for CI fixtures). |
| `TELEMETRY_API_KEY_TRACKING_SALT` | `cognee.telemetry.api-key-tracking.v1` | Override the PBKDF2 salt (locked decision 12). See [Salt rotation](#salt-rotation). |
| `TELEMETRY_REQUEST_TIMEOUT` | `5` | Total HTTP timeout in seconds. Clamped to `[1, 60]`. Read once per process at first event. |

## Salt rotation

For deployments that want a **private analytics namespace** —
i.e. their `api_key_tracking_id` should not collide with the public
cognee namespace — set `TELEMETRY_API_KEY_TRACKING_SALT` to a
deployment-unique string.

```bash
export TELEMETRY_API_KEY_TRACKING_SALT="acme-corp-2026"
```

Guidance for fleet operators:

- Choose a salt that is unique to your deployment but stable across
  rollouts. Rotating the salt rotates every `api_key_tracking_id`
  derived from it — your dashboards will show the same key as a "new"
  user after the rotation.
- The salt is read at every event-emission. Propagate it through your
  process supervisor (systemd `Environment=`, docker `--env`,
  Kubernetes `env:`); a salt set only in a parent shell will not
  reach a daemonised cognee process.
- The default salt is well-known. Once a deployment overrides it, its
  `api_key_tracking_id` values are **incomparable** with the public
  namespace — that is the whole point of the override.

## Wire format reference

Every event POSTs the following body to `https://test.prometh.ai`:

```jsonc
{
  "anonymous_id": "<uuid4>",
  "event_name":   "<event>",
  "user_properties": {
    "user_id":             "<UUID or empty>",
    "persistent_id":       "<uuid4>",
    "api_key_tracking_id": "ak_<32hex>",
    "api_key_hash":        "ak_<32hex>"
  },
  "properties": {
    "time":                "MM/DD/YYYY",
    "user_id":             "<UUID or empty>",
    "anonymous_id":        "<uuid4>",
    "persistent_id":       "<uuid4>",
    "api_key_tracking_id": "ak_<32hex>",
    "api_key_hash":        "ak_<32hex>",
    "sdk_runtime":         "rust",
    "cognee_version":      "<semver>"
    /* …caller-supplied additional_properties (sanitized)… */
  }
}
```

Notes:

- `api_key_hash` is a backward-compatibility alias of
  `api_key_tracking_id` (Python carries both for legacy dashboards).
- `sdk_runtime` is added by the Rust SDK; Python may add it in a
  future release.
- `additional_properties` are flattened into `properties` on the wire
  — there is no nested object. Reserved keys (`time`, `user_id`,
  `anonymous_id`, `persistent_id`, `api_key_tracking_id`,
  `api_key_hash`, `sdk_runtime`, `cognee_version`) MUST NOT be passed
  in `additional_properties`.

## Pipeline + task lifecycle events

Fired automatically by every pipeline run that goes through
`cognee_core::pipeline::execute()`. Mirrors Python's emission from
`run_tasks_with_telemetry.py` and `run_tasks_base.py`. Pipeline-run
events are emitted *next to* the `PipelineWatcher` callbacks, not
through them — the watcher is a structural extension point and is
not part of the analytics surface.

| Event | When fired | Identity | Properties |
|---|---|---|---|
| `Pipeline Run Started` | After `execute()` builds `run_info`, before tasks run. | `user_id` from `PipelineContext.user_id` (else `"sdk"`). | `pipeline_name`, `cognee_version`, `tenant_id` (`"Single User Tenant"` when unset), plus the curated config snapshot — see below. |
| `Pipeline Run Completed` | On the `Ok(...)` arm of `execute()`. | same | same |
| `Pipeline Run Errored` | On both `Err` arms (`Cancelled` and generic `Err`). No error string in the payload. | same | same |
| `${task_type} Task Started` | Once per task, before the first attempt of `call_with_retry`. | same as enclosing run | `task_name` (else `"unknown"`), `cognee_version`, `tenant_id` |
| `${task_type} Task Completed` | Once per task, on the first successful attempt. | same | same |
| `${task_type} Task Errored` | Once per task, after retries are exhausted. No error string. | same | same |

`${task_type}` is one of `Function`, `Coroutine`, `Generator`, or
`Async Generator` — see `Task::python_task_type()` in
[`crates/core/src/task.rs`](../../crates/core/src/task.rs) for the
mapping. Async closures resolve to `Coroutine`, sync closures to
`Function`; `Generator` and `Async Generator` are reserved for the
streaming task variants and are emitted byte-equal to Python.

### Curated `Pipeline Run *` config snapshot

The settings dump merged into `Pipeline Run Started/Completed/Errored`
events is a hand-curated allowlist — never the full `Config` struct.
Currently allowed:

- `sdk_runtime` (`"rust"` literal)
- `vector_db_provider`, `graph_db_provider`, `relational_db_provider`
- `llm_provider`, `llm_model`
- `embedding_provider`, `embedding_model`, `embedding_dimensions`
- `chunk_strategy`

Adding a field to this allowlist requires a code change in
[`crates/lib/src/config.rs`](../../crates/lib/src/config.rs)
(`Settings::telemetry_snapshot()`) and an update to the snapshot test
that locks the wire shape. URLs, credentials, and file paths
(including `embedding_model_path`, `embedding_endpoint`, and any API
key) are intentionally omitted from this snapshot. The wider redaction
contract is the same as for caller-supplied `additional_properties`
(see the [Wire format reference](#wire-format-reference) above and
the privacy section below).

## Search lifecycle events

| Event | When fired | Identity | Properties |
|---|---|---|---|
| `cognee.search EXECUTION STARTED` | First statement of `SearchOrchestrator::search`, before any work. | `request.user_id` | `cognee_version`, `tenant_id` |
| `cognee.search EXECUTION COMPLETED` | Each `Ok(...)` return path of `SearchOrchestrator::search`. Not fired on errors. | same | same |

These are the internal-pipeline pair; the user-facing SDK entry point
emits `cognee.recall` once per call (see the table in
[`docs/telemetry/03-pipeline-task-api-events.md`](../telemetry/03-pipeline-task-api-events.md)
for the full SDK-API event catalog).

## Privacy and compliance

`api_key_tracking_id` is a salted PBKDF2-HMAC-SHA256 hash of
`LLM_API_KEY`:

- Cost per candidate: ~50 ms on commodity CPU (100 000 iterations).
- 16-byte (128-bit) output.
- Recovering a 40-char OpenAI-style key with reasonable entropy is
  computationally infeasible against a non-targeted attacker.

This is **not** a key-secrecy guarantee against a determined attacker
with significant compute and a small candidate set. If your threat
model includes that adversary, set `TELEMETRY_DISABLED=1` (the
recommended posture for any production deployment under a privacy
regulation).

What the proxy operator can see:

- Frequency of cognee usage by `persistent_id`.
- Aggregate event distribution (`cognee.forget` vs `cognee.recall`).
- The `cognee_version` running.
- The `sdk_runtime`.

What the proxy operator cannot see (without breaking PBKDF2):

- The raw `LLM_API_KEY`.
- The user's queries, datasets, or document content.
- File paths, SQL queries, or HTTP URLs (the only URLs accepted under
  sanitized keys are pre-hashed via uuid5).

## Troubleshooting

All telemetry diagnostics use the `cognee.telemetry` tracing target.
To see them:

```bash
RUST_LOG=cognee.telemetry=debug cognee <command>
```

### "I see no telemetry events but expected some"

Look for one of the following log lines:

- `send_telemetry called from a non-tokio context; spinning up a one-shot runtime`
  — emitted at `warn` level when `send_telemetry` is called from a
  synchronous context. Behaviour is correct (the event still fires)
  but indicates a perf-improvement opportunity (call from async).
- `telemetry proxy returned non-2xx` — proxy is reachable but rejected
  the payload. The captured status code is on the same log line.
- `telemetry request failed` — DNS or transport error. Check network
  egress to `test.prometh.ai`.
- `additional_properties was not an object; dropping` — caller passed a
  non-object as `additional_properties`. Defensive safety drop; payload
  is still sent, but with no caller-supplied properties.
- `telemetry payload serialization failed` — practically unreachable;
  indicates an internal schema bug.
- No `cognee.telemetry` logs at all — disabled at runtime. Verify
  `TELEMETRY_DISABLED` and `ENV`.

### "I built without the `telemetry` feature and got no events"

That is the documented compile-time opt-out. `send_telemetry` is
linked as a noop in that build; no HTTP code path exists. Rebuild
with the default features (or `--features telemetry`) to re-enable.

### "I want to verify cross-SDK identity grouping"

```bash
# In a single shell, with a shared HOME and LLM_API_KEY:
export HOME=/tmp/cognee-parity
export LLM_API_KEY=sk-test-...

# Run python:
python -c "from cognee.shared.utils import send_telemetry; send_telemetry('debug', user_id='x')"

# Run rust (any SDK call that fires a telemetry event):
cognee delete --all --dry-run
```

Both should now share the same `~/.cognee/.persistent_id` and the same
`api_key_tracking_id` derivation. Network capture (e.g.
`mitmproxy --listen-host 127.0.0.1`) will confirm.

### "My deployment-specific salt doesn't take effect"

The salt is read on every event-emission. Verify:

- `echo $TELEMETRY_API_KEY_TRACKING_SALT` in the same shell that runs
  cognee.
- The shell environment is propagated through any process supervisor
  (systemd, docker, k8s) — check `/proc/<pid>/environ`.

## See also

- [OpenTelemetry / OTLP export](opentelemetry.md) — the *other*
  telemetry pillar (process-level traces, not product analytics).
- Python equivalent:
  [`cognee/shared/utils.py`](https://github.com/topoteretes/cognee/blob/main/cognee/shared/utils.py).
