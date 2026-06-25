import { native, NativeBox } from "./native";

/** Callback interface for pipeline lifecycle events. All methods are optional. */
export interface WatcherEvents {
  onPipelineRunStarted?(runId: string, name: string): void;
  onPipelineRunCompleted?(runId: string, outputCount: number): void;
  onPipelineRunErrored?(runId: string, error: string): void;
  onTaskStarted?(runId: string, taskName: string, taskIndex: number): void;
  onTaskCompleted?(runId: string, taskName: string, resultCount: number): void;
  onTaskErrored?(runId: string, taskName: string, error: string): void;
}

/** Opaque watcher handle. */
export class Watcher {
  /** @internal */
  readonly _box: NativeBox;

  /** @internal */
  constructor(box_: NativeBox) {
    this._box = box_;
  }
}

/** Create a watcher that forwards events to the given callbacks. */
export function createWatcher(events: WatcherEvents): Watcher {
  return new Watcher(native.watcherNew(events));
}

/** Create a no-op watcher (discards all events). */
export function createNoopWatcher(): Watcher {
  return new Watcher(native.watcherNoop());
}
