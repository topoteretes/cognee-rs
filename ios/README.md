# CogneeSDK — iOS Swift Package

Swift async/await wrapper for [cognee-rs](https://github.com/topoteretes/cognee-rs), the Rust AI memory SDK. Exposes the full `add → cognify → search` pipeline to iOS apps via a hand-written Swift/C bridge over `cognee_sdk.h`.

## Requirements

- Xcode 15 or later
- iOS 13+ deployment target
- `CogneeSDK.xcframework` built locally (see below — the binary is not committed to git)

## Building the xcframework

The xcframework contains pre-compiled static libraries for both iOS device (`aarch64-apple-ios`) and Simulator (`aarch64-apple-ios-sim`). It must be built once before the Swift package resolves.

```bash
# From the repo root
./capi/scripts/build_xcframework.sh
```

This takes 20–30 minutes on first run. The output is `capi/CogneeSDK.xcframework`. After the build you can delete `capi/target/` to recover ~15 GB of disk space — the compiled code is already embedded in the xcframework.

> **Why is the xcframework not in git?** Each slice is ~3.3 GB. Committing 6.6 GB of binary to git history would make the repo impractical to clone and would rot on every Rust update. The build script is the source of truth.

## Adding to your project

In `Package.swift`, add a local dependency on the `ios/` directory:

```swift
.package(path: "/path/to/cognee-rs/ios")
```

Or copy `ios/` into your own repo and adjust the `binaryTarget` path in `Package.swift` to point at your local `CogneeSDK.xcframework`.

## Usage

```swift
import CogneeSDK

let cognee = try Cognee()
try await cognee.warm()

let _ = try await cognee.add(
    inputsJSON: #"[{"type":"text","text":"Alan Turing was a mathematician..."}]"#,
    dataset: "research"
)
let _ = try await cognee.cognify(dataset: "research")
let results = try await cognee.search(query: "Who was Alan Turing?")
print(results)
```

Every method maps directly to a `cg_sdk_*` C function in `cognee_sdk.h`. The bridging uses `withCheckedThrowingContinuation` — no GCD, no semaphores, no blocking.

## Offline / mock mode

The SDK can run fully offline using a pre-recorded LLM cassette. This is how the XCTest suite runs — no network, no model, no API key.

```swift
let cognee = try Cognee()
try cognee.configureMockMode(cassettePath: "/path/to/cassette.json")
try await cognee.warm()
// ... add, cognify, search work against the cassette
```

`configureMockMode` sets five config keys at once:
- `llm_mock = true` — replay LLM responses from the cassette instead of calling a real model
- `llm_cassette` — path to the cassette JSON file
- `embedding_provider = mock` — deterministic mock embeddings, no model needed
- `vector_db_provider = brute-force` — in-memory vector store
- `graph_database_provider = mock` — in-memory graph store

Individual keys can also be set with `configure(_:jsonValue:)` (for booleans/numbers) or `configure(_:value:)` (for strings).

## Running the tests

The XCTest suite in `Tests/CogneeSDKTests/` runs the full pipeline offline using a cassette recorded against `llama3.2:3b`. The cassette is committed at `Tests/CogneeSDKTests/Fixtures/demo_cassette.json`.

```bash
cd ios
xcodebuild test \
  -scheme CogneeSDK \
  -destination 'platform=iOS Simulator,name=iPhone 17' \
  2>&1 | grep -E 'Test Suite|Test Case|PASS|FAIL|error:'
```

Expected output:

```
Test Suite 'All tests' started at …
Test Suite 'CogneeSDKTests.xctest' started at …
Test Suite 'PipelineTests' started at …
Test Case '-[CogneeSDKTests.PipelineTests testConfigureMockMode]' passed (0.031 seconds).
Test Case '-[CogneeSDKTests.PipelineTests testSDKInit]' passed (0.001 seconds).
Test Case '-[CogneeSDKTests.PipelineTests testWarmAddCognifySearch]' passed (0.172 seconds).
Test Suite 'PipelineTests' passed at …
Test Suite 'All tests' passed at …
```

## CI

The `ios` workflow (`.github/workflows/ios.yml`) runs on every push and PR on a `macos-14` Apple Silicon runner. Because a full xcframework build exceeds GitHub Actions' available disk, CI uses `cargo check` rather than `cargo build`:

- `cargo check --target aarch64-apple-ios` — type-checks the Rust C API for the device target
- `cargo check --target aarch64-apple-ios-sim` — type-checks for the simulator target
- `swift package dump-package` — validates `Package.swift`
- `swiftc -typecheck` — type-checks the Swift wrapper against the real C API header via a synthesised Clang module, catching renamed `cg_sdk_*` functions, changed argument counts, and changed `CgErrorCode` values — no xcframework needed
- `swiftc -parse` — syntax-checks the Swift test sources (`@testable import CogneeSDK` requires a built module, so only parse-checking is possible in CI)

This catches Rust type errors, C API signature drift, and Swift syntax mistakes without needing the ~6.6 GB xcframework on the runner. XCTest behavioral tests run manually via `xcodebuild test` before each push.

## Architecture

```
cognee_sdk.h          C API — ~50 cg_sdk_* async functions (callback pattern)
      │
CogneeSDKCore         xcframework — compiled Rust static library + header
      │
CogneeSDK             Swift package
  ├── ContinuationBox.swift   heap wrapper bridging Swift continuation across C boundary
  ├── CogneeError.swift       maps CgErrorCode → Swift Error
  └── Cognee.swift            public API — withCheckedThrowingContinuation per method
```

Every `cg_sdk_*` function returns immediately and fires a `CgSdkResultCallback` exactly once from a tokio worker thread. The Swift bridge uses `Unmanaged.passRetained` to keep the continuation alive across that boundary and `takeRetainedValue()` in the callback to release it.

## Recording a new cassette

The committed cassette was recorded against `llama3.2:3b` via Ollama. To record a fresh one with different content:

```bash
# Start Ollama
ollama serve &
ollama pull llama3.2:3b

# Record (from repo root).
#
# COGNEE_RECORD_LLM   – cassette output path (wraps the real LLM adapter;
#                        writes the file on Drop).
# MOCK_EMBEDDING      – use deterministic mock embeddings so only LLM
#                        responses are recorded (no embedding API calls).
# LLM_API_BASE / LLM_API_KEY – Ollama's OpenAI-compatible endpoint.
COGNEE_RECORD_LLM="$(pwd)/ios/Tests/CogneeSDKTests/Fixtures/demo_cassette.json" \
MOCK_EMBEDDING=deterministic \
LLM_API_BASE=http://localhost:11434/v1 \
LLM_API_KEY=ollama \
  cargo run --release -p cognee-cli -- bench \
    --memories ios/Tests/CogneeSDKTests/Fixtures/memories.json \
    --llm-provider openai \
    --llm-model llama3.2:3b \
    --output /dev/null
```

The cassette keys on `sha256(user_input + schema)` so the same text always produces the same key — replay is deterministic regardless of model temperature settings.
