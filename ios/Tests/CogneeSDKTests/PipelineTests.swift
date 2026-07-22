// Tests/CogneeSDKTests/PipelineTests.swift
//
// Offline integration test: add → cognify → search using a pre-recorded
// LLM cassette so the test requires no network access and no running model.
//
// Run on iOS Simulator:
//   xcodebuild test \
//     -scheme CogneeSDK \
//     -destination 'platform=iOS Simulator,name=iPhone 17' \
//     2>&1 | grep -E 'Test|PASS|FAIL|error'

import XCTest
@testable import CogneeSDK

final class PipelineTests: XCTestCase {

    // MARK: – Helpers

    /// Returns the filesystem path to the bundled demo cassette.
    private var cassettePath: String {
        get throws {
            guard let url = Bundle.module.url(
                forResource: "demo_cassette", withExtension: "json"
            ) else {
                throw XCTSkip("demo_cassette.json not found in test bundle — "
                    + "ensure Fixtures/demo_cassette.json is declared as a resource "
                    + "in Package.swift")
            }
            return url.path
        }
    }

    /// Build a JSON array string for a single text input.
    private func textInputsJSON(_ text: String) throws -> String {
        let payload: [[String: String]] = [["type": "text", "text": text]]
        let data = try JSONSerialization.data(withJSONObject: payload)
        return try XCTUnwrap(String(data: data, encoding: .utf8))
    }

    /// Load the first entry from the bundled memories.json and apply the same
    /// `memory_to_text` shaping used by the Rust bench command:
    ///   "Title: {title}\n\n{content}\n\nReferences: {refs}"
    ///
    /// This guarantees the text passed to `add()` produces the same
    /// sha256(user_input + schema) keys that were written into the cassette
    /// during recording — without duplicating the text in the test source.
    private func memoryTextFromFixture() throws -> String {
        guard let url = Bundle.module.url(forResource: "memories", withExtension: "json") else {
            throw XCTSkip("memories.json not found in test bundle")
        }
        let data = try Data(contentsOf: url)
        guard let memories = try JSONSerialization.jsonObject(with: data) as? [[String: Any]],
              let first = memories.first else {
            throw XCTSkip("memories.json is empty or not an array of objects")
        }
        let title   = first["title"]   as? String ?? "Untitled"
        let content = first["content"] as? String ?? ""
        // Mirror Rust: empty array → "none", non-empty string array → joined
        let refs: String
        if let arr = first["references"] as? [String], !arr.isEmpty {
            refs = arr.joined(separator: ", ")
        } else {
            refs = "none"
        }
        return "Title: \(title)\n\n\(content)\n\nReferences: \(refs)"
    }

    // MARK: – Tests

    /// Full offline pipeline: warm → add → cognify → search.
    ///
    /// The LLM cassette was recorded against `llama3.2:3b` via Ollama.
    /// With `configureMockMode`, the SDK uses:
    ///   - mock LLM  (replays from cassette)
    ///   - mock embeddings  (deterministic, no model)
    ///   - in-memory graph store  (mock)
    ///   - in-memory brute-force vector store
    func testWarmAddCognifySearch() async throws {
        let path = try cassettePath

        // ── 1. Create SDK and configure for offline mock mode ─────────────
        let cognee = try Cognee()
        try cognee.configureMockMode(cassettePath: path)

        // ── 2. Warm (builds in-memory stores using mock config) ────────────
        try await cognee.warm()

        // ── 3. Add — load text from memories.json and apply memory_to_text
        //     shaping so the cassette's sha256(user_input + schema) keys are
        //     matched byte-for-byte without duplicating the prose in the test.
        let memoryText = try memoryTextFromFixture()

        let inputsJSON = try textInputsJSON(memoryText)
        let addResult = try await cognee.add(inputsJSON: inputsJSON, dataset: "demo")

        XCTAssertFalse(addResult.isEmpty,   "add() must return a non-empty JSON string")
        XCTAssertNotEqual(addResult, "null", "add() must not return null")

        // ── 4. Cognify — replays cassette: graph extraction + summarisation ─
        let cognifyResult = try await cognee.cognify(dataset: "demo")

        XCTAssertFalse(cognifyResult.isEmpty,    "cognify() must return a non-empty JSON string")
        XCTAssertNotEqual(cognifyResult, "null",  "cognify() must not return null")

        // ── 5. Search — brute-force vector search over the stored chunks ────
        //     The mock graph store is a no-op so graph-based enrichment is
        //     skipped; the raw chunk hits are still returned.
        let searchResult = try await cognee.search(query: "Who was Alan Turing?")

        XCTAssertFalse(searchResult.isEmpty,    "search() must return a non-empty JSON string")
        XCTAssertNotEqual(searchResult, "null",  "search() must not return null")
    }

    /// Smoke-test: SDK initialises without throwing.
    func testSDKInit() throws {
        XCTAssertNoThrow(try Cognee())
    }

    /// Smoke-test: mock config can be applied without crashing.
    func testConfigureMockMode() throws {
        let path = try cassettePath
        let cognee = try Cognee()
        XCTAssertNoThrow(try cognee.configureMockMode(cassettePath: path))
    }
}
