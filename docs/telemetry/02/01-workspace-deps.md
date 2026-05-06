# Task 02-01 — Add workspace dependencies for `send_telemetry`

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**: nothing — this is the first task in the
[02-send-telemetry-analytics](../02-send-telemetry-analytics.md)
initiative.
**Blocks**:
- [Task 02-02 — `cognee-telemetry` crate scaffold](02-telemetry-crate-scaffold.md)
  (its `[dependencies]` table will pull from the workspace entries
  added here).
- [Task 02-03 — Identity derivation](03-id-derivation.md) (consumes
  `pbkdf2`, `hmac`, `hex`).
- [Task 02-05 — Client / dispatch / opt-out](05-client-dispatch-and-optout.md)
  (consumes `once_cell`, `reqwest`).
- Indirectly all subsequent tasks.

**Parent doc**: [02 — `send_telemetry()` Product-Analytics Client](../02-send-telemetry-analytics.md)

---

## 1. Goal

Add four entries to `[workspace.dependencies]` in the root
[`Cargo.toml`](../../../Cargo.toml) so the new `cognee-telemetry`
crate scaffolded in [task 02-02](02-telemetry-crate-scaffold.md) can
declare them as `{ workspace = true, optional = true }`:

| Crate | Version | Purpose | Notes |
|---|---|---|---|
| `pbkdf2` | `0.12` | PBKDF2-HMAC-SHA256 key-tracking-id derivation | `default-features = false` — we plug in our own PRF (HMAC-SHA256) via the `hmac` crate. The default feature pulls a random number generator we don't need. |
| `hmac` | `0.12` | HMAC-SHA256 PRF for `pbkdf2` | Pinned to `0.12` because that is the major-version pair `pbkdf2 = "0.12"` accepts. |
| `hex` | `0.4` | Lower-case hex encoding of the 16-byte PBKDF2 output | Tiny crate, no transitive deps. Could be replaced with a 12-line hand-rolled function, but using `hex` mirrors the rest of the workspace style. |
| `once_cell` | `1` | Process-wide `Lazy<reqwest::Client>` and `Lazy<PathBuf>` for cached IDs | Avoids re-running the TLS handshake and the home-directory lookup on every event. |

All four are minor crates with no surprise transitive bloat. The four
heavyweight deps (`reqwest`, `sha2`, `serde_json`, `dirs`, `chrono`,
`tracing`, `uuid`) are **already** in `[workspace.dependencies]` —
this task does **not** touch them.

The actual consumption of these deps happens in tasks 02-02, 02-03,
and 02-05; this task only edits `Cargo.toml`.

## 2. Rationale — why these four, why these versions

### `pbkdf2` 0.12 + `hmac` 0.12

Python uses `hashlib.pbkdf2_hmac("sha256", key, salt, 100_000, 16)`.
The Rust equivalent is the [`pbkdf2`](https://crates.io/crates/pbkdf2)
crate driven by `Hmac<Sha256>` from
[`hmac`](https://crates.io/crates/hmac). The version pair is
**load-bearing** for byte-parity:

- `pbkdf2 = "0.12"` exposes
  `pbkdf2::pbkdf2::<Hmac<Sha256>>(key, salt, iterations, &mut out)`.
  The signature changed in 0.13 (newer crates added a `hmac::digest`
  generic argument). Pinning to `0.12` keeps the call shape we cite
  in [task 02-03](03-id-derivation.md).
- `hmac = "0.12"` is the major-version pair: `pbkdf2 = "0.12"` requires
  `hmac >=0.12, <0.13`. Cargo will resolve correctly with any minor
  bump within `0.12.x`.

`default-features = false` is set on `pbkdf2` because the default
features include `parallel` (Rayon-based parallel iteration over many
keys) which we don't use — every event derives a single ID.

Python's reference iterations and dklen are `100_000` and `16`; both
are crate-agnostic constants enforced in [task 02-03](03-id-derivation.md).

### `hex` 0.4

Single function used: `hex::encode(&[u8; 16]) -> String`. Lowercase
output by default, matching Python's `derived.hex()`. The crate is
1.5 KB compiled and pulls nothing; adding it is cheaper than writing
and reviewing a hand-rolled implementation that has to be tested for
byte parity.

### `once_cell` 1

Two uses:

1. **`Lazy<reqwest::Client>`** — the HTTP client is constructed once
   per process. `reqwest::Client::new()` triggers TLS-stack
   initialisation (rustls trust-store load + certificate parsing)
   that is expensive enough to matter when telemetry is fired from
   every API call. `once_cell::sync::Lazy<reqwest::Client>` is the
   standard pattern.
2. **`Lazy<PathBuf>` for `~/.cognee/.persistent_id`** — the home-dir
   resolution is cheap but the result is immutable for the life of
   the process. Cache it once.

`std::sync::OnceLock` (stable since Rust 1.70) is an alternative, but
the workspace already uses `once_cell` extensively (40+ occurrences)
and the API is more ergonomic for closures.

### Why no `wiremock`

The original draft of the parent doc proposed `wiremock` for
integration tests. **Decision 10** in the locked decisions table
overrides this: the workspace already uses `mockito` (see
[`crates/cli/Cargo.toml:71-74`](../../../crates/cli/Cargo.toml) and
`crates/cloud/Cargo.toml`). Adding a second HTTP-mock library
duplicates dev-dep weight. `mockito` 1.x supports request matching,
delayed responses, and per-test isolation — sufficient for tasks
02-09 and 02-10.

## 3. Pre-conditions

- Repository at a clean working tree (`git status` reports nothing).
- `cargo check --workspace` passes on `main`.

## 4. Step-by-step

### 4.1 Open the workspace manifest

[`Cargo.toml`](../../../Cargo.toml). The `[workspace.dependencies]`
block currently spans lines 43-109. New entries must respect the
**alphabetical ordering** convention used in the rest of the block.

### 4.2 Insert `hex`

Sort position: `hex` slots between `futures-util` (line 59) and
`lbug` (line 60). Insert after line 59:

```toml
hex = "0.4"
```

### 4.3 Insert `hmac`

Sort position: `hmac` slots between `futures-util` (after `hex` is
inserted) and `lbug`. Cargo treats `h` < `l`. Insert immediately
after the `hex` line:

```toml
hmac = "0.12"
```

### 4.4 Insert `once_cell`

Sort position: `once_cell` slots between `notify` (line 65) and
`opentelemetry` (line 66). Insert after line 65:

```toml
once_cell = "1"
```

### 4.5 Insert `pbkdf2`

Sort position: `pbkdf2` slots between `pdfium-render` (line 74) and
`predicates` (line 75). Cargo treats `pb` < `pd` < `pe` so the
correct order is `pdf-extract`, `pdfium-auto`, `pdfium-render`,
`pbkdf2`, `predicates` — wait, that's wrong: `pb` < `pd`, so
`pbkdf2` actually slots **before** `pdf-extract`. Verify by sorting
the four strings alphabetically:

```
pbkdf2
pdf-extract
pdfium-auto
pdfium-render
predicates
```

So the insertion point is **between `opentelemetry_sdk` (line 70)**
(actually, `o` < `p`, so any `p*` entry comes after) **and the
existing `pdf-extract` (line 71)**. Insert after line 70:

```toml
pbkdf2 = { version = "0.12", default-features = false }
```

After all four insertions, the `[workspace.dependencies]` block looks
like:

```toml
...
futures = "0.3"
futures-util = "0.3"
hex = "0.4"
hmac = "0.12"
lbug = "0.14"
log = "0.4"
...
notify = "6.1"
once_cell = "1"
opentelemetry = { version = "=0.31", default-features = false, features = ["trace"] }
opentelemetry-otlp = { version = "=0.31", default-features = false, features = ["trace", "grpc-tonic", "http-proto", "reqwest-client"] }
opentelemetry-semantic-conventions = "=0.31"
opentelemetry_sdk = { version = "=0.31", default-features = false, features = ["trace", "rt-tokio"] }
pbkdf2 = { version = "0.12", default-features = false }
pdf-extract = "0.10"
...
```

### 4.6 Save the file. Do **not** run `cargo update` — the new
entries are inert until a member crate references them with
`workspace = true`. Cargo will resolve them on the next
`cargo check`.

## 5. Verification

```bash
# 1. The workspace still resolves.
cargo metadata --format-version 1 --no-deps > /dev/null

# 2. The four new entries are present and parse.
cargo tree --workspace --depth 0 --quiet 2>&1 | grep -E '^(hex|hmac|once_cell|pbkdf2)'
# Expected: each entry appears in the output.
# (No output for these crates yet because no member references them
# — that's fine. The check below is the real gate.)

# 3. The manifest is well-formed TOML and respects alphabetical order.
cargo metadata --format-version 1 --no-deps | jq '.workspace_members | length'

# 4. Full workspace check.
cargo check --workspace --all-targets
```

The fourth command is the gate. It must succeed unchanged from the
pre-condition state — this task adds inert workspace entries and
should not change resolution.

## 6. Files modified

Single file:

- [`Cargo.toml`](../../../Cargo.toml) — four insertions in
  `[workspace.dependencies]`.

No member-crate `Cargo.toml` changes in this task; those land in
[task 02-02](02-telemetry-crate-scaffold.md) (`cognee-telemetry/Cargo.toml`)
and follow-up tasks.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Version skew between `pbkdf2` and `hmac` | Low — major versions are pinned | If a future bump moves to `pbkdf2 = "0.13"`, [task 02-03](03-id-derivation.md) byte-parity tests will catch any signature drift before any caller breaks. |
| `once_cell` already pulled in transitively at a different version | None observed; current resolution is consistent | `cargo tree -i once_cell` to verify a single resolved version. If duplicates appear, use `[patch.crates-io]` to unify. |
| `hex` lowercase invariant breaks parity | None — `hex::encode` is lowercase by default and has been since 0.3 | A unit test in [task 02-08](08-unit-tests.md) asserts `id.starts_with("ak_")` and that the suffix is `[0-9a-f]{32}`. |
| Cargo alphabetical ordering drift on rebase | Low | The `cargo fmt --check` step in `scripts/check_all.sh` does not enforce TOML ordering, but the project convention is visible in the file — a reviewer would catch a drift. |

## 8. Out of scope

- Member-crate manifest edits (covered by [task 02-02](02-telemetry-crate-scaffold.md)).
- Public-API design (covered by [task 02-06](06-public-api-and-noop.md)).
- Test dev-deps (`mockito` is already present; no new dev-dep added
  by this task).
