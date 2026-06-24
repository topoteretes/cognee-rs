import { native, NativeBox } from "./native";

export class ProgressToken {
  /** @internal */
  readonly _box: NativeBox;

  /** @internal */
  constructor(box_: NativeBox) {
    this._box = box_;
  }

  /** Create a root progress token at 0%. */
  static create(): ProgressToken {
    return new ProgressToken(native.progressNew());
  }

  /** Set this token's progress fraction (clamped to [0, 1]). */
  set(fraction: number): void {
    native.progressSet(this._box, fraction);
  }

  /** This token's progress fraction in [0, 1]. */
  get fraction(): number {
    return native.progressFraction(this._box);
  }

  /** This token's width as a fraction of the root [0, 1] range. */
  get width(): number {
    return native.progressWidth(this._box);
  }

  /** Whether this token's progress >= 1.0. */
  get isComplete(): boolean {
    return native.progressIsComplete(this._box);
  }

  /** Overall progress across the entire tree. */
  get rootFraction(): number {
    return native.progressRootFraction(this._box);
  }

  /** Split into subtokens by relative weights. */
  split(weights: number[]): ProgressToken[] {
    const boxes = native.progressSplit(this._box, weights);
    return boxes.map((b) => new ProgressToken(b));
  }

  /** Create one child subtoken covering `fracWidth` of this token's range. */
  subtoken(fracWidth: number): ProgressToken {
    return new ProgressToken(native.progressSubtoken(this._box, fracWidth));
  }

  clone(): ProgressToken {
    return new ProgressToken(native.progressClone(this._box));
  }
}
