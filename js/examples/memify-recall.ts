/**
 * memify-recall.ts — Runnable example: graph enrichment (memify) + session recall.
 *
 * Prerequisites
 * -------------
 * 1. Build the native addon:
 *      cd js/cognee-neon && npm run build
 * 2. Set the following environment variables:
 *      OPENAI_URL=https://api.openai.com/v1    # or any OpenAI-compatible endpoint
 *      OPENAI_TOKEN=sk-...                     # API key
 *      OPENAI_MODEL=gpt-4o-mini                # optional
 *      MOCK_EMBEDDING=true                     # skip ONNX download in CI / quick tests
 *
 * Running
 * -------
 *   npm run example:memify          (from the js/ directory)
 *   npx ts-node js/examples/memify-recall.ts
 *
 * What it does
 * ------------
 * 1. Adds text and runs the cognify pipeline to build a knowledge graph.
 * 2. Runs memify to create triplet embeddings (enables TripletCompletion search).
 * 3. Demonstrates recall() with session-first routing.
 * 4. Shows remember(), the single-call add+cognify shortcut.
 */

import { Cognee } from "../src";

async function main(): Promise<void> {
  // Validate required credentials up front.
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

  // ── Step 1: construct and warm up ─────────────────────────────────────────
  const cognee = new Cognee({
    llmEndpoint,
    llmApiKey,
    llmModel: process.env.OPENAI_MODEL ?? "gpt-4o-mini",
    ...(useMock
      ? { embeddingProvider: "mock" }
      : {
          embeddingProvider: process.env.EMBEDDING_PROVIDER ?? "openai",
          embeddingEndpoint: llmEndpoint,
          embeddingApiKey: llmApiKey,
          embeddingModel: process.env.EMBEDDING_MODEL ?? "text-embedding-3-small",
          embeddingDimensions: Number(process.env.EMBEDDING_DIMENSIONS ?? "1536"),
        }),
  });

  console.log("Warming up cognee services...");
  await cognee.warm();

  const datasetName = "memify-demo";

  // ── Step 2: add + cognify ──────────────────────────────────────────────────
  console.log(`\nAdding text to dataset "${datasetName}"...`);
  await cognee.add(
    {
      type: "text",
      text:
        "Marie Curie was a physicist and chemist who conducted pioneering research " +
        "on radioactivity. She was the first woman to win a Nobel Prize, and the " +
        "only person to win the Nobel Prize in two different sciences.",
    },
    datasetName
  );

  console.log("Running cognify pipeline...");
  const cognifyResult = await cognee.cognify(datasetName);
  console.log(
    `Cognify complete: ${cognifyResult.chunks} chunk(s), ` +
    `${cognifyResult.entities} entit(ies).`
  );

  // ── Step 3: memify ─────────────────────────────────────────────────────────
  //
  // Builds triplet embeddings from all edges in the knowledge graph.
  // After memify, you can use searchType: "TRIPLET_COMPLETION" in search.
  // Idempotent — safe to call multiple times.
  console.log("\nRunning memify (builds triplet embeddings)...");
  const memifyResult = await cognee.memify();
  console.log(
    `Memify complete: ${memifyResult.tripletCount} triplet(s), ` +
    `${memifyResult.indexedCount} indexed.`
  );

  // ── Step 4: recall ─────────────────────────────────────────────────────────
  //
  // recall() routes the query through session history first, then falls back
  // to graph search.  scope controls which sources are checked.
  console.log("\nRecalling with TRIPLET_COMPLETION search type...");
  const recallResult = await cognee.recall(
    "What fields did Marie Curie work in?",
    { searchType: "TRIPLET_COMPLETION", topK: 5 }
  );
  console.log(
    `Recall: searchTypeUsed=${recallResult.searchTypeUsed}, ` +
    `autoRouted=${recallResult.autoRouted}`
  );
  console.log("Items:", JSON.stringify(recallResult.items, null, 2));

  // ── Step 5: remember ───────────────────────────────────────────────────────
  //
  // remember() is a single-call add+cognify shortcut.
  // Pass selfImprovement: true to also run a memify pass.
  console.log("\nDemonstrating remember() (add + cognify in one call)...");
  const rememberResult = await cognee.remember(
    {
      type: "text",
      text: "Curie's husband Pierre Curie was also a physicist.",
    },
    datasetName
  );
  console.log("Remember result keys:", Object.keys(rememberResult));
}

main().catch((err) => {
  console.error("Example failed:", err);
  process.exit(1);
});
