/**
 * Data ops — forget / update / pruneData / pruneSystem.
 *
 * All tests are Tier-A: no LLM / no network calls.
 * Config follows the same pattern as other test files:
 *   - MOCK_EMBEDDING=true  → no model downloads
 *   - dummy llm_api_key    → OpenAIAdapter constructs, never reaches network
 *   - isolated temp dirs + in-process SQLite DB
 *
 * `update` (delete + re-add + re-cognify) is LLM-dependent; it is tested in
 * Tier-B which skips cleanly when OPENAI_* env vars are absent.
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native, NativeBox } from "../src/native";

// ─── helpers ─────────────────────────────────────────────────────────────────

function makeTempHandle(email: string): { tmpDir: string; handle: NativeBox } {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-dops-"));
  const dbPath = path.join(tmpDir, "cognee.db");
  const handle = native.cogneeNew({
    system_root_directory: tmpDir,
    data_root_directory: path.join(tmpDir, "data"),
    relational_db_url: `sqlite:${dbPath}?mode=rwc`,
    embedding_provider: "mock",
    llm_api_key: "test-dummy-key",
    default_user_email: email,
  });
  return { tmpDir, handle };
}

function cleanup(dir: string) {
  try {
    fs.rmSync(dir, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
}

// ─── Tier-A ──────────────────────────────────────────────────────────────────

describe("data ops (Tier-A — no LLM)", () => {
  beforeAll(() => {
    process.env.MOCK_EMBEDDING = "true";
  });

  afterAll(() => {
    delete process.env.MOCK_EMBEDDING;
  });

  // ── forget ────────────────────────────────────────────────────────────────

  describe("forget", () => {
    it("forget all deletes all data and returns a ForgetResult", async () => {
      const h = makeTempHandle("dops_forget_all@example.com");
      await native.cogneeWarm(h.handle);
      try {
        await native.cogneeAdd(
          h.handle,
          { type: "text", text: "data to forget" },
          "forget_all_ds"
        );
        const result = await native.cogneeForget(h.handle, { kind: "all" });
        expect(typeof result).toBe("object");
        expect(result.target).toBe("all");
        expect(typeof result.deleteResult).toBe("object");
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("forget all on an empty owner completes without error", async () => {
      const h = makeTempHandle("dops_forget_empty@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const result = await native.cogneeForget(h.handle, { kind: "all" });
        expect(typeof result).toBe("object");
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("forget dataset by name deletes the dataset", async () => {
      const h = makeTempHandle("dops_forget_ds@example.com");
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
        cleanup(h.tmpDir);
      }
    });

    it("forget item by dataId deletes a specific item", async () => {
      const h = makeTempHandle("dops_forget_item@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const addResult = await native.cogneeAdd(
          h.handle,
          { type: "text", text: "item to forget" },
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
        cleanup(h.tmpDir);
      }
    });

    it("forget with an unknown kind throws an error", async () => {
      const h = makeTempHandle("dops_forget_bad@example.com");
      await native.cogneeWarm(h.handle);
      try {
        await expect(
          native.cogneeForget(h.handle, {
            // @ts-expect-error intentionally invalid kind
            kind: "not_a_real_kind",
          })
        ).rejects.toThrow();
      } finally {
        cleanup(h.tmpDir);
      }
    });
  });

  // ── pruneData ─────────────────────────────────────────────────────────────

  describe("pruneData", () => {
    it("completes without error on an empty dataset", async () => {
      const h = makeTempHandle("dops_prune_data@example.com");
      await native.cogneeWarm(h.handle);
      try {
        await expect(native.cogneePruneData(h.handle)).resolves.toBeUndefined();
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("completes without error after data has been added", async () => {
      const h = makeTempHandle("dops_prune_data2@example.com");
      await native.cogneeWarm(h.handle);
      try {
        await native.cogneeAdd(
          h.handle,
          { type: "text", text: "content to prune" },
          "prune_data_ds"
        );
        await expect(native.cogneePruneData(h.handle)).resolves.toBeUndefined();
      } finally {
        cleanup(h.tmpDir);
      }
    });
  });

  // ── pruneSystem ───────────────────────────────────────────────────────────

  describe("pruneSystem", () => {
    it("with defaults returns a PruneResult with boolean fields", async () => {
      const h = makeTempHandle("dops_prune_sys@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const result = await native.cogneePruneSystem(h.handle);
        expect(typeof result).toBe("object");
        expect(typeof result.graphPruned).toBe("boolean");
        expect(typeof result.vectorPruned).toBe("boolean");
        expect(typeof result.metadataPruned).toBe("boolean");
        expect(typeof result.cachePruned).toBe("boolean");
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("with all opts=false returns all-false result", async () => {
      const h = makeTempHandle("dops_prune_sys_false@example.com");
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
        cleanup(h.tmpDir);
      }
    });

    it("pruneSystem after forget all completes without error", async () => {
      const h = makeTempHandle("dops_prune_after_forget@example.com");
      await native.cogneeWarm(h.handle);
      try {
        await native.cogneeAdd(
          h.handle,
          { type: "text", text: "content before prune" },
          "prune_after_forget_ds"
        );
        await native.cogneeForget(h.handle, { kind: "all" });
        const result = await native.cogneePruneSystem(h.handle);
        expect(typeof result.graphPruned).toBe("boolean");
      } finally {
        cleanup(h.tmpDir);
      }
    });
  });
});

// ─── Tier-B (live LLM for update) ────────────────────────────────────────────

const OPENAI_URL = process.env.OPENAI_URL;
const OPENAI_TOKEN = process.env.OPENAI_TOKEN;
const EMBED_MODEL_PATH = process.env.COGNEE_E2E_EMBED_MODEL_PATH;
const TOKENIZER_PATH = process.env.COGNEE_E2E_TOKENIZER_PATH;

const haveCreds =
  !!OPENAI_URL && !!OPENAI_TOKEN && !!EMBED_MODEL_PATH && !!TOKENIZER_PATH;

const describeMaybe = haveCreds ? describe : describe.skip;

describeMaybe("data ops — update (Tier-B — live LLM)", () => {
  let tmpDir: string;
  let handle: NativeBox;
  let dataId: string;

  beforeAll(async () => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-dops-b-"));
    const dbPath = path.join(tmpDir, "cognee.db");
    handle = native.cogneeNew({
      system_root_directory: tmpDir,
      data_root_directory: path.join(tmpDir, "data"),
      relational_db_url: `sqlite:${dbPath}?mode=rwc`,
      default_user_email: "dops_b@example.com",
      llm_provider: "openai",
      llm_endpoint: OPENAI_URL,
      llm_api_key: OPENAI_TOKEN,
      llm_model: process.env.OPENAI_MODEL || "gpt-4o-mini",
      embedding_provider: "onnx",
      embedding_model_path: EMBED_MODEL_PATH,
      embedding_tokenizer_path: TOKENIZER_PATH,
    });
    await native.cogneeWarm(handle);

    const addResult = await native.cogneeAdd(
      handle,
      { type: "text", text: "original content about the solar system" },
      "update_ds"
    );
    dataId = addResult.added[0].id;
  }, 120_000);

  afterAll(() => {
    cleanup(tmpDir);
  });

  it(
    "cogneeUpdate re-adds and re-cognifies content",
    async () => {
      const result = await native.cogneeUpdate(
        handle,
        dataId,
        { type: "text", text: "updated content: the solar system has 8 planets" },
        "update_ds"
      );
      expect(typeof result).toBe("object");
    },
    300_000
  );
});
