# [iCodex] - 2026-07-20T08:46:00Z - Cognee sovereign intake productionization
# iPlan_Cognee_Sovereign_Intake_20260720

Signoff: iCodex (GPT-5), under Vishvakarma
Status: PARTIAL — scoped implementation complete; Rust execution held after interruption

## Authority and scope

This plan completes the highest-value credential-free portion of P1 after
reviewing `night/k-p1-cognee-codex` and the uncommitted sovereign-candidate
spike in `/Users/bardiya/iYogiForks/cognee-rs`. Work is isolated on
`codex/cognee-sovereign-intake-v1` from clean upstream commit
`c1b5570a53d74e2bb1fa8af1f6280cbacc6d240d`.

The deliverable is a pure, default-off Rust evidence boundary. It may convert
exact Cognee ingestion bytes and metadata into immutable iHash-bound candidate
records. It may not perform semantic resolution, audit, admission, database
writes, network I/O, model calls, or start any service. Existing uncommitted
fork work is evidence to audit and selectively recover; its worktree remains
untouched.

No native production database, credentials, Docker/VM/container, daemon,
watcher, HTTP/API server, socket listener, or external model endpoint is in
scope. The office MCP/model chain remains separately tracked by PR #6 and is
not started here.

## Names and identity boundaries

- `cognee-sovereign-candidate` is an ordinary, non-Loka internal Rust crate.
  Owner: the Cognee sovereign-successor lane. Bounded purpose: canonicalize,
  verify, and replay candidate evidence without admission authority.
- `sovereign_candidate_intake` is an ordinary, default-off ingestion module.
  Owner: `cognee-ingestion`. Bounded purpose: bind exact source/extracted bytes
  and Cognee digests into the candidate contract.
- Terminal wire labels ending in `-v1` are compatibility identities. Any field
  order or semantic change requires a new terminal wire version.
- iPikc's `ipikc-ihash` remains the digest authority. A convenience duplicate
  that merely emits the same BLAKE3 text is not acceptable as authority.
- No new Devata, Loka identity, compatibility alias, or admission capability is
  authorized.

## Anticipated outcomes

- [ ] **A1 — clean recovery:** selectively recover only the candidate-contract
      crate and default-off ingestion adapter from the dirty fork; no unrelated
      telemetry, bindings, HTTP, demo, license, workspace-pruning, or local
      patch changes enter this branch.
- [ ] **A2 — portable iHash authority:** bind the crate to an immutable,
      repository-portable `ipikc-ihash` source identity and freeze known-answer
      parity. No absolute path dependency or duplicate digest authority.
- [ ] **A3 — fail-closed candidate contract:** canonical bytes, strict sealed
      replay, immutable query/assertion identity, independent-auditor rules,
      contiguous transition/Karma heads, bounded fields/event counts, and
      corruption rejection are preserved by targeted tests.
- [ ] **A4 — zero-authority intake:** expose the adapter only behind the
      default-off `sovereign-candidate-intake` feature. It recomputes declared
      source/extracted digests and produces candidate status only; tests prove
      metadata relocation cannot alter exact observation identity and digest
      drift fails before sealing.
- [ ] **A5 — no runtime expansion:** the scoped diff contains no server,
      listener, socket, API route, database client/write path, credential read,
      telemetry, model call, downloader, or background-process lifecycle.
- [ ] **A6 — bounded quality gates:** with host safety checked first, run
      formatting, `cargo check`, tests, and clippy only for the new crate and
      ingestion feature, with `--jobs 3` and bounded timeouts. Record exact
      commands and counts.
- [ ] **A7 — publication hygiene:** inspect the entire scoped diff, stage exact
      paths, verify no secret/runtime/generated artifacts, commit with iCodex
      identity, push a `codex/*` branch if a writable GitHub fork is available,
      and open a draft PR without merging.
- [x] **A8 — GATE 2:** create
      `iWalkthrough/iWalkthrough_Cognee_Sovereign_Intake_20260720.md` mapping
      A1–A8 to `DONE / PARTIAL / SKIPPED / FAILED`, including tests, changed
      paths, names, blockers, publication state, and final process check.

## Execution bounds

- Build/test concurrency: at most 3 jobs.
- No repository-wide test, lint, build, or Git diff scan.
- No mutations in `/Users/bardiya/iYogi`,
  `/Users/bardiya/iYogiForks/cognee-rs`, or native PostgreSQL.
- If portable iHash authority cannot be established from an immutable source,
  stop the production claim and report A2/A7 as blocked rather than weakening
  identity.

Signoff: iCodex (GPT-5), under Vishvakarma

## Amendment 1 — 2026-07-20T08:50:00Z — fail-closed office telemetry

Dependency discovery established that the current standalone
`ipikc-ihash` commit `cd92502bb606879473c498bd0be5f45f8fbd6b30` is not a
portable Cargo source: its manifest intentionally resolves BLAKE3 through the
monorepo-local path `../../../../../../iYogiForks/blake3`. `ipikc-ihash` is not
published in a registry. Recovering the candidate spike would therefore force
one of three forbidden outcomes: an absolute host dependency, a copied digest
authority, or an unsanctioned direct change to the canonical iPikc repository.
The identity guard worked: the candidate source is not copied or weakened.

This amendment supersedes anticipated outcomes A1–A7 before source mutation;
the walkthrough will mark them `SKIPPED` with this evidence. A8 remains the
GATE 2 requirement. The highest-value independently completable P1 hardening is
instead the fork's existing fail-closed product-telemetry patch: network
emission becomes impossible unless an operator explicitly opts in. This closes
an immediate external-egress path for the local Cognee office instrument while
leaving all candidate/admission code absent and untouched.
The isolated branch was correspondingly renamed to
`codex/cognee-fail-closed-telemetry`; the original branch name remains in the
pre-amendment record as chronology, not standing scope.

### Amended anticipated outcomes

- [ ] **B1 — explicit permission:** product analytics remain disabled when
      `COGNEE_PRODUCT_TELEMETRY_ENABLED` is missing, empty, false-like, or
      unknown; recognized ASCII-case-insensitive values are exactly `1`,
      `true`, `yes`, and `on`.
- [ ] **B2 — suppression precedence:** a non-empty `TELEMETRY_DISABLED`,
      `ENV=test|dev`, and binding-host suppression still prevent emission even
      when explicit permission is present.
- [x] **B3 — pre-transport gate:** permission is evaluated before identity
      derivation, client construction, runtime fallback, or HTTP dispatch. No
      new API, endpoint, listener, server, credential, or background process is
      added.
- [ ] **B4 — bounded regression coverage:** update only the telemetry unit and
      existing integration fixtures that require intentional opt-in. Run
      formatter, telemetry library tests, check, and clippy with at most 3
      jobs. Socket-backed integration tests are inspected but not executed
      under this task's no-server bound.
- [x] **B5 — operator truth:** crate and operator documentation consistently
      state explicit opt-in, accepted values, and higher-priority suppression;
      no document claims opt-out/default-on behavior.
- [x] **B6 — scoped publication:** only telemetry behavior, its focused tests,
      documentation, this plan, and the GATE 2 walkthrough enter the commit.
      No files from the unrelated dirty fork worktree are copied. Publication
      remains draft-only and never merges.

`COGNEE_PRODUCT_TELEMETRY_ENABLED` is a new environment configuration token,
not a Loka identity. Its accepted-value and suppression semantics are now a
compatibility boundary; incompatible changes require a reviewed follow-up.

Signoff: iCodex (GPT-5), under Vishvakarma
