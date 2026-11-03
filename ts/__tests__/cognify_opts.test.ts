/**
 * Tier-A test for the cognify options (NO LLM, NO network).
 *
 * `graphSchema` / `chunkAttachment` are validated before the dataset is
 * resolved and before `addAndCognify` adds anything, so an invalid value is
 * rejected without an LLM call. Config mirrors `add.test.ts`: the mock
 * embedding provider and a dummy LLM key let the engines build offline.
 */
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

import { native, NativeBox } from "../src/native";
import type { GraphSchemaSpec } from "../src/types";

const GRAPH_SCHEMA = {
  title: "Graph",
  type: "object",
  properties: {
    name: { type: "string" },
    metadata: {
      type: "object",
      default: { identity_fields: ["name"], index_fields: ["name"] },
    },
  },
  required: ["name"],
};

describe("cognify options (Tier-A, no LLM)", () => {
  let tmpDir: string;
  let handle: NativeBox;

  beforeAll(async () => {
    process.env.MOCK_EMBEDDING = "true";
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cognee-cognify-opts-"));
    handle = native.cogneeNew({
      system_root_directory: tmpDir,
      data_root_directory: path.join(tmpDir, "data"),
      relational_db_url: `sqlite:${path.join(tmpDir, "cognee.db")}?mode=rwc`,
      embedding_provider: "mock",
      llm_api_key: "test-dummy-key",
      default_user_email: "cognify_opts_tier_a@example.com",
    });
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

  it("rejects a chunkAttachment outside direct/all", async () => {
    await expect(
      native.cogneeCognify(handle, "missing", {
        graphSchema: GRAPH_SCHEMA,
        // Deliberately outside the declared union.
        chunkAttachment: "everything" as unknown as "all",
      })
    ).rejects.toThrow(/chunk_attachment/);
  });

  it("rejects chunkAttachment without graphSchema", async () => {
    await expect(
      native.cogneeCognify(handle, "missing", { chunkAttachment: "direct" })
    ).rejects.toThrow(/requires graph_schema/);
  });

  it("rejects a graph schema that does not compile, also as a string", async () => {
    const bad = { title: "lower", properties: {} };
    await expect(
      native.cogneeCognify(handle, "missing", { graphSchema: bad })
    ).rejects.toThrow(/Invalid graph schema/);
    await expect(
      native.cogneeCognify(handle, "missing", { graphSchema: JSON.stringify(bad) })
    ).rejects.toThrow(/Invalid graph schema/);
  });

  it("compiles a graphSpec and rejects an invalid one before the dataset lookup", async () => {
    const spec: GraphSchemaSpec = {
      entities: [
        {
          name: "Person",
          fields: [
            {
              kind: "relation",
              name: "knows",
              relation: { targetEntityName: "Person", cardinality: "many" },
            },
          ],
        },
      ],
    };
    await expect(
      native.cogneeCognify(handle, "missing", { graphSpec: spec, chunkAttachment: "all" })
    ).rejects.toThrow(/was not found/);
    await expect(
      native.cogneeCognify(handle, "missing", { graphSpec: JSON.stringify(spec) })
    ).rejects.toThrow(/was not found/);
    await expect(
      native.cogneeCognify(handle, "missing", {
        graphSpec: { entities: [{ name: "P" }, { name: "P" }] },
      })
    ).rejects.toThrow(/Entity names must be unique\./);
    await expect(
      native.cogneeCognify(handle, "missing", {
        graphSpec: spec,
        graphSchema: GRAPH_SCHEMA,
      })
    ).rejects.toThrow(/not both/);
  });

  it("types and accepts a snake_case spec, as the docs write it", async () => {
    // The guide's spec, verbatim: snake_case keys type-check too.
    const spec: GraphSchemaSpec = {
      entities: [
        {
          name: "Person",
          index_fields: ["name"],
          fields: [
            { kind: "primitive", name: "age", primitive_type: "number" },
            { kind: "enum", name: "mood", enum_values: ["happy", "grumpy"] },
            {
              kind: "relation",
              name: "works_at",
              relation: { target_entity_name: "Organization" },
            },
          ],
        },
        { name: "Organization" },
      ],
    };
    await expect(
      native.cogneeCognify(handle, "missing", { graphSpec: spec })
    ).rejects.toThrow(/was not found/);

    const bothSpellings: GraphSchemaSpec = {
      // @ts-expect-error -- one spelling per key, as the native parser requires
      entities: [{ name: "P", indexFields: ["name"], index_fields: ["name"] }],
    };
    await expect(
      native.cogneeCognify(handle, "missing", { graphSpec: bothSpellings })
    ).rejects.toThrow(/invalid `graphSpec`/);
  });

  it("accepts valid options, failing only on the missing dataset", async () => {
    await expect(
      native.cogneeCognify(handle, "missing", {
        graphSchema: GRAPH_SCHEMA,
        chunkAttachment: "all",
      })
    ).rejects.toThrow(/was not found/);
  });

  it("addAndCognify adds nothing when the options are invalid", async () => {
    await expect(
      native.cogneeAddAndCognify(
        handle,
        { type: "text", text: "Alice works at Acme." },
        "opts_ds",
        { chunkAttachment: "all" }
      )
    ).rejects.toThrow(/requires graph_schema/);
    // The add half never ran, so the same content is still new.
    const added = await native.cogneeAdd(
      handle,
      { type: "text", text: "Alice works at Acme." },
      "opts_ds"
    );
    expect(added.addedCount).toBe(1);
  });
});
