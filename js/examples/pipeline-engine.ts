/**
 * pipeline-engine.ts — Runnable example: low-level pipeline API.
 *
 * Prerequisites
 * -------------
 * 1. Build the native addon:
 *      cd js/cognee-neon && npm run build
 *
 * Running
 * -------
 *   npm run example:pipeline        (from the js/ directory)
 *   npx ts-node js/examples/pipeline-engine.ts
 *
 * What it does
 * ------------
 * Demonstrates the low-level pipeline API:
 * 1. Constructs a multi-step Pipeline from plain TypeScript functions.
 * 2. Executes it with a mock TaskContext (no real DBs needed).
 * 3. Uses a Watcher to observe lifecycle events during execution.
 * 4. Runs the same pipeline using executeInBackground() and waits via RunHandle.
 * 5. Demonstrates createIterTask (one-to-many fanout).
 * 6. Demonstrates ProgressToken for progress tracking.
 * 7. Demonstrates CancellationHandle / CancellationToken for mid-run abort.
 *
 * No environment variables or LLM credentials are required.
 */

import {
  init,
  CogneeValue,
  TaskContext,
  Pipeline,
  createTask,
  createIterTask,
  createWatcher,
  createCancellationPair,
  ProgressToken,
} from "../src";

// ── Helper ─────────────────────────────────────────────────────────────────────

function tag(label: string): void {
  console.log(`[${label}]`);
}

// ── Example 1: basic multi-step pipeline ──────────────────────────────────────

async function exampleBasicPipeline(): Promise<void> {
  tag("Example 1: basic multi-step pipeline");

  // Build a pipeline that:
  //   step A: append " | step-A" to the string value
  //   step B: convert the string to uppercase
  //
  // CogneeValue is a plain union type: string | number | boolean | Buffer.
  // Task functions receive and return CogneeValue directly.
  const p = new Pipeline("demo-pipeline");

  p.addTask(
    createTask((v: CogneeValue) => {
      const s = typeof v === "string" ? v : String(v);
      return `${s} | step-A`;
    }, { name: "append-step-a" })
  );

  p.addTask(
    createTask((v: CogneeValue) => {
      const s = typeof v === "string" ? v : String(v);
      return s.toUpperCase();
    }, { name: "to-uppercase" })
  );

  const { context } = TaskContext.mock();

  const inputs: CogneeValue[] = ["hello", "world"];
  const outputs = await p.execute(inputs, context);

  console.log(`  Input count:  ${inputs.length}`);
  console.log(`  Output count: ${outputs.length}`);
  for (const out of outputs) {
    console.log(`  output: ${out}`);
  }
  // Expected: "HELLO | STEP-A", "WORLD | STEP-A"
}

// ── Example 2: watcher ────────────────────────────────────────────────────────

async function exampleWatcher(): Promise<void> {
  tag("Example 2: pipeline with watcher");

  const events: string[] = [];

  // WatcherEvents callbacks are named after the pipeline lifecycle events.
  // All callbacks are optional.
  const watcher = createWatcher({
    onPipelineRunStarted:   (_runId, name)         => events.push(`run-started:${name}`),
    onPipelineRunCompleted: (_runId, count)        => events.push(`run-completed:${count}`),
    onTaskStarted:          (_runId, task, _idx)   => events.push(`task-started:${task}`),
    onTaskCompleted:        (_runId, task, count)  => events.push(`task-completed:${task}:${count}`),
  });

  const p = new Pipeline("watcher-demo");
  p.addTask(
    createTask((v: CogneeValue) => `${typeof v === "string" ? v : String(v)}!`)
  );

  init(); // start the global tokio runtime (required for executeWithWatcher)

  const { context } = TaskContext.mock();
  const outputs = await p.executeWithWatcher(
    ["ping"],
    context,
    watcher
  );

  console.log(`  Output: ${outputs[0]}`);
  console.log(`  Events fired: ${events.join(", ")}`);
}

// ── Example 3: background execution ──────────────────────────────────────────

async function exampleBackground(): Promise<void> {
  tag("Example 3: executeInBackground + RunHandle.wait()");

  const p = new Pipeline("bg-demo");
  p.addTask(
    createTask((v: CogneeValue) => {
      const s = typeof v === "string" ? v : String(v);
      return `bg:${s}`;
    })
  );

  const { context } = TaskContext.mock();

  // executeInBackground returns immediately; use handle.wait() to retrieve results.
  const handle = p.executeInBackground(
    ["item-1", "item-2"],
    context
  );

  console.log("  Background execution started, doing other work...");
  const outputs = await handle.wait();
  console.log(`  Background outputs: ${outputs.join(", ")}`);
}

// ── Example 4: iterator task ──────────────────────────────────────────────────

async function exampleIterTask(): Promise<void> {
  tag("Example 4: createIterTask (one-to-many fanout)");

  const p = new Pipeline("iter-demo");

  // An iterator task returns an array: each input item fans out to multiple outputs.
  p.addTask(
    createIterTask((v: CogneeValue): CogneeValue[] => {
      const s = typeof v === "string" ? v : String(v);
      return [
        `${s}-part-1`,
        `${s}-part-2`,
        `${s}-part-3`,
      ];
    }, { name: "fanout" })
  );

  const { context } = TaskContext.mock();
  const outputs = await p.execute(["doc"], context);
  console.log(
    `  Input: 1 item → Output: ${outputs.length} items ` +
    `(${outputs.join(", ")})`
  );
}

// ── Example 5: ProgressToken ──────────────────────────────────────────────────

function exampleProgress(): void {
  tag("Example 5: ProgressToken");

  // ProgressToken.create() constructs a root token at 0%.
  const root = ProgressToken.create();
  const [childA, childB] = root.split([1, 3]); // childA gets 25%, childB gets 75%

  childA.set(1.0);
  console.log(`  After childA complete: root=${root.rootFraction.toFixed(2)}`);

  childB.set(0.5);
  console.log(`  After childB 50%:      root=${root.rootFraction.toFixed(2)}`);

  childB.set(1.0);
  console.log(`  After childB complete: root=${root.rootFraction.toFixed(2)}`);
  console.log(`  root.isComplete: ${root.isComplete}`);
}

// ── Example 6: CancellationHandle / CancellationToken ─────────────────────────

function exampleCancellation(): void {
  tag("Example 6: CancellationHandle / CancellationToken");

  // createCancellationPair returns { handle, token } — not an array.
  const { handle, token } = createCancellationPair();
  console.log(`  token.isCancelled before cancel: ${token.isCancelled}`);

  handle.cancel();
  console.log(`  token.isCancelled after cancel:  ${token.isCancelled}`);
  console.log(`  handle.isCancelled after cancel: ${handle.isCancelled}`);
}

// ── Main ───────────────────────────────────────────────────────────────────────

async function main(): Promise<void> {
  await exampleBasicPipeline();
  await exampleWatcher();
  await exampleBackground();
  await exampleIterTask();
  exampleProgress();
  exampleCancellation();
  console.log("\nAll pipeline-engine examples complete.");
}

main().catch((err) => {
  console.error("Example failed:", err);
  process.exit(1);
});
