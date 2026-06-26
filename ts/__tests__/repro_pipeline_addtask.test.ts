import { Pipeline, TaskInfo, createTask, CogneeValue } from "../src";

// Repro for the reported v0.1.1 bug: Pipeline.addTask(...) throws a Neon
// "failed to downcast" error when tasks are added the way the README
// documents (Appendix: low-level pipeline API):
//   const task = createTask(fn);
//   p.addTask(new TaskInfo(task));
describe("repro: Pipeline.addTask downcast bug (README documented usage)", () => {
  it("addTask accepts a TaskInfo wrapping a createTask result", () => {
    const p = new Pipeline("repro-pipeline");

    const task = createTask(
      (v: CogneeValue) => (typeof v === "string" ? v : String(v)) + " | step-A",
      { name: "append-step-a" }
    );

    // The exact failing call the user hit, mirroring ts/README.md.
    expect(() => p.addTask(new TaskInfo(task))).not.toThrow();
  });

  it("addTask also accepts a bare createTask result (already a TaskInfo)", () => {
    const p = new Pipeline("repro-pipeline-2");
    const task = createTask((v: CogneeValue) => v, { name: "identity" });
    expect(() => p.addTask(task)).not.toThrow();
  });
});
