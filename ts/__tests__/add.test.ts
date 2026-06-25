/**
 * Tier-A test for Phase-3 `add` (pure ingestion — NO LLM, NO network).
 *
 * `add` performs MD5 content hashing, deterministic UUID5 ids, dedup, and
 * `text_<md5>.txt` storage entirely in-process, so it is fully deterministic
 * and runs green in the `ts-check` CI job (no `OPENAI_*` / model env needed).
 *
 * Config mirrors `sdk_handle.test.ts`:
 *   - MOCK_EMBEDDING=true            → no embedding model download / network
 *   - a non-empty dummy llm_api_key  → OpenAIAdapter constructs (no network I/O)
 *   - temp system/data dirs + a temp sqlite DB → isolated, no shared state
 *
 * A single handle is built in `beforeAll` and warmed once (the native engines —
 * qdrant / ladybug — are only constructed on the first op); all tests reuse it
 * so the cold-start cost is paid exactly once and dedup is exercised against a
 * stable owner/DB.
 *
 * Asserts:
 *   - text add returns the newly-added `Data` item(s) (id / content_hash / …);
 *   - re-adding identical content returns an empty `added` array (dedup);
 *   - the dataset row is created and resolvable (a second op on the same name
 *     does not error);
 *   - array + file-path inputs work;
 *   - malformed / unsupported inputs reject.
 *
 * NOTE: `cogneeCognify` is deliberately NOT exercised here — it needs an LLM +
 * embeddings and lives in the Tier-B suite (`cognify.test.ts`), which skips
 * cleanly without creds.
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native, NativeBox } from "../src/native";

describe("Phase-3 add (Tier-A, no LLM)", () => {
  let tmpDir: string;
  let dbPath: string;
  let handle: NativeBox;
  const email = "phase3_add_tier_a@example.com";

  beforeAll(async () => {
    process.env.MOCK_EMBEDDING = "true";
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-add-"));
    dbPath = path.join(tmpDir, "cognee.db");
    handle = native.cogneeNew({
      system_root_directory: tmpDir,
      data_root_directory: path.join(tmpDir, "data"),
      relational_db_url: `sqlite:${dbPath}?mode=rwc`,
      embedding_provider: "mock",
      llm_api_key: "test-dummy-key",
      default_user_email: email,
    });
    // Pay the cold engine-build cost once, up front.
    await native.cogneeWarm(handle);
  });

  afterAll(() => {
    delete process.env.MOCK_EMBEDDING;
    try {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    } catch {
      // best effort
    }
  });

  it("adds a text item and returns the newly-added Data", async () => {
    const result = await native.cogneeAdd(
      handle,
      { type: "text", text: "The quick brown fox jumps over the lazy dog." },
      "tier_a_text"
    );

    expect(result.datasetName).toBe("tier_a_text");
    expect(result.addedCount).toBe(1);
    expect(Array.isArray(result.added)).toBe(true);
    expect(result.added).toHaveLength(1);

    const item = result.added[0];
    // Data is Serialize'd directly — these are the Rust field names.
    expect(typeof item.id).toBe("string");
    expect(typeof item.content_hash).toBe("string");
    expect(item.content_hash).toMatch(/^[0-9a-f]{32}$/); // MD5 hex
    expect(typeof item.mime_type).toBe("string");
    expect(item.raw_data_location).toContain("file://");
  });

  it("dedups identical content on re-add (empty added array)", async () => {
    const text = "Deterministic dedup payload — content-addressed by MD5.";

    const first = await native.cogneeAdd(
      handle,
      { type: "text", text },
      "tier_a_dedup"
    );
    expect(first.addedCount).toBe(1);
    expect(first.deduplicatedCount).toBe(0);
    const firstId = first.added[0].id;

    // Re-adding the exact same content to the same dataset is a no-op: the
    // pipeline returns the pre-existing (content-addressed) row, so it lands in
    // `deduplicated`, not `added`.
    const second = await native.cogneeAdd(
      handle,
      { type: "text", text },
      "tier_a_dedup"
    );
    expect(second.addedCount).toBe(0);
    expect(second.added).toHaveLength(0);
    expect(second.deduplicatedCount).toBe(1);
    // Content-addressed UUID5: the duplicate carries the same id.
    expect(second.deduplicated[0].id).toBe(firstId);
  });

  it("creates a dataset row that is resolvable by name", async () => {
    // First add creates the dataset; a second add to the SAME name must resolve
    // the existing row (get-or-create) rather than erroring.
    await native.cogneeAdd(handle, { type: "text", text: "alpha" }, "tier_a_ds");
    const again = await native.cogneeAdd(
      handle,
      { type: "text", text: "beta" },
      "tier_a_ds"
    );
    // "beta" is new content, so it is added under the already-existing dataset.
    expect(again.datasetName).toBe("tier_a_ds");
    expect(again.addedCount).toBe(1);
  });

  it("accepts an array of inputs", async () => {
    const result = await native.cogneeAdd(
      handle,
      [
        { type: "text", text: "first array item" },
        { type: "text", text: "second array item" },
      ],
      "tier_a_array"
    );
    expect(result.addedCount).toBe(2);
    expect(result.added).toHaveLength(2);
  });

  it("adds from a file path", async () => {
    const filePath = path.join(tmpDir, "doc.txt");
    fs.writeFileSync(filePath, "File-based content for the add path.\n");

    const result = await native.cogneeAdd(
      handle,
      { type: "file", path: filePath },
      "tier_a_file"
    );
    expect(result.addedCount).toBe(1);
    expect(result.added).toHaveLength(1);
    expect(result.added[0].raw_data_location).toContain("file://");
  });

  it("rejects a malformed input (missing text)", async () => {
    await expect(
      // @ts-expect-error intentionally malformed for the negative case
      native.cogneeAdd(handle, { type: "text" }, "tier_a_bad")
    ).rejects.toThrow();
  });

  it("rejects an unsupported input variant (s3)", async () => {
    await expect(
      // @ts-expect-error s3 is not part of the supported union
      native.cogneeAdd(handle, { type: "s3", url: "s3://b/k" }, "tier_a_s3")
    ).rejects.toThrow();
  });
});
