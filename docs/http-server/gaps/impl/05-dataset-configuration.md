# Gap 05 — Dataset configuration (GET/PUT /datasets/{id}/schema)

## 1. Source / current state

- **GET handler stub** (`get_dataset_schema`):
  [crates/http-server/src/routers/datasets.rs:312-333](../../../../crates/http-server/src/routers/datasets.rs#L312-L333). After the read-ACL check, returns `Json(DatasetSchemaResponseDTO { graph_schema: None, custom_prompt: None })` unconditionally. The payload is hard-coded; no DB lookup happens.
- **PUT handler stub** (`update_dataset_schema`):
  [crates/http-server/src/routers/datasets.rs:386-402](../../../../crates/http-server/src/routers/datasets.rs#L386-L402). Performs a `write` permission check, binds the body as `Json(_payload): Json<DatasetSchemaPayloadDTO>` (discarding it), and returns `Json(serde_json::json!({"status": "ok"}))`. No DB write happens.
- **Router wiring**: [crates/http-server/src/routers/datasets.rs:539-541](../../../../crates/http-server/src/routers/datasets.rs#L539-L541)
  ```
  .route("/{dataset_id}/schema", get(get_dataset_schema))
  .route("/{dataset_id}/schema", put(update_dataset_schema))
  ```
- **DTOs**: [crates/http-server/src/dto/datasets.rs:68-86](../../../../crates/http-server/src/dto/datasets.rs#L68-L86)
  - `DatasetSchemaPayloadDTO` (request): camelCase wire; `graph_schema: Option<serde_json::Value>`, `custom_prompt: Option<String>`; `#[serde(default, skip_serializing_if = "Option::is_none")]` on both — `null` clears, absent leaves untouched (already matches Python parity comments).
  - `DatasetSchemaResponseDTO` (response): **snake_case** wire (`#[derive(Serialize)]` without `rename_all`); fields `graph_schema: Option<serde_json::Value>`, `custom_prompt: Option<String>`.
- **Skip-stub integration test**: [crates/http-server/tests/test_datasets_schema.rs](../../../../crates/http-server/tests/test_datasets_schema.rs) — four tests today: 401 for GET, "placeholder" body for GET, 401 for PUT, "placeholder" `{"status":"ok"}` body for PUT. None hit a DB row.

## 2. Why it's blocked

There is no `dataset_configurations` table in the Rust SeaORM schema, no entity for it, and no trait/impl to read or upsert it. The handlers ship with explicit blocker markers in code:

- `datasets.rs:310`: `/// **BLOCKING GAP**: get_dataset_configuration does not exist.`
- `datasets.rs:328`: `// TODO(blocking): implement dataset_configurations table in cognee-models/cognee-database`
- `datasets.rs:384`: `/// **BLOCKING GAP**: dataset_configurations table does not exist.`
- `datasets.rs:400`: `// TODO(blocking): implement dataset_configurations upsert in cognee-database`

The gap inventory entry ([docs/http-server/gaps/README.md](../README.md)) confirms both halves block on the same missing table.

## 3. Data model

### 3.1 Confirm against Python

Python's `cognee/modules/data/models/DatasetConfiguration.py`:

```python
class DatasetConfiguration(Base):
    __tablename__ = "dataset_configurations"
    id            = Column(UUID, primary_key=True, default=uuid4)
    dataset_id    = Column(UUID, ForeignKey("datasets.id", ondelete="CASCADE"),
                           unique=True, nullable=False)
    graph_schema  = Column(GenericJSON, nullable=True)
    custom_prompt = Column(Text, nullable=True)
    created_at    = Column(DateTime(tz=True), default=now)
    updated_at    = Column(DateTime(tz=True), onupdate=now)
```

Python's `alembic/versions/d4e5f6a7b8c9_add_dataset_configuration_table.py` matches: surrogate `id`, **unique** `dataset_id`, JSON `graph_schema`, text `custom_prompt`, two timestamps.

**Scoping**: Python's table has **no owner_id / tenant_id column**. The configuration is scoped *per dataset* (one row per dataset_id, enforced by `UNIQUE`). Access control is done by routing through `get_authorized_existing_datasets([dataset_id], "read"|"write", user)` — i.e. the dataset ACL gates it. The Rust port must do the same: **do not add `owner_id` / `tenant_id` columns** to this table; use the existing ACL (`check_permission_via_handles` on the parent `dataset_id`) instead. This is the only way to stay Python-parity and the only way the planned tests against shared datasets will work.

### 3.2 Proposed Rust schema

Table `dataset_configurations`:

| Column | Type | Constraints | Notes |
|---|---|---|---|
| `id` | `text` (uuid hex) | `PRIMARY KEY`, `NOT NULL` | Mirrors the rest of the schema (`uuid_hex`) |
| `dataset_id` | `text` (uuid hex) | `NOT NULL`, `UNIQUE` | One row per dataset; FK to `datasets.id` with `ON DELETE CASCADE` |
| `graph_schema` | `json` | `NULL` | Stored as opaque `serde_json::Value` (Python compatibility: arbitrary JSON Schema-ish object) |
| `custom_prompt` | `text` | `NULL` | |
| `created_at` | `timestamp with time zone` | `NOT NULL` | Set on insert by repository |
| `updated_at` | `timestamp with time zone` | `NULL` | Set on update by repository |

**Indexes**:

- `UNIQUE INDEX uq_dataset_configurations_dataset_id ON dataset_configurations(dataset_id)` — both enforces 1-row-per-dataset *and* makes lookup-by-dataset O(log n).

**FK to datasets**: matches `notebooks` precedent — declare via `ForeignKey::create() ... .on_delete(ForeignKeyAction::Cascade)` when SeaORM supports it. SQLite respects it when `PRAGMA foreign_keys = ON`; PostgreSQL respects it natively. (Notebooks chose *not* to FK to users; here the parity is strict — Python has the FK, so we add it.)

**No `tenant_id` column** — same reasoning as `notebooks`: the parent (`datasets`) carries tenant scope, and ACL checks gate access.

## 4. Implementation steps

### 4.a New SeaORM migration

Create `crates/database/src/migrator/m20260528_000001_create_dataset_configurations.rs` (file date matches today; sequence `_000001` follows the convention in `m20260501_000001_create_notebooks.rs`). Template directly off `m20260501_000001_create_notebooks.rs`:

- `up()`:
  - `create_table(...).if_not_exists()` with the six columns above. `Id` as `.text().not_null().primary_key()`. `DatasetId` as `.text().not_null().unique_key()`. `GraphSchema` as `.json().null()`. `CustomPrompt` as `.text().null()`. `CreatedAt` as `.timestamp_with_time_zone().not_null()`. `UpdatedAt` as `.timestamp_with_time_zone().null()`.
  - Foreign key: `foreign_key(ForeignKey::create().from(DatasetConfigurations::Table, DatasetConfigurations::DatasetId).to(Datasets::Table, Datasets::Id).on_delete(ForeignKeyAction::Cascade))`. The `Datasets` ident already exists; reuse from `m20250101_000001_initial_schema` if exported, otherwise re-declare locally as `#[derive(DeriveIden)] enum Datasets { Table, Id }` per migration-file convention.
  - `create_index(...).name("uq_dataset_configurations_dataset_id").table(DatasetConfigurations::Table).col(DatasetConfigurations::DatasetId).unique().if_not_exists()`.
- `down()`:
  - `manager.drop_table(Table::drop().table(DatasetConfigurations::Table).if_exists().to_owned()).await`
  - **Reversible** — mandatory per the constraints. (Contrast with `m20260428_000001_tenants_rbac`, which is intentionally no-op down; here we own the table outright so we can drop it cleanly.)
- `#[derive(DeriveIden)] pub(crate) enum DatasetConfigurations { Table, Id, DatasetId, GraphSchema, CustomPrompt, CreatedAt, UpdatedAt }`.

**Register** in `crates/database/src/migrator/mod.rs`:
- Add `mod m20260528_000001_create_dataset_configurations;` (line ~16, after `m20260901_000003_pipeline_run_dataset_nullable`). **Note:** `m20260528...` is chronologically before `m20260901...`; place it accordingly to keep dates monotonically increasing in the module list (between `m20260512_...add_parent_user_id` and `m20260901_..._pipeline_run_dataset_nullable`).
- Add `Box::new(m20260528_000001_create_dataset_configurations::Migration),` to the `migrations()` vec **in the same chronological slot**.

### 4.b New SeaORM entity

Create `crates/database/src/entities/dataset_configuration.rs` (template off `crates/database/src/entities/principal_configuration.rs`, which has the most similar shape — uuid-hex id + JSON column + created/updated timestamps):

```rust
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "dataset_configurations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    #[sea_orm(unique)]
    pub dataset_id: String,
    #[sea_orm(column_type = "Json", nullable)]
    pub graph_schema: Option<serde_json::Value>,
    #[sea_orm(column_type = "Text", nullable)]
    pub custom_prompt: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::dataset::Entity",
        from = "Column::DatasetId",
        to = "super::dataset::Column::Id",
        on_delete = "Cascade"
    )]
    Dataset,
}

impl Related<super::dataset::Entity> for Entity {
    fn to() -> RelationDef { Relation::Dataset.def() }
}

impl ActiveModelBehavior for ActiveModel {}
```

**Register** in `crates/database/src/entities/mod.rs`: add `pub mod dataset_configuration;` next to `pub mod dataset;`.

### 4.c New trait `DatasetConfigDb`

Create `crates/database/src/traits/dataset_config_db.rs`. Template off `crates/database/src/traits/notebook_db.rs`:

```rust
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::DatabaseError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetConfiguration {
    pub id: Uuid,
    pub dataset_id: Uuid,
    pub graph_schema: Option<serde_json::Value>,
    pub custom_prompt: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Patch semantics for `upsert`: `None` on a field means "leave existing
/// row's value unchanged on update / NULL on insert". This mirrors the
/// `skip_serializing_if = "Option::is_none"` shape of the request DTO
/// and Python's `if payload.graph_schema is not None: ...` logic.
#[derive(Debug, Clone, Default)]
pub struct DatasetConfigurationPatch {
    pub graph_schema: Option<serde_json::Value>,
    pub custom_prompt: Option<String>,
}

#[async_trait]
pub trait DatasetConfigDb: Send + Sync + 'static {
    /// Fetch the configuration row for a given dataset_id, if any.
    async fn get_by_dataset_id(
        &self,
        dataset_id: Uuid,
    ) -> Result<Option<DatasetConfiguration>, DatabaseError>;

    /// Insert-or-update: if a row exists for `dataset_id`, apply the patch
    /// (only `Some` fields are written, matching Python parity); otherwise
    /// create a new row with the provided fields. Returns the resulting row.
    async fn upsert(
        &self,
        dataset_id: Uuid,
        patch: DatasetConfigurationPatch,
    ) -> Result<DatasetConfiguration, DatabaseError>;
}
```

> **Trait naming**: requirements say `DatasetConfigDb` and `get_by_dataset_and_owner` + `upsert`. Because Python does *not* scope by owner (see §3.1) and the table has no `owner_id`, the read method is named `get_by_dataset_id` to match the actual key. Ownership/ACL enforcement lives in the router via `check_permission_via_handles` — i.e. the planner is honoring the "scoping" spirit of the requirement at the router boundary rather than denormalizing owner_id into the table. **Call out for review at implementation time.**

**Register** in `crates/database/src/traits/mod.rs`:
- `mod dataset_config_db;`
- `pub use dataset_config_db::{DatasetConfigDb, DatasetConfiguration, DatasetConfigurationPatch};`

**Re-export** in `crates/database/src/lib.rs` `pub use traits::{ ... }` block: add `DatasetConfigDb, DatasetConfiguration, DatasetConfigurationPatch`.

### 4.d Trait impl on `DatabaseConnection`

Create `crates/database/src/ops/dataset_configurations.rs` (template off `ops/notebooks.rs`):

- `fn model_to_domain(m: dataset_configuration::Model) -> Result<DatasetConfiguration, DatabaseError>` — uuid_hex::from_hex for `id` + `dataset_id`, passthrough for the rest.
- `impl DatasetConfigDb for DatabaseConnection`:
  - `get_by_dataset_id`: `dataset_configuration::Entity::find().filter(dataset_configuration::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id))).one(self).await.map_err(map_sea_err)?` → optional convert.
  - `upsert`:
    1. SELECT existing by dataset_id.
    2. If `Some(model)`: clone into `ActiveModel`; for each `Some(...)` field in patch, `Set(...)` it; always `Set(updated_at = Some(Utc::now()))`. Call `.update(self).await`.
    3. If `None`: build a fresh `ActiveModel` with a new uuid, `Set(dataset_id)`, the patch fields (or `Set(None)` if absent), `created_at = now`, `updated_at = Set(None)`. Call `.insert(self).await`.
    4. Return converted domain row.
  - Both methods use the `#[instrument]` macro with `cognee.db.relational.dataset_configurations.{op}` span names, mirroring `ops/notebooks.rs`.
- Tracing: `Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self))` and `COGNEE_DB_ROW_COUNT` exactly as in notebooks.
- **No `.unwrap()` anywhere** — every error path returns `Result<_, DatabaseError>` via `map_sea_err`.

**Register** in `crates/database/src/ops/mod.rs`: add `pub mod dataset_configurations;`.

### 4.e Wire trait composition

The codebase aggregates DB traits onto `DatabaseConnection` directly — there is no super-trait like "DatabaseConnection: IngestDb + DeleteDb + ...". The router pulls them in by `use cognee_database::{IngestDb, DeleteDb, ...}` and calls `IngestDb::method(&*db, ...)` / `db.method(...)`. So:

- **No change** to `IngestDb` or `DeleteDb`.
- The router will add `DatasetConfigDb` to its imports at the top of `routers/datasets.rs` and call `DatasetConfigDb::get_by_dataset_id(&*db, dataset_id)` / `DatasetConfigDb::upsert(&*db, dataset_id, patch)`.

The constraint in the requirements ("Wire IngestDb / DeleteDb trait composition if needed") is therefore **a no-op for this gap** — the existing aggregation pattern is per-call-site trait import, not a super-trait. Document this in the plan comments.

### 4.f GET handler — replace placeholder

Edit [crates/http-server/src/routers/datasets.rs:312-333](../../../../crates/http-server/src/routers/datasets.rs#L312-L333). New body:

```rust
pub async fn get_dataset_schema(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(dataset_id): Path<Uuid>,
) -> Result<Json<DatasetSchemaResponseDTO>, ApiError> {
    let components = state.components().ok_or_else(|| {
        ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
    })?;

    // Read ACL on the parent dataset (Python parity:
    // get_authorized_existing_datasets([dataset_id], "read", user)).
    check_permission_via_handles(components, user.id, dataset_id, "read")
        .await
        .map_err(|_| {
            ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
        })?;

    let db = components.database.clone();

    match DatasetConfigDb::get_by_dataset_id(&*db, dataset_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?
    {
        Some(config) => Ok(Json(DatasetSchemaResponseDTO {
            graph_schema: config.graph_schema,
            custom_prompt: config.custom_prompt,
        })),
        None => Ok(Json(DatasetSchemaResponseDTO {
            graph_schema: None,
            custom_prompt: None,
        })),
    }
}
```

**Important parity decision**: Python returns `{"graph_schema": null, "custom_prompt": null}` (HTTP 200) when no configuration row exists — *not* 404. See `get_datasets_router.py:541-542`. The requirements ask for "404 if no configuration row exists" — that **diverges from Python parity**. Flag this as a parity question for the implementation agent; the Python-parity behavior (200 + nulls) is the safer default and matches the existing skip-stub test's expectation that the body shape is stable. Recommend keeping 200 + nulls; only the *dataset not found / no read ACL* case yields 404.

Drop the now-obsolete `BLOCKING GAP` and `TODO(blocking)` comments.

### 4.g PUT handler — replace placeholder (Python-parity response body)

Edit [crates/http-server/src/routers/datasets.rs:386-402](../../../../crates/http-server/src/routers/datasets.rs#L386-L402). New body:

```rust
pub async fn update_dataset_schema(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(dataset_id): Path<Uuid>,
    Json(payload): Json<DatasetSchemaPayloadDTO>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let components = state.components().ok_or_else(|| {
        ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
    })?;

    // Write ACL — mirrors routers/update.rs:116.
    check_permission_via_handles(components, user.id, dataset_id, "write")
        .await
        .map_err(|_| {
            ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
        })?;

    let db = components.database.clone();

    let patch = cognee_database::DatasetConfigurationPatch {
        graph_schema: payload.graph_schema,
        custom_prompt: payload.custom_prompt,
    };

    // Upsert is required to return the typed row so internal callers can
    // confirm what was persisted (and so the regression-guard test can
    // distinguish a real upsert from the old `{"status":"ok"}` stub), but
    // the wire shape returned to HTTP callers stays Python-parity.
    let _saved = DatasetConfigDb::upsert(&*db, dataset_id, patch)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    Ok(Json(serde_json::json!({"status": "ok"})))
}
```

Changes vs the stub:
- The return body shape is **unchanged from the stub** — `{"status": "ok"}` — to preserve Python parity (`cognee/api/v1/datasets/routers/update_datasets_router.py` returns the same envelope). The earlier draft of this plan proposed switching to a typed `DatasetSchemaResponseDTO`; that was rejected in favour of Python parity.
- What changes vs the stub is the *behavior*: the payload is no longer discarded — `Json(_payload)` becomes `Json(payload)` and is consumed by the `DatasetConfigDb::upsert(...)` call. The return body shape only confirms the write completed; clients must follow up with `GET /datasets/{id}/schema` to read back the persisted state (this is the documented Python pattern).
- Axum's `Json<DatasetSchemaPayloadDTO>` extractor will already reject payloads that don't deserialize cleanly into the DTO with a 422 `JsonRejection`. The `serde_json::Value` field type means `graph_schema` can still be any JSON object; that's intentional (Python uses `Optional[Dict[str, Any]]`). The strict-schema constraint is satisfied because *the DTO shape itself* is enforced — extra fields are ignored (per serde default), `graph_schema` must be `null` / object / valid value, `custom_prompt` must be null or string.
- Imports needed at the top of `datasets.rs`: add `DatasetConfigDb` to the `use cognee_database::{...}` line.

Drop the obsolete `BLOCKING GAP` and `TODO(blocking)` comments.

**Note on the inline regression guard (§5.C)**: because the wire shape stays `{"status":"ok"}`, the regression guard cannot rely on a body-shape diff. Instead it must assert a real round-trip: PUT then GET, and assert the GET returns the upserted contents. See §5.C below for the updated guard structure.

### 4.h Update DTO consistency (no code change)

The existing `DatasetSchemaPayloadDTO` (`dto/datasets.rs:69-78`) already uses `serde(default, skip_serializing_if = "Option::is_none")` on both fields. **Keep as-is.** The `DatasetSchemaResponseDTO` is already correctly snake-case (`#[derive(Serialize)]` without `rename_all`, per Python "raw dict" parity comment on line 81). No DTO changes required.

### 4.i Type choice for `graph_schema`

**Use `serde_json::Value`.** Reasons:
- Python uses `Optional[Dict[str, Any]]` — any JSON-Object shape is legal.
- Python parity tests in `/tmp/cognee-python/cognee/tests/unit/api/v1/test_dataset_schema_endpoints.py:13` accept arbitrary nested shapes (`{"nodes": [{"name": "Person", "fields": ["name", "age"]}]}`).
- The DTO already binds to `Option<serde_json::Value>` and the SeaORM column is `Json`.
- Introducing a typed `GraphSchema` struct would require us to define and freeze the schema-of-schemas, which Python explicitly leaves open.

Call this out explicitly so the implementation agent doesn't try to over-type the column.

## 5. Tests

### 5.A Unit tests in `crates/database/src/ops/dataset_configurations.rs`

Mirror the inline `#[cfg(test)] mod tests` block from `ops/notebooks.rs`. Use `connect("sqlite::memory:")` + `initialize` to run migrations.

Required cases:

1. **`upsert_inserts_new_row`**: call `upsert(dataset_id, patch{graph_schema=Some({"type":"object"}), custom_prompt=Some("X")})` against a fresh DB; assert returned row has both fields set, `created_at` ≈ `now`, `updated_at` is `None`. Then `get_by_dataset_id(dataset_id)` returns `Some(row)` with matching contents.
2. **`upsert_updates_existing_row`**: insert via first `upsert`, then call again with `patch{graph_schema=Some({"new":"shape"}), custom_prompt=None}`. Assert returned row has the new `graph_schema`, `custom_prompt` **unchanged** (still `"X"`), and `updated_at` is `Some(...)` newer than the first call.
3. **`upsert_clears_field_when_none_is_explicit`**: NB. With `Option<Option<T>>` we *could* model "explicit null" vs "absent"; we are *not* doing that — the patch's `None` always means "leave alone". Python does the same (`if payload.graph_schema is not None: config.graph_schema = payload.graph_schema`). So this test asserts that calling `upsert(... patch{graph_schema=None, custom_prompt=Some("new")})` does **not** wipe `graph_schema`. (Document this clearly in the doc comment on `DatasetConfigurationPatch`.)
4. **`get_returns_none_when_missing`**: `get_by_dataset_id(random_uuid)` → `Ok(None)`.
5. **`unique_constraint_enforced`**: insert two rows by reaching past `upsert` (use the entity directly with two different `id`s and the same `dataset_id`); assert the second `insert` returns `DatabaseError::UniqueViolation(_)` via `map_sea_err`.
6. **`cascade_delete_on_dataset_removal`**: Seed a `datasets` row via the `dataset::Entity` insert pattern (copy from `tests/support/mod.rs:665-687`), `upsert` a config, then `dataset::Entity::delete_by_id(...).exec(&db).await`. Assert `get_by_dataset_id` now returns `None`. (Verifies the `ON DELETE CASCADE`.)

### 5.B Rewrite `crates/http-server/tests/test_datasets_schema.rs`

Drop the four "placeholder" tests; keep the two 401-no-auth tests. Add the following real-DB tests (use `build_auth_test_state` + `seed_user` + `seed_dataset` helpers from `tests/support/mod.rs`):

1. **`test_get_schema_returns_nulls_when_no_config_row`**:
   - Seed a user. Seed a dataset owned by user. Grant `read` ACL (use the same pattern as `tests/test_datasets_graph.rs` or equivalent — `db.ensure_principal(user.id, "user").await?; db.grant_permission(user.id, dataset_id, "read").await?`).
   - GET `/api/v1/datasets/{id}/schema` with bearer. Assert 200 and body `{"graph_schema": null, "custom_prompt": null}`.
   - This is **Python parity** — confirms 4.f's parity decision.
2. **`test_get_schema_returns_404_when_dataset_not_accessible`**:
   - Seed user; do **not** seed dataset / do not grant read. GET with bearer for a random dataset_id. Assert 404.
3. **`test_put_schema_returns_status_ok_then_get_returns_saved_fields`** (the full round-trip per Python parity):
   - Seed user, dataset, grant `write` (and `read`).
   - PUT body: `{"graph_schema":{"nodes":[{"name":"Person"}]},"custom_prompt":"Extract people."}`. Assert 200 and response body **equals `{"status":"ok"}`** exactly (Python parity).
   - Subsequent GET on the same dataset returns the saved fields: assert `graph_schema == {"nodes":[{"name":"Person"}]}` and `custom_prompt == "Extract people."`. This is the regression guard that distinguishes a real upsert from the old `{"status":"ok"}` stub.
4. **`test_put_schema_updates_existing_row`**:
   - Same setup as 3. Run PUT with `{"graph_schema":{"a":1}}`; assert 200 + `{"status":"ok"}`. Then a second PUT with `{"custom_prompt":"new"}` (omit `graph_schema`); assert 200 + `{"status":"ok"}`.
   - GET asserts `graph_schema == {"a":1}` (preserved across the second PUT — `None` field means "leave alone") and `custom_prompt == "new"`.
   - Confirms patch semantics. This is the second regression guard against the old stub.
5. **`test_put_schema_rejects_invalid_payload`**:
   - PUT body: `{"custom_prompt": 42}` (number, not string). Assert 4xx (axum's `Json` rejection yields 422 by default; or whatever the existing error pipeline maps it to — check `ApiError`'s mapping). The point: not 200.
6. **`test_put_schema_without_write_acl_returns_404`**:
   - Seed user A (owner with write+read), seed user B (no ACL). Authenticate as B. PUT against A's dataset.
   - Assert 404 (matches the existing pattern in `update_dataset_schema`, which masks 403 as 404 to avoid leaking dataset existence — `routers/datasets.rs:396-397`).
   - If the parity team wants 403 distinctness, that's a separate gap.
7. **`test_put_schema_no_auth_returns_401`**: keep the existing one verbatim.
8. **`test_get_schema_no_auth_returns_401`**: keep the existing one verbatim.

### 5.C Inline router test guards in `routers/datasets.rs`

Because the PUT response body stays `{"status":"ok"}` for Python parity (§4.g), the regression guard cannot rely on a body-shape diff. The guard must instead assert a real upsert occurred — i.e. that after a PUT, a GET returns the upserted fields. Add a `#[cfg(test)] mod schema_guard` block at the bottom of `datasets.rs` (matches the inline-guard pattern landed gaps adopted — see `update.rs:403-414` and `responses.rs:427-445`):

- A focused test that builds a minimal `AppState` (mock components, real in-memory DB) and:
  - Seeds a dataset + ACL.
  - Calls `update_dataset_schema(...)` directly with `{"graph_schema":{"marker":"persisted"},"custom_prompt":"X"}`. Asserts the response body equals `{"status":"ok"}` (Python parity).
  - Then calls `get_dataset_schema(...)` directly and asserts the returned `DatasetSchemaResponseDTO.graph_schema == {"marker":"persisted"}` and `.custom_prompt == "X"`. **This is the real regression guard** — if a future regression reverts to discarding the payload, the GET will return `{null, null}` and the assertion fails.
- A second test asserts that calling `get_dataset_schema(...)` for a dataset that has *never* had `update_dataset_schema` called returns `{"graph_schema": null, "custom_prompt": null}` (the Python-parity empty case — not 404).

These two guards together cover the "blocking placeholder reintroduced" silent-regression scenario despite the response body alone being ambiguous (`{"status":"ok"}` could come from either the stub or a real upsert).

## 6. Acceptance criteria

- [ ] Migration `m20260528_000001_create_dataset_configurations` creates the table with the columns/indexes/FK in §3.2, registered in `migrator/mod.rs`.
- [ ] `down()` cleanly drops the table; `migration_compat` test (`crates/database/tests/migration_compat.rs`) passes round-trip.
- [ ] Entity `dataset_configuration::{Entity, Model, Column, ActiveModel}` compiles and `dataset_configuration` is re-exported via `entities/mod.rs`.
- [ ] Trait `DatasetConfigDb` exists with `get_by_dataset_id` + `upsert`, has an impl on `DatabaseConnection`, is re-exported from `cognee_database`.
- [ ] All `BLOCKING GAP` and `TODO(blocking)` comments in `routers/datasets.rs:310, 328, 384, 400` are removed.
- [ ] GET `/datasets/{id}/schema` returns real `graph_schema` + `custom_prompt` from the DB when a row exists; returns `{null, null}` body (200) when no row exists; returns 404 when the dataset is inaccessible.
- [ ] PUT `/datasets/{id}/schema` upserts the row and returns the Python-parity envelope `{"status":"ok"}` (NOT a typed `DatasetSchemaResponseDTO`). A subsequent GET returns the upserted contents — this round-trip is the regression guard.
- [ ] PUT enforces `write` ACL via `check_permission_via_handles(..., "write")` (mirrors `routers/update.rs:116`).
- [ ] PUT rejects malformed payloads (non-string `custom_prompt`, non-object `graph_schema`) at the axum extractor layer with 4xx.
- [ ] Patch semantics: a `None` field in `DatasetConfigurationPatch` leaves the existing column unchanged on update.
- [ ] No `.unwrap()` introduced in non-test code (`crates/database/src/ops/dataset_configurations.rs`, `crates/database/src/traits/dataset_config_db.rs`, edits in `routers/datasets.rs`).
- [ ] Unit tests in §5.A all pass on `sqlite::memory:`.
- [ ] Integration tests in `crates/http-server/tests/test_datasets_schema.rs` (§5.B) all pass; no skip-stub markers remain.
- [ ] Inline `routers/datasets.rs` regression guards (§5.C) assert the response body is **not** the placeholder shape.
- [ ] [docs/http-server/gaps/README.md](../README.md) rows for 5a and 5b updated to **landed** (merge + gap commit refs filled in by the implementation agent).

## 7. Status

**not-started**

---

## Locked Python-parity decisions (no longer open for review)

The implementation must follow Python parity on all three questions that were flagged earlier in this plan's drafts. Locked on 2026-05-28:

1. **Per-dataset, not per-user scoping** — `dataset_configurations` has NO `owner_id` / `tenant_id` column. Access control is delegated to the existing dataset ACL via `check_permission_via_handles` on the parent `dataset_id`. (§3.1, §4.a)
2. **GET on missing row returns 200 + nulls** — when no configuration row exists, return `Json(DatasetSchemaResponseDTO { graph_schema: None, custom_prompt: None })` with HTTP 200. 404 is reserved for the dataset-inaccessible case. (§4.f)
3. **PUT response body is `{"status":"ok"}`** — the handler upserts and returns the Python-parity envelope, NOT a typed `DatasetSchemaResponseDTO`. Clients confirm persistence by issuing a subsequent GET. The integration test in §5.B.3 and the inline regression guard in §5.C assert the PUT→GET round-trip. (§4.g, §5.B.3, §5.C)
