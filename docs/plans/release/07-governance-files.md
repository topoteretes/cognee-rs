# 07 — Governance files

> Wave 1 · Priority P1 (should-fix) · Track A · Release-blocking: no · Effort: 0.5d ·
> Depends on: [01 — Release decisions (D2 license)](01-decisions.md) ·
> Source: [release-readiness-plan.md](../release-readiness-plan.md) §7 T6.1–T6.4

[← Back to index](00-INDEX.md)

## Goal

Add the four standard open-source governance documents the repo is currently missing,
so the `0.1.0` release looks credible and contributors know how to participate:

| File | Path | Purpose |
|---|---|---|
| Contribution guide | `CONTRIBUTING.md` (repo root) | branching, commit style, test workflow, per-binding notes |
| Security policy | `SECURITY.md` (repo root) | private disclosure contact + supported-versions table |
| Code of conduct (optional) | `CODE_OF_CONDUCT.md` (repo root) | adopt Contributor Covenant v2.1 **by reference** |
| Release runbook | `docs/RELEASE.md` | how to cut/publish a release (crates.io / npm / PyPI / C artifact) |

All four are **new** files (verified: none exist yet). This task is **documentation only** —
it touches no `.rs`, no manifests, no schema, no IDs. **Zero parity / determinism risk.**

## Background & why

The repo ships three language bindings and a CLI but has **no contribution, security, or
release documentation**. A first public release needs at minimum a contribution guide and a
security policy; a code of conduct and a release runbook are strongly expected by GitHub's
community-health checks and by downstream packagers.

Two facts must be captured *accurately* (do not invent them — they are verified below):

1. **Commit convention.** Recent `git log` shows **Conventional Commits** with an optional
   scope and a mandatory `Co-Authored-By` trailer for AI-assisted commits. Examples from
   real history:
   - `fix(python): disable Rust test harness for the PyO3 extension module`
   - `python: hoist cloud ops, add serve/disconnect module-level functions (python bindings T10)`
   - `docs: record T11 commit hash`
   - Trailer used by this project: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
2. **Test workflow.** The canonical gates are `scripts/check_all.sh` (fmt → check →
   clippy -D warnings → C/Python/JS binding checks) and `scripts/run_tests_with_openai.sh`
   (downloads embedding models, runs the workspace tests single-threaded for LLM isolation).
   Per-binding check scripts: `capi/scripts/check.sh`, `js/scripts/check.sh`,
   `python/scripts/check.sh`.

The **license name** in these files is gated on **decision D2** (recommendation: `Apache-2.0`,
matching Python cognee). Use the value recorded in [01-decisions.md](01-decisions.md);
this doc assumes `Apache-2.0`.

## Prerequisites — read first

```bash
git checkout -b task/07-governance-files
```

Read / confirm before writing:

```bash
# 1. Confirm the resolved license (D2). Substitute everywhere if it differs.
sed -n '/D2/p' docs/plans/release/01-decisions.md

# 2. Re-confirm the commit convention from real history.
git log -15 --format='%s%n%b%n---'

# 3. Confirm the test scripts exist and read their headers.
ls scripts/check_all.sh scripts/run_tests_with_openai.sh \
   capi/scripts/check.sh js/scripts/check.sh python/scripts/check.sh

# 4. Confirm the contact email and repo URL to use.
#    Maintainer email (from project): dmytro@topoteretes.com
#    Repo: https://github.com/topoteretes/cognee-rust  (confirm the actual slug)
```

> If `scripts/check_all.sh` content drifts from what is described here, prefer what the
> script actually does — re-read it and adjust the wording.

## Files to change

| Path | Change |
|---|---|
| `CONTRIBUTING.md` (new) | fill the skeleton in Step 1 |
| `SECURITY.md` (new) | fill the skeleton in Step 2 |
| `CODE_OF_CONDUCT.md` (new) | fill the skeleton in Step 3 (reference only) |
| `docs/RELEASE.md` (new) | fill the skeleton in Step 4 |

## Implementation steps

These are **ready-to-fill skeletons**, not finished prose. Fill the `<...>` placeholders
with the values confirmed in Prerequisites. Keep them concise — do not pad with boilerplate.

### Step 1 — `CONTRIBUTING.md`

Create `/Users/dmytro/dev/cognee/cognee-rust/CONTRIBUTING.md`:

````markdown
# Contributing to cognee-rust

Thanks for contributing! cognee-rust is a Rust port of the Python
[cognee](https://github.com/topoteretes/cognee) AI-memory pipeline, with C, JS, and Python
bindings. The headline goal is **90%+ behavioral parity with Python cognee** — keep that in
mind when changing pipeline output, IDs, schema, or ranking.

## Getting started

```bash
git clone https://github.com/topoteretes/cognee-rust
cd cognee-rust
cargo build
```

See [`.claude/CLAUDE.md`](.claude/CLAUDE.md) for an architecture overview and the crate map.

## Branching & PRs

- **Branch off `main`.** Never commit directly to `main`.
- One logical change per branch / PR. Don't batch unrelated work.
- Suggested branch name: `task/<short-slug>` or `<type>/<short-slug>`.
- Open the PR against `main`; ensure CI (`ci.yml`) is green before requesting review.

## Commit messages

We use **[Conventional Commits](https://www.conventionalcommits.org/)** with an optional
scope:

```
<type>(<optional scope>): <imperative summary>

<optional body, wrapped at ~72 cols, explaining the *why*>
```

- **Types:** `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `ci`, `build`.
- **Scope** is usually a crate/binding (e.g. `python`, `cognify`, `capi`, `js`).
- Examples from this repo:
  - `fix(python): disable Rust test harness for the PyO3 extension module`
  - `feat(search): add triplet-completion retriever`
  - `docs: add release runbook`
- **AI-assisted commits** must include a co-author trailer, e.g.:
  ```
  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
  ```

## Coding conventions

- Run `cargo fmt` before committing.
- **`unwrap()` is forbidden in non-test code.** Use `expect("why it cannot fail")` with a
  reason, or propagate the error (`?` / `map_err` / `ok_or`). `Mutex/RwLock` lock guards may
  `.unwrap()` with a `// lock poison is unrecoverable` comment.
- Use `thiserror` for library error enums, `anyhow` in binaries/examples.
- Prefer `Arc<dyn Trait>` abstractions; keep public traits `Send + Sync`.
- **Parity is sacred:** do not change on-disk DB columns, content-hash inputs, UUID5
  namespaces/inputs, vector collection name formats, or stored-file naming unless the change
  is explicitly intended and called out — these stay byte-compatible with Python cognee.

## Testing

Before pushing, run the full gate:

```bash
scripts/check_all.sh
# fmt --check → cargo check --all-targets → clippy -D warnings → C/Python/JS binding checks
```

For tests that exercise the LLM / embedding path (cognify, search, fact extraction), use:

```bash
# Downloads BGE-Small embedding models if missing; runs single-threaded for LLM isolation.
bash scripts/run_tests_with_openai.sh                 # full workspace
bash scripts/run_tests_with_openai.sh <test_name>     # a single test
```

These need an OpenAI-compatible endpoint. Configure via `.env` at the repo root:
`OPENAI_URL`, `OPENAI_TOKEN`, and optionally `OPENAI_MODEL`
(see the "Running Integration & E2E Tests" section in `.claude/CLAUDE.md`).

Plain unit tests that don't touch the LLM run with `cargo test`.

## Language bindings

Each binding has its own check script (also invoked by `scripts/check_all.sh`):

| Binding | Source | Check | Notes |
|---|---|---|---|
| **C API** (`capi/`) | separate Cargo workspace | `bash capi/scripts/check.sh` | FFI must never panic across the boundary — sanitize/propagate, never `unwrap()` caller data. Headers + built lib are the artifact. |
| **JavaScript** (`js/`) | Neon (`js/cognee-neon/`, standalone crate) | `bash js/scripts/check.sh` | Return JS errors instead of panicking into the V8 runtime. |
| **Python** (`python/`) | PyO3 (`cognee-python`, workspace member) | `bash python/scripts/check.sh` | Exercised by pytest (the Rust test harness is disabled for the extension module — it has no libpython at link time). |

When you change core crate behavior, check whether the bindings expose it and update them
(and their tests) to keep the SDK surfaces in sync.

## Cross-SDK parity

Parity with Python cognee is verified by the `e2e-cross-sdk/` Docker harness:

```bash
cd e2e-cross-sdk && docker compose up --build
```

If your change could affect IDs, schema, chunking, prompts, or vector collections, run it.

## License

By contributing you agree your contributions are licensed under the project's
**Apache-2.0** license (see [`LICENSE`](LICENSE)).
````

### Step 2 — `SECURITY.md`

Keep it a **short professional stub** in GitHub's standard security-policy format
(see <https://docs.github.com/code-security/getting-started/adding-a-security-policy-to-your-repository>).
Create `/Users/dmytro/dev/cognee/cognee-rust/SECURITY.md`:

````markdown
# Security Policy

## Supported versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | ✅        |
| < 0.1   | ❌        |

## Reporting a vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Report privately via one of:

- GitHub's [private vulnerability reporting](https://github.com/topoteretes/cognee-rust/security/advisories/new)
  ("Report a vulnerability"), or
- email **<security@topoteretes.com — confirm this alias exists; if not, use info@topoteretes.com (Python cognee's security contact) or dmytro@topoteretes.com>**.

Please include a description, reproduction steps, affected version(s), and impact.
We aim to acknowledge within **3 business days** and to provide a remediation timeline after
triage. Coordinated disclosure is appreciated — we will credit reporters unless they prefer
to remain anonymous.
````

> Enable GitHub "Private vulnerability reporting" in repo Settings → Security so the advisory
> link works. If you do not have a dedicated security mailbox, drop the first bullet and keep
> only the email.

### Step 3 — `CODE_OF_CONDUCT.md` (optional)

Adopt the **Contributor Covenant v2.1 by reference** — do **not** paste the full text.
Create `/Users/dmytro/dev/cognee/cognee-rust/CODE_OF_CONDUCT.md`:

````markdown
# Code of Conduct

This project adopts the **[Contributor Covenant, version 2.1](https://www.contributor-covenant.org/version/2/1/code_of_conduct/)**.

By participating, you are expected to uphold this code. Report unacceptable behavior to
**<conduct@topoteretes.com — confirm this alias exists; fallback dmytro@topoteretes.com — note: the Python cognee COC has this contact unfilled>**. All reports are
reviewed and handled confidentially.

The full text is available at
<https://www.contributor-covenant.org/version/2/1/code_of_conduct/>.
````

> If you prefer the full text in-repo for GitHub's community-profile checkmark, download the
> canonical `CODE_OF_CONDUCT.md` from contributor-covenant.org and fill the contact line —
> but the by-reference stub above satisfies the release requirement.

### Step 4 — `docs/RELEASE.md`

Create `/Users/dmytro/dev/cognee/cognee-rust/docs/RELEASE.md` — an outline runbook that
points at the existing scripts/CI rather than restating them:

````markdown
# Release runbook

How to cut and publish a cognee-rust release. Two tracks (see
[docs/plans/release/00-INDEX.md](plans/release/00-INDEX.md)):

- **Track A** — bindings + source: PyPI (`cognee`), npm (`cognee`), C-library artifact,
  GitHub source tag. Does **not** require crates.io publishability.
- **Track B** — crates.io: publish the `cognee-*` library crates (gated on removing
  git deps / `[patch.crates-io]`; see release task 24).

## Pre-flight (all tracks)

1. Ensure `main` is green: `ci.yml` passing, cross-SDK parity (`http-parity.yml`) green
   where applicable.
2. Run the local gate: `scripts/check_all.sh` (fmt → check → clippy -D warnings → binding
   checks).
3. Bump versions:
   - `[workspace.package] version` in `Cargo.toml` (crates inherit via `version.workspace`).
   - `capi/Cargo.toml`, `js/cognee-neon/Cargo.toml` (separate/standalone — bump manually).
   - `python/pyproject.toml`, `js/package.json`.
   - Keep all four in sync.
4. Update `CHANGELOG.md` (Keep a Changelog format) with the new version section.
   *(Note: `CHANGELOG.md` does not exist yet — it is created by release task 22. Add a stub entry here or skip until task 22 lands.)*
5. Confirm `LICENSE` and license metadata are present (see release task 02).

## Tag

```bash
git checkout main && git pull
git tag -a vX.Y.Z -m "cognee-rust vX.Y.Z"
git push origin vX.Y.Z
```

## Publish — PyPI (Python binding)

```bash
bash python/scripts/check.sh        # gate
cd python
maturin build --release             # build wheel(s) + sdist
# Dry run first, then publish:
maturin publish --dry-run
maturin publish                     # needs PyPI token (MATURIN_PYPI_TOKEN / ~/.pypirc)
```

## Publish — npm (JS binding)

```bash
bash js/scripts/check.sh            # gate (builds the .node artifact)
cd js
npm publish --dry-run
npm publish                         # needs npm auth (npm login / NPM_TOKEN)
```

Confirm `package.json` `files` allowlist includes `cognee_neon.node`, `lib/`, and `LICENSE`.

## Publish — C-library artifact

```bash
bash capi/scripts/check.sh          # gate
# Build the release library + assemble headers + LICENSE into a dist dir/tarball.
# (capi/ is its own workspace; build from there.)
```

Attach the resulting tarball (lib + headers + `LICENSE`) to the GitHub Release for the tag.

## Publish — crates.io (Track B only)

> Blocked until release task 24 removes git deps / `[patch.crates-io]` from the published
> dependency graph. Until then, `cargo publish` will refuse non-leaf crates.

```bash
# Dry-run each crate in dependency order (leaves first):
cargo publish --dry-run -p cognee-models
# ... then publish in the same order:
cargo publish -p cognee-models
```

## Post-release

1. Create a GitHub Release from the tag; paste the `CHANGELOG.md` section; attach the C
   artifact.
2. Verify installs: `pip install cognee==X.Y.Z`, `npm install cognee@X.Y.Z`.
3. Open the next `-dev` version bump PR if you use one.
````

> The exact `maturin` / `npm` / C packaging commands may already live in `python/scripts/`,
> `js/scripts/`, or CI — prefer wiring the runbook to those scripts over hand-typed commands
> if they exist. Re-check before finalizing.

## Verification

```bash
# 1. All four files exist.
ls CONTRIBUTING.md SECURITY.md CODE_OF_CONDUCT.md docs/RELEASE.md

# 2. No leftover placeholders.
grep -rn '<.*confirm.*>\|<...>\|TODO' CONTRIBUTING.md SECURITY.md CODE_OF_CONDUCT.md docs/RELEASE.md \
  && echo "FILL THE PLACEHOLDERS ABOVE" || echo "no placeholders ✔"

# 3. License name matches D2 everywhere it appears.
grep -rn -i 'apache\|mit\|license' CONTRIBUTING.md SECURITY.md  # eyeball against D2

# 4. The scripts referenced actually exist.
for s in scripts/check_all.sh scripts/run_tests_with_openai.sh \
         capi/scripts/check.sh js/scripts/check.sh python/scripts/check.sh; do
  test -f "$s" && echo "ok $s" || echo "MISSING $s"
done

# 5. Markdown links resolve (manual or with a link checker if available).
#    Check the relative link to LICENSE and to .claude/CLAUDE.md in CONTRIBUTING.md.
```

No source test is needed (docs-only). Optionally confirm GitHub's community-profile page
(`/community`) shows all four checkmarks after the files land on the default branch.

## Acceptance criteria

- [ ] `CONTRIBUTING.md` exists: branching-off-`main`, Conventional-Commits + `Co-Authored-By`
      trailer, `scripts/check_all.sh` + `scripts/run_tests_with_openai.sh` workflow, and
      per-binding (C/JS/Python) notes.
- [ ] `SECURITY.md` exists: private disclosure channel + supported-versions table (GitHub
      standard format).
- [ ] `CODE_OF_CONDUCT.md` exists and adopts **Contributor Covenant v2.1 by reference**
      (full text not pasted) with a working contact.
- [ ] `docs/RELEASE.md` exists: pre-flight + tag + per-channel publish steps (PyPI / npm /
      C artifact / crates.io) referencing the existing scripts/CI.
- [ ] License name in all files matches the D2 decision.
- [ ] No unresolved `<...>` placeholders remain.
- [ ] No `.rs` / manifest / schema changes (docs-only).

## Gotchas / do-not

- **Do not guess the license.** It is gated on D2. If D2 ≠ `Apache-2.0`, substitute the
  SPDX name in `CONTRIBUTING.md` Step 1 and anywhere else it appears.
- **Do not paste the Contributor Covenant in full** — reference it by name + version + URL.
  Keep `SECURITY.md` and `CODE_OF_CONDUCT.md` short and standard; don't author bespoke legal
  or sensitive policy text.
- **Do not invent the commit convention** — it's verified from `git log`: Conventional
  Commits + the `Co-Authored-By` trailer for AI-assisted commits. Don't drop the trailer.
- **Confirm contact addresses** before publishing; the `security@`/`conduct@` aliases may
  not exist — fall back to `dmytro@topoteretes.com` if not.
- **Confirm the repo slug** in URLs (`topoteretes/cognee-rust`) — the advisory link must
  point at the real repo, and GitHub private vulnerability reporting must be enabled for it
  to work.
- **Don't restate the test scripts' internals** in `RELEASE.md` — point at them; they are the
  source of truth and may change.

## Rollback

Pure additive docs. To revert:

```bash
rm -f CONTRIBUTING.md SECURITY.md CODE_OF_CONDUCT.md docs/RELEASE.md
```

No code, schema, or data implications.
