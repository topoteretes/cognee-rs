# Task 02-11 — User-facing documentation

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 02-05 — Client / dispatch / opt-out](05-client-dispatch-and-optout.md)
- [Task 02-06 — Public API + noop fallback](06-public-api-and-noop.md)
- [Task 02-07 — Callsite migration](07-callsite-migration.md)

**Blocks**: nothing — this task is purely documentation. Land last.

**Parent doc**: [02 — `send_telemetry()` Product-Analytics Client](../02-send-telemetry-analytics.md)

---

## 1. Goal

Author the operator-facing documentation that explains:

1. What `send_telemetry` is, what it sends, when it sends it.
2. Privacy guarantees (PBKDF2 key handling, what is **not**
   transmitted).
3. How to opt out — runtime (`TELEMETRY_DISABLED`) and compile-time
   (`--no-default-features`).
4. Salt rotation for deployments that want a private analytics
   namespace.
5. Wire-format reference (the exact payload schema) so a network
   admin can reason about firewall traffic.
6. Troubleshooting (logs, common failures).

The new doc lives at
`docs/observability/send_telemetry.md`, sibling to the gap-01
[`docs/observability/opentelemetry.md`](../../observability/opentelemetry.md).

Plus rustdoc updates on the public API and a README pointer.

## 2. Rationale

### Why a sibling to `opentelemetry.md`

Both docs are operator-facing and answer the same shape of question
("what does this SDK transmit, how do I configure it, how do I turn
it off"). Keeping them under
[`docs/observability/`](../../observability/) makes the
discoverability story uniform: an operator looking for "what does
cognee send over the network" finds both files in one place.

### Why a wire-format reference

Network admins frequently reject SDKs whose payloads they cannot
audit. Documenting the exact JSON schema is cheap and removes a
common deployment blocker.

### Privacy framing

The PBKDF2 cost (100k iterations × HMAC-SHA256) is **not** key-secrecy
proof — given enough compute, a 16-byte tracking ID can be brute-forced
back to a 40-char API key. The framing is "computationally
infeasible for a non-targeted attacker" — same as Python's framing
in `cognee/shared/utils.py` comments. Don't oversell the protection.

## 3. Pre-conditions

- The public surface is final ([task 02-06](06-public-api-and-noop.md)).
- Env vars are stable (no more renames after [task 02-05](05-client-dispatch-and-optout.md)).
- The `forget.rs` placeholder is replaced ([task 02-07](07-callsite-migration.md))
  so a "your first event" recipe in the doc actually fires.

## 4. Step-by-step

### 4.1 Create `docs/observability/send_telemetry.md`

Skeleton (full content authored at implementation time):

```markdown
# Product-Analytics Telemetry (`send_telemetry`)

Cognee's Rust SDK ships an opt-out HTTP product-analytics client
that mirrors Python's `cognee.shared.utils.send_telemetry`. This
document covers what it sends, how to disable it, and how to
configure it for your deployment.

> **TL;DR — turn it off:**
> ```bash
> export TELEMETRY_DISABLED=1
> ```
> Or at compile time:
> ```bash
> cargo build --no-default-features --features <minimal-set>
> ```

## What it sends

Every public API call (e.g. `cognee.forget`, `cognee.recall`) emits
a single fire-and-forget HTTP POST to `https://test.prometh.ai`.

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
| `anonymous_id` | Project-local: `<cwd>/.anon_id`. Resets on git re-clone. | Override with `TRACKING_ID=<uuid>`. |
| `persistent_id` | Machine-local: `~/.cognee/.persistent_id`. Survives `forget(everything=True)`. | Created on first call. |
| `api_key_tracking_id` | Stable for the same `LLM_API_KEY` across machines. | PBKDF2-HMAC-SHA256, 100 000 iter, 16-byte output, prefix `ak_`. |

`anonymous_id` is **not** expected to match between Python and Rust
SDKs running in the same project — they have different working
directories. `persistent_id` (machine-level) and
`api_key_tracking_id` (key-level) **are** byte-identical between
SDKs sharing `~/.cognee/` and `LLM_API_KEY`.

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
cargo build -p cognee-cli --no-default-features --features <minimal-set>
```

The `telemetry` cargo feature is ON by default for `cognee-cli` and
`cognee-lib`. Disable with `--no-default-features` and re-enable
only the features you need.

## Configuration

| Env var | Default | Effect |
|---|---|---|
| `TELEMETRY_DISABLED` | unset | Any non-empty value disables. |
| `ENV` | unset | If `test` or `dev`, disables. |
| `TELEMETRY_REQUEST_TIMEOUT` | `5` | Total HTTP timeout in seconds. Clamped to `[1, 60]`. |
| `TELEMETRY_API_KEY_TRACKING_SALT` | `cognee.telemetry.api-key-tracking.v1` | Override the PBKDF2 salt. See [Salt rotation](#salt-rotation). |
| `TRACKING_ID` | unset | Override `anonymous_id` (rarely used; intended for CI fixtures). |

## Salt rotation

For deployments that want a **private analytics namespace** —
i.e. their `api_key_tracking_id` should not collide with the public
cognee namespace — set `TELEMETRY_API_KEY_TRACKING_SALT` to a
deployment-unique string.

```bash
export TELEMETRY_API_KEY_TRACKING_SALT="acme-corp-2026"
```

This is a one-way switch: once a deployment sets a salt, its
`api_key_tracking_id` values are **incomparable** with the public
namespace. The default (well-known) salt exists so OSS users
converge to a single analytics bucket; private deployments break
out.

## Wire format reference

Every event POSTs the following body to
`https://test.prometh.ai`:

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
    "cognee_version":      "<semver>",
    /* …caller-supplied additional_properties (sanitized)… */
  }
}
```

Notes:

- `api_key_hash` is a backward-compatibility alias of
  `api_key_tracking_id` (Python carries both for legacy
  dashboards).
- `sdk_runtime` is added by the Rust SDK only; Python may add
  it in a future release.
- `additional_properties` are flattened into `properties` on the
  wire — there is no nested object.

## Privacy and compliance

`api_key_tracking_id` is a salted PBKDF2-HMAC-SHA256 hash of the
`LLM_API_KEY`:

- Cost per candidate: ~50 ms on commodity CPU (100 000 iterations).
- 16-byte (128-bit) output.
- Recovering a 40-char OpenAI-style key with reasonable entropy is
  computationally infeasible against a non-targeted attacker.

This is **not** a key-secrecy guarantee against a determined
attacker with significant compute and a small candidate set. If
your threat model includes that adversary, set
`TELEMETRY_DISABLED=1` (the recommended posture for any
production deployment under a privacy regulation).

What the proxy operator can see:

- Frequency of cognee usage by `persistent_id`.
- Aggregate event distribution (`cognee.forget` vs `cognee.recall`).
- The `cognee_version` running.
- The `sdk_runtime`.

What the proxy operator cannot see (without breaking PBKDF2):

- The raw `LLM_API_KEY`.
- The user's queries, datasets, or document content.
- File paths, SQL queries, or HTTP URLs (the only URLs accepted
  under sanitized keys are pre-hashed via uuid5).

## Troubleshooting

### "I see no telemetry events but expected some"

```bash
RUST_LOG=cognee.telemetry=debug cognee <command>
```

Look for one of:

- `send_telemetry called but telemetry feature disabled at compile time` —
  rebuild with the `telemetry` feature.
- `telemetry proxy returned non-2xx` — proxy is up but rejected the
  payload. Check `RUST_LOG` for the captured status code.
- `telemetry request failed` — DNS or transport error. Check
  network egress to `test.prometh.ai`.
- No `cognee.telemetry` logs at all — disabled at runtime. Verify
  `TELEMETRY_DISABLED` and `ENV`.

### "I want to verify cross-SDK identity grouping"

```bash
# In a single shell, with a shared HOME and LLM_API_KEY:
export HOME=/tmp/cognee-parity
export LLM_API_KEY=sk-test-...

# Run python:
python -c "from cognee.shared.utils import send_telemetry; send_telemetry('debug', user_id='x')"

# Run rust (any SDK call that fires forget):
cognee delete --all --dry-run
```

Both should now share the same `~/.cognee/.persistent_id` and the
same `api_key_tracking_id` derivation. Network capture (e.g.
`mitmproxy --listen-host 127.0.0.1`) will confirm.

### "My deployment-specific salt doesn't take effect"

The salt is read **at every event-emission**. Verify:

- `echo $TELEMETRY_API_KEY_TRACKING_SALT` in the same shell that
  runs cognee.
- The shell environment is propagated through any process
  supervisor (systemd, docker, k8s) — check
  `/proc/<pid>/environ`.

## See also

- [OpenTelemetry / OTLP export](opentelemetry.md) — the *other*
  telemetry pillar (process-level traces, not product analytics).
- [Engineering gap analysis](../telemetry/gap-analysis.md) — the
  per-gap engineering plan; not relevant for operators.
- Python equivalent: [`cognee/shared/utils.py`](https://github.com/topoteretes/cognee/blob/main/cognee/shared/utils.py).
```

### 4.2 Update the README

Add a one-liner under the existing Observability section of
`README.md`:

```markdown
- **Product analytics** — opt-out HTTP events to
  `https://test.prometh.ai`. Mirrors Python.
  [`docs/observability/send_telemetry.md`](docs/observability/send_telemetry.md)
```

(Confirm the existing structure when implementing — adapt to whatever
section the OTLP gap-01 reference lives under.)

### 4.3 Update rustdoc on the public API

`crates/telemetry/src/lib.rs` already has top-level rustdoc from
[task 02-06](06-public-api-and-noop.md). Make sure each of the
following is present:

- The "Quick start" `no_run` example.
- The runtime-fallback warning text.
- A link to `docs/observability/send_telemetry.md` for further
  reading.

Add a note on the env vars to the function rustdoc on
`send_telemetry`:

```rust
/// # Environment variables
///
/// | Var | Default | Effect |
/// |---|---|---|
/// | `TELEMETRY_DISABLED` | unset | Any non-empty value disables. |
/// | `ENV` | unset | If `test` or `dev`, disables. |
/// | `TELEMETRY_REQUEST_TIMEOUT` | `5` | Total HTTP timeout (seconds). |
/// | `TELEMETRY_API_KEY_TRACKING_SALT` | (default well-known) | Override PBKDF2 salt. |
/// | `TRACKING_ID` | unset | Override `anonymous_id`. |
///
/// See `docs/observability/send_telemetry.md` for the full reference.
```

### 4.4 Verify

```bash
# 1. Doc compiles (catches broken links and missing sections).
cargo doc -p cognee-telemetry --no-deps --features telemetry

# 2. Spell-check (manual).
codespell docs/observability/send_telemetry.md

# 3. Markdown lint (if a tool is configured).
# The workspace doesn't enforce a markdownlint config; visual review
# is the gate.

# 4. The README pointer is reachable.
grep -F 'send_telemetry.md' README.md
```

## 5. Verification

```bash
# 1. The new doc renders cleanly.
# (Open in a markdown previewer or `mdbook serve` if available.)

# 2. The rustdoc generates without warnings.
cargo doc -p cognee-telemetry --no-deps --features telemetry 2>&1 \
  | grep -E '(warning|error)'

# 3. Sub-agent E for [task 02-12] verifies that doc-bench (if any)
#    passes — gap 01's CI lane runs `cargo doc --no-deps`.
```

## 6. Files modified

- `docs/observability/send_telemetry.md` — new file (~250 lines).
- `README.md` — one bullet point added.
- `crates/telemetry/src/lib.rs` — extend top-level + function-level
  rustdoc.
- (Possibly) `docs/telemetry/gap-analysis.md` — flip the row for gap
  02 in the prioritized list to indicate the user doc exists.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Doc references env-var names that change before merge | Mitigated by landing this task last (per the runbook). | Sub-agent A re-reads the env names from `crates/telemetry/src/env.rs` and updates the doc table accordingly. |
| Privacy framing oversells the PBKDF2 protection | Real risk — engineers tend to claim more than the math supports | Drafted text is intentionally honest about the brute-force cost. Reviewer should sanity-check. |
| Wire-format JSON example drifts from the implementation | Likely if the schema changes after merge | The doc references the [task 02-04](04-payload-and-sanitize.md) struct as the source of truth. Add a doc-test that renders `TelemetryPayload` and asserts the captured JSON matches the documented example, in a follow-up. |
| `cognee.telemetry` log target string is documented but the implementation uses a different one | Mitigated by `grep -rn 'cognee.telemetry' crates/telemetry/src/` — must produce the same string | Cross-check at task implementation time. |
| README pointer placed in wrong section | Cosmetic | Reviewer chooses the section; sub-agent A flags if the README structure makes the placement unclear. |

## 8. Out of scope

- Engineering gap docs (already covered by
  [`docs/telemetry/gap-analysis.md`](../gap-analysis.md) and the per-gap
  `02-*.md` files).
- Cross-SDK parity recipes are documented in this user doc but the
  test infrastructure lives in [task 02-10](10-cross-sdk-parity.md).
- CI lane changes (covered by [task 02-12](12-ci-updates.md)).
