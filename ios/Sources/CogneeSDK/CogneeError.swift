// Sources/CogneeSDK/CogneeError.swift
import CogneeSDKCore

/// An error returned by the cognee C API.
///
/// `code` is the raw `CgErrorCode` integer value from cognee_sdk.h:
///   0  = CG_OK                     (never thrown — means success)
///   1  = CG_ERR_NULL_POINTER
///   3  = CG_ERR_RUNTIME
///  10  = CG_ERR_UTF8
///  11  = CG_ERR_COMPONENT
///  12  = CG_ERR_SERVICE_BUILD
///  13  = CG_ERR_USER_BOOTSTRAP
///  14  = CG_ERR_SDK_VALIDATION
///  15  = CG_ERR_UNSUPPORTED
///  16  = CG_ERR_FEATURE_NOT_BUILT
///  17  = CG_ERR_UNKNOWN_CONFIG_KEY
///  18  = CG_ERR_CONFIG_TYPE_MISMATCH
public struct CogneeError: Error, CustomStringConvertible {

    /// Raw integer value of the `CgErrorCode` that caused this error.
    public let code: Int32

    /// Human-readable message forwarded directly from the C callback's
    /// `error_message` parameter (valid only inside the callback; the Swift
    /// wrapper copies it here before the callback returns).
    public let message: String

    public var description: String {
        "CogneeError(code: \(code), message: \"\(message)\")"
    }

    /// Internal init used by callback closures (message from C pointer).
    init(code: CgErrorCode, message: UnsafePointer<CChar>?) {
        self.code = Int32(bitPattern: code.rawValue)
        self.message = message.map { String(cString: $0) }
            ?? "Unknown error (CgErrorCode rawValue: \(code.rawValue))"
    }

    /// Convenience init used when the message is already a Swift String.
    init(code: CgErrorCode, message: String) {
        self.code = Int32(bitPattern: code.rawValue)
        self.message = message
    }
}
