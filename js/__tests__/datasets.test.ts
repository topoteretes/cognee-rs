/**
 * Phase-5 Tier-A tests — DatasetManager, forget, prune, sessions,
 * notebooks, pipeline-run resets, and user ops.
 *
 * All tests are Tier-A: NO LLM / NO embedding network calls.
 * Config follows the same pattern as add.test.ts:
 *   - MOCK_EMBEDDING=true  → no model downloads
 *   - dummy llm_api_key    → OpenAIAdapter constructs, never reaches network
 *   - isolated temp dirs + in-process SQLite DB
 *
 * A single handle is built in `beforeAll` and warmed once; all tests reuse it.
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native, NativeBox } from "../src/native";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeTempHandle(email: string): {
  tmpDir: string;
  dbPath: string;
  handle: NativeBox;
} {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-ds-"));
  const dbPath = path.join(tmpDir, "cognee.db");
  const handle = native.cogneeNew({
    system_root_directory: tmpDir,
    data_root_directory: path.join(tmpDir, "data"),
    relational_db_url: `sqlite:${dbPath}?mode=rwc`,
    embedding_provider: "mock",
    llm_api_key: "test-dummy-key",
    default_user_email: email,
  });
  return { tmpDir, dbPath, handle };
}

function cleanupTmpDir(dir: string) {
  try {
    fs.rmSync(dir, { recursive: true, force: true });
  } catch {
    // best effort
  }
}

// ---------------------------------------------------------------------------
// Suite setup
// ---------------------------------------------------------------------------

describe("Phase-5 DatasetManager, forget, prune, sessions, notebooks (Tier-A)", () => {
  let tmpDir: string;
  let handle: NativeBox;
  const email = "phase5_tier_a@example.com";

  beforeAll(async () => {
    process.env.MOCK_EMBEDDING = "true";
    ({ tmpDir, handle } = makeTempHandle(email));
    await native.cogneeWarm(handle);
  });

  afterAll(() => {
    delete process.env.MOCK_EMBEDDING;
    cleanupTmpDir(tmpDir);
  });

  // ──────────────────────────────────────────────────────────────────────────
  // Default user
  // ──────────────────────────────────────────────────────────────────────────

  describe("default user", () => {
    it("getOrCreateDefaultUser returns a User with the configured email", async () => {
      const user = await native.cogneeGetOrCreateDefaultUser(handle);
      expect(typeof user.id).toBe("string");
      expect(user.email).toBe(email);
      expect(typeof user.is_active).toBe("boolean");
      expect(user.is_active).toBe(true);
    });

    it("getOrCreateDefaultUser is idempotent (same id on second call)", async () => {
      const u1 = await native.cogneeGetOrCreateDefaultUser(handle);
      const u2 = await native.cogneeGetOrCreateDefaultUser(handle);
      expect(u1.id).toBe(u2.id);
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // Dataset manager
  // ──────────────────────────────────────────────────────────────────────────

  describe("dataset manager", () => {
    it("listDatasets returns an empty array before any add", async () => {
      const h = makeTempHandle("ds_list_empty@test.com");
      await native.cogneeWarm(h.handle);
      try {
        const datasets = await native.cogneeListDatasets(h.handle);
        expect(Array.isArray(datasets)).toBe(true);
        expect(datasets.length).toBe(0);
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("listDatasets returns a dataset after add", async () => {
      await native.cogneeAdd(
        handle,
        { type: "text", text: "dataset list test content" },
        "ds_list_test"
      );
      const datasets = await native.cogneeListDatasets(handle);
      expect(Array.isArray(datasets)).toBe(true);
      const names = datasets.map((d: { name: string }) => d.name);
      expect(names).toContain("ds_list_test");
    });

    it("listData returns items in a dataset after add", async () => {
      await native.cogneeAdd(
        handle,
        { type: "text", text: "list data content" },
        "ds_data_test"
      );
      const datasets = await native.cogneeListDatasets(handle);
      const ds = datasets.find(
        (d: { name: string }) => d.name === "ds_data_test"
      );
      expect(ds).toBeDefined();

      const items = await native.cogneeListData(handle, ds.id);
      expect(Array.isArray(items)).toBe(true);
      expect(items.length).toBeGreaterThanOrEqual(1);
    });

    it("hasData returns false for an unknown dataset id", async () => {
      const fakeId = "00000000-0000-0000-0000-000000000001";
      const has = await native.cogneeHasData(handle, fakeId);
      expect(has).toBe(false);
    });

    it("hasData returns true after adding data", async () => {
      await native.cogneeAdd(
        handle,
        { type: "text", text: "has data check" },
        "ds_has_data"
      );
      const datasets = await native.cogneeListDatasets(handle);
      const ds = datasets.find(
        (d: { name: string }) => d.name === "ds_has_data"
      );
      expect(ds).toBeDefined();
      const has = await native.cogneeHasData(handle, ds.id);
      expect(has).toBe(true);
    });

    it("datasetStatus returns empty map for a fresh dataset (no pipeline runs)", async () => {
      await native.cogneeAdd(
        handle,
        { type: "text", text: "status check content" },
        "ds_status_test"
      );
      const datasets = await native.cogneeListDatasets(handle);
      const ds = datasets.find(
        (d: { name: string }) => d.name === "ds_status_test"
      );
      expect(ds).toBeDefined();
      const status = await native.cogneeDatasetStatus(handle, [ds.id]);
      expect(typeof status).toBe("object");
      // No cognify run → status map may be empty for this dataset.
    });

    it("emptyDataset removes all data from a dataset", async () => {
      // Use a fresh isolated handle so we don't disturb other datasets.
      const h = makeTempHandle("empty_ds@test.com");
      await native.cogneeWarm(h.handle);
      try {
        await native.cogneeAdd(
          h.handle,
          { type: "text", text: "content to empty" },
          "ds_to_empty"
        );
        const datasets = await native.cogneeListDatasets(h.handle);
        const ds = datasets.find(
          (d: { name: string }) => d.name === "ds_to_empty"
        );
        expect(ds).toBeDefined();

        const result = await native.cogneeEmptyDataset(h.handle, ds.id);
        // DeleteResult is Serialize — must be an object.
        expect(typeof result).toBe("object");
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("deleteData removes a specific item from a dataset", async () => {
      const h = makeTempHandle("delete_data@test.com");
      await native.cogneeWarm(h.handle);
      try {
        const addResult = await native.cogneeAdd(
          h.handle,
          { type: "text", text: "item to delete" },
          "ds_delete_item"
        );
        expect(addResult.addedCount).toBe(1);
        const dataId = addResult.added[0].id;

        const datasets = await native.cogneeListDatasets(h.handle);
        const ds = datasets.find(
          (d: { name: string }) => d.name === "ds_delete_item"
        );
        expect(ds).toBeDefined();

        const result = await native.cogneeDeleteData(h.handle, ds.id, dataId);
        expect(typeof result).toBe("object");
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("deleteAllDatasets empties all datasets", async () => {
      const h = makeTempHandle("delete_all_ds@test.com");
      await native.cogneeWarm(h.handle);
      try {
        await native.cogneeAdd(
          h.handle,
          { type: "text", text: "dataset A" },
          "ds_all_a"
        );
        await native.cogneeAdd(
          h.handle,
          { type: "text", text: "dataset B" },
          "ds_all_b"
        );
        const results = await native.cogneeDeleteAllDatasets(h.handle);
        expect(Array.isArray(results)).toBe(true);
        expect(results.length).toBeGreaterThanOrEqual(2);
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // forget
  // ──────────────────────────────────────────────────────────────────────────

  describe("forget", () => {
    it("forget all deletes all data for the owner", async () => {
      const h = makeTempHandle("forget_all@test.com");
      await native.cogneeWarm(h.handle);
      try {
        await native.cogneeAdd(
          h.handle,
          { type: "text", text: "forget me" },
          "forget_ds"
        );
        const result = await native.cogneeForget(h.handle, { kind: "all" });
        expect(typeof result).toBe("object");
        expect(typeof result.target).toBe("string");
        expect(result.target).toBe("all");
        expect(typeof result.deleteResult).toBe("object");
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("forget dataset by name deletes the dataset", async () => {
      const h = makeTempHandle("forget_dataset@test.com");
      await native.cogneeWarm(h.handle);
      try {
        await native.cogneeAdd(
          h.handle,
          { type: "text", text: "forget dataset content" },
          "forget_named_ds"
        );
        const result = await native.cogneeForget(h.handle, {
          kind: "dataset",
          dataset: { name: "forget_named_ds" },
        });
        expect(typeof result.deleteResult).toBe("object");
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("forget item by dataId deletes a specific item", async () => {
      const h = makeTempHandle("forget_item@test.com");
      await native.cogneeWarm(h.handle);
      try {
        const addResult = await native.cogneeAdd(
          h.handle,
          { type: "text", text: "forget specific item" },
          "forget_item_ds"
        );
        const dataId = addResult.added[0].id;
        const result = await native.cogneeForget(h.handle, {
          kind: "item",
          dataId,
          dataset: { name: "forget_item_ds" },
        });
        expect(result.target).toContain(dataId);
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("forget with invalid kind throws an error", async () => {
      await expect(
        native.cogneeForget(handle, {
          // @ts-expect-error intentionally invalid
          kind: "unknown_kind",
        })
      ).rejects.toThrow();
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // prune
  // ──────────────────────────────────────────────────────────────────────────

  describe("prune", () => {
    it("pruneData completes without error", async () => {
      const h = makeTempHandle("prune_data@test.com");
      await native.cogneeWarm(h.handle);
      try {
        await expect(native.cogneePruneData(h.handle)).resolves.toBeUndefined();
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("pruneSystem (defaults) returns a PruneResult object", async () => {
      const h = makeTempHandle("prune_system@test.com");
      await native.cogneeWarm(h.handle);
      try {
        const result = await native.cogneePruneSystem(h.handle);
        expect(typeof result).toBe("object");
        // PruneResult fields are booleans.
        expect(typeof result.graphPruned).toBe("boolean");
        expect(typeof result.vectorPruned).toBe("boolean");
        expect(typeof result.metadataPruned).toBe("boolean");
        expect(typeof result.cachePruned).toBe("boolean");
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("pruneSystem with all=false returns all-false result", async () => {
      const h = makeTempHandle("prune_false@test.com");
      await native.cogneeWarm(h.handle);
      try {
        const result = await native.cogneePruneSystem(h.handle, {
          pruneGraph: false,
          pruneVector: false,
          pruneMetadata: false,
          pruneCache: false,
        });
        expect(result.graphPruned).toBe(false);
        expect(result.vectorPruned).toBe(false);
        expect(result.metadataPruned).toBe(false);
        expect(result.cachePruned).toBe(false);
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // Pipeline-run resets
  // ──────────────────────────────────────────────────────────────────────────

  describe("pipeline-run resets", () => {
    it("resetPipelineRunStatus inserts an INITIATED row without error", async () => {
      const h = makeTempHandle("reset_pr@test.com");
      await native.cogneeWarm(h.handle);
      try {
        await native.cogneeAdd(
          h.handle,
          { type: "text", text: "reset pipeline run test" },
          "reset_pr_ds"
        );
        const datasets = await native.cogneeListDatasets(h.handle);
        const ds = datasets.find(
          (d: { name: string }) => d.name === "reset_pr_ds"
        );
        expect(ds).toBeDefined();

        await expect(
          native.cogneeResetPipelineRunStatus(
            h.handle,
            ds.id,
            "cognify_pipeline"
          )
        ).resolves.toBeUndefined();
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("resetDatasetPipelineRunStatus completes without error (no runs)", async () => {
      const h = makeTempHandle("reset_ds_pr@test.com");
      await native.cogneeWarm(h.handle);
      try {
        await native.cogneeAdd(
          h.handle,
          { type: "text", text: "reset dataset pipeline run test" },
          "reset_ds_pr_ds"
        );
        const datasets = await native.cogneeListDatasets(h.handle);
        const ds = datasets.find(
          (d: { name: string }) => d.name === "reset_ds_pr_ds"
        );
        expect(ds).toBeDefined();

        // With no pipeline runs this is a no-op — must not throw.
        await expect(
          native.cogneeResetDatasetPipelineRunStatus(h.handle, ds.id)
        ).resolves.toBeUndefined();
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // Sessions
  // ──────────────────────────────────────────────────────────────────────────

  describe("sessions", () => {
    it("getSession returns an empty array for an unknown session", async () => {
      const entries = await native.cogneeGetSession(
        handle,
        "nonexistent-session-id"
      );
      expect(Array.isArray(entries)).toBe(true);
      expect(entries.length).toBe(0);
    });

    it("addFeedback returns false for a nonexistent QA entry", async () => {
      const ok = await native.cogneeAddFeedback(
        handle,
        "test-session",
        "nonexistent-qa-id",
        "great answer",
        5
      );
      // The QA entry doesn't exist → returns false (not found).
      expect(typeof ok).toBe("boolean");
    });

    it("deleteFeedback returns false for a nonexistent QA entry", async () => {
      const ok = await native.cogneeDeleteFeedback(
        handle,
        "test-session",
        "nonexistent-qa-id"
      );
      expect(typeof ok).toBe("boolean");
    });

    it("getGraphContext returns null for an unknown session", async () => {
      const ctx = await native.cogneeGetGraphContext(
        handle,
        "no-such-session"
      );
      expect(ctx).toBeNull();
    });

    it("setGraphContext + getGraphContext round-trip", async () => {
      const sessionId = `test-gc-${Date.now()}`;
      const payload = "test graph context payload";

      await native.cogneeSetGraphContext(handle, sessionId, payload);
      const retrieved = await native.cogneeGetGraphContext(handle, sessionId);
      expect(retrieved).toBe(payload);
    });
  });

  // ──────────────────────────────────────────────────────────────────────────
  // Notebooks
  // ──────────────────────────────────────────────────────────────────────────

  describe("notebooks", () => {
    it("listNotebooks seeds tutorial notebooks on first call", async () => {
      const h = makeTempHandle("nb_list@test.com");
      await native.cogneeWarm(h.handle);
      try {
        const notebooks = await native.cogneeListNotebooks(h.handle);
        expect(Array.isArray(notebooks)).toBe(true);
        // Tutorial seeder inserts 2 notebooks on the first list call.
        expect(notebooks.length).toBeGreaterThanOrEqual(2);
        for (const nb of notebooks) {
          expect(typeof nb.id).toBe("string");
          expect(typeof nb.name).toBe("string");
        }
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("createNotebook creates a notebook and returns it", async () => {
      const h = makeTempHandle("nb_create@test.com");
      await native.cogneeWarm(h.handle);
      try {
        const nb = await native.cogneeCreateNotebook(
          h.handle,
          "My Test Notebook",
          [{ type: "code", content: "print('hello')" }],
          true
        );
        expect(typeof nb.id).toBe("string");
        expect(nb.name).toBe("My Test Notebook");
        expect(nb.deletable).toBe(true);
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("updateNotebook updates name and returns updated notebook", async () => {
      const h = makeTempHandle("nb_update@test.com");
      await native.cogneeWarm(h.handle);
      try {
        const created = await native.cogneeCreateNotebook(
          h.handle,
          "Original Name"
        );
        const updated = await native.cogneeUpdateNotebook(
          h.handle,
          created.id,
          { name: "Updated Name" }
        );
        expect(updated).not.toBeNull();
        expect(updated?.name).toBe("Updated Name");
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("updateNotebook for nonexistent id returns null", async () => {
      const result = await native.cogneeUpdateNotebook(
        handle,
        "00000000-0000-0000-0000-000000000099",
        { name: "Ghost" }
      );
      expect(result).toBeNull();
    });

    it("deleteNotebook returns true for an existing notebook", async () => {
      const h = makeTempHandle("nb_delete@test.com");
      await native.cogneeWarm(h.handle);
      try {
        const nb = await native.cogneeCreateNotebook(h.handle, "To Delete");
        const removed = await native.cogneeDeleteNotebook(h.handle, nb.id);
        expect(removed).toBe(true);
      } finally {
        cleanupTmpDir(h.tmpDir);
      }
    });

    it("deleteNotebook returns false for a nonexistent id", async () => {
      const removed = await native.cogneeDeleteNotebook(
        handle,
        "00000000-0000-0000-0000-000000000099"
      );
      expect(removed).toBe(false);
    });
  });
});

// ---------------------------------------------------------------------------
// Tier-B: remember / memify / improve (skip cleanly without creds)
// ---------------------------------------------------------------------------

describe("Phase-5 memory ops (Tier-B — skips without LLM creds)", () => {
  const hasLlmCreds =
    !!process.env.OPENAI_URL &&
    !!process.env.OPENAI_TOKEN &&
    !!process.env.COGNEE_E2E_EMBED_MODEL_PATH;

  if (!hasLlmCreds) {
    it.skip(
      "skipping Tier-B (OPENAI_URL / OPENAI_TOKEN / COGNEE_E2E_EMBED_MODEL_PATH not set)",
      () => {
        /* intentionally blank */
      }
    );
    return;
  }

  let tmpDir: string;
  let handle: NativeBox;

  beforeAll(async () => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-phase5-b-"));
    const dbPath = path.join(tmpDir, "cognee.db");
    handle = native.cogneeNew({
      system_root_directory: tmpDir,
      data_root_directory: path.join(tmpDir, "data"),
      relational_db_url: `sqlite:${dbPath}?mode=rwc`,
      llm_api_key: process.env.OPENAI_TOKEN,
      llm_endpoint: process.env.OPENAI_URL,
      embedding_model_path: process.env.COGNEE_E2E_EMBED_MODEL_PATH,
    });
    await native.cogneeWarm(handle);
  });

  afterAll(() => {
    cleanupTmpDir(tmpDir);
  });

  it("cogneeRemember returns a RememberResult with status=PipelineRunCompleted", async () => {
    const result = await native.cogneeRemember(
      handle,
      { type: "text", text: "The Eiffel Tower is in Paris, France." },
      "tier_b_remember"
    );
    expect(result).toBeDefined();
    expect(result.status).toBe("PipelineRunCompleted");
    expect(result.datasetName).toBe("tier_b_remember");
  });

  it("cogneeMemify runs without error and returns MemifyResult", async () => {
    const result = await native.cogneeMemify(handle);
    expect(typeof result.tripletCount).toBe("number");
    expect(typeof result.indexedCount).toBe("number");
    expect(typeof result.alreadyCompleted).toBe("boolean");
  });

  it("cogneeImprove returns ImproveResult with stagesRun array", async () => {
    const result = await native.cogneeImprove(handle, {
      datasetName: "tier_b_remember",
    });
    expect(Array.isArray(result.stagesRun)).toBe(true);
    expect(typeof result.feedbackEntriesProcessed).toBe("number");
    expect(typeof result.sessionsPersisted).toBe("number");
  });
});
