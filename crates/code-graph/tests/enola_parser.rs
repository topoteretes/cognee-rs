//! Port of Python's `tests/unit/tasks/code_graph/test_enola_parser.py` and the
//! database-free half of `test_enola_contract.py`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

mod common;

use cognee_code_graph::{
    CodeNodeType, ParsedSnapshot, SUPPORTED_FORMAT_VERSIONS, build_code_graph_edges,
    map_facts_to_data_points, parse_enola_snapshot, snapshot_identity, validate_receipt,
};
use common::{by_name, entity, facts, id, write_snapshot};
use serde_json::{Value, json};

const REPO: &str = "acme/shop";

fn id_of(character: char) -> String {
    character.to_string().repeat(32)
}

fn receipt() -> Value {
    json!({
        "snapshot_id": "sha256:1ceea542",
        "format_version": 1,
        "enola_version": "0.4.12",
        "extractor_version": "v259",
        "generated_at": "2026-09-03T05:54:25Z",
        "duration": "160.7ms",
        "repo_path": "/home/someone/src/shop",
        "git": {"ref": "main", "commit": "17f4e44c", "dirty": false},
        "extractors": ["manifests", "python"],
        "output_hashes": {"facts.jsonl": "sha256:3468"},
        "fact_count": 2,
        "insight_count": 0,
        "quality": {"files_seen": 6, "files_parsed": 5, "parse_errors": 0},
    })
}

fn two_facts() -> Value {
    json!([
        {"kind": "symbol", "name": "app/db.Database", "file": "app/db.py", "repo": REPO, "id": id_of('a')},
        {"kind": "symbol", "name": "app/api.handler", "file": "app/api.py", "repo": REPO, "id": id_of('b')},
    ])
}

// --- test_enola_parser.py ------------------------------------------------------

#[test]
fn parse_enola_snapshot_reads_all_valid_facts() {
    let (facts, _receipt) = parse_enola_snapshot(common::basic_dir()).unwrap();
    // 16 valid facts, one corrupt line, one blank line.
    assert_eq!(facts.len(), 16);
    let kinds: std::collections::BTreeSet<&str> = facts
        .iter()
        .map(|fact| fact["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "dependency",
            "module",
            "route",
            "service",
            "storage",
            "symbol"
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn parse_enola_snapshot_skips_corrupt_line() {
    let (facts, _receipt) = parse_enola_snapshot(common::basic_dir()).unwrap();
    let names: Vec<&str> = facts
        .iter()
        .map(|fact| fact["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"main"));
    assert!(names.contains(&"helper"));
    assert!(!facts.iter().any(|fact| {
        Value::Object(fact.clone())
            .to_string()
            .contains("not valid json")
    }));
}

#[test]
fn parse_enola_snapshot_reads_receipt() {
    let (_facts, receipt) = parse_enola_snapshot(common::basic_dir()).unwrap();
    let receipt = receipt.unwrap();
    assert_eq!(receipt["enola_version"], "0.4.12");
    assert_eq!(receipt["format_version"], 1);
    assert_eq!(receipt["snapshot_id"], "sha256:abc123def456");
}

#[test]
fn parse_enola_snapshot_missing_receipt_is_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("facts.jsonl"),
        "{\"kind\": \"module\", \"name\": \"app\"}\n",
    )
    .unwrap();
    let (facts, receipt) = parse_enola_snapshot(dir.path()).unwrap();
    assert_eq!(facts.len(), 1);
    assert!(receipt.is_none());
}

#[test]
fn parse_enola_snapshot_missing_facts_raises() {
    let dir = tempfile::tempdir().unwrap();
    let error = parse_enola_snapshot(dir.path()).unwrap_err();
    assert_eq!(error.name(), "EnolaSnapshotError");
    assert_eq!(error.status_code(), 500);
}

#[test]
fn parse_enola_snapshot_reads_crlf_and_carriage_return_lines() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("facts.jsonl"),
        "{\"kind\": \"module\", \"name\": \"a\"}\r\n{\"kind\": \"module\", \"name\": \"b\"}\r{\"kind\": \"module\", \"name\": \"c\"}",
    )
    .unwrap();
    let (facts, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
    assert_eq!(facts.len(), 3);
}

fn write_minimal_snapshot(dir: &std::path::Path, insights: Value) {
    std::fs::write(
        dir.join("facts.jsonl"),
        "{\"kind\": \"symbol\", \"name\": \"app/db.Database\", \"file\": \"app/db.py\"}\n",
    )
    .unwrap();
    std::fs::write(dir.join("insights.json"), insights.to_string()).unwrap();
}

#[test]
fn parse_enola_snapshot_synthesizes_insight_facts() {
    let dir = tempfile::tempdir().unwrap();
    write_minimal_snapshot(
        dir.path(),
        json!([{
            "title": "Call-graph hotspot: app/db.Database",
            "source": "hotspots",
            "confidence": 0.7,
            "description": "A pinch point.",
            "evidence": [
                {"symbol": "app/db.Database", "detail": "fan-in 9"},
                {"detail": "no target here"},
            ],
        }]),
    );
    let (facts, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
    let insights: Vec<_> = facts
        .iter()
        .filter(|fact| fact["kind"] == "insight")
        .collect();
    assert_eq!(insights.len(), 1);
    let insight = insights[0];
    assert_eq!(insight["name"], "Call-graph hotspot: app/db.Database");
    assert_eq!(insight["props"]["source"], "hotspots");
    assert_eq!(insight["props"]["confidence"], 0.7);
    assert_eq!(
        insight["relations"],
        json!([{"kind": "evidences", "target": "app/db.Database"}])
    );
}

#[test]
fn parse_enola_snapshot_tolerates_bad_insights() {
    let dir = tempfile::tempdir().unwrap();
    write_minimal_snapshot(dir.path(), json!(["not-a-dict", {"no_title": true}]));
    let (facts, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
    let kinds: Vec<&Value> = facts.iter().map(|fact| &fact["kind"]).collect();
    assert_eq!(kinds, vec![&json!("symbol")]);
}

#[test]
fn parse_enola_snapshot_ignores_corrupt_insights_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("facts.jsonl"),
        "{\"kind\": \"module\", \"name\": \"app\"}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("insights.json"), "{ not json").unwrap();
    let (facts, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
    assert_eq!(facts.len(), 1);
}

// --- test_enola_contract.py: receipt ------------------------------------------

#[test]
fn supported_format_versions_is_exactly_one() {
    assert_eq!(SUPPORTED_FORMAT_VERSIONS, &[1]);
}

#[test]
fn format_version_1_and_historical_receipts_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    write_snapshot(dir.path(), &two_facts(), Some(&receipt()), None, None);
    let (facts, receipt_read) = parse_enola_snapshot(dir.path()).unwrap();
    assert_eq!(facts.len(), 2);
    assert_eq!(receipt_read.unwrap()["format_version"], 1);

    let mut historical = receipt();
    historical.as_object_mut().unwrap().remove("format_version");
    write_snapshot(dir.path(), &two_facts(), Some(&historical), None, None);
    let (facts, receipt_read) = parse_enola_snapshot(dir.path()).unwrap();
    assert_eq!(facts.len(), 2);
    assert!(!receipt_read.unwrap().contains_key("format_version"));
}

#[test]
fn unsupported_format_version_is_rejected() {
    for format_version in [json!(0), json!(2), json!("1"), json!(true), json!(1.0)] {
        let dir = tempfile::tempdir().unwrap();
        let mut bad = receipt();
        bad["format_version"] = format_version.clone();
        write_snapshot(dir.path(), &two_facts(), Some(&bad), None, None);

        let error = parse_enola_snapshot(dir.path()).unwrap_err().to_string();
        assert!(
            error.contains("format_version"),
            "{format_version}: {error}"
        );
        assert!(error.contains("0.4.12"), "{format_version}: {error}");
    }
}

/// SDK-1703: the `format_version` error text is Python's, `repr` included.
/// The messages are what Python a59433e6b's `validate_receipt` raised for
/// these receipts (snapshot dir `/tmp/snapshot`): generated by running
/// Python, not written by hand.
#[test]
fn the_unsupported_format_version_message_is_pythons() {
    const PYTHON_MESSAGES: &[(&str, &str)] = &[
        (
            r##"{"format_version": 2, "enola_version": "0.6.0"}"##,
            r##"Unsupported enola snapshot format_version 2 in '/tmp/snapshot' (enola 0.6.0); this cognee reads format version(s) [1]. Upgrade cognee, or pin an enola release that writes a supported format."##,
        ),
        (
            r##"{"format_version": "1"}"##,
            r##"Unsupported enola snapshot format_version '1' in '/tmp/snapshot' (enola ?); this cognee reads format version(s) [1]. Upgrade cognee, or pin an enola release that writes a supported format."##,
        ),
        (
            r##"{"format_version": true, "enola_version": null}"##,
            r##"Unsupported enola snapshot format_version True in '/tmp/snapshot' (enola None); this cognee reads format version(s) [1]. Upgrade cognee, or pin an enola release that writes a supported format."##,
        ),
        (
            r##"{"format_version": 1.0, "enola_version": 7}"##,
            r##"Unsupported enola snapshot format_version 1.0 in '/tmp/snapshot' (enola 7); this cognee reads format version(s) [1]. Upgrade cognee, or pin an enola release that writes a supported format."##,
        ),
        (
            r##"{"format_version": ["it's", null], "enola_version": "0.6.0"}"##,
            r##"Unsupported enola snapshot format_version ["it's", None] in '/tmp/snapshot' (enola 0.6.0); this cognee reads format version(s) [1]. Upgrade cognee, or pin an enola release that writes a supported format."##,
        ),
        (
            r##"{"format_version": {"major": "\u00e9\n"}, "enola_version": "0.6.0"}"##,
            r##"Unsupported enola snapshot format_version {'major': 'é\n'} in '/tmp/snapshot' (enola 0.6.0); this cognee reads format version(s) [1]. Upgrade cognee, or pin an enola release that writes a supported format."##,
        ),
    ];
    let dir = std::path::Path::new("/tmp/snapshot");
    for (receipt_text, message) in PYTHON_MESSAGES {
        let receipt = serde_json::from_str::<Value>(receipt_text).unwrap();
        let error = validate_receipt(receipt.as_object(), dir, None).unwrap_err();
        assert_eq!(error.name(), "EnolaSnapshotError");
        assert_eq!(&error.to_string(), message, "{receipt_text}");
    }
}

#[test]
fn validate_receipt_tolerates_missing_or_odd_receipts() {
    let dir = std::path::Path::new("/tmp/snapshot");
    let object = |value: Value| value.as_object().cloned().unwrap();
    validate_receipt(None, dir, None).unwrap();
    validate_receipt(Some(&object(json!({}))), dir, Some(3)).unwrap();
    let mut mismatch = receipt();
    mismatch["fact_count"] = json!(99);
    validate_receipt(Some(&object(mismatch)), dir, Some(2)).unwrap();
    let mut quality = receipt();
    quality["quality"] = json!({
        "files_seen": 10,
        "files_parsed": 4,
        "parse_errors": 2,
        "parse_error_sample": [{"extractor": "python", "msg": "boom"}],
        "census": {"top_skip_causes": [{"cause": "claimed by python", "count": 1}]},
    });
    validate_receipt(Some(&object(quality)), dir, Some(2)).unwrap();
    let mut odd = receipt();
    odd["quality"] = json!("not a dict");
    odd["fact_count"] = json!("2");
    validate_receipt(Some(&object(odd)), dir, Some(2)).unwrap();
}

#[test]
fn snapshot_identity_prefers_the_receipt_then_hashes_facts() {
    let dir = tempfile::tempdir().unwrap();
    write_snapshot(dir.path(), &two_facts(), None, None, None);
    let object = receipt().as_object().cloned().unwrap();
    assert_eq!(
        snapshot_identity(dir.path(), Some(&object)).as_deref(),
        Some("sha256:1ceea542")
    );
    let bytes = std::fs::read(dir.path().join("facts.jsonl")).unwrap();
    let digest = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(&bytes))
    };
    assert_eq!(
        snapshot_identity(dir.path(), None),
        Some(format!("sha256:{digest}"))
    );
    let empty = serde_json::Map::new();
    assert_eq!(
        snapshot_identity(dir.path(), Some(&empty)),
        Some(format!("sha256:{digest}"))
    );
    assert_eq!(snapshot_identity(dir.path().join("missing"), None), None);
}

/// The parsed snapshot's identity is the one `snapshot_identity` computes,
/// from the digest taken while reading (CRLF and all: the bytes, not the
/// parsed lines).
#[test]
fn a_parsed_snapshot_knows_its_identity_without_a_second_read() {
    let dir = tempfile::tempdir().unwrap();
    write_snapshot(dir.path(), &two_facts(), None, None, None);
    let text = std::fs::read_to_string(dir.path().join("facts.jsonl")).unwrap();
    std::fs::write(
        dir.path().join("facts.jsonl"),
        text.replace('\n', "\r\n") + "\n\n",
    )
    .unwrap();
    let parsed = ParsedSnapshot::read(dir.path()).unwrap();
    assert_eq!(parsed.identity(), snapshot_identity(dir.path(), None));
    assert!(parsed.identity().unwrap().starts_with("sha256:"));
    std::fs::write(dir.path().join("receipt.json"), receipt().to_string()).unwrap();
    let parsed = ParsedSnapshot::read(dir.path()).unwrap();
    assert_eq!(parsed.identity().as_deref(), Some("sha256:1ceea542"));
}

// --- test_enola_contract.py: facts, kinds, target ids -------------------------

#[test]
fn enola_id_and_end_line_are_mapped_onto_nodes() {
    let facts = facts(json!([
        {"kind": "symbol", "name": "x", "repo": REPO, "id": id_of('a'), "line": 3, "end_line": 9},
        {"kind": "symbol", "name": "y", "repo": REPO, "id": "not-a-hex-id", "end_line": "9"},
        {"kind": "symbol", "name": "z", "repo": REPO},
    ]));
    let nodes = map_facts_to_data_points(&facts, Some("/shop"), None, None);
    let x = entity(by_name(&nodes, "x"));
    assert_eq!(x.enola_id.as_deref(), Some(id_of('a').as_str()));
    assert_eq!((x.line, x.end_line), (Some(3), Some(9)));
    let y = entity(by_name(&nodes, "y"));
    assert_eq!((y.enola_id.as_deref(), y.end_line), (None, None));
    assert_eq!(entity(by_name(&nodes, "z")).enola_id, None);
    assert_eq!(x.base.id, id(REPO, "symbol", "x"));
}

#[test]
fn new_fact_kinds_map_to_typed_models() {
    let facts = facts(json!([
        {"kind": "intent", "name": "delivery", "repo": REPO,
         "props": {"intent_kind": "layer", "layer_name": "delivery", "order": 0}},
        {"kind": "extraction", "name": "python:calls", "file": "shop", "repo": REPO,
         "props": {"extractor": "python", "language": "python"}},
        {"kind": "association", "name": "Order#items", "repo": REPO,
         "props": {"model": "Order", "macro": "has_many", "target": "items"}},
        {"kind": "lint", "name": "RUBOCOP-001", "file": "app/models/order.rb", "repo": REPO,
         "props": {"lint_engine": "rubocop", "lint_severity": "warning"}},
        {"kind": "dependency", "name": "pkg:pypi/requests", "file": "pyproject.toml", "repo": REPO,
         "props": {"type": "package", "ecosystem": "pypi", "pinned": false}},
    ]));
    let nodes = map_facts_to_data_points(&facts, Some("/shop"), None, None);
    let node_type = |name: &str| by_name(&nodes, name).node_type().unwrap();
    assert_eq!(node_type("delivery"), CodeNodeType::CodeIntent);
    assert_eq!(
        node_type("python:calls"),
        CodeNodeType::CodeExtractionAccount
    );
    assert_eq!(node_type("Order#items"), CodeNodeType::CodeAssociation);
    assert_eq!(node_type("RUBOCOP-001"), CodeNodeType::CodeLintFinding);
    assert_eq!(
        node_type("pkg:pypi/requests"),
        CodeNodeType::ExternalDependency
    );
    assert_eq!(
        entity(by_name(&nodes, "pkg:pypi/requests")).fact_properties["ecosystem"],
        "pypi"
    );
    assert_eq!(nodes.len(), 6);
}

#[test]
fn unknown_kinds_are_dropped_without_failing() {
    let facts = facts(json!([
        {"kind": "topic", "name": "orders.created", "repo": REPO},
        {"kind": "symbol", "name": "main", "repo": REPO,
         "relations": [{"kind": "calls", "target": "orders.created"}]},
    ]));
    let nodes = map_facts_to_data_points(&facts, Some("/shop"), None, None);
    let (edges, skipped) = build_code_graph_edges(&facts, Some("/shop"));
    let names: Vec<&str> = nodes.iter().map(|node| node.name()).collect();
    assert_eq!(names, vec![REPO, "main"]);
    assert!(edges.is_empty());
    assert_eq!(skipped, 1);
}

#[test]
fn target_id_resolves_what_a_bare_name_cannot() {
    let raw = json!([
        {"kind": "symbol", "name": "helper", "file": "a.py", "repo": "repo-a", "id": id_of('a')},
        {"kind": "symbol", "name": "helper", "file": "b.py", "repo": "repo-b", "id": id_of('b')},
        {"kind": "symbol", "name": "main", "repo": "repo-c", "id": id_of('c'),
         "relations": [{"kind": "calls", "target": "helper", "target_id": id_of('b')}]},
    ]);
    let (edges, skipped) = build_code_graph_edges(&facts(raw.clone()), None);
    assert_eq!(skipped, 0);
    let pairs: Vec<_> = edges
        .iter()
        .map(|edge| {
            (
                edge.source_id,
                edge.target_id,
                edge.relationship_name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![(
            id("repo-c", "symbol", "main"),
            id("repo-b", "symbol", "helper"),
            "calls"
        )]
    );

    let mut without = raw;
    without[2]["relations"][0]
        .as_object_mut()
        .unwrap()
        .remove("target_id");
    let (edges, skipped) = build_code_graph_edges(&facts(without), None);
    assert!(edges.is_empty());
    assert_eq!(skipped, 1);
}

#[test]
fn unknown_target_id_falls_back_to_name_resolution() {
    let facts = facts(json!([
        {"kind": "symbol", "name": "helper", "repo": REPO, "id": id_of('a')},
        {"kind": "symbol", "name": "main", "repo": REPO, "id": id_of('b'),
         "relations": [{"kind": "calls", "target": "helper", "target_id": id_of('f')}]},
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    assert_eq!(skipped, 0);
    assert_eq!(edges[0].target_id, id(REPO, "symbol", "helper"));
}

#[test]
fn target_id_pointing_at_an_unmapped_kind_is_skipped_not_bound() {
    let facts = facts(json!([
        {"kind": "topic", "name": "orders", "repo": REPO, "id": id_of('a')},
        {"kind": "symbol", "name": "orders", "repo": REPO, "id": id_of('b')},
        {"kind": "symbol", "name": "main", "repo": REPO, "id": id_of('c'),
         "relations": [{"kind": "calls", "target": "orders", "target_id": id_of('a')}]},
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    assert_eq!(skipped, 0);
    assert_eq!(edges[0].target_id, id(REPO, "symbol", "orders"));
}

#[test]
fn records_sharing_an_enola_id_collapse_onto_one_node() {
    let facts = facts(json!([
        {"kind": "symbol", "name": "init", "file": "a.go", "repo": REPO, "id": id_of('a')},
        {"kind": "symbol", "name": "init", "file": "a.go", "repo": REPO, "id": id_of('a')},
        {"kind": "symbol", "name": "main", "repo": REPO, "id": id_of('b'),
         "relations": [{"kind": "calls", "target": "init", "target_id": id_of('a')}]},
    ]));
    let nodes = map_facts_to_data_points(&facts, Some("/shop"), None, None);
    let (edges, skipped) = build_code_graph_edges(&facts, Some("/shop"));
    let names: Vec<&str> = nodes.iter().map(|node| node.name()).collect();
    assert_eq!(names, vec![REPO, "init", "main"]);
    assert_eq!(skipped, 0);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].target_id, id(REPO, "symbol", "init"));
}

#[test]
fn insight_evidence_fact_id_disambiguates_and_metrics_ride_along() {
    let dir = tempfile::tempdir().unwrap();
    let raw = json!([
        {"kind": "symbol", "name": "app/db.Database", "file": "app/db.py", "repo": REPO, "id": id_of('a')},
        {"kind": "symbol", "name": "lib/db.Database", "file": "lib/db.py", "repo": REPO, "id": id_of('b')},
    ]);
    let insights = json!([
        {
            "title": "Call-graph hotspot: Database",
            "source": "hotspots",
            "description": "A pinch point.",
            "confidence": 0.7,
            "metrics": {"fan_in": 9, "fan_out": 2},
            "suggested_actions": ["Split the class."],
            "evidence": [
                {"symbol": "Database", "fact_id": id_of('b'), "detail": "fan-in 9"},
                {"fact": "Database", "detail": "no id here: ambiguous by name"},
            ],
        },
        {
            "title": "Domain findings do not apply to this repository",
            "source": "domain",
            "description": "Nothing to grade.",
            "confidence": 1,
            "informational": true,
            "evidence": [{"detail": "examined: 0 associations"}],
        },
    ]);
    write_snapshot(dir.path(), &raw, Some(&receipt()), Some(&insights), None);

    let (parsed, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
    let insight_facts: Vec<_> = parsed
        .iter()
        .filter(|fact| fact["kind"] == "insight")
        .collect();
    assert_eq!(insight_facts.len(), 2);
    let (hotspot, informational) = (insight_facts[0], insight_facts[1]);
    assert_eq!(
        hotspot["props"]["metrics"],
        json!({"fan_in": 9, "fan_out": 2})
    );
    assert_eq!(
        hotspot["props"]["suggested_actions"],
        json!(["Split the class."])
    );
    assert_eq!(
        hotspot["relations"],
        json!([
            {"kind": "evidences", "target": "Database", "target_id": id_of('b')},
            {"kind": "evidences", "target": "Database"},
        ])
    );
    assert_eq!(informational["props"]["informational"], true);
    assert_eq!(informational["relations"], json!([]));

    let (edges, skipped) = build_code_graph_edges(&parsed, Some("/shop"));
    let evidences: Vec<_> = edges
        .iter()
        .filter(|edge| edge.relationship_name == "evidences")
        .map(|edge| edge.target_id)
        .collect();
    assert_eq!(evidences, vec![id(REPO, "symbol", "lib/db.Database")]);
    assert_eq!(skipped, 1);

    let nodes = map_facts_to_data_points(&parsed, Some("/shop"), None, None);
    let hotspot = entity(by_name(&nodes, "Call-graph hotspot: Database"));
    assert_eq!(hotspot.description.as_deref(), Some("A pinch point."));
    assert_eq!(hotspot.fact_properties["metrics"]["fan_in"], 9);
}

/// SDK-1703: a bare `-0` reads as Python's int `0` (a float `-0` stays
/// `-0.0`), so the fact hashes and describes as Python's does. The hash,
/// description, parsed fact and receipt (`json.dumps(..., sort_keys=True)`)
/// are what Python a59433e6b computed for these files (`parse_enola_snapshot`
/// + `map_facts_to_data_points(facts, repo_path="/tmp/r")`).
#[test]
fn a_negative_zero_integer_reads_as_python_reads_it() {
    let dir = tempfile::tempdir().unwrap();
    let line = r#"{"kind": "symbol", "name": "zero", "repo": "r", "line": -0, "props": {"n": -0, "f": -0.0, "e": -0e0, "list": [-0, "-0"], "nested": {"m": -0}}}"#;
    std::fs::write(dir.path().join("facts.jsonl"), format!("{line}\n")).unwrap();
    std::fs::write(
        dir.path().join("receipt.json"),
        r#"{"format_version": 1, "fact_count": -0}"#,
    )
    .unwrap();

    let (facts, receipt) = parse_enola_snapshot(dir.path()).unwrap();
    assert_eq!(
        cognee_code_graph::pyjson::dumps_sorted(&Value::Object(facts[0].clone())),
        r#"{"kind": "symbol", "line": 0, "name": "zero", "props": {"e": -0.0, "f": -0.0, "list": [0, "-0"], "n": 0, "nested": {"m": 0}}, "repo": "r"}"#
    );
    assert_eq!(
        cognee_code_graph::pyjson::dumps_sorted(&Value::Object(receipt.unwrap())),
        r#"{"fact_count": 0, "format_version": 1}"#
    );

    let nodes = map_facts_to_data_points(&facts, Some("/tmp/r"), None, None);
    let zero = entity(by_name(&nodes, "zero"));
    assert_eq!(
        zero.fact_hash.as_deref(),
        Some("sha256:2b9bc3518858f6318a81d370d6be63c7f33b071c526e9c7bef47237e06f13cf8")
    );
    assert_eq!(
        zero.description.as_deref(),
        Some("symbol: e=-0.0, f=-0.0, n=0")
    );
    assert_eq!(zero.line, Some(0));
}
