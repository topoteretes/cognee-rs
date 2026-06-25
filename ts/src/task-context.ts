import { native, NativeBox } from "./native";
import { CancellationHandle } from "./cancellation";

/** Opaque handle to a cognee-core TaskContext (thread pool, databases, cancellation, progress). */
export class TaskContext {
  /** @internal */
  readonly _box: NativeBox;

  /** @internal */
  constructor(box_: NativeBox) {
    this._box = box_;
  }

  /** Create a mock context with in-memory backends (for testing). */
  static mock(): { handle: CancellationHandle; context: TaskContext } {
    const result = native.taskContextMock();
    return {
      handle: new CancellationHandle(result.handle),
      context: new TaskContext(result.context),
    };
  }

  /** Cheap Arc-bump clone. */
  clone(): TaskContext {
    return new TaskContext(native.taskContextClone(this._box));
  }
}
