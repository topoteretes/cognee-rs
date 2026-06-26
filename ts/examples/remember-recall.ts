/**
 * remember-recall.ts — Runnable example: the high-level remember → recall API.
 *
 * `remember()` and `recall()` are the recommended entry points: `remember()`
 * ingests and extracts a knowledge graph in a single call (the add + cognify
 * shortcut), and `recall()` retrieves with source-aware, session-first routing.
 *
 * Prerequisites
 * -------------
 * 1. Build the native addon:
 *      cd ts/cognee-ts-neon && npm run build
 * 2. Set the following environment variables (or export them before running):
 *      OPENAI_URL=https://api.openai.com/v1    # or any OpenAI-compatible endpoint
 *      OPENAI_TOKEN=sk-...                     # API key
 *      OPENAI_MODEL=gpt-4o-mini                # model name (optional)
 *      EMBEDDING_PROVIDER=openai               # use "openai" for text-embedding-3-small
 *      EMBEDDING_DIMENSIONS=1536               # must match the model
 *      MOCK_EMBEDDING=true                     # skip ONNX download in CI / quick tests
 *
 * Running
 * -------
 *   npm run example:remember        (from the ts/ directory)
 *   npx ts-node ts/examples/remember-recall.ts
 *
 * What it does
 * ------------
 * 1. Creates a Cognee instance configured from the environment.
 * 2. Warms up the services (builds engines and resolves the default user).
 * 3. remember()s two short text snippets into a dataset named "demo".
 * 4. recall()s the graph with a natural-language query and prints the answer.
 */

import { Cognee } from "../src";

async function main(): Promise<void> {
  // Validate that the minimum required env vars are set before starting.
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

  // Set MOCK_EMBEDDING=true to skip the ONNX model download and use mock
  // zero-vectors instead (fast, no GPU required; useful for CI smoke tests).
  const useMock =
    (process.env.MOCK_EMBEDDING ?? "").toLowerCase() === "true" ||
    process.env.MOCK_EMBEDDING === "1";

  // ── Step 1: construct a Cognee instance ────────────────────────────────────
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

  // ── Step 2: warm up ────────────────────────────────────────────────────────
  //
  // Builds all engines (vector DB, graph DB, embedding engine) and resolves
  // the default user. Safe to call multiple times (idempotent).
  console.log("Warming up cognee services...");
  await cognee.warm();
  const ownerId = await cognee.ownerId();
  console.log(`Owner ID: ${ownerId}`);

  // ── Step 3: remember ───────────────────────────────────────────────────────
  //
  // remember() ingests the inputs and extracts a knowledge graph in one call —
  // the high-level equivalent of add() followed by cognify(). Pass
  // { selfImprovement: true } to also run a memify pass, or { sessionId } to
  // store into session memory instead of the graph.
  const datasetName = "demo";
  console.log(`\nRemembering text snippets in dataset "${datasetName}"...`);

  await cognee.remember(
    [
      {
        type: "text",
        text:
          "The Eiffel Tower was built between 1887 and 1889 as a centerpiece for the " +
          "1889 World's Fair in Paris. It was designed by Gustave Eiffel's engineering " +
          "company and stands 330 metres tall.",
      },
      {
        type: "text",
        text:
          "The Statue of Liberty was a gift from France to the United States, dedicated " +
          "in 1886. It was designed by Frédéric Auguste Bartholdi with its metal framework " +
          "built by Gustave Eiffel.",
      },
    ],
    datasetName
  );
  console.log("Remembered. Knowledge graph extracted.");

  // ── Step 4: recall ─────────────────────────────────────────────────────────
  //
  // recall() retrieves with source-aware routing: it checks session QA history
  // first (when a sessionId is set) and otherwise falls back to graph search.
  // The synthesized answer is on `searchResponse.result.data`; `items` carries
  // the source-tagged results that contributed.
  const query = "Who designed the Eiffel Tower?";
  console.log(`\nRecalling: "${query}"`);
  const recall = await cognee.recall(query);

  console.log(
    `\nRecall: ${recall.items.length} item(s), ` +
    `searchTypeUsed=${recall.searchTypeUsed}, autoRouted=${recall.autoRouted}`
  );
  console.log("Answer:", recall.searchResponse?.result?.data);
}

main().catch((err) => {
  console.error("Example failed:", err);
  process.exit(1);
});
