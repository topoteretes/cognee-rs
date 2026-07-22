// Sources/CogneeSDK/ContinuationBox.swift

/// Wraps a Swift `CheckedContinuation` so it can cross a C `void*` boundary.
///
/// ## Why a class?
/// A class (reference type) has a stable memory address that does not change
/// when the object is passed around.  We round-trip this address through
/// `Unmanaged.passRetained(_:).toOpaque()` → `void* user_data` → back to
/// Swift via `Unmanaged.fromOpaque(_:).takeRetainedValue()` in the C callback.
///
/// ## Why Unmanaged?
/// Swift's ARC would normally release the box when it drops out of scope
/// (right after the `cg_sdk_*` call returns — before the callback fires).
/// `passRetained` increments the retain count by 1, keeping the box alive.
/// `takeRetainedValue` decrements it again when the callback fires, balancing
/// the extra retain exactly once.  The pattern is correct for single-use
/// callbacks (R1 rule: every `cg_sdk_*` callback fires exactly once).
final class ContinuationBox<T: Sendable>: @unchecked Sendable {
    let continuation: CheckedContinuation<T, Error>

    init(_ continuation: CheckedContinuation<T, Error>) {
        self.continuation = continuation
    }
}
