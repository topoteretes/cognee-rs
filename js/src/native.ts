/** Opaque native handle types returned by the Neon addon. */
export type NativeBox = object;

/** Shape of the native Neon module. */
export interface NativeBindings {
  // Runtime
  init(): void;
  initWithThreads(n: number): void;
  shutdown(): void;

  // Values
  valueFromNumber(n: number): NativeBox;
  valueFromBool(b: boolean): NativeBox;
  valueFromString(s: string): NativeBox;
  valueFromBuffer(buf: Buffer): NativeBox;
  valueAsNumber(val: NativeBox): number;
  valueAsBool(val: NativeBox): boolean;
  valueAsString(val: NativeBox): string;
  valueAsBuffer(val: NativeBox): Buffer;
  valueClone(val: NativeBox): NativeBox;

  // Tasks
  createTask(fn: Function): NativeBox;
  createIterTask(fn: Function): NativeBox;
  createBatchTask(fn: Function): NativeBox;

  // TaskInfo
  taskInfoNew(
    task: NativeBox,
    options?: { name?: string; batchSize?: number; weight?: number; summaryTemplate?: string }
  ): NativeBox;

  // Pipeline
  pipelineNew(description?: string): NativeBox;
  pipelineSetName(pipeline: NativeBox, name: string): void;
  pipelineAddTask(pipeline: NativeBox, taskInfo: NativeBox): void;
  pipelineSetBatchSize(pipeline: NativeBox, size: number): void;
  pipelineSetConcurrency(pipeline: NativeBox, n: number): void;
  pipelineSetRetry(pipeline: NativeBox, policy: object): void;

  // Pipeline execution
  pipelineExecute(pipeline: NativeBox, inputs: unknown[], ctx: NativeBox): Promise<unknown[]>;
  pipelineExecuteAsync(pipeline: NativeBox, inputs: unknown[], ctx: NativeBox): Promise<unknown[]>;
  pipelineExecuteBackground(pipeline: NativeBox, inputs: unknown[], ctx: NativeBox): NativeBox;
  pipelineExecuteWithWatcher(
    pipeline: NativeBox,
    inputs: unknown[],
    ctx: NativeBox,
    watcher: NativeBox
  ): Promise<unknown[]>;

  // Run handle
  runHandleIsFinished(handle: NativeBox): boolean;
  runHandleAbort(handle: NativeBox): void;
  runHandleWait(handle: NativeBox): Promise<unknown[]>;

  // Task context
  taskContextMock(): { handle: NativeBox; context: NativeBox };
  taskContextClone(ctx: NativeBox): NativeBox;

  // Cancellation
  cancellationPair(): { handle: NativeBox; token: NativeBox };
  cancellationHandleCancel(handle: NativeBox): void;
  cancellationHandleIsCancelled(handle: NativeBox): boolean;
  cancellationTokenIsCancelled(token: NativeBox): boolean;
  cancellationHandleClone(handle: NativeBox): NativeBox;
  cancellationTokenClone(token: NativeBox): NativeBox;

  // Progress
  progressNew(): NativeBox;
  progressSet(token: NativeBox, fraction: number): void;
  progressFraction(token: NativeBox): number;
  progressWidth(token: NativeBox): number;
  progressIsComplete(token: NativeBox): boolean;
  progressRootFraction(token: NativeBox): number;
  progressSplit(token: NativeBox, weights: number[]): NativeBox[];
  progressSubtoken(token: NativeBox, fracWidth: number): NativeBox;
  progressClone(token: NativeBox): NativeBox;

  // Watcher
  watcherNew(obj: object): NativeBox;
  watcherNoop(): NativeBox;
}

// eslint-disable-next-line @typescript-eslint/no-var-requires
export const native: NativeBindings = require("../cognee_neon.node");
