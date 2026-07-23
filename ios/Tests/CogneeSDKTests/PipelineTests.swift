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

    // MARK: – Result types

    /// Typed mirror of `CogneeAddResult` (cognee_sdk.h §D3):
    /// `{"datasetName":"…","added":[…],"addedCount":N,"deduplicated":[…],"deduplicatedCount":M}`
    private struct AddResult: Decodable {
        let addedCount: Int
        let deduplicatedCount: Int
    }

    /// Typed mirror of `CogneeCognifyResult` (cognee_sdk.h §D3):
    /// `{"chunks":N,"entities":N,"edges":N,"summaries":N}`
    private struct CognifyResult: Decodable {
        let chunks: Int
        let entities: Int
        let edges: Int
        let summaries: Int
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

        // Decode CogneeAddResult and assert on numeric fields.
        // On a fresh in-memory store the first add() must ingest ≥ 1 chunk with
        // nothing deduplicated (the store was empty before warm()).
        let addParsed = try JSONDecoder().decode(
            AddResult.self,
            from: try XCTUnwrap(addResult.data(using: .utf8), "add() returned non-UTF-8 data")
        )
        XCTAssertGreaterThan(addParsed.addedCount, 0,
            "add() must ingest ≥ 1 chunk; got addedCount=\(addParsed.addedCount)")
        XCTAssertEqual(addParsed.deduplicatedCount, 0,
            "first add() on a fresh store must deduplicate 0 items")

        // ── 4. Cognify — replays cassette: graph extraction + summarisation ─
        let cognifyResult = try await cognee.cognify(dataset: "demo")

        // Decode CogneeCognifyResult and assert on numeric fields.
        // The cassette records 6 extracted nodes and 5 edges, so all counts ≥ 1.
        let cognifyParsed = try JSONDecoder().decode(
            CognifyResult.self,
            from: try XCTUnwrap(cognifyResult.data(using: .utf8), "cognify() returned non-UTF-8 data")
        )
        XCTAssertGreaterThan(cognifyParsed.chunks, 0,
            "cognify() must process ≥ 1 chunk")
        XCTAssertGreaterThan(cognifyParsed.entities, 0,
            "cognify() must extract ≥ 1 entity; cassette records 6 nodes")
        XCTAssertGreaterThan(cognifyParsed.edges, 0,
            "cognify() must extract ≥ 1 edge; cassette records 5 edges")

        // ── 5. Search — brute-force vector search over the stored chunks ────
        //     The mock graph store is a no-op so graph-based enrichment is
        //     skipped; the raw chunk hits are still returned.
        let searchResult = try await cognee.search(query: "Who was Alan Turing?")

        // The mock graph store is a no-op, so only brute-force vector search
        // runs — no LLM completion call is made.  The returned chunks must
        // contain content from the Alan Turing text that was added.
        XCTAssertFalse(searchResult.isEmpty,
            "search() must return a non-empty JSON string")
        XCTAssertNotEqual(searchResult, "null",
            "search() must not return the JSON null literal")
        XCTAssertTrue(
            searchResult.lowercased().contains("turing") ||
            searchResult.lowercased().contains("alan") ||
            searchResult.lowercased().contains("mathematician"),
            "search('Who was Alan Turing?') must return content about Alan Turing; " +
            "got: \(searchResult.prefix(200))"
        )
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
