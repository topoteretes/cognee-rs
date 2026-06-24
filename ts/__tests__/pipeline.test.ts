import {
  init,
  Pipeline,
  TaskContext,
  createTask,
  createIterTask,
  createBatchTask,
  CogneeValue,
} from "../src";

// Initialise the global tokio runtime once for all tests.
beforeAll(() => {
  init();
});

function mockCtx() {
  return TaskContext.mock();
}

// ─── Single task types ──────────────────────────────────────────────────────

describe("single task", () => {
  it("sync task: double a number", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("double");
    p.addTask(createTask((x) => (x as number) * 2, { name: "double" }));

    const results = await p.execute([10], context);
    expect(results).toEqual([20]);
  });

  it("sync task: uppercase a string", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("upper");
    p.addTask(createTask((s) => (s as string).toUpperCase(), { name: "upper" }));

    const results = await p.execute(["hello", "world"], context);
    expect((results as string[]).sort()).toEqual(["HELLO", "WORLD"]);
  });

  it("async task: delayed double", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("async-double");
    p.addTask(
      createTask(
        (x) =>
          new Promise<CogneeValue>((resolve) =>
            setTimeout(() => resolve((x as number) * 2), 10)
          ),
        { name: "async-double" }
      )
    );

    const results = await p.execute([7], context);
    expect(results).toEqual([14]);
  });

  it("iter task: split string into words", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("split");
    p.addTask(
      createIterTask((text) => (text as string).split(" "), { name: "split" })
    );

    const results = await p.execute(["hello world"], context);
    expect((results as string[]).sort()).toEqual(["hello", "world"]);
  });

  it("iter task: async array return", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("async-split");
    p.addTask(
      createIterTask(
        (text) =>
          new Promise<CogneeValue[]>((resolve) =>
            setTimeout(() => resolve((text as string).split(" ")), 10)
          ),
        { name: "async-split" }
      )
    );

    const results = await p.execute(["foo bar baz"], context);
    expect((results as string[]).sort()).toEqual(["bar", "baz", "foo"]);
  });
});

// ─── Multiple inputs ────────────────────────────────────────────────────────

describe("multiple inputs", () => {
  it("processes each input independently", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("square");
    p.addTask(createTask((x) => (x as number) * (x as number), { name: "square" }));

    const results = await p.execute([2, 3, 4], context);
    expect((results as number[]).sort((a, b) => a - b)).toEqual([4, 9, 16]);
  });
});

// ─── Task chains ────────────────────────────────────────────────────────────

describe("task chains", () => {
  it("chain two sync tasks: (x+1)*3", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("chain");
    p.addTask(createTask((x) => (x as number) + 1, { name: "add_one" }));
    p.addTask(createTask((x) => (x as number) * 3, { name: "times_three" }));

    const results = await p.execute([10], context);
    expect(results).toEqual([33]);
  });

  it("chain sync then async", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("sync-async");
    p.addTask(createTask((x) => (x as number) + 1, { name: "add" }));
    p.addTask(
      createTask(
        (x) =>
          new Promise<CogneeValue>((resolve) =>
            setTimeout(() => resolve((x as number) * 2), 5)
          ),
        { name: "mul" }
      )
    );

    // (3 + 1) * 2 = 8
    const results = await p.execute([3], context);
    expect(results).toEqual([8]);
  });

  it("chain async then sync", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("async-sync");
    p.addTask(
      createTask(
        (x) => Promise.resolve((x as number) + 10),
        { name: "add10" }
      )
    );
    p.addTask(createTask((x) => (x as number) * 2, { name: "double" }));

    // (5 + 10) * 2 = 30
    const results = await p.execute([5], context);
    expect(results).toEqual([30]);
  });

  it("chain three tasks", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("three-step");
    p.addTask(createTask((x) => (x as number) + 1, { name: "step1" }));
    p.addTask(createTask((x) => (x as number) * 2, { name: "step2" }));
    p.addTask(createTask((x) => (x as number) - 3, { name: "step3" }));

    // ((10 + 1) * 2) - 3 = 19
    const results = await p.execute([10], context);
    expect(results).toEqual([19]);
  });
});

// ─── Iter + downstream combinations ────────────────────────────────────────

describe("iter task combinations", () => {
  it("iter then sync: split words then uppercase", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("split-upper");
    p.addTask(
      createIterTask((text) => (text as string).split(" "), { name: "split" })
    );
    p.addTask(
      createTask((w) => (w as string).toUpperCase(), { name: "upper" })
    );

    const results = await p.execute(["hello world"], context);
    expect((results as string[]).sort()).toEqual(["HELLO", "WORLD"]);
  });

  it("iter then async: split words then async transform", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("split-async");
    p.addTask(
      createIterTask((text) => (text as string).split(" "), { name: "split" })
    );
    p.addTask(
      createTask(
        (w) => Promise.resolve(`[${w}]`),
        { name: "bracket" }
      )
    );

    const results = await p.execute(["a b c"], context);
    expect((results as string[]).sort()).toEqual(["[a]", "[b]", "[c]"]);
  });

  it("sync then iter: transform then fan-out", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("transform-split");
    p.addTask(
      createTask((s) => (s as string).toUpperCase(), { name: "upper" })
    );
    p.addTask(
      createIterTask((text) => (text as string).split(" "), { name: "split" })
    );

    const results = await p.execute(["hello world"], context);
    expect((results as string[]).sort()).toEqual(["HELLO", "WORLD"]);
  });

  it("iter with multiple inputs", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("multi-split");
    p.addTask(
      createIterTask((text) => (text as string).split(","), { name: "split" })
    );

    const results = await p.execute(["a,b", "c,d"], context);
    expect((results as string[]).sort()).toEqual(["a", "b", "c", "d"]);
  });
});

// ─── Batch task combinations ────────────────────────────────────────────────
//
// Batch tasks can only appear after an iter/stream task that produces items
// to accumulate — the pipeline executor dispatches via call_batch() only
// when items have been collected from an iterator or stream.

describe("batch task combinations", () => {
  it("iter then batch: split then join", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("iter-batch");
    p.addTask(
      createIterTask((text) => (text as string).split(" "), { name: "split" })
    );
    p.addTask(
      createBatchTask(
        (words) => (words as string[]).join("-"),
        { name: "join", batchSize: 100 }
      )
    );

    const results = await p.execute(["hello world"], context);
    expect(results).toEqual(["hello-world"]);
  });

  it("iter then batch: fan-out numbers then sum", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("fan-sum");
    // First, fan-out a string of numbers into individual numbers.
    p.addTask(
      createIterTask(
        (text) => (text as string).split(",").map(Number),
        { name: "split-nums" }
      )
    );
    // Then batch-sum them.
    p.addTask(
      createBatchTask(
        (items) => (items as number[]).reduce((a, b) => a + b, 0),
        { name: "sum", batchSize: 100 }
      )
    );

    const results = await p.execute(["1,2,3,4"], context);
    expect(results).toEqual([10]);
  });

  it("sync then iter then batch: transform, fan-out, aggregate", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("transform-split-batch");
    p.addTask(
      createTask((x) => (x as number) * 10, { name: "x10" })
    );
    p.addTask(
      createIterTask(
        (x) => [x, (x as number) + 1, (x as number) + 2],
        { name: "expand" }
      )
    );
    p.addTask(
      createBatchTask(
        (items) => (items as number[]).reduce((a, b) => a + b, 0),
        { name: "sum", batchSize: 100 }
      )
    );

    // Input 1: 1*10=10, expand to [10,11,12], sum=33
    const results = await p.execute([1], context);
    expect(results).toEqual([33]);
  });

  it("async batch task after iter", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("async-batch");
    p.addTask(
      createIterTask(
        (text) => (text as string).split(" "),
        { name: "split" }
      )
    );
    p.addTask(
      createBatchTask(
        (items) =>
          new Promise<CogneeValue>((resolve) =>
            setTimeout(
              () => resolve((items as string[]).join("+")),
              5
            )
          ),
        { name: "async-join", batchSize: 100 }
      )
    );

    const results = await p.execute(["a b c"], context);
    expect(results).toEqual(["a+b+c"]);
  });
});

// ─── Pipeline configuration ─────────────────────────────────────────────────

describe("pipeline configuration", () => {
  it("empty pipeline throws NoTasks error", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("empty");

    await expect(p.execute([1], context)).rejects.toThrow();
  });

  it("pipeline with concurrency > 1", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("concurrent");
    p.setConcurrency(4);
    p.addTask(createTask((x) => (x as number) + 1, { name: "inc" }));

    const results = await p.execute([1, 2, 3, 4, 5], context);
    expect((results as number[]).sort((a, b) => a - b)).toEqual([2, 3, 4, 5, 6]);
  });

  it("executeAsync works like execute", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("async-exec");
    p.addTask(createTask((x) => (x as number) * 3, { name: "triple" }));

    const results = await p.executeAsync([4], context);
    expect(results).toEqual([12]);
  });
});

// ─── Value types ────────────────────────────────────────────────────────────

describe("value types", () => {
  it("boolean values pass through", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("bool");
    p.addTask(createTask((x) => !(x as boolean), { name: "negate" }));

    const results = await p.execute([true, false], context);
    expect((results as boolean[]).sort()).toEqual([false, true]);
  });

  it("mixed types across pipeline stages", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("num-to-str");
    p.addTask(
      createTask((x) => `value:${x}`, { name: "stringify" })
    );

    const results = await p.execute([42], context);
    expect(results).toEqual(["value:42"]);
  });

  it("number precision preserved", async () => {
    const { context } = mockCtx();
    const p = new Pipeline("precision");
    p.addTask(createTask((x) => (x as number) + 0.1, { name: "add01" }));

    const results = await p.execute([0.2], context);
    expect(results[0]).toBeCloseTo(0.3, 10);
  });
});
