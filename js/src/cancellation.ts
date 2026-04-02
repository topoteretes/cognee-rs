import { native, NativeBox } from "./native";

export class CancellationHandle {
  /** @internal */
  readonly _box: NativeBox;

  /** @internal */
  constructor(box_: NativeBox) {
    this._box = box_;
  }

  cancel(): void {
    native.cancellationHandleCancel(this._box);
  }

  get isCancelled(): boolean {
    return native.cancellationHandleIsCancelled(this._box);
  }

  clone(): CancellationHandle {
    return new CancellationHandle(native.cancellationHandleClone(this._box));
  }
}

export class CancellationToken {
  /** @internal */
  readonly _box: NativeBox;

  /** @internal */
  constructor(box_: NativeBox) {
    this._box = box_;
  }

  get isCancelled(): boolean {
    return native.cancellationTokenIsCancelled(this._box);
  }

  clone(): CancellationToken {
    return new CancellationToken(native.cancellationTokenClone(this._box));
  }
}

/** Create a linked (handle, token) cancellation pair. */
export function createCancellationPair(): {
  handle: CancellationHandle;
  token: CancellationToken;
} {
  const result = native.cancellationPair();
  return {
    handle: new CancellationHandle(result.handle),
    token: new CancellationToken(result.token),
  };
}
