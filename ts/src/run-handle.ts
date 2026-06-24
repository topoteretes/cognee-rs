import { native, NativeBox } from "./native";
import { CogneeValue } from "./value";

/** Handle to a pipeline run executing in the background. */
export class RunHandle {
  /** @internal */
  readonly _box: NativeBox;

  /** @internal */
  constructor(box_: NativeBox) {
    this._box = box_;
  }

  /** Whether the background run has completed (success or failure). */
  get isFinished(): boolean {
    return native.runHandleIsFinished(this._box);
  }

  /** Request cancellation of the background run. */
  abort(): void {
    native.runHandleAbort(this._box);
  }

  /** Wait for completion. Returns the output values. Consumes the handle. */
  async wait(): Promise<CogneeValue[]> {
    return native.runHandleWait(this._box) as Promise<CogneeValue[]>;
  }
}
