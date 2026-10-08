import { native, NativeBox } from "./native";
import { CogneeValue } from "./value";
import { TaskContext } from "./task-context";

/** One value in → one value out. */
export type TaskFn = (
  input: CogneeValue,
  ctx: TaskContext
) => CogneeValue | Promise<CogneeValue>;

/**
 * What a stream task may produce: an array, any (sync) iterable such as a
 * generator, or an async iterable such as an `async function*`. Arrays and
 * sync iterables may also be returned through a Promise.
 */
export type StreamOutput =
  | CogneeValue[]
  | Iterable<CogneeValue>
  | AsyncIterable<CogneeValue>
  | Promise<CogneeValue[] | Iterable<CogneeValue>>;

/** One value in → many values out, each fanned out to the next task. */
export type IterTaskFn = (input: CogneeValue, ctx: TaskContext) => StreamOutput;

/** A batch of values in → one value out. */
export type BatchTaskFn = (
  inputs: CogneeValue[],
  ctx: TaskContext
) => CogneeValue | Promise<CogneeValue>;

/** A batch of values in → many values out. */
export type IterBatchTaskFn = (inputs: CogneeValue[], ctx: TaskContext) => StreamOutput;

export interface TaskOptions {
  name?: string;
  batchSize?: number;
  weight?: number;
  summaryTemplate?: string;
}

/** Opaque handle wrapping a cognee-core TaskInfo (task + metadata). */
export class TaskInfo {
  /** @internal */
  readonly _box: NativeBox;

  /** @internal */
  constructor(box_: NativeBox) {
    this._box = box_;
  }
}

function describe(value: unknown): string {
  if (value === null) return "null";
  if (Array.isArray(value)) return "array";
  return typeof value === "object" ? (value.constructor?.name ?? "object") : typeof value;
}

/**
 * Normalise whatever a stream callback produces into one async iterator, which
 * is the only shape the native side pulls from. The callback runs lazily on
 * the first `next()`, so a synchronous throw becomes a rejection like any
 * other failure.
 */
function toAsyncIterator(produce: () => StreamOutput): AsyncIterator<CogneeValue> {
  return (async function* () {
    const out: unknown = await produce();
    if (out !== null && typeof out === "object") {
      if (typeof (out as AsyncIterable<CogneeValue>)[Symbol.asyncIterator] === "function") {
        yield* out as AsyncIterable<CogneeValue>;
        return;
      }
      // Buffers and typed arrays are iterable (over bytes) but are values,
      // not collections of values.
      if (
        !ArrayBuffer.isView(out) &&
        typeof (out as Iterable<CogneeValue>)[Symbol.iterator] === "function"
      ) {
        yield* out as Iterable<CogneeValue>;
        return;
      }
    }
    throw new TypeError(
      `stream task must return an array, an iterable or an async iterable, got ${describe(out)}`
    );
  })();
}

function info(nativeTask: NativeBox, options?: TaskOptions): TaskInfo {
  return new TaskInfo(native.taskInfoNew(nativeTask, options));
}

/** Create a single-value task from a JS function (sync or Promise-returning). */
export function createTask(fn: TaskFn, options?: TaskOptions): TaskInfo {
  return info(
    native.createTask((input: CogneeValue, ctx: NativeBox) => fn(input, new TaskContext(ctx))),
    options
  );
}

/**
 * Create a fan-out task: each value it produces is passed to the next task
 * individually. Accepts arrays, iterables, generators and async generators;
 * iterables are pulled lazily, one item per downstream request, and a throw
 * or rejection at any point fails the task.
 */
export function createIterTask(fn: IterTaskFn, options?: TaskOptions): TaskInfo {
  return info(
    native.createIterTask((input: CogneeValue, ctx: NativeBox) =>
      toAsyncIterator(() => fn(input, new TaskContext(ctx)))
    ),
    options
  );
}

/** Create a batch task: it receives up to `batchSize` accumulated values at once. */
export function createBatchTask(fn: BatchTaskFn, options?: TaskOptions): TaskInfo {
  return info(
    native.createBatchTask((inputs: CogneeValue[], ctx: NativeBox) =>
      fn(inputs, new TaskContext(ctx))
    ),
    options
  );
}

/** Create a batch task that produces many values (batch in, fan-out out). */
export function createIterBatchTask(fn: IterBatchTaskFn, options?: TaskOptions): TaskInfo {
  return info(
    native.createIterBatchTask((inputs: CogneeValue[], ctx: NativeBox) =>
      toAsyncIterator(() => fn(inputs, new TaskContext(ctx)))
    ),
    options
  );
}

// ── Async-named creators ────────────────────────────────────────────────────
//
// Naming parity with the C API (`cg_task_async`, `cg_task_async_batch`,
// `cg_task_async_stream`, `cg_task_async_stream_batch`). Every JS task is
// already async under the hood — the callback runs on the Node event loop and
// its result (or the Promise it returns) is awaited off-thread — so these are
// the very same functions under a second name.

/** Same function as {@link createTask}. */
export const createAsyncTask = createTask;

/** Same function as {@link createBatchTask}. */
export const createAsyncBatchTask = createBatchTask;

/** Same function as {@link createIterTask}; pass an `async function*` to stream. */
export const createAsyncStreamTask = createIterTask;

/** Same function as {@link createIterBatchTask}. */
export const createAsyncStreamBatchTask = createIterBatchTask;
