// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "CogneeSDK",
    platforms: [
        .iOS(.v13),
    ],
    products: [
        .library(name: "CogneeSDK", targets: ["CogneeSDK"]),
    ],
    targets: [
        // Pre-built xcframework containing the cognee-capi static library +
        // headers for both iOS device (arm64) and simulator (arm64-sim).
        .binaryTarget(
            name: "CogneeSDKCore",
            path: "../capi/CogneeSDK.xcframework"
        ),
        // Swift wrapper that imports CogneeSDKCore and exposes async/await API.
        .target(
            name: "CogneeSDK",
            dependencies: ["CogneeSDKCore"],
            path: "Sources/CogneeSDK"
        ),
    ]
)
