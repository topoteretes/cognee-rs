# Task 01-01: Add OpenTelemetry workspace dependencies

## Status

Not started.

## Owner / dependencies

- **Depends on**: nothing. This is the first task in the
  [01-otel-otlp-export](../01-otel-otlp-export.md) initiative — it
  introduces the manifest entries that every later task references.
- **Blocks**:
  - [Task 01-02 — `cognee-observability` crate scaffold](02-cognee-observability-crate.md)
    (its `[dependencies]` will pull from these workspace entries).
  - [Task 01-04 — `init_telemetry` implementation](04-init-telemetry.md)
    (consumes the OTEL types declared here).
  - Indirectly all other 01/* tasks, since they build on the crate
    scaffolded in 01-02.
- **Owner**: TBD.

## Rationale

The five OTEL crates listed below are the minimum surface needed to
satisfy the architecture in
[`01-otel-otlp-export.md` § "Proposed design"](../01-otel-otlp-export.md#proposed-design):

| Crate | Why it is required |
|---|---|
| `opentelemetry` | Stable API surface (`KeyValue`, `global`, `trace::TracerProvider`). Every downstream crate that touches OTEL must depend on the API crate. |
| `opentelemetry_sdk` | Concrete `SdkTracerProvider`, `Resource`, `BatchSpanProcessor`. Needed to build the provider in [task 01-04](04-init-telemetry.md). |
| `opentelemetry-otlp` | OTLP exporter (gRPC + HTTP/protobuf). The whole point of the gap is OTLP export. |
| `opentelemetry-semantic-conventions` | Stable string constants (`SERVICE_NAME`, `SERVICE_VERSION`, `DEPLOYMENT_ENVIRONMENT_NAME`). Hand-rolling these strings is error-prone and version-coupled to the OTEL spec. |
| `tracing-opentelemetry` | The bridge layer that converts the 62+ existing `#[tracing::instrument]` sites into OTEL spans without per-site changes. |

### Why pin to `=0.31` / `=0.32`

The OTEL Rust crates have undergone breaking renames between minor
versions (e.g. `TracerProvider` → `SdkTracerProvider`,
`with_simple_exporter` → `with_simple_exporter`+`SdkTracerProvider`
builder, `force_flush` signature). `tracing-opentelemetry` lags one
minor release behind the core OTEL crates: `tracing-opentelemetry`
0.32 pairs with `opentelemetry`/`opentelemetry_sdk`/`opentelemetry-otlp`
0.31. Mismatched pairs fail to compile because the bridge expects
specific trait shapes. Exact `=` pinning at the workspace level keeps
CI deterministic across re-locks and prevents `cargo update` from
silently bumping one crate without its peers.

These are the same versions called out in the
[design table of `01-otel-otlp-export.md`](../01-otel-otlp-export.md#crate-selection-versions-current-as-of-2026-05).
A web check at implementation time should confirm no compatible patch
release supersedes them; if a newer set (e.g. 0.32/0.33) is mutually
compatible, bump both — but never split the pair.

### Why both gRPC and HTTP/protobuf

[Decision 3](../01-otel-otlp-export.md#design-decisions-locked) of the
locked design says ship **both** transports:

- `grpc-tonic` is the default and matches Python's
  `_try_add_otlp_exporter` order (it tries gRPC first).
- `http-proto` is the fallback for environments where outbound gRPC is
  blocked (corporate proxies, browser-style egress, some serverless
  runtimes). It also avoids the `tonic` transitive dependency for users
  who want a slimmer build.

The Python extra `[tracing]` lists *both*
`opentelemetry-exporter-otlp-proto-grpc` and
`opentelemetry-exporter-otlp-proto-http` for the same reason —
parity dictates the Rust port does the same.

### Difference from Python's `[tracing]` extra

| Aspect | Python (`pyproject.toml` [tracing]) | Rust (this task) |
|---|---|---|
| Package style | Two separate exporter packages (`-grpc` and `-http`) | Single `opentelemetry-otlp` crate with two cargo features |
| Default install | Off (must `pip install cognee[tracing]`) | Off at the workspace level — declared but not enabled by any crate yet (see [decision 1](../01-otel-otlp-export.md#design-decisions-locked)) |
| Version range | `>=1.20.0,<2` (loose) | `=0.31` / `=0.32` exact pins (Rust API instability necessitates strictness) |
| Bridge to logger | `LoggingHandler` for stdlib logging | `tracing-opentelemetry` for the `tracing` ecosystem (no Python equivalent — Python instruments via OTEL API directly) |

## Pre-conditions

None.

## Step-by-step implementation

1. Open the workspace root manifest:
   [`Cargo.toml`](../../../Cargo.toml).

2. In the `[workspace.dependencies]` block (currently lines 42–103 in
   [`Cargo.toml`](../../../Cargo.toml)), insert the following entries.
   Maintain the **alphabetical ordering** convention used in the rest
   of the block. Insertion points:

   - `opentelemetry`, `opentelemetry-otlp`,
     `opentelemetry-semantic-conventions`, `opentelemetry_sdk` go in
     the `o` cluster, between `notify` (line 64) and `ort` (line 65).
     Note: cargo treats `opentelemetry_sdk` (underscore) as
     alphabetically after the hyphenated forms because `_` (0x5F) >
     `-` (0x2D); place it last among the four.
   - `tracing-opentelemetry` goes between `tokio-stream` (line 97) and
     `toml` (line 98) — i.e. immediately above `tracing` (line 99).
     Wait — `tracing-opentelemetry` sorts *after* `tracing-subscriber`
     in alphabetical order (`-o` < `-s`), so the correct position is
     between `tracing` (line 99) and `tracing-subscriber` (line 100).
     Verify lexicographic order locally before committing.

   Lines to add:

   ```toml
   opentelemetry = { version = "=0.31", default-features = false, features = ["trace"] }
   opentelemetry-otlp = { version = "=0.31", default-features = false, features = ["trace", "grpc-tonic", "http-proto", "reqwest-client"] }
   opentelemetry-semantic-conventions = "=0.31"
   opentelemetry_sdk = { version = "=0.31", default-features = false, features = ["trace", "rt-tokio"] }
   ```

   And separately near `tracing`:

   ```toml
   tracing-opentelemetry = { version = "=0.32", default-features = false, features = ["tracing-log"] }
   ```

3. Save the file.

4. From the workspace root, run a manifest-resolution sanity check.
   Because no crate in the workspace has yet added these to its own
   `[dependencies]`, cargo will resolve the manifest and update
   `Cargo.lock` only if a member references them. We therefore run a
   plain check first to confirm the manifest is syntactically valid
   and the version selectors are satisfiable:

   ```bash
   cargo check --all-targets
   ```

   Expected outcome: no new compilation work (no member uses these
   yet); cargo will simply parse and validate the manifest.

5. Force a resolution pass against the new entries to surface any
   conflicts with the existing `[patch.crates-io]` block (which forks
   `tonic` and `hyper` for qdrant compatibility — see
   [Risks](#risks)):

   ```bash
   cargo tree -i opentelemetry --workspace 2>&1 | head -n 40
   cargo tree -i tonic --workspace --depth 2 2>&1 | head -n 40
   ```

   Until a workspace member actually depends on `opentelemetry`, the
   first `cargo tree` will print "package not found" — that is
   expected. The second confirms what `tonic` version is currently in
   the lockfile (the qdrant fork) so [task 01-02](02-cognee-observability-crate.md)
   knows what it has to coexist with.

6. (Optional but recommended) Verify the registry has the exact pins
   available:

   ```bash
   cargo search opentelemetry --limit 3
   cargo search tracing-opentelemetry --limit 3
   ```

7. Commit only `Cargo.toml`. Do **not** commit `Cargo.lock` changes
   yet — the lock will only meaningfully update once
   [task 01-02](02-cognee-observability-crate.md) introduces a
   consumer.

## Resulting file diff

```diff
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -62,6 +62,10 @@ mail-parser = "0.11"
 mime_guess = "2.0"
 ndarray = "0.17"
 notify = "6.1"
+opentelemetry = { version = "=0.31", default-features = false, features = ["trace"] }
+opentelemetry-otlp = { version = "=0.31", default-features = false, features = ["trace", "grpc-tonic", "http-proto", "reqwest-client"] }
+opentelemetry-semantic-conventions = "=0.31"
+opentelemetry_sdk = { version = "=0.31", default-features = false, features = ["trace", "rt-tokio"] }
 ort = { version = "2.0.0-rc.11", features = ["ndarray", "cuda", "tensorrt"] }
 pdf-extract = "0.10"
 quick-xml = "0.39"
@@ -97,6 +101,7 @@ tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time"
 tokio-stream = { version = "0.1", features = ["sync"] }
 toml = "0.8"
 tracing = "0.1"
+tracing-opentelemetry = { version = "=0.32", default-features = false, features = ["tracing-log"] }
 tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
 url = "2.5"
 uuid = { version = "1.21", features = ["v4", "v5", "serde"] }
```

(Line numbers are approximate; rely on alphabetical ordering rather
than absolute positions.)

## Verification

Run each command and confirm the expected outcome:

- [ ] `cargo check --all-targets` — completes without errors. No
  member yet depends on the new crates, so output should be
  unchanged from before this task aside from a possible
  "Updating crates.io index" line.
- [ ] `cargo metadata --format-version 1 --no-deps | jq '.workspace_metadata'`
  succeeds (manifest is parsable).
- [ ] `grep -c '^opentelemetry' Cargo.toml` returns `4` (four crates
  starting with `opentelemetry`).
- [ ] `grep -c '^tracing-opentelemetry' Cargo.toml` returns `1`.
- [ ] `cargo update -p opentelemetry --dry-run` — prints "package
  `opentelemetry` not found in the dependency graph" (or equivalent)
  because no member uses it yet. This is the expected and correct
  state at the end of this task.
- [ ] No member crate's `Cargo.toml` was modified.

## Files modified

- [`Cargo.toml`](../../../Cargo.toml) — only the workspace root
  manifest. Five new lines in `[workspace.dependencies]`.

## Risks

1. **`tonic` patch conflict.** The workspace pins `tonic` to a qdrant
   fork via `[patch.crates-io]`
   ([`Cargo.toml:109`](../../../Cargo.toml#L109)):
   `tonic = { git = "https://github.com/qdrant/tonic", branch = "v0.11.0-qdrant" }`.
   `opentelemetry-otlp` 0.31 with the `grpc-tonic` feature depends on
   a *modern* `tonic` (≥ 0.12). The qdrant fork is based on tonic
   `0.11.0` and is unlikely to satisfy the OTLP exporter's API
   expectations. **Mitigation paths**, in priority order, to be
   resolved in [task 01-02](02-cognee-observability-crate.md) when the
   first consumer actually pulls `opentelemetry-otlp`:
   - Disable the `grpc-tonic` feature, ship HTTP/protobuf only as the
     default, and document gRPC as "build it yourself with a custom
     patch override".
   - Convince `cognee-observability` to build with a *different*
     `tonic` version by removing the global patch (would break qdrant
     vector storage — non-starter).
   - Wait for qdrant to upgrade to a tonic version compatible with
     `opentelemetry-otlp` 0.31's `tonic` requirement.
   - Drop OTLP gRPC support entirely and rely on HTTP/protobuf
     (already shipped via `http-proto` + `reqwest-client`).

   This task only declares the workspace dep — it does not yet
   compile against the patched tonic, so the conflict will not surface
   until [task 01-02](02-cognee-observability-crate.md). It must be
   re-evaluated there.

2. **`hyper` patch conflict.** Similarly,
   [`Cargo.toml:110`](../../../Cargo.toml#L110) patches `hyper` to a
   qdrant fork at `v0.14.26`. `reqwest = "0.12"` (already in the
   workspace) and `opentelemetry-otlp`'s `reqwest-client` feature
   require `hyper` 1.x. Cargo's patch override is by name, not
   version, so the workspace currently builds *only* because nothing
   in the dependency graph forces `hyper` 1.x against the patch. As
   soon as the OTLP HTTP exporter is wired (task 01-04), this could
   surface as a "patch did not apply" warning or a hard build failure.
   **Mitigation**: confirmed at task 01-04 implementation time;
   options include scoping the patch (per-target) or upgrading
   qdrant.

3. **Exact-version pinning regret.** `=0.31`/`=0.32` is strict. If a
   `0.31.1` patch ships with a security fix or a critical bug, every
   bump requires a manifest edit. Documented trade-off; loosened in a
   follow-up once the API is exercised in CI.

4. **Future workspace members.** If a new crate (e.g. an Otel-aware
   metrics layer) is added later and needs a *different*
   `opentelemetry` minor, the workspace will reject the build because
   only one version of a `[workspace.dependencies]` entry can be
   selected. Resolution: either bump the whole workspace together
   (preferred, given pair-coupling above) or fall back to per-crate
   `[dependencies]` entries that bypass the workspace pin. This
   matches the Rust ecosystem's general expectation for OTEL.

5. **`opentelemetry-semantic-conventions` 0.31 stability.** Semconv
   0.31 declares some constants `unstable_*`. The names referenced
   later (`SERVICE_NAME`, `SERVICE_VERSION`,
   `DEPLOYMENT_ENVIRONMENT_NAME`) are stable as of this version. If
   the implementer of [task 01-04](04-init-telemetry.md) reaches for
   an unstable constant, they may need to enable a non-default
   feature on this crate. Out of scope here.

## Out of scope

This task **only** edits `[workspace.dependencies]`. The following are
explicitly deferred:

- Adding any of these crates to a member crate's `[dependencies]`
  table — that is [task 01-02](02-cognee-observability-crate.md)
  (`cognee-observability` crate scaffold) and
  [task 01-04](04-init-telemetry.md) (`init_telemetry` implementation).
- Wiring the `telemetry` cargo feature in `cognee-lib` or
  `cognee-cli` — that is [task 01-07](07-cli-feature-wiring.md) and
  related.
- Adding any source code, modules, or runtime initialisation —
  scaffolded by tasks 01-02 and onwards.
- Updating `Cargo.lock` with the new entries — happens automatically
  as part of [task 01-02](02-cognee-observability-crate.md).

## References

- Parent gap document:
  [`docs/telemetry/01-otel-otlp-export.md`](../01-otel-otlp-export.md),
  particularly the
  [Crate selection table](../01-otel-otlp-export.md#crate-selection-versions-current-as-of-2026-05)
  and
  [Design decisions (locked)](../01-otel-otlp-export.md#design-decisions-locked).
- [`opentelemetry` on crates.io](https://crates.io/crates/opentelemetry)
- [`opentelemetry_sdk` 0.31 docs](https://docs.rs/opentelemetry_sdk/0.31.0/opentelemetry_sdk/)
- [`opentelemetry-otlp` 0.31 docs](https://docs.rs/opentelemetry-otlp/0.31.0/opentelemetry_otlp/)
- [`opentelemetry-semantic-conventions` 0.31 docs](https://docs.rs/opentelemetry-semantic-conventions/0.31.0/opentelemetry_semantic_conventions/)
- [`tracing-opentelemetry` 0.32 docs](https://docs.rs/tracing-opentelemetry/0.32.1/tracing_opentelemetry/)
- [OpenTelemetry Rust release notes](https://github.com/open-telemetry/opentelemetry-rust/releases)
- [Cargo Book — workspace inheritance](https://doc.rust-lang.org/cargo/reference/workspaces.html#the-dependencies-table)
- Python parity reference:
  [`cognee/pyproject.toml [tracing] extra`](https://github.com/topoteretes/cognee/blob/main/pyproject.toml)
  and
  [`cognee/modules/observability/tracing.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py).
