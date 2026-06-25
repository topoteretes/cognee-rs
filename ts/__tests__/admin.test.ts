/**
 * Admin ops — getOrCreateDefaultUser / resetPipelineRunStatus /
 *             resetDatasetPipelineRunStatus.
 *
 * All tests are Tier-A: no LLM / no network calls.
 * Config follows the same pattern as other test files:
 *   - MOCK_EMBEDDING=true  → no model downloads
 *   - dummy llm_api_key    → OpenAIAdapter constructs, never reaches network
 *   - isolated temp dirs + in-process SQLite DB
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native, NativeBox } from "../src/native";

// ─── helpers ─────────────────────────────────────────────────────────────────

function makeTempHandle(email: string): { tmpDir: string; handle: NativeBox } {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-adm-"));
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

// ─── suite ────────────────────────────────────────────────────────────────────

describe("admin ops (Tier-A — no LLM)", () => {
  beforeAll(() => {
    process.env.MOCK_EMBEDDING = "true";
  });

  afterAll(() => {
    delete process.env.MOCK_EMBEDDING;
  });

  // ── getOrCreateDefaultUser ────────────────────────────────────────────────

  describe("getOrCreateDefaultUser", () => {
    it("returns a User with the configured email", async () => {
      const email = "admin_test_a@example.com";
      const h = makeTempHandle(email);
      await native.cogneeWarm(h.handle);
      try {
        const user = await native.cogneeGetOrCreateDefaultUser(h.handle);
        expect(typeof user.id).toBe("string");
        expect(user.id.length).toBeGreaterThan(0);
        expect(user.email).toBe(email);
        expect(typeof user.is_active).toBe("boolean");
        expect(user.is_active).toBe(true);
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("is idempotent — same id on second call", async () => {
      const h = makeTempHandle("admin_idem@example.com");
      await native.cogneeWarm(h.handle);
      try {
        const u1 = await native.cogneeGetOrCreateDefaultUser(h.handle);
        const u2 = await native.cogneeGetOrCreateDefaultUser(h.handle);
        expect(u1.id).toBe(u2.id);
        expect(u1.email).toBe(u2.email);
      } finally {
        cleanup(h.tmpDir);
      }
    });

    it("two handles with different emails return different user ids", async () => {
      const h1 = makeTempHandle("admin_diff_a@example.com");
      const h2 = makeTempHandle("admin_diff_b@example.com");
      await native.cogneeWarm(h1.handle);
      await native.cogneeWarm(h2.handle);
      try {
        const u1 = await native.cogneeGetOrCreateDefaultUser(h1.handle);
        const u2 = await native.cogneeGetOrCreateDefaultUser(h2.handle);
        expect(u1.email).toBe("admin_diff_a@example.com");
        expect(u2.email).toBe("admin_diff_b@example.com");
        // Different emails → different UUIDs (UUID5 is deterministic per email).
        expect(u1.id).not.toBe(u2.id);
      } finally {
        cleanup(h1.tmpDir);
        cleanup(h2.tmpDir);
      }
    });
  });

  // ── resetPipelineRunStatus ────────────────────────────────────────────────

  describe("resetPipelineRunStatus", () => {
    it("completes without error for a known dataset", async () => {
      const h = makeTempHandle("admin_reset_pr@example.com");
      await native.cogneeWarm(h.handle);
      try {
        // Add data to create a dataset first.
        await native.cogneeAdd(
          h.handle,
          { type: "text", text: "pipeline run status reset test" },
          "admin_pr_ds"
        );
        const datasets = await native.cogneeListDatasets(h.handle);
        const ds = datasets.find(
          (d: { name: string }) => d.name === "admin_pr_ds"
        );
        expect(ds).toBeDefined();

        await expect(
          native.cogneeResetPipelineRunStatus(
            h.handle,
            ds!.id,
            "cognify_pipeline"
          )
        ).resolves.toBeUndefined();
      } finally {
        cleanup(h.tmpDir);
      }
    });
  });

  // ── resetDatasetPipelineRunStatus ─────────────────────────────────────────

  describe("resetDatasetPipelineRunStatus", () => {
    it("completes without error (no pipeline runs to reset)", async () => {
      const h = makeTempHandle("admin_reset_ds_pr@example.com");
      await native.cogneeWarm(h.handle);
      try {
        await native.cogneeAdd(
          h.handle,
          { type: "text", text: "dataset pipeline run status reset test" },
          "admin_ds_pr_ds"
        );
        const datasets = await native.cogneeListDatasets(h.handle);
        const ds = datasets.find(
          (d: { name: string }) => d.name === "admin_ds_pr_ds"
        );
        expect(ds).toBeDefined();

        // With no pipeline runs this is a no-op — must not throw.
        await expect(
          native.cogneeResetDatasetPipelineRunStatus(h.handle, ds!.id)
        ).resolves.toBeUndefined();
      } finally {
        cleanup(h.tmpDir);
      }
    });
  });
});
