/**
 * add-cognify-search.ts — Runnable example: full add → cognify → search pipeline.
 *
 * Prerequisites
 * -------------
 * 1. Build the native addon:
 *      cd js/cognee-neon && npm run build
 * 2. Set the following environment variables (or export them before running):
 *      OPENAI_URL=https://api.openai.com/v1    # or any OpenAI-compatible endpoint
 *      OPENAI_TOKEN=sk-...                     # API key
 *      OPENAI_MODEL=gpt-4o-mini                # model name (optional, defaults to gpt-4o-mini)
 *      EMBEDDING_PROVIDER=openai               # use "openai" for text-embedding-3-small
 *      EMBEDDING_DIMENSIONS=1536               # must match the model
 *      COGNEE_BINDING_SUPPRESS_LOGS=1          # suppress Rust tracing on stderr (optional)
 *
 * Running
 * -------
 *   npx ts-node js/examples/add-cognify-search.ts
 *
 * What it does
 * ------------
 * 1. Creates a Cognee instance configured from the environment.
 * 2. Warms up the services (builds engines and resolves the default user).
 * 3. Adds two short text snippets to a dataset named "demo".
 * 4. Runs the cognify pipeline to extract a knowledge graph.
 * 5. Searches the graph with a natural-language query and prints the result.
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

  // ── Step 1: construct a Cognee instance ────────────────────────────────────
  //
  // Pass only the keys you want to override. Absent keys fall back to the
  // environment (OPENAI_URL / OPENAI_TOKEN / OPENAI_MODEL etc.) and then to
  // built-in defaults. The Settings overlay order is: defaults < env < object.
  const cognee = new Cognee({
    llmEndpoint,
    llmApiKey,
    llmModel: process.env.OPENAI_MODEL ?? "gpt-4o-mini",
    embeddingProvider: process.env.EMBEDDING_PROVIDER ?? "openai",
    embeddingEndpoint: llmEndpoint,
    embeddingApiKey: llmApiKey,
    embeddingModel: process.env.EMBEDDING_MODEL ?? "text-embedding-3-small",
    embeddingDimensions: Number(process.env.EMBEDDING_DIMENSIONS ?? "1536"),
  });

  // ── Step 2: warm up ────────────────────────────────────────────────────────
  //
  // Builds all engines (vector DB, graph DB, embedding engine) and resolves
  // the default user. Safe to call multiple times (idempotent).
  console.log("Warming up cognee services...");
  await cognee.warm();
  const ownerId = await cognee.ownerId();
  console.log(`Owner ID: ${ownerId}`);

  // ── Step 3: add data ───────────────────────────────────────────────────────
  //
  // Text inputs are streamed as UTF-8 blobs. You can also pass a file path
  // ({ type: "file", path: "/path/to/doc.txt" }) or multiple inputs as an array.
  const datasetName = "demo";
  console.log(`\nAdding text snippets to dataset "${datasetName}"...`);

  const addResult = await cognee.add(
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

  console.log(`Added ${addResult.addedCount} item(s), ${addResult.deduplicatedCount} duplicate(s).`);

  // ── Step 4: cognify ────────────────────────────────────────────────────────
  //
  // Extracts entities, relationships, and summaries from the ingested text via
  // the LLM, then indexes them in the graph and vector databases.
  // This step requires a live LLM endpoint.
  console.log("\nRunning cognify pipeline (this calls the LLM)...");
  const cognifyResult = await cognee.cognify(datasetName);

  console.log(
    `Cognify complete: ${cognifyResult.chunks} chunk(s), ` +
    `${cognifyResult.entities} entit(ies), ` +
    `${cognifyResult.edges} edge(s).`
  );

  // ── Step 5: search ─────────────────────────────────────────────────────────
  //
  // Queries the knowledge graph. The default search type is GRAPH_COMPLETION,
  // which uses the LLM to synthesize an answer from the retrieved graph context.
  const query = "Who designed the Eiffel Tower?";
  console.log(`\nSearching: "${query}"`);
  const searchResult = await cognee.search(query);

  console.log("\nSearch result:");
  console.log(JSON.stringify(searchResult, null, 2));
}

main().catch((err) => {
  console.error("Example failed:", err);
  process.exit(1);
});
