import {
  init,
  pipeline,
  Pipeline,
  TaskContext,
  CogneeValue,
  createTask,
  createIterTask,
  createBatchTask,
  createIterBatchTask,
  createAsyncTask,
  createAsyncBatchTask,
  createAsyncStreamTask,
  createAsyncStreamBatchTask,
} from "../src";

beforeAll(() => {
  init();
});

function mockCtx() {
  return TaskContext.mock();
}

function delayed<T>(value: T): Promise<T> {
  return new Promise((resolve) => setTimeout(() => resolve(value), 5));
}

async function run(tasks: ReturnType<typeof createTask>[], inputs: CogneeValue[]) {
  const { context } = mockCtx();
  const p = new Pipeline("task-creators");
  for (const t of tasks) p.addTask(t);
  return p.execute(inputs, context);
}

// ─── Async-named aliases ────────────────────────────────────────────────────

describe("async-named creators", () => {
  it("are the same functions as the unprefixed creators", () => {
    expect(createAsyncTask).toBe(createTask);
    expect(createAsyncBatchTask).toBe(createBatchTask);
    expect(createAsyncStreamTask).toBe(createIterTask);
    expect(createAsyncStreamBatchTask).toBe(createIterBatchTask);
  });

  it("are also exported under the pipeline namespace", () => {
    expect(pipeline.createAsyncTask).toBe(createAsyncTask);
    expect(pipeline.createAsyncBatchTask).toBe(createAsyncBatchTask);
    expect(pipeline.createAsyncStreamTask).toBe(createAsyncStreamTask);
    expect(pipeline.createAsyncStreamBatchTask).toBe(createAsyncStreamBatchTask);
    expect(pipeline.createIterBatchTask).toBe(createIterBatchTask);
  });

  it("createAsyncTask awaits a Promise-returning callback", async () => {
    const results = await run(
      [createAsyncTask((x) => delayed((x as number) * 2), { name: "double" })],
      [7]
    );
    expect(results).toEqual([14]);
  });
});

// ─── Streaming ──────────────────────────────────────────────────────────────

describe("stream tasks", () => {
  it("accept an async generator", async () => {
    const results = await run(
      [
        createAsyncStreamTask(async function* (text) {
          for (const n of (text as string).split(",")) {
            yield delayed(Number(n));
          }
        }),
        createAsyncTask((x) => delayed((x as number) * 2)),
      ],
      ["1,2,3"]
    );
    expect((results as number[]).sort()).toEqual([2, 4, 6]);
  });

  it("accept a sync generator", async () => {
    const results = await run(
      [
        createIterTask(function* (x) {
          yield x;
          yield (x as number) + 1;
        }),
      ],
      [10]
    );
    expect(results).toEqual([10, 11]);
  });

  it("accept a Promise of an array", async () => {
    const results = await run(
      [createIterTask((text) => delayed((text as string).split(" ")))],
      ["a b"]
    );
    expect(results).toEqual(["a", "b"]);
  });

  it("are pulled lazily, one item per downstream request", async () => {
    const events: string[] = [];
    await run(
      [
        createAsyncStreamTask(async function* () {
          for (const n of [1, 2, 3]) {
            events.push(`yield ${n}`);
            yield n;
          }
        }),
        // batchSize 1 so each item is dispatched as soon as it is pulled.
        createAsyncTask(
          (x) => {
            events.push(`consume ${x}`);
            return x;
          },
          { batchSize: 1 }
        ),
      ],
      [0]
    );
    expect(events).toEqual([
      "yield 1",
      "consume 1",
      "yield 2",
      "consume 2",
      "yield 3",
      "consume 3",
    ]);
  });

  it("feed a batch task", async () => {
    const results = await run(
      [
        createAsyncStreamTask(async function* () {
          yield 1;
          yield 2;
          yield 3;
        }),
        createAsyncBatchTask((items) => delayed((items as number[]).reduce((a, b) => a + b, 0)), {
          batchSize: 100,
        }),
      ],
      [0]
    );
    expect(results).toEqual([6]);
  });

  it("createAsyncStreamBatchTask: batch in, stream out", async () => {
    const results = await run(
      [
        createIterTask(() => [1, 2, 3]),
        createAsyncStreamBatchTask(
          async function* (items) {
            for (const i of items as number[]) yield i * 10;
          },
          { batchSize: 100 }
        ),
      ],
      [0]
    );
    expect(results).toEqual([10, 20, 30]);
  });
});

// ─── Failures surface as errors ─────────────────────────────────────────────

describe("callback failures", () => {
  // A sync throw used to be left pending on the Neon context, where it could
  // escape as an uncaught exception on top of failing the task.
  let uncaught: unknown[];
  const onUncaught = (e: unknown) => uncaught.push(e);
  beforeEach(() => {
    uncaught = [];
    process.on("uncaughtException", onUncaught);
  });
  afterEach(() => {
    process.off("uncaughtException", onUncaught);
    expect(uncaught).toEqual([]);
  });

  it("a synchronous throw fails the run with its message", async () => {
    await expect(
      run(
        [
          createTask(() => {
            throw new Error("sync boom");
          }),
        ],
        [1]
      )
    ).rejects.toThrow(/sync boom/);
  });

  it("a rejected Error keeps its message and name", async () => {
    await expect(
      run(
        [
          createAsyncTask(async () => {
            throw new TypeError("bad input");
          }),
        ],
        [1]
      )
    ).rejects.toThrow(/TypeError: bad input/);
  });

  it("a non-Error rejection is stringified", async () => {
    await expect(run([createAsyncTask(() => Promise.reject(42))], [1])).rejects.toThrow(/42/);
  });

  it("a stream that throws mid-way fails the run instead of ending early", async () => {
    await expect(
      run(
        [
          createAsyncStreamTask(async function* () {
            yield 1;
            throw new Error("mid-stream boom");
          }),
          createTask((x) => x),
        ],
        [0]
      )
    ).rejects.toThrow(/task 0 failed.*mid-stream boom/);
  });

  it("a stream callback that throws synchronously fails the run", async () => {
    await expect(
      run(
        [
          createIterTask(() => {
            throw new Error("no stream for you");
          }),
        ],
        [0]
      )
    ).rejects.toThrow(/no stream for you/);
  });

  it("a stream callback returning a non-iterable fails the run", async () => {
    await expect(
      run([createIterTask(() => 5 as unknown as number[])], [0])
    ).rejects.toThrow(/must return an array, an iterable or an async iterable, got number/);
  });

  it("a batch callback that rejects fails the run", async () => {
    await expect(
      run(
        [
          createIterTask(() => [1, 2]),
          createBatchTask(async () => {
            throw new Error("batch boom");
          }),
        ],
        [0]
      )
    ).rejects.toThrow(/batch boom/);
  });
});

// ─── Context and values ─────────────────────────────────────────────────────

describe("callback arguments", () => {
  it("every task kind receives a TaskContext", async () => {
    const seen: string[] = [];
    const record = (kind: string, ctx: TaskContext) => {
      if (ctx instanceof TaskContext) seen.push(kind);
    };
    await run(
      [
        createTask((x, ctx) => {
          record("single", ctx);
          return x;
        }),
        createIterTask((x, ctx) => {
          record("iter", ctx);
          return [x];
        }),
        createIterBatchTask(
          (items, ctx) => {
            record("iter-batch", ctx);
            return items;
          },
          { batchSize: 100 }
        ),
        createBatchTask(
          (items, ctx) => {
            record("batch", ctx);
            return items.length;
          },
          { batchSize: 100 }
        ),
      ],
      [1]
    );
    expect(seen).toEqual(["single", "iter", "iter-batch", "batch"]);
  });

  it("object items reach a batch callback intact", async () => {
    const results = await run(
      [
        createIterTask(() => [{ id: 1 }, { id: 2 }]),
        createBatchTask((items) => (items as { id: number }[]).map((i) => i.id), {
          batchSize: 100,
        }),
      ],
      [0]
    );
    expect(results).toEqual([[1, 2]]);
  });

  it("objects keep their identity through a pipeline", async () => {
    const original = { tag: "same" };
    const results = await run([createTask((x) => x)], [original]);
    expect(results[0]).toBe(original);
  });
});
