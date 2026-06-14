/**
 * datasets.ts — Runnable example: dataset listing, status, and deletion.
 *
 * Prerequisites
 * -------------
 * 1. Build the native addon:
 *      cd js/cognee-neon && npm run build
 * 2. Set the following environment variables:
 *      OPENAI_URL=https://api.openai.com/v1
 *      OPENAI_TOKEN=sk-...
 *      MOCK_EMBEDDING=true    # skip ONNX download
 *
 * Running
 * -------
 *   npm run example:datasets        (from the js/ directory)
 *   npx ts-node js/examples/datasets.ts
 *
 * What it does
 * ------------
 * 1. Adds text to two named datasets.
 * 2. Lists all datasets and prints their IDs.
 * 3. Checks whether the first dataset has content (has).
 * 4. Queries pipeline-run statuses for all dataset IDs.
 * 5. Lists data items inside one dataset.
 * 6. Empties one dataset and confirms it is gone.
 */

import { Cognee } from "../src";

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

  // ── Step 1: populate two datasets ─────────────────────────────────────────
  console.log("\nAdding data to 'dataset-alpha' and 'dataset-beta'...");
  await cognee.add(
    { type: "text", text: "Alpha dataset: content about AI memory systems." },
    "dataset-alpha"
  );
  await cognee.add(
    { type: "text", text: "Beta dataset: content about knowledge graphs." },
    "dataset-beta"
  );
  console.log("Add complete.");

  // ── Step 2: list all datasets ──────────────────────────────────────────────
  console.log("\nListing all datasets...");
  const datasets = await cognee.datasets.list();
  console.log(`Found ${datasets.length} dataset(s):`);
  for (const ds of datasets) {
    console.log(`  id=${ds.id}  name=${ds.name}`);
  }

  if (datasets.length === 0) {
    console.log("No datasets found — exiting early.");
    return;
  }

  const firstId = datasets[0].id;

  // ── Step 3: has() — check content ─────────────────────────────────────────
  const hasContent = await cognee.datasets.has(firstId);
  console.log(`\ndatasets.has("${firstId}") = ${hasContent}`);

  // ── Step 4: status() — pipeline-run statuses ──────────────────────────────
  const allIds = datasets.map((ds) => ds.id);
  console.log(`\nQuerying pipeline-run status for ${allIds.length} dataset(s)...`);
  const statuses = await cognee.datasets.status(allIds);
  for (const [dsId, status] of Object.entries(statuses)) {
    console.log(`  ${dsId}: ${status}`);
  }

  // ── Step 5: listData() — data items in the first dataset ──────────────────
  console.log(`\nListing data items in dataset "${firstId}"...`);
  const items = await cognee.datasets.listData(firstId);
  console.log(`Found ${items.length} item(s):`);
  for (const item of items) {
    console.log(`  id=${item.id}  name=${item.name}`);
  }

  // ── Step 6: empty() — delete a dataset ────────────────────────────────────
  console.log(`\nEmptying dataset "${firstId}"...`);
  const deleteResult = await cognee.datasets.empty(firstId);
  console.log("Delete result:", JSON.stringify(deleteResult, null, 2));

  const stillHas = await cognee.datasets.has(firstId);
  console.log(`has("${firstId}") after empty: ${stillHas}`);
}

main().catch((err) => {
  console.error("Example failed:", err);
  process.exit(1);
});
