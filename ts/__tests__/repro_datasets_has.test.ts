/**
 * Repro for bug: `datasets.has()` throws on a non-UUID string instead of
 * returning `false`. Offline setup (MOCK_EMBEDDING, dummy key, temp SQLite),
 * mirroring datasets.test.ts, but via the high-level Cognee class.
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { Cognee } from "../src/cognee";

describe("datasets.has() with a non-UUID id (repro)", () => {
  let tmpDir: string;
  let c: Cognee;

  beforeAll(async () => {
    process.env.MOCK_EMBEDDING = "true";
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-has-repro-"));
    const dbPath = path.join(tmpDir, "cognee.db");
    c = new Cognee({
      system_root_directory: tmpDir,
      data_root_directory: path.join(tmpDir, "data"),
      relational_db_url: `sqlite:${dbPath}?mode=rwc`,
      embedding_provider: "mock",
      llm_api_key: "test-dummy-key",
      default_user_email: "has_repro@example.com",
    });
    await c.warm();
  });

  afterAll(() => {
    delete process.env.MOCK_EMBEDDING;
    try { fs.rmSync(tmpDir, { recursive: true, force: true }); } catch { /* best effort */ }
  });

  it("returns false for a non-UUID id instead of throwing", async () => {
    await expect(c.datasets.has("nonexistent-id")).resolves.toBe(false);
  });

  it("returns false for a valid but unknown UUID", async () => {
    await expect(
      c.datasets.has("00000000-0000-0000-0000-000000000001")
    ).resolves.toBe(false);
  });
});
