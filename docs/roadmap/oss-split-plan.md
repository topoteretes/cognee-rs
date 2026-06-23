# OSS / Closed-source repository split — plan

Status: **planning**. This is the index + decision log for splitting cognee-rust
into a public, crates.io-publishable open-source SDK and a private closed-source
cloud product.

## 1. Rationale

Today cognee-rust is one workspace that mixes three concerns:

1. **A portable AI-memory SDK** (add → cognify → search) that should be free,
   open, and trivially adoptable (`cargo add cognee-lib`, `pip install`,
   `npm install`).
2. **Cloud / multi-tenant functionality** — Auth0 device-code login, the
   `CloudClient` proxy, tenant provisioning, users/roles/tenants/ACL — that is
   the commercial product.
3. **Heavyweight or git-only adapters** (Qdrant via git, LiteRT via git) that
   both block crates.io publishing and, in some cases, represent paid scale.

Splitting lets us (a) publish a clean OSS core to crates.io with C/Python/TS
bindings, maximising adoption, while (b) keeping authorization, multi-tenancy,
the cloud control plane, and premium adapters in a private repo that layers on
top of the published crates.

### The boundary principle

**A Cargo feature flag is not a security boundary** — anyone with the source can
re-enable it. The only real boundary is the *physical absence of the source*.
Therefore:

- The OSS repo contains **no cloud crate, no auth implementation, no
  `cloud`/`server` cloud code, and no `cloud` feature**.
- The OSS repo exposes **traits + injection/registration seams**. Closed code
  *provides implementations* and assembles its own binaries/cdylibs.
- Wiring is `Arc<dyn Trait>` injection, not `#[cfg(feature)]` selection — the
  OSS code never names a closed type.

## 2. Locked decisions

| Decision | Choice |
|---|---|
| Dependency channel | Closed repo depends on OSS via `git` + `rev` (commit-pinned), with a `[patch]` to a local path for dev. OSS also publishes versioned crates to **crates.io** for the public. |
| OSS tools | Single-user **CLI + HTTP server** (no auth, no multi-tenant, no cloud/serve). |
| OSS vector store | **pgvector** moved to OSS (pure-Rust client, no git deps → publishable). Plus a small **pure-Rust brute-force** embedded adapter so the offline/Android profile keeps a vector store. |
| Identity model | Keep `owner_id`/`tenant_id` in the core data model and UUID5 ID derivation (Python parity, content-addressing). Single-user mode = default all-zeros owner, `tenant_id = None`, `enable_access_control = false`. **No** ACL *implementation* in OSS — only the `AclDb` trait. |
| ONNX | Stays **open** (preserves local/edge embeddings; `ort` is on crates.io). |
| Qdrant | → **closed** (git dep; also the scale differentiator). |
| LiteRT (on-device LLM) | → **closed** (git dep; on-device LLM becomes premium). |
| Cloud (serve/disconnect/CloudClient), auth, users/roles/tenants/ACL impl | → **closed**. |
| OSS license | **`MIT OR Apache-2.0`** (Rust-ecosystem default; max adoption). NB: the current workspace declares `license = "Apache-2.0"` only ([Cargo.toml](../../Cargo.toml)); adopting the dual license requires adding a `LICENSE-MIT` file and bumping the per-crate `license` field (Phase 1, step 1). Accepted consequence: a permissive core lets anyone — including competitors — host cognee; the split protects only the cloud/auth/proprietary-adapter code, **not** the core. The closed superset is all-rights-reserved (never touches crates.io). |

### Empirically-closed alternative

Ladybug-as-vector-store was investigated and **rejected** (spike, RED): lbug
0.14.1 ships no bundled `vector` extension, `INSTALL vector` needs network, and
the downloaded extension fails `dlopen` against our `static:+whole-archive`
linking (`undefined symbol: lbug::catalog::IndexAuxInfo`). See branch
`worktree-agent-a7d44fb260bfaa70e`, test `crates/graph/tests/ladybug_vector_spike.rs`.

### Known gap (tracked)

pgvector requires a Postgres server, so it is **not embedded**. The brute-force
adapter restores an embedded vector option for edge/Android; without it the OSS
offline profile has no vector search. The brute-force adapter is therefore a
required Phase-2 item, not optional.

## 3. Target topology

```
OSS REPO  (cognee-rust, MIT OR Apache-2.0, public, crates.io)
├── crates (publishable, ZERO git deps):
│   models, utils, storage, logging, telemetry, observability,
│   chunking, core, ingestion, cognify, search, delete,
│   database (sqlite + postgres), graph (ladybug + pggraph),
│   vector (trait + brute-force + pgvector + Mock),
│   embedding (onnx + openai + ollama + Mock),
│   llm (trait + openai + Mock), session (fs + redis + sea-orm),
│   ontology, visualization, lib (single-user umbrella),
│   bindings-common (core ops)
├── internal (in-repo, publish = false): examples, bench, test-utils,
│   e2e-cross-sdk/telemetry-emit  (test/bench harnesses, not on crates.io)
├── tools: cognee-cli (single-user), cognee-http-server (no-auth)
├── bindings: capi/ (C headers + artifacts), python/ (PyPI `cognee-pipeline`,
│   module `cognee_pipeline`), js/ (npm `cognee`)  — core surface
└── .github/workflows: lint, test, doc, publish-dry-run, bindings build
```

> Errata: T10a marks `bindings-common` as `publish = false` per Option C; it ships compiled-in inside the wheel/npm/tarball alongside `python`, `js/cognee-neon`, and `capi/cognee-capi`. The Option A move-to-closed (relocating the cloud glue so `bindings-common` can publish to crates.io) is scheduled for T15.
>
> Errata: T10c removes `cognee-http-server`'s runtime dependency on `cognee-test-utils` (the unpublishable in-repo harness). The `dev-mock` feature now enables `cognee-vector/testing` directly — `MockVectorDB` already lives in `cognee-vector` behind that feature, and `cognee-test-utils` only re-exported it. No new `cognee-vector-mock` crate was needed; the publishable surface stays the same.

```
CLOSED REPO  (cognee-cloud-rust, private)  — depends on OSS via git rev
├── cognee-cloud            (serve/disconnect/CloudClient, Auth0)
├── cognee-access-control   (users/roles/tenants/ACL impl of AclDb)
├── cognee-vector-qdrant    (git dep, scale vector)
├── cognee-llm-litert       (git dep, on-device LLM)
├── http-auth               (auth/* + auth/users/api-key routers)
├── cognee-cloud-lib        (umbrella: re-export OSS lib + inject closed adapters + cloud)
├── bindings-cloud          (capi/python/js with full + cloud surface)
├── tools: full CLI + full HTTP server (auth, multi-tenant, serve)
└── .github/workflows: lint, test, build, bindings publish (private registries)
```

## 4. Seams that must exist before the split

These land in the **current** repo first (mergeable to `main`, no split yet):

- **S1 — Adapter injection.** Every backend reachable through a builder taking
  `Arc<dyn Trait>` (VectorDB, GraphDBTrait, EmbeddingEngine, Llm, SessionStore,
  AclDb). Audit `cognee-lib::component_manager` / `CogneeServices` for any
  `#[cfg(feature)]`-based selection of a *closed* adapter and replace with
  injection. (Mostly already true.)
- **S2 — ACL trait boundary (orphan-rule aware).** The auth logic today is
  `impl AclDb/UserDb/RoleDb/TenantDb **for DatabaseConnection**`
  ([acl_db.rs:80](../../crates/database/src/traits/acl_db.rs), `ops/{user,role,tenant}.rs`).
  A closed crate cannot re-impl an OSS trait for the OSS `DatabaseConnection`
  type (orphan rule). Therefore:
  - `AclDb` **trait stays in OSS** `cognee-database` (OSS `ingestion`/`delete`/`lib`
    reference `dyn AclDb`); its **impl is removed from `DatabaseConnection`** and
    re-created in the closed crate on a **newtype wrapper**
    (`struct AccessControl(DatabaseConnection)`). OSS must expose the underlying
    sea-orm connection/pool so the wrapper can query.
  - `UserDb`/`RoleDb`/`TenantDb` traits + impls + ops move **entirely** to closed
    (nothing in the OSS pipeline needs them once S2b lands).
  - **S2b — single baseline migration must be cleaved.** `migrator/mod.rs`
    registers one migration ([`m20260914_000001_baseline`](../../crates/database/src/migrator/m20260914_000001_baseline.rs))
    that creates *all 33 tables* and runs seed/backfill SQL. OSS ships a
    **core-only** baseline and exposes `core_migrations()` publicly; the closed
    `Migrator` **composes** core + an auth migration that adds
    `users/roles/tenants/acls/principals/permissions` to the same DB. The
    `acls.dataset_id → datasets.id` FK is closed→OSS (one-directional), so
    ordering is safe. Three subtleties the cleave **must** handle (verified):
    - **Seeds move with the auth tables, not just the DDL.** The baseline's seed
      block (≈ lines 1419–1480) is not only `permissions` rows — it **backfills
      `principals` and `acls` by reading the `datasets` table**, and inserts the
      default user/principal. The principals/acls backfill therefore **reads core
      tables**, which is why the auth migration must run *after* the core baseline
      (closed→OSS read; ordering already enforced by registration order).
    - **The seeds are SQLite-only.** They use `datetime('now')` / `randomblob(16)`;
      the in-file comment already notes "Postgres lane may not run these seeds."
      The relocated auth migration must become **dialect-aware** (or the Postgres
      lane must seed separately) — do not silently inherit the SQLite-only SQL.
    - **Migration-identity hazard for already-migrated DBs.** SeaORM records
      applied migrations by name (`DeriveMigrationName` → module name). A DB that
      already applied the *combined* `m20260914_000001_baseline` has that name in
      `seaql_migrations`, so a **renamed-but-shrunk** OSS baseline under the same
      name is skipped (DB keeps its full shape — acceptable). Safety therefore
      requires: (a) the closed auth migration uses a **new name**
      (`m20260914_000002_auth`) with `if_not_exists`, so it is a no-op on
      pre-split DBs; and (b) `down()` is **split** — the OSS baseline's `down()`
      must stop dropping auth tables, or a rollback on a fresh OSS DB will try to
      drop tables it never created.
  - **S2c — OSS default user is DB-free.** `get_or_create_default_user`
    ([api/user.rs](../../crates/lib/src/api/user.rs)) currently writes a `users`
    row via `&dyn UserDb`. `data`/`dataset.owner_id` is a plain indexed `TEXT`
    (no FK to `users`), so OSS needs no users table: the OSS default user becomes
    a **constant `User`** from `default_user_id`; the DB-backed version moves to
    closed. Note this function is also called from `bindings-common`
    ([services.rs](../../crates/bindings-common/src/services.rs)) and
    `ops/admin.rs`, so the constant-user path must be threaded through those
    bootstrap sites too.
  - **S2d — remove the `DatabaseConnection → dyn AclDb` casts in OSS.** Removing
    `impl AclDb for DatabaseConnection` (S2) breaks every site that *self-builds*
    an `AclDb` from the bare connection. Audit confirms the **production** sites
    are in the HTTP server — `routers/{add,update,remember}.rs`
    (`with_acl_db(database.clone() as Arc<dyn AclDb>)`) — and the CLI
    `commands/delete.rs` (`--enforce-acl` path); the casts in
    [api/datasets.rs](../../crates/lib/src/api/datasets.rs) (≈ 471/658/683) and the
    `delete` crate are **tests**. The core pipeline itself is already safe: it
    takes `Option<&dyn AclDb>` and skips checks when `None`, so single-user
    `add`/`cognify`/`delete` need no impl. Fix: these sites take an **injected**
    `Option<Arc<dyn AclDb>>` (closed supplies the `AccessControl` newtype; OSS
    passes `None`) instead of constructing one from the connection. The HTTP add/
    update/remember routers move to (or are gated alongside) the closed server per
    S3; the CLI `--enforce-acl` path becomes a closed-only flag or an injected
    backend. `MockAclDb` ([test-utils](../../crates/test-utils/src/mock_acl_db.rs))
    already proves a non-DB `AclDb` impl is trivial for the test sites.
  - **S2e — the auth *entities* and domain models move too, not just the
    traits/ops.** The 13 sea-orm entity files for auth tables (`user`, `tenant`,
    `role`, `principal`, `permission`, `acl`, `user_tenant`, `user_role`,
    `user_api_key`, the three `*_default_permission` tables, `principal_configuration`)
    live one-per-file under [crates/database/src/entities/](../../crates/database/src/entities/)
    co-located with the core entities — a clean file move, but
    [entities/mod.rs](../../crates/database/src/entities/mod.rs) re-exports must be
    cleaved and the OSS database crate must compile without them (verified: no OSS
    core code references the auth entity types). Domain models: `User` stays in OSS
    `cognee-models` (the DB-free default user of S2c needs it) and the `permission`
    **string constants** stay OSS (the `AclDb` trait references them); `Role`/`Tenant`
    and the auth-only entity structs move to closed.
- **S3 — HTTP auth + cloud-router injection.** `build_router`
  ([http-server/src/lib.rs](../../crates/http-server/src/lib.rs)) currently
  **statically `.nest()`s every router** — there is no injection seam today. The
  refactor makes the closed-surface routers injected/optional, runs the OSS server
  on the no-auth default-user path (`require_authentication=false`), and lets the
  closed server mount the rest + an `ExtraAuthValidator`. Verified scope (larger
  than the original "move 2 routers"):
  - **9 routers move to / are gated alongside the closed server**, not 2: the
    cloud pair `sync` ([routers/sync.rs](../../crates/http-server/src/routers/sync.rs))
    + `checks` ([routers/checks.rs](../../crates/http-server/src/routers/checks.rs)),
    **plus the auth/identity family** — `auth`, `auth_register`,
    `auth_reset_password`, `auth_verify`, `api_keys`, `users`, `users_by_email`.
  - **`cognee-cloud` is a *hard* dep** ([Cargo.toml:93](../../crates/http-server/Cargo.toml)),
    used only by `checks` (`check_api_key`/`cloud_url`) and `sync`
    (`sync::run_background`). Moving those two routers removes the dep from the OSS
    http-server (the dep travels with the routers, since `run_background` lives in
    `cognee-cloud`).
  - **Already-safe, needs no change (audit-confirmed):** `SyncRegistry`
    ([sync/registry.rs](../../crates/http-server/src/sync/registry.rs)) is a *local*
    in-memory type with **no** cloud coupling; the shutdown hook in
    [lifecycle.rs](../../crates/http-server/src/lifecycle.rs) is already
    `if let Some(sync_ops)`-guarded and no-ops when sync isn't wired; `AppState.auth`/
    `.lib` and the `ComponentHandles` `sync_ops`/`cloud_client`/`permissions` fields
    are already `Option` and default to `None`/empty.
  - **Follow the existing local-trait injection precedent.** `CloudDeleteClient`
    ([cloud_client.rs](../../crates/http-server/src/cloud_client.rs)) is already the
    pattern: a local trait held as `Option<Arc<dyn …>>` in `ComponentHandles`,
    `None` by default, checked before use. `ExtraAuthValidator`
    ([auth/context.rs](../../crates/http-server/src/auth/context.rs)) likewise exists
    but is **always `None`** — S3 adds a `with_extra_validator(...)` builder so the
    closed server can inject it at router-build time.
- **S4 — Git-dep adapter extraction.** Move the Qdrant adapter out of
  `cognee-vector` into its own crate and the LiteRT adapter out of `cognee-llm`,
  so neither published crate declares a git dependency (even optional ones block
  crates.io).
- **S5 — Bindings reuse seam.** The reuse is *already structurally present*: the
  Python (PyO3) and JS (Neon) bindings both call `cognee_bindings_common::ops::*`
  directly (thin language-marshaling glue, no duplicated logic), and cloud is
  isolated to a single `ops/cloud.rs` (verified — it is the only cloud-coupled
  file). So the closed cdylib just **depends on the OSS `bindings-common` crate and
  adds a cloud-ops module** — no function-pointer registry needed (see §6.1).
  S5's only real work is making `ops/cloud.rs` cleanly liftable to the closed side.
- **S6 — Cloud liftability.** `cognee-lib` re-exports of `cognee_cloud::*` and
  `api/serve.rs` are isolated so they move out wholesale; OSS lib compiles with
  no reference to cloud.
- **S7 — Feature-default hygiene (`cloud` is currently ON by default).** Verified:
  `cloud` sits in the `default = [...]` set of **five** crates — `cognee-lib`,
  `cognee-cli`, `cognee-bindings-common`, `python` (`cognee-pipeline`), and
  `js/cognee-neon` — all forwarding to `cognee-lib/cloud → dep:cognee-cloud`. The
  OSS repo has no `cloud` feature at all, so before/at the split `cloud` must be
  removed from every `default` set (and the closed builds re-add it). Note this is
  a **behaviour change for today's users**: a default `cargo build` / default wheel
  currently ships cloud ops and will stop doing so. `serve`/`disconnect` in the CLI
  are already `#[cfg(feature = "cloud")]`-gated, so they vanish cleanly; the
  default-user bootstrap is config-driven (no DB write), so the OSS CLI is otherwise
  clean. (`visualization`, `qdrant`, `pgvector`, `telemetry` in the default sets are
  fine — `visualization` is OSS; `qdrant` is feature-gated and simply absent in OSS;
  `telemetry` flips to opt-in per §6.)

## 5. Step-by-step plan

### Phase 0 — Seams (in current repo, no split)
1. S1 adapter-injection audit + builder hardening.
2. S2 extract `cognee-access-control` (traits/ops/**entities**/migrations); split
   the baseline migration into core + auth migrations (S2b); make the OSS default
   user DB-free (S2c); remove the `DatabaseConnection → dyn AclDb` casts in OSS
   production sites (S2d); move the 13 auth entity files + `Role`/`Tenant` domain
   models, keeping `AclDb` trait, `User`, and the permission constants in OSS
   (S2e). **Highest risk — do first, behind full test run.**
3. S3 `build_router` injection + OSS no-auth default path (9 routers + the
   `cognee-cloud` dep removal + the `ExtraAuthValidator` builder).
4. S4 extract `cognee-vector-qdrant` + `cognee-llm-litert`; confirm
   `cargo tree -e no-dev` shows no git deps in the to-be-published crates.
5. Add the pure-Rust **brute-force** `VectorDB` impl + move pgvector into OSS
   vector defaults.
6. S5 bindings reuse seam; isolate `ops/cloud.rs` (see §6.1 — prefer a plain
   crate dependency over a function-pointer registry).
7. S6 isolate cloud re-exports; S7 remove `cloud` from the `default` feature set
   of the five crates that carry it (lib, cli, bindings-common, python, neon).
8. **Continuous OSS-isolation CI gate (do not defer to split day).** Today the
   OSS subset does **not** build alone — `cognee-http-server`'s hard `cognee-cloud`
   dep means any workspace command pulls in closed code. Add CI that proves OSS
   self-containment *continuously*, growing with the seams:
   - *Now (cheap guard):* loop `cargo check -p <crate> --no-default-features` over
     every to-be-OSS crate; fail on any non-optional dep on a to-be-closed crate.
     (`ci.yml` already does this for `cognee-lib` alone — generalise it.)
   - *After S3/S4 land:* a second virtual workspace manifest (OSS members only,
     closed crates excluded) running `cargo check --all-targets` +
     `cargo tree -e no-dev | grep git+` as a hard gate. This is the real proof and
     the precursor to Step 1 of §8.
9. **Land the partition-manifest gate early (from §8 Step 0).** Check
   `scripts/split/{oss,closed}-paths.txt` into the *current* repo now, with the CI
   assertion that their union equals `git ls-files` and their intersection is
   empty. Doing this in Phase 0 (not at split day) forces every newly-added file
   into a classification the moment it lands, so nothing can silently leak later.
   *Exit criteria:* current repo still green (`scripts/check_all.sh` + full test
   run); the OSS-only manifest builds `--all-targets` with zero git deps; the
   partition manifest covers 100% of tracked paths.

### Phase 1 — crates.io readiness (still one repo)
1. Per-crate metadata: `description`, `license = "MIT OR Apache-2.0"`,
   `repository`, `readme`, `keywords`, `categories`; crate-level `README.md` where
   missing.
2. **Convert internal deps to `path` + `version`.** crates.io forbids bare
   `path`/`git` deps; every `cognee-* = { path = ... }` becomes
   `{ path = "...", version = "0.1" }`. (dev-deps are exempt.)
3. **docs.rs for ONNX crates.** Any crate touching `ort` needs
   `[package.metadata.docs.rs] features = ["load-dynamic"]` +
   `rustdoc-args = ["--cfg","docsrs"]`, else docs.rs (no network) fails to build.
4. Reserve the names **now** (placeholders) — marquee names get squatted. This
   spans **three registries**: crates.io (`cognee-*`), PyPI (`cognee-pipeline` —
   verified *not* colliding with the Python SDK's `cognee`), and npm (`cognee` —
   confirm the topoteretes org actually owns this name before relying on it).
5. **Mark internal crates `publish = false`.** `test-utils`, `bench`, `examples`,
   and `e2e-cross-sdk/telemetry-emit` are in-repo harnesses, not crates.io
   artifacts. `test-utils` in particular currently defaults to `publish = true` and
   would otherwise be pushed; it also carries `MockAclDb` (an `AclDb` mock) so when
   `AclDb`'s impl moves closed (S2) its test mocks may need a closed companion.
6. **Pin or vendor the LiteRT git dep.** `cognee-litert-lm`
   ([Cargo.toml:67](../../Cargo.toml)) tracks the default branch with **no rev/tag**
   — a reproducibility/security hazard and a crates.io blocker. Pin it to a commit
   (the qdrant git deps are already tag-pinned). It moves to closed with S4, but
   pin it regardless.
7. CI job: `cargo publish --dry-run -p <crate>` for every OSS crate in
   topological order; fail on any git dep (`cargo tree -e no-dev | grep git+`).
8. Adopt **release-plz** (CI) for version bumps + dependency-ordered publishing;
   document the order (foundation → middle → pipelines → lib → bindings).

### Phase 2 — Physical split
1. Create the private `cognee-cloud-rust` repo.
2. Move into it: `cognee-cloud`, `cognee-access-control`, `cognee-vector-qdrant`,
   `cognee-llm-litert`, the http auth module + auth/users/api-key routers,
   `cognee-cloud-lib` (umbrella), the cloud bindings crates, and the full
   CLI/server binaries.
3. Delete those paths from the OSS repo; remove the `cloud`/`server` cloud
   features and cloud re-exports.
4. Wire closed `Cargo.toml`: `cognee-lib = { git = "…cognee-rust", rev = "…" }`
   for releases, with `[patch."https://github.com/topoteretes/cognee-rust"]`
   pointing to a local path for development.
5. **Concretize inherited deps.** Crates moved to the closed repo lose the OSS
   `[workspace.dependencies]` table — every `{ workspace = true }` (e.g.
   `cognee-cloud`'s `reqwest`/`serde`) becomes a concrete version in the closed
   workspace.
6. **API-boundary contract tests** in the closed repo that exercise the OSS
   seams (`AclDb` wrapper, `build_router` injection, adapter registration), so an
   OSS trait change surfaces on the next rev bump instead of silently.
   *Exit criteria:* OSS repo builds/tests/publishes-dry-run with no cloud/auth
   source present; closed repo builds against the pinned OSS rev and reproduces
   today's full-feature CLI/server/bindings.

### Phase 3 — CI for both repos
- **OSS** `.github/workflows`: `lint` (fmt + check + clippy -D warnings),
  `test` (nextest, OpenAI secret), `doc`, `publish-dry-run` (all crates),
  `bindings` (capi/python/js build), and a tagged `publish` workflow (crates.io
  token, topological order). Plus npm + PyPI publish for the OSS bindings.
- **Closed** `.github/workflows`: same lint/test, build against pinned OSS rev,
  cloud-bindings build + publish to **private** npm/PyPI registries, and a
  scheduled job that bumps the pinned OSS `rev` and runs the suite.

### Phase 4 — Bindings distribution
- OSS: publish `cognee` (npm), `cognee` / `cognee-pipeline` (PyPI), C headers +
  release artifacts on GitHub.
- Closed: publish `cognee-cloud` equivalents to private registries; depend on the
  OSS `bindings-common` crate and add only a cloud-ops module (see S5 / §6.1).

## 6. Defaulted choices (veto if wrong)

- OSS license is **`MIT OR Apache-2.0`** (consistent with §2; supersedes any
  Apache-only wording). OSS publishes the **granular** `cognee-*` crate set (not a
  single mega-crate) since the workspace is already split — but see the reviewer
  addendum in §6.1 on narrowing the *semver-supported* surface.
- Closed bindings use the **registration-seam** approach (reuse OSS wrappers),
  not a fork of the binding crates.
- Redis session store and the Postgres relational/graph (`pggraph`) adapters
  stay **OSS** (pure-Rust, publishable); the closed "more adapters" set starts as
  {Qdrant, LiteRT} and grows with future proprietary adapters (e.g. S3 storage).
- Closed repo name `cognee-cloud-rust`, closed umbrella crate `cognee-cloud-lib`.
- **Telemetry defaults to OFF (opt-in) in OSS.** `cognee-telemetry` currently
  POSTs to prometh.ai by default (opt-out); an OSS crate that beacons by default
  is a trust liability. Cloud/closed builds may default it on.
- The `e2e-cross-sdk` parity harness splits: single-user/default-owner parity
  stays OSS; multi-tenant/auth parity moves to closed.

### 6.1 Reviewer addenda (open decisions — veto or confirm)

These came out of a code-level review and touch *locked* choices, so they are
flagged here rather than silently applied:

- **Narrow the *semver-supported* public surface.** Publishing ~20 crates makes
  every internal refactor a coordinated semver event across all of them. Suggest
  the only crates with a *stability promise* are the umbrella (`cognee-lib`, likely
  renamed `cognee`) + the **trait crates** third parties actually extend
  (`vector`, `llm`, `embedding`, the `database` traits). The rest are still
  published (the dep graph requires it) but documented as **internal/unstable**.
  release-plz handles the mechanics; this is about the commitment, not the tooling.
- **Drop the function-pointer "registry" framing in S5.** Bindings already call
  `cognee_bindings_common::ops::*` directly and cloud is isolated to `ops/cloud.rs`,
  so a closed cdylib can simply **depend on the OSS `bindings-common` crate and add
  a cloud-ops module** — full type safety, no dynamic dispatch. A runtime registry
  only buys plugin loading we don't need (closed compiles its own cdylib). Keep S5
  as "reuse the OSS op wrappers via a normal crate dep."
- **Dev ergonomics of the git-rev pin.** The `rev`-pin-for-releases /
  `[patch]`-to-local-path-for-dev split is correct, but the daily friction is real.
  Ship a checked-in script that flips between the two states and make
  **local-path the default committed state** of the closed dev branch, so a fresh
  clone builds against a sibling checkout with no manual edits.
- **Telemetry default-off interacts with parity.** Flipping `cognee-telemetry` to
  opt-in is the right trust call, but the `e2e-cross-sdk` telemetry parity tests
  drive the `telemetry-emit` harness through `send_telemetry`; with the default OFF
  they must explicitly set `COGNEE_TELEMETRY_INTEGRATION_TEST=1` (or be skipped),
  else the wire-payload comparison has nothing to compare.
- **The cross-SDK harness has a hidden build-context dependency.** Its Dockerfile
  builds the **Python `cognee` reference SDK from the monorepo** (build context
  `../..`), not from a published wheel. The OSS repo won't have the Python monorepo
  as a sibling, so the OSS harness must either pin a published PyPI `cognee` or
  vendor it — decide before the split. Also: `test_http_auth.py` is currently
  bucketed as Phase-1 but exercises register/login/me — against an OSS no-auth
  server those endpoints are absent, so it belongs on the closed side (or must be
  conditionalised); and ~11 `test_*_parity.py` / `test_cross_*.py` files are
  unbucketed and need an explicit OSS-vs-closed classification before the harness
  is split.

### Precedents (verified)

Closest structural match is **Wasmer** (real MIT core crate on crates.io +
commercial backend in separate private repos), then **Deno** (MIT runtime crates
+ separate cloud). Note most open-core Rust projects instead keep the commercial
superset in one **monorepo behind a license boundary** (Meilisearch EE, Tabby
`ee/`, Sentry Relay/FSL) and only name-squat the core crate — our real-crate +
separate-private-repo model is more conservative and is justified only because
*physically hiding the closed source* is a stated requirement.

## 7. Risk register

| Risk | Mitigation |
|---|---|
| `cognee-database` auth extraction (S2) — orphan rule blocks re-impl + single baseline migration | Trait stays OSS, impl moves to a closed newtype (S2); cleave the baseline + expose `core_migrations()` (S2b); make OSS default user DB-free (S2c). Do first, isolated PR, full test + cross-SDK parity. |
| Removing `impl AclDb for DatabaseConnection` (S2) breaks OSS self-built `dyn AclDb` casts | Production breaks are HTTP `add`/`update`/`remember` routers + CLI `--enforce-acl`; convert to **injected** `Option<Arc<dyn AclDb>>` (closed supplies the newtype, OSS passes `None`). Pipeline core is already `Option`-based and safe (S2d). |
| Migration cleave — seeds backfill from `datasets`, are SQLite-only, and identity collides on pre-split DBs | Move seed/backfill SQL to the auth migration (runs after core; reads core tables); make it dialect-aware; give the auth migration a **new name** + `if_not_exists`; **split `down()`** so the OSS baseline stops dropping auth tables (S2b). |
| `cognee-http-server` cloud/auth coupling is wider than the cloud routers | **9 routers** move/gate to closed (cloud `sync`+`checks` *and* the `auth`/`users`/`api-keys` family), removing the hard `cognee-cloud` dep; `SyncRegistry`, lifecycle hooks and the `Option` state fields are already safe and need no change (S3). |
| OSS subset silently regains closed coupling between Phase 0 and split | Continuous OSS-isolation CI from Phase 0 (per-crate `--no-default-features` now; OSS-only manifest after S3/S4), plus the partition-manifest 100%-coverage gate landed early (Phase 0 steps 8–9). |
| `cloud` is ON by default in 5 crates — removing it silently drops cloud from default builds/wheels | Plan it as the explicit S7 step; communicate the behaviour change; closed builds re-add `cloud` to their defaults. |
| Auth sea-orm *entities* + `Role`/`Tenant` models left behind in OSS database (dead code or compile break) | Move the 13 auth entity files + auth domain models with the ops; keep `AclDb`/`User`/permission-constants in OSS; verify OSS database compiles without the moved entities (S2e). |
| Unpinned `cognee-litert-lm` git dep — non-reproducible + crates.io blocker | Pin to a commit rev now (Phase 1, step 6); it moves to closed with S4 regardless. |
| Cross-SDK harness builds the Python SDK from the monorepo context — breaks in a standalone OSS repo | Pin a published PyPI `cognee` or vendor it; reclassify `test_http_auth` + the unbucketed parity tests OSS-vs-closed (§6.1). |
| Binding name squatting across registries (npm `cognee`, PyPI `cognee-pipeline`) | Reserve all three registries in Phase 1, step 4; confirm npm `cognee` org ownership before depending on it. |
| Permissive core lets competitors host cognee | Accepted (license decision: MIT/Apache for adoption). Only AGPL/BSL would deter it; revisit only if competitive hosting becomes a real threat. |
| Hidden git/path dep slips into a published crate | CI `publish --dry-run` gate on every OSS crate. |
| Python parity regression from touching IDs | Never alter `owner_id`/`tenant_id` in ID derivation; run `e2e-cross-sdk` after Phase 0. |
| OSS edge profile left with no vector store | Brute-force adapter is a required Phase-2 item. |
| Closed/OSS version drift | Closed pins OSS by `rev`; scheduled CI bumps + tests. |

## 8. Repository creation algorithm (clean history, no closed source in OSS)

**Goal:** the public OSS repo must be *born clean* — closed source must never
exist in any commit, tag, or reachable object. A `git rm` in the current repo is
**not** sufficient: the blobs remain recoverable from history. Therefore we
create **two brand-new repos from scratch**; the current mixed repo is retired to
a private archive and never becomes the public one.

**Prerequisite:** Phase 0 seams are merged, so OSS and closed code live in
disjoint crates/directories and the OSS subset builds in isolation.

**Guiding rule — allowlist, never denylist.** The OSS repo is populated by
*copying only explicitly-classified OSS paths*. Anything unclassified is excluded
by default, so newly-added or forgotten closed files cannot leak.

### Step 0 — Author the partition manifest
Two literal path lists, checked into the **current** repo under `scripts/split/`:

- `oss-paths.txt` — every file/dir that belongs to OSS (the §3 OSS tree).
- `closed-paths.txt` — every file/dir that belongs to closed.

CI gate: every tracked path must appear in exactly one list (a script asserts the
union equals `git ls-files` and the intersection is empty). This is the single
source of truth for the split.

### Step 1 — Pre-flight: prove the OSS subset is self-contained
In a throwaway checkout, reduce the workspace to OSS members only and verify:
```bash
# OSS-only workspace members + adapters; then:
cargo check --all-targets
cargo tree -e no-dev --prefix none | grep -i 'git+' && echo "GIT DEP LEAK" && exit 1
grep -rIl 'cognee-cloud\|cognee-access-control\|cognee-vector-qdrant\|cognee-llm-litert' \
  $(cat scripts/split/oss-paths.txt) && echo "CLOSED REF LEAK" && exit 1
```
Do not proceed until this is clean.

### Step 2 — Create the OSS repo (fresh history)
```bash
SRC=$(pwd)                       # current mixed repo
mkdir ../cognee-rust && cd ../cognee-rust && git init -b main
# Allowlist copy — only classified OSS paths:
rsync -a --files-from="$SRC/scripts/split/oss-paths.txt" "$SRC"/ .
# Write OSS-only root Cargo.toml (workspace members = OSS crates only),
# scrub README/docs/CLAUDE.md of closed references, drop qdrant [patch] blocks
# from the capi/ and js/ binding workspaces.
git add -A && git commit -m "chore: initial open-source release"
```
*History-preserving variant (optional):* instead of a squash init, run
`git filter-repo --paths-from-file scripts/split/oss-paths.txt` on a fresh clone.
This keeps OSS commit history while dropping closed paths from **every** commit.
Only use it if Step 7's leak audit passes — file moves across the OSS/closed line
over time can leave stray blobs, which the squash init cannot.

### Step 3 — Verify the OSS repo builds & publishes in isolation
```bash
cd ../cognee-rust
bash scripts/check_all.sh
bash scripts/run_tests_with_openai.sh
# dry-run every crate in topological order; fail on any git/path dep:
for c in $(scripts/split/publish-order.sh); do cargo publish -p "$c" --dry-run || exit 1; done
```

### Step 4 — Leak audit of the OSS repo (the whole point)
```bash
cd ../cognee-rust
# 1. No closed paths anywhere in history:
git log --all --oneline -- $(cat "$SRC/scripts/split/closed-paths.txt") | grep . && echo LEAK
# 2. No closed object names in any reachable blob:
git rev-list --objects --all | grep -E 'cloud|access-control|qdrant|litert|auth/' && echo LEAK
# 3. Sentinel content grep across all history:
git grep -I -l -e 'CloudClient' -e 'ExtraAuthValidator' -e 'hashed_password' \
  -e 'AUTH0' -e 'device_code' $(git rev-list --all) && echo LEAK
```
Any hit ⇒ discard the repo and rebuild from Step 2 (do not patch in place).

### Step 5 — Publish OSS & capture the pin
```bash
gh repo create topoteretes/cognee-rust --public --source=. --push
git tag v0.1.0 && git push --tags
OSS_REV=$(git rev-parse HEAD)            # closed repo pins this
# (crates.io publish can happen here or after closed is verified)
```

### Step 6 — Create the closed repo (fresh history, pinned to OSS)
```bash
mkdir ../cognee-cloud-rust && cd ../cognee-cloud-rust && git init -b main
rsync -a --files-from="$SRC/scripts/split/closed-paths.txt" "$SRC"/ .
# Cargo.toml of every closed crate:
#   cognee-lib = { git = "https://github.com/topoteretes/cognee-rust", rev = "$OSS_REV" }
# Dev ergonomics: a [patch] block (in a dev-only include or documented) ->
#   [patch."https://github.com/topoteretes/cognee-rust"]
#   cognee-lib = { path = "../cognee-rust/crates/lib" }
git add -A && git commit -m "chore: initial closed-source cloud product"
gh repo create topoteretes/cognee-cloud-rust --private --source=. --push
```

### Step 7 — Verify the closed repo
```bash
cd ../cognee-cloud-rust
cargo build --workspace                  # builds against pinned OSS rev
bash scripts/check_all.sh
bash scripts/run_tests_with_openai.sh    # incl. multi-tenant + auth + cloud
# reproduce today's full-feature CLI/server/bindings; run e2e-cross-sdk parity
```

### Step 8 — Retire the old repo & bring up CI
- Make the current mixed repo **private/archived**; it is never published. The
  public repo is the new `cognee-rust` from Step 2.
- Stand up Phase 3 CI in both new repos.

### Why fresh-init over filtering the existing repo
`git filter-repo` *can* meet the no-closed-history bar, but it is error-prone
(missed paths, dangling blobs from renames, ref/reflog/tag residue) and the
result still has to pass the Step 4 audit. A fresh allowlist-copy is *provably*
clean by construction — the OSS repo's object database only ever receives files
named in `oss-paths.txt`. Prefer it unless preserving OSS commit history is worth
the extra audit burden.
