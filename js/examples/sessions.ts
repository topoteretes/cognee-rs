/**
 * sessions.ts — Runnable example: QA-history sessions and feedback.
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
 *   npm run example:sessions        (from the js/ directory)
 *   npx ts-node js/examples/sessions.ts
 *
 * What it does
 * ------------
 * 1. Adds text and runs cognify so the graph has content.
 * 2. Searches with saveInteraction: true to persist a QA entry to a session.
 * 3. Retrieves the stored session history.
 * 4. Adds and then removes feedback on a QA entry.
 * 5. Stores and reads back a graph-context snapshot on the session.
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

  const datasetName = "session-demo";

  // ── Step 1: populate the graph ─────────────────────────────────────────────
  console.log(`\nAdding data to dataset "${datasetName}"...`);
  await cognee.add(
    {
      type: "text",
      text:
        "Isaac Newton formulated the laws of motion and universal gravitation. " +
        "He also made foundational contributions to optics and invented calculus " +
        "independently of Leibniz.",
    },
    datasetName
  );
  console.log("Running cognify pipeline...");
  await cognee.cognify(datasetName);
  console.log("Cognify complete.");

  // ── Step 2: search and save the interaction ────────────────────────────────
  //
  // Passing saveInteraction: true (or sessionId) persists a QA entry so it
  // can be retrieved later from session history.
  const sessionId = "example-session-001";
  const query = "What did Isaac Newton discover?";
  console.log(`\nSearching with sessionId="${sessionId}": "${query}"`);
  await cognee.search(query, {
    saveInteraction: true,
    sessionId,
  });
  console.log("Search saved to session history.");

  // ── Step 3: retrieve session history ──────────────────────────────────────
  console.log(`\nRetrieving session history for "${sessionId}"...`);
  const entries = await cognee.sessions.get(sessionId);
  console.log(`Found ${entries.length} QA entry(ies):`);
  for (const entry of entries) {
    console.log(`  id=${entry.id}  question=${JSON.stringify(entry.question ?? "")}`);
  }

  if (entries.length === 0) {
    console.log("No session entries found — feedback demo skipped.");
  } else {
    const qaId = entries[0].id;

    // ── Step 4: add feedback ─────────────────────────────────────────────────
    console.log(`\nAdding feedback to QA entry "${qaId}"...`);
    const added = await cognee.sessions.addFeedback(
      sessionId,
      qaId,
      "Very helpful!",
      5
    );
    console.log("addFeedback returned:", added);

    // ── Step 5: remove feedback ──────────────────────────────────────────────
    console.log(`\nRemoving feedback from QA entry "${qaId}"...`);
    const removed = await cognee.sessions.deleteFeedback(sessionId, qaId);
    console.log("deleteFeedback returned:", removed);
  }

  // ── Step 6: graph-context snapshot ────────────────────────────────────────
  const ctxBefore = await cognee.sessions.getGraphContext(sessionId);
  console.log(`\nGraph context before set: ${JSON.stringify(ctxBefore)}`);

  await cognee.sessions.setGraphContext(sessionId, '{"nodes": ["newton"]}');
  const ctxAfter = await cognee.sessions.getGraphContext(sessionId);
  console.log(`Graph context after set: ${JSON.stringify(ctxAfter)}`);
}

main().catch((err) => {
  console.error("Example failed:", err);
  process.exit(1);
});
