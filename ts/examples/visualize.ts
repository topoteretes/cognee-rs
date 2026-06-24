/**
 * visualize.ts — Runnable example: render the knowledge graph to HTML.
 *
 * Prerequisites
 * -------------
 * 1. Build the native addon:
 *      cd ts/cognee-ts-neon && npm run build
 * 2. Set the following environment variables:
 *      OPENAI_URL=https://api.openai.com/v1
 *      OPENAI_TOKEN=sk-...
 *      MOCK_EMBEDDING=true    # skip ONNX download
 *
 * Running
 * -------
 *   npm run example:visualize       (from the ts/ directory)
 *   npx ts-node ts/examples/visualize.ts
 *
 * What it does
 * ------------
 * 1. Adds text and runs the cognify pipeline so the graph has nodes and edges.
 * 2. Calls visualize() to get the full self-contained d3.js HTML as a string.
 * 3. Calls visualizeToFile() to write the HTML to /tmp and prints the path.
 *
 * Requires the 'visualization' Cargo feature to be compiled in.
 */

import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { Cognee, FeatureNotBuiltError } from "../src";

async function main(): Promise<void> {
  const llmEndpoint = process.env.OPENAI_URL;
  const llmApiKey = process.env.OPENAI_TOKEN;
  if (!llmEndpoint || !llmApiKey) {
    console.error(
      "ERROR: OPENAI_URL and OPENAI_TOKEN must be set.\n" +
      "Example:\n" +
      "  export OPENAI_URL=https://api.openai.com/v1\n" +
      "  export OPENAI_TOKEN=sk-...\n"
    );
    process.exit(1);
  }

  const useMock =
    (process.env.MOCK_EMBEDDING ?? "").toLowerCase() === "true" ||
    process.env.MOCK_EMBEDDING === "1";

  const cognee = new Cognee({
    llmEndpoint,
    llmApiKey,
    llmModel: process.env.OPENAI_MODEL ?? "gpt-4o-mini",
    ...(useMock ? { embeddingProvider: "mock" } : {}),
  });

  console.log("Warming up cognee services...");
  await cognee.warm();

  const datasetName = "viz-demo";

  // ── Step 1: build a graph ──────────────────────────────────────────────────
  console.log(`\nAdding data to dataset "${datasetName}"...`);
  await cognee.add(
    {
      type: "text",
      text:
        "Albert Einstein developed the theory of relativity. " +
        "He was awarded the Nobel Prize in Physics in 1921 for his discovery " +
        "of the law of the photoelectric effect.",
    },
    datasetName
  );

  console.log("Running cognify pipeline...");
  const cognifyResult = await cognee.cognify(datasetName);
  console.log(
    `Cognify complete: ${cognifyResult.chunks} chunk(s), ` +
    `${cognifyResult.entities} entit(ies).`
  );

  // ── Step 2: visualize() — get HTML as a string ────────────────────────────
  console.log("\nRendering knowledge graph to HTML string...");
  let html: string;
  try {
    html = await cognee.visualize();
  } catch (err) {
    if (err instanceof FeatureNotBuiltError) {
      console.log(
        "SKIP: The 'visualization' Cargo feature was not compiled in.\n" +
        "Rebuild with: cargo build --features visualization"
      );
      return;
    }
    throw err;
  }

  const htmlSizeKb = Math.round(Buffer.byteLength(html, "utf8") / 1024);
  console.log(`HTML length: ${htmlSizeKb} KB`);
  console.log(`Contains d3.js: ${html.toLowerCase().includes("d3")}`);

  // ── Step 3: visualizeToFile() — write to disk ─────────────────────────────
  const destination = path.join(os.tmpdir(), `cognee_graph_${Date.now()}.html`);
  console.log(`\nWriting graph HTML to "${destination}"...`);
  const writtenPath = await cognee.visualizeToFile({ destinationPath: destination });
  console.log(`Written to: ${writtenPath}`);
  console.log(`File exists on disk: ${fs.existsSync(writtenPath)}`);
  console.log("\nVisualize example complete.");
}

main().catch((err) => {
  console.error("Example failed:", err);
  process.exit(1);
});
