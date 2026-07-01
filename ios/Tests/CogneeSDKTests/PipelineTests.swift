// Tests/CogneeSDKTests/PipelineTests.swift
//
// Offline integration test: add → cognify → search using a pre-recorded
// LLM cassette so the test requires no network access and no running model.
//
// Run on iOS Simulator:
//   xcodebuild test \
//     -scheme CogneeSDK \
//     -destination 'platform=iOS Simulator,name=iPhone 16' \
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
        return String(data: data, encoding: .utf8)!
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

        // ── 3. Add — text must match what was used during cassette recording
        //     (memory_to_text format: "Title: …\n\n<content>\n\nReferences: none")
        let memoryText = """
            Title: Alan Turing

            Alan Turing was a British mathematician and computer scientist born \
            in London in 1912. He is widely considered the father of theoretical \
            computer science and artificial intelligence. During World War II, \
            Turing worked at Bletchley Park where he led the team that cracked \
            the Enigma cipher used by Nazi Germany, significantly shortening the \
            war. After the war, he worked at the University of Manchester and \
            developed the Turing test to evaluate machine intelligence. Turing \
            was awarded the OBE in 1946 for his wartime services.

            References: none
            """

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
