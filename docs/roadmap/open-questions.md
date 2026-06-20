# Open Questions

Design decisions that are still unresolved across the surviving docs. These are *choices to be made*,
not missing features — for the latter, see [not-implemented.md](not-implemented.md). Each item links
back to the doc and section where it is discussed in full.

## Auth — [http-server/auth.md §15](../http-server/auth.md)

1. **Argon2 parameters** — the OWASP 2024 baseline (m=19456 KiB, t=2, p=1) is CPU-conservative. On
   constrained hardware (Android runner, embedded) we may need a lower `m`. Decide once benchmarks land.
2. **Bcrypt re-hash on reset** — we re-hash on successful login. Should we also re-hash on
   `/api/v1/auth/reset-password`? (Probably yes — it's free.)
3. **JWT secret rotation** — single secret today; multi-key rotation (`kid` header + secret map) is a
   follow-up. (Also tracked at architecture.md §22 Q4 — resolve in one place.)
4. **Default-user creation race** — both a startup hook and a lazy first-request path can create the
   default user when `REQUIRE_AUTHENTICATION=false`. Confirm we never create two from a race.

## Tenancy / RBAC — [http-server/tenants.md §13](../http-server/tenants.md)

1. **Global / cross-tenant roles** — a role in tenant A cannot grant permissions in tenant B by
   design. Is there a use case for global roles (e.g. a billing admin across tenants)? Would require
   schema changes.
2. **Soft-delete vs hard-delete** — Python hard-deletes ACL/role rows and we match. Some compliance
   regimes prefer soft-delete; out of scope for now.
3. **Tenant slug vs name** — `tenants.name` is unique, but frontends often want a URL-safe slug
   separate from the display name. Not in Python; deferred.
4. **Permission-set caching** — resolution is uncached. If `user_can` dominates request latency, a
   short-lived LRU is the simplest mitigation.

## Observability — [http-server/observability.md §11](../http-server/observability.md)

1. **Per-tenant span buffer** — the `/activity/spans` buffer is global, so a tenant sees other
   tenants' traces (Python has the same issue on this admin debug endpoint). Filter by `user_id`?
   Deferred until the multi-tenant story lands.
2. **Span sampling** — every span is recorded. High-traffic endpoints (e.g. `/datasets/status`
   polling) may want adaptive sampling.
3. **OTLP export timing** — when does the deployment story justify pulling in `opentelemetry-sdk`?
   Likely tied to multi-replica deployment.

## Pipelines — [http-server/pipelines.md §15](../http-server/pipelines.md)

1. **`PipelineRunRepository` crate placement** — does the trait live in `cognee-database` (consumed
   by `cognee-core` behind a feature flag) or in a new micro-crate? Lean: `cognee-database`.
2. **Multi-replica WebSocket fan-out** — the process-local registry doesn't fan out across replicas
   (sticky-session WS routing or Redis pub/sub). Lean: document the constraint, defer the fix. (See
   also [not-implemented.md](not-implemented.md).)
3. **`ENABLE_BACKEND_ACCESS_CONTROL` semantics** — Python toggles permission enforcement via this env
   var. Whether the Rust port honors the same toggle (vs. always enforcing) needs confirmation.

## Responses router — [http-server/routers/responses.md](../http-server/routers/responses.md)

1. **`ChatUsage` field renaming** — Python renames OpenAI's `input_tokens` / `output_tokens` to
   `prompt_tokens` / `completion_tokens`. We keep the rename for client-compat; documented divergence
   from raw OpenAI output.
2. **Hard-coded `gpt-4o` upstream model** — matches Python; no env var to lift the default. Should we
   add one (operators currently must rebuild from source)?
3. **OpenAI bearer-token leakage** — the upstream client must not log `Authorization: Bearer sk-…`.
   The redaction layer handles span attributes, but `reqwest` trace logs (`RUST_LOG=trace`) bypass it.
   Disable `reqwest` trace logging unconditionally, or rely on operator discipline?

## Notebooks router — [http-server/routers/notebooks.md](../http-server/routers/notebooks.md)

1. **Empty `cells` overwrite** — Python's `PUT` ignores `"cells": []` (you can't delete all cells via
   the endpoint). We keep the quirk for parity; documented.
2. **Sandbox `cognee` package availability** — bundle CPython + the cognee wheel in the image, or
   require operators to install it?
3. **Sandbox auth / state propagation** — how do cell-initiated `cognee.add(...)` calls get scoped
   credentials so a notebook can't use the operator's keys against another tenant?
4. **Tenancy retrofit** — the `notebooks` table has no `tenant_id`. If multi-tenant notebooks land
   later, derive `tenant_id` from `owner_id`'s primary tenant, or keep notebooks user-scoped forever?

## Architecture — [http-server/architecture.md §22](../http-server/architecture.md)

1. **Shared `CogneeLib` instance vs per-request** — current decision is one shared `Arc<CogneeLib>`
   in `AppState`. Validate under load.
2. **DB pool sizing** — confirm the `ComponentManager` pool default is sensible for a multi-connection
   HTTP server.
