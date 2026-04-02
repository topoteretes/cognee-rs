/**
 * Smoke tests for the cognee-neon bindings.
 *
 * These tests verify the TypeScript API compiles and the class/function
 * signatures are correct. They do NOT require the native .node addon to
 * be built — they only exercise the TypeScript layer.
 *
 * To run integration tests with the actual native module, build the Rust
 * crate first: `npm run build:rust`
 */

import {
  Pipeline,
  TaskContext,
  createTask,
  createIterTask,
  createBatchTask,
  createCancellationPair,
  ProgressToken,
  createWatcher,
  createNoopWatcher,
} from "../src";

// Type-level tests — these just verify the API shape at compile time.
// They will throw at runtime if the native module isn't loaded.

describe("TypeScript API shape", () => {
  it("Pipeline has expected methods", () => {
    expect(Pipeline).toBeDefined();
    expect(Pipeline.prototype.setName).toBeDefined();
    expect(Pipeline.prototype.addTask).toBeDefined();
    expect(Pipeline.prototype.setBatchSize).toBeDefined();
    expect(Pipeline.prototype.setConcurrency).toBeDefined();
    expect(Pipeline.prototype.setRetry).toBeDefined();
    expect(Pipeline.prototype.execute).toBeDefined();
    expect(Pipeline.prototype.executeAsync).toBeDefined();
    expect(Pipeline.prototype.executeInBackground).toBeDefined();
    expect(Pipeline.prototype.executeWithWatcher).toBeDefined();
  });

  it("TaskContext has expected static methods", () => {
    expect(TaskContext.mock).toBeDefined();
    expect(TaskContext.prototype.clone).toBeDefined();
  });

  it("task creators are defined", () => {
    expect(createTask).toBeDefined();
    expect(createIterTask).toBeDefined();
    expect(createBatchTask).toBeDefined();
  });

  it("cancellation API is defined", () => {
    expect(createCancellationPair).toBeDefined();
  });

  it("progress API is defined", () => {
    expect(ProgressToken).toBeDefined();
    expect(ProgressToken.create).toBeDefined();
    expect(ProgressToken.prototype.set).toBeDefined();
    expect(ProgressToken.prototype.split).toBeDefined();
    expect(ProgressToken.prototype.subtoken).toBeDefined();
    expect(ProgressToken.prototype.clone).toBeDefined();
  });

  it("watcher API is defined", () => {
    expect(createWatcher).toBeDefined();
    expect(createNoopWatcher).toBeDefined();
  });
});
