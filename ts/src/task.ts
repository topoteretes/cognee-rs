import { native, NativeBox } from "./native";
import { CogneeValue } from "./value";
import { TaskContext } from "./task-context";

export type TaskFn = (
  input: CogneeValue,
  ctx?: TaskContext
) => CogneeValue | Promise<CogneeValue>;

export type IterTaskFn = (
  input: CogneeValue,
  ctx?: TaskContext
) => CogneeValue[] | Promise<CogneeValue[]>;

export type BatchTaskFn = (
  inputs: CogneeValue[],
  ctx?: TaskContext
) => CogneeValue | Promise<CogneeValue>;

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

/** Create a single-value async task from a JS function. */
export function createTask(fn: TaskFn, options?: TaskOptions): TaskInfo {
  const nativeTask = native.createTask(fn as Function);
  const nativeInfo = native.taskInfoNew(nativeTask, options);
  return new TaskInfo(nativeInfo);
}

/** Create an iterator task from a JS function that returns an array. */
export function createIterTask(fn: IterTaskFn, options?: TaskOptions): TaskInfo {
  const nativeTask = native.createIterTask(fn as Function);
  const nativeInfo = native.taskInfoNew(nativeTask, options);
  return new TaskInfo(nativeInfo);
}

/** Create a batch task from a JS function that receives an array. */
export function createBatchTask(fn: BatchTaskFn, options?: TaskOptions): TaskInfo {
  const nativeTask = native.createBatchTask(fn as Function);
  const nativeInfo = native.taskInfoNew(nativeTask, options);
  return new TaskInfo(nativeInfo);
}
