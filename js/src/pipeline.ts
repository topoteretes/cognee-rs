import { native, NativeBox } from "./native";
import { CogneeValue } from "./value";
import { TaskInfo } from "./task";
import { TaskContext } from "./task-context";
import { Watcher } from "./watcher";
import { RunHandle } from "./run-handle";

export type RetryPolicy =
  | { type: "none" }
  | {
      type: "limited";
      maxAttempts: number;
      delay:
        | { type: "constant"; ms: number }
        | { type: "exponential"; baseMs: number; factor?: number };
    };

/** Builder for a cognee-core Pipeline. */
export class Pipeline {
  /** @internal */
  readonly _box: NativeBox;

  constructor(description: string = "") {
    this._box = native.pipelineNew(description);
  }

  setName(name: string): this {
    native.pipelineSetName(this._box, name);
    return this;
  }

  addTask(task: TaskInfo): this {
    native.pipelineAddTask(this._box, task._box);
    return this;
  }

  setBatchSize(size: number): this {
    native.pipelineSetBatchSize(this._box, size);
    return this;
  }

  setConcurrency(n: number): this {
    native.pipelineSetConcurrency(this._box, n);
    return this;
  }

  setRetry(policy: RetryPolicy): this {
    native.pipelineSetRetry(this._box, policy);
    return this;
  }

  /**
   * Execute the pipeline synchronously (blocking worker thread).
   * Does NOT require `init()` — creates its own single-threaded tokio runtime.
   */
  async execute(inputs: CogneeValue[], ctx: TaskContext): Promise<CogneeValue[]> {
    return native.pipelineExecute(this._box, inputs, ctx._box) as Promise<
      CogneeValue[]
    >;
  }

  /**
   * Execute the pipeline asynchronously on the global tokio runtime.
   * Requires `init()` to have been called.
   */
  async executeAsync(
    inputs: CogneeValue[],
    ctx: TaskContext
  ): Promise<CogneeValue[]> {
    return native.pipelineExecuteAsync(
      this._box,
      inputs,
      ctx._box
    ) as Promise<CogneeValue[]>;
  }

  /**
   * Execute the pipeline in the background. Returns a handle immediately.
   * Requires `init()` to have been called.
   */
  executeInBackground(inputs: CogneeValue[], ctx: TaskContext): RunHandle {
    const handle = native.pipelineExecuteBackground(
      this._box,
      inputs,
      ctx._box
    );
    return new RunHandle(handle);
  }

  /**
   * Execute with a watcher for lifecycle event callbacks.
   * Requires `init()` to have been called.
   */
  async executeWithWatcher(
    inputs: CogneeValue[],
    ctx: TaskContext,
    watcher: Watcher
  ): Promise<CogneeValue[]> {
    return native.pipelineExecuteWithWatcher(
      this._box,
      inputs,
      ctx._box,
      watcher._box
    ) as Promise<CogneeValue[]>;
  }
}
