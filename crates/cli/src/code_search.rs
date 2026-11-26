//! CLI plumbing shared by `search` and `recall` for `--query-type CODE`.
//!
//! Port of Python's `cognee/cli/code_search.py` (parity pin `a59433e6b`).
//! Three options ride on top of the ordinary search flags:
//!
//! - `--code-query JSON`: the structured `code_query` object (operation and
//!   arguments) the SDK and API take, as a JSON string.
//! - `--diagram {mermaid,dot}`: ask for the result rendered as a diagram too.
//! - `--diagram-out PATH`: write the diagram somewhere useful — a `.html`
//!   page that renders Mermaid in any browser, an `.svg` / `.png` / `.pdf`
//!   produced by Graphviz for DOT, or the raw source for any other extension.
//!
//! The pretty printer ([`render_code_results`]) shows each structured result
//! and its diagram source in a ```` ```mermaid ```` fence, which GitHub, most
//! docs sites and chat clients render as a picture.
//!
//! Graphviz runs as the `dot` program found on `PATH` by the operating
//! system's own process lookup: nothing here reads the environment.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::ValueEnum;
use cognee::cognee_code_graph::pyjson::dumps_indented;
use serde_json::{Map, Value};

use crate::error::CliError;

/// `--diagram`: the diagram format a CODE result is drawn in (Python's
/// `DIAGRAM_FORMAT_CHOICES`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DiagramFormatArg {
    #[value(name = "mermaid")]
    Mermaid,
    #[value(name = "dot")]
    Dot,
}

impl DiagramFormatArg {
    /// The wire name the CODE retriever's `diagram` argument takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mermaid => "mermaid",
            Self::Dot => "dot",
        }
    }
}

/// The CODE-only options, as `search` and `recall` parse them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeOptions<'a> {
    /// `--code-query`: the JSON text, unparsed.
    pub code_query: Option<&'a str>,
    /// `--diagram`.
    pub diagram: Option<DiagramFormatArg>,
    /// `--diagram-out`.
    pub diagram_out: Option<&'a Path>,
}

impl CodeOptions<'_> {
    fn is_empty(&self) -> bool {
        self.code_query.is_none() && self.diagram.is_none() && self.diagram_out.is_none()
    }
}

/// The help of `--code-query`: Python's, with every operation the CODE
/// retriever runs (`code_search.py:83-96`). A test checks the list against
/// [`cognee::cognee_code_graph::query::CodeOperation::ALL`].
pub const CODE_QUERY_HELP: &str = "Structured code-graph query for --query-type CODE, as JSON: \
     '{\"operation\": \"architecture\"}', '{\"operation\": \"impact_analysis\", \
     \"name\": \"pkg.Symbol\", \"max_depth\": 3}', '{\"operation\": \"ask\", \
     \"question\": \"where are auth tokens validated?\"}'. Operations: query_facts, explore, \
     traverse, find_path, impact_analysis, insights, architecture, delta, capabilities, search, \
     neighbors, subgraph, flows, hubs, ask";

/// The help of `--diagram` (`code_search.py:98-106`).
pub const DIAGRAM_HELP: &str = "Also render the CODE result as a diagram (Mermaid or Graphviz \
     DOT source). The architecture operation draws Mermaid by default.";

/// The help of `--diagram-out` (`code_search.py:107-116`).
pub const DIAGRAM_OUT_HELP: &str = "Write the diagram to PATH: .html renders Mermaid in a \
     browser, .svg/.png/.pdf run Graphviz `dot` on a DOT diagram, anything else gets the raw \
     source. Implies --diagram (format inferred from the extension).";

const DOT_EXTENSIONS: [&str; 2] = ["dot", "gv"];
const GRAPHVIZ_RENDER_EXTENSIONS: [&str; 3] = ["svg", "png", "pdf"];
const HTML_EXTENSIONS: [&str; 2] = ["html", "htm"];
const MERMAID_CDN: &str = "https://cdnjs.cloudflare.com/ajax/libs/mermaid/11.15.0/mermaid.min.js";
/// How long Graphviz may take (Python's `subprocess.run(..., timeout=120)`).
const GRAPHVIZ_TIMEOUT: Duration = Duration::from_secs(120);

/// Assemble `code_query` from `--code-query` / `--diagram` / `--diagram-out`,
/// or `None` without any of them (Python's `build_code_query`,
/// `code_search.py:119-145`).
///
/// `is_code` says whether the command runs `--query-type CODE`: the options
/// are refused with any other type. `--diagram` sets the `diagram` argument;
/// `--diagram-out` alone sets it from the file extension
/// ([`infer_diagram_format`]) unless the JSON already has one.
///
/// # Errors
///
/// [`CliError::Validation`] for the options without `--query-type CODE`, and
/// for a `--code-query` that is not a JSON object.
pub fn build_code_query(
    options: &CodeOptions<'_>,
    is_code: bool,
) -> Result<Option<Map<String, Value>>, CliError> {
    if options.is_empty() {
        return Ok(None);
    }
    if !is_code {
        return Err(CliError::Validation(
            "--code-query, --diagram and --diagram-out require --query-type CODE.".to_string(),
        ));
    }
    let mut code_query = Map::new();
    if let Some(raw) = options.code_query {
        match serde_json::from_str::<Value>(raw) {
            Ok(Value::Object(parsed)) => code_query.extend(parsed),
            Ok(_) => {
                return Err(CliError::Validation(
                    "--code-query must be a JSON object.".to_string(),
                ));
            }
            Err(error) => {
                return Err(CliError::Validation(format!(
                    "--code-query is not valid JSON: {error}"
                )));
            }
        }
    }
    if let Some(diagram) = options.diagram {
        code_query.insert(
            "diagram".to_string(),
            Value::String(diagram.as_str().to_string()),
        );
    } else if let Some(path) = options.diagram_out
        && !code_query.contains_key("diagram")
    {
        code_query.insert(
            "diagram".to_string(),
            Value::String(infer_diagram_format(path).to_string()),
        );
    }
    Ok(Some(code_query))
}

/// The lowercased extension of `path`, without its dot (Python's
/// `os.path.splitext(path)[1].lower()`, minus the dot); empty without one.
fn extension(path: &Path) -> String {
    path.extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// The diagram format `--diagram-out` implies: DOT for `.dot` / `.gv` and the
/// Graphviz renderings, Mermaid otherwise (`code_search.py:148-152`).
pub fn infer_diagram_format(path: &Path) -> &'static str {
    let extension = extension(path);
    if DOT_EXTENSIONS.contains(&extension.as_str())
        || GRAPHVIZ_RENDER_EXTENSIONS.contains(&extension.as_str())
    {
        "dot"
    } else {
        "mermaid"
    }
}

/// Every CODE operation result inside a search or recall response, in
/// document order: the objects with a string `operation` key, found by
/// walking objects and arrays (Python's `iter_code_results`,
/// `code_search.py:155-176`). A result is not searched further.
pub fn iter_code_results(results: &Value) -> Vec<&Map<String, Value>> {
    let mut found = Vec::new();
    collect_code_results(results, &mut found);
    found
}

fn collect_code_results<'a>(value: &'a Value, found: &mut Vec<&'a Map<String, Value>>) {
    match value {
        Value::Object(map) => {
            if map.get("operation").is_some_and(Value::is_string) {
                found.push(map);
                return;
            }
            for item in map.values() {
                collect_code_results(item, found);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_code_results(item, found);
            }
        }
        _ => {}
    }
}

/// The first diagram with a source among the results (`code_search.py:179-184`).
pub fn first_diagram(results: &Value) -> Option<&Map<String, Value>> {
    iter_code_results(results).into_iter().find_map(|result| {
        result
            .get("diagram")
            .and_then(Value::as_object)
            .filter(|diagram| diagram.get("source").is_some_and(is_truthy))
    })
}

fn is_truthy(value: &Value) -> bool {
    cognee::cognee_code_graph::pyjson::is_truthy(value)
}

/// Python's `str(value)` for the JSON scalars a summary line shows.
fn py_str(value: &Value) -> String {
    cognee::cognee_code_graph::pyjson::py_str(value)
}

/// One result's heading: the operation and its counts (Python's
/// `_summary_line`, `code_search.py:187-207`).
fn summary_line(result: &Map<String, Value>) -> String {
    let operation = result.get("operation").map(py_str).unwrap_or_default();
    let mut parts = Vec::new();
    if let Some(total) = result.get("total") {
        parts.push(format!("{} total", py_str(total)));
    }
    if let Some(Value::Object(stats)) = result.get("stats") {
        for key in [
            "nodes_shown",
            "nodes_visited",
            "edges_shown",
            "edges_traversed",
            "truncated",
        ] {
            if let Some(value) = stats.get(key) {
                parts.push(format!("{key}={}", py_str(value)));
            }
        }
    }
    if let Some(found) = result.get("found") {
        parts.push(
            if is_truthy(found) {
                "path found"
            } else {
                "no path"
            }
            .to_string(),
        );
    }
    if let Some(summary) = result.get("summary") {
        parts.push(py_str(summary));
    }
    // The navigation operations (neighbors, subgraph, flows, ask) report cuts
    // at the top level.
    if let Some(Value::Bool(truncated)) = result.get("truncated") {
        let reason = result
            .get("truncation_reason")
            .filter(|reason| is_truthy(reason))
            .map(|reason| format!(" ({})", py_str(reason)))
            .unwrap_or_default();
        parts.push(format!(
            "truncated={}{reason}",
            py_str(&Value::Bool(*truncated))
        ));
    }
    if parts.is_empty() {
        operation
    } else {
        format!("{operation} ({})", parts.join(", "))
    }
}

/// The pretty text of every CODE result in `results`: per result its
/// heading, the payload as indented JSON without the diagram, then the
/// diagram source fenced (or its note); `None` when there is no CODE result,
/// so the caller falls back to its generic printer (Python's
/// `print_code_results`, `code_search.py:210-238`).
pub fn render_code_results(results: &Value) -> Option<String> {
    let code_results = iter_code_results(results);
    if code_results.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (index, result) in code_results.into_iter().enumerate() {
        out.push_str(&format!("Result {}: {}\n", index + 1, summary_line(result)));
        let payload: Map<String, Value> = result
            .iter()
            .filter(|(key, _)| key.as_str() != "diagram")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        out.push_str(&dumps_indented(&Value::Object(payload), 2));
        out.push('\n');
        if let Some(Value::Object(diagram)) = result.get("diagram") {
            if diagram.get("source").is_some_and(is_truthy) {
                // Python's `diagram.get(key)`: a missing key prints `None`.
                let field = |key: &str| diagram.get(key).map_or_else(|| "None".to_string(), py_str);
                let format = field("format");
                out.push('\n');
                out.push_str(&format!(
                    "Diagram ({format}): {} node(s), {} edge(s)\n",
                    field("nodes"),
                    field("edges")
                ));
                out.push_str(&format!("```{format}\n"));
                let source = diagram.get("source").map(py_str).unwrap_or_default();
                out.push_str(source.trim_end_matches('\n'));
                out.push_str("\n```\n");
            } else if let Some(note) = diagram.get("note").filter(|note| is_truthy(note)) {
                out.push_str(&format!("Diagram: {}\n", py_str(note)));
            }
        }
        out.push('\n');
    }
    Some(out)
}

/// Write the first diagram in `results` to `path` (Python's `write_diagram`,
/// `code_search.py:241-302`), creating its directory.
///
/// `.html` / `.htm` wraps Mermaid source in a page that renders it (Mermaid
/// loaded from a CDN when opened); `.svg` / `.png` / `.pdf` render DOT
/// through Graphviz's `dot`; anything else receives the raw source.
///
/// # Errors
///
/// [`CliError::Runtime`] when the results carry no diagram, the format does
/// not fit the extension, Graphviz is missing, fails or times out, or the
/// file cannot be written.
pub fn write_diagram(results: &Value, path: &Path) -> Result<PathBuf, CliError> {
    let Some(diagram) = first_diagram(results) else {
        return Err(CliError::Runtime(
            "No diagram to write: the result carries none. Add --diagram or use an operation \
             that returns a graph (delta does not)."
                .to_string(),
        ));
    };
    let format = diagram.get("format").and_then(Value::as_str).unwrap_or("");
    let source = diagram.get("source").map(py_str).unwrap_or_default();
    let extension = extension(path);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            CliError::Runtime(format!("Cannot create {}: {error}", parent.display()))
        })?;
    }
    let write = |text: &str| {
        std::fs::write(path, text)
            .map_err(|error| CliError::Runtime(format!("Cannot write {}: {error}", path.display())))
    };

    if HTML_EXTENSIONS.contains(&extension.as_str()) {
        if format != "mermaid" {
            return Err(CliError::Runtime(
                "An .html diagram page needs the Mermaid format; use --diagram mermaid, or \
                 write DOT to .svg/.png/.pdf (Graphviz) or .dot."
                    .to_string(),
            ));
        }
        let title = mermaid_title(&source).unwrap_or_else(|| "cognee code graph".to_string());
        write(&html_page(&title, &source))?;
        return Ok(path.to_path_buf());
    }

    if GRAPHVIZ_RENDER_EXTENSIONS.contains(&extension.as_str()) {
        if format != "dot" {
            return Err(CliError::Runtime(format!(
                "Rendering to .{extension} needs the DOT format (Graphviz); use --diagram dot, \
                 or write Mermaid to .html."
            )));
        }
        render_with_graphviz("dot", &source, &extension, path)?;
        return Ok(path.to_path_buf());
    }

    write(&source)?;
    Ok(path.to_path_buf())
}

/// Run Graphviz (`program`, normally `dot`) on `source`, writing `path` in
/// the `extension` format (`code_search.py:276-298`).
fn render_with_graphviz(
    program: &str,
    source: &str,
    extension: &str,
    path: &Path,
) -> Result<(), CliError> {
    let mut child = Command::new(program)
        .arg(format!("-T{extension}"))
        .arg("-o")
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => CliError::Runtime(format!(
                "Rendering to .{extension} needs Graphviz (`dot`) on PATH; install it or write \
                 the raw source to a .dot file."
            )),
            _ => CliError::Runtime(format!("Graphviz could not start: {error}")),
        })?;
    // stderr is drained on its own thread, started before any input is
    // written, so a chatty run cannot block on a full pipe while it still
    // reads its source.
    let stderr = child.stderr.take().map(|mut stderr| {
        std::thread::spawn(move || {
            let mut text = Vec::new();
            // A read error leaves what was read: the message is best effort.
            let _ = stderr.read_to_end(&mut text);
            text
        })
    });
    // The source is written whole and stdin closed (dropped) so Graphviz sees
    // its end. A broken pipe means Graphviz stopped reading early (a syntax
    // error): its exit status and stderr below say why.
    if let Some(mut stdin) = child.stdin.take()
        && let Err(error) = stdin.write_all(source.as_bytes())
        && error.kind() != std::io::ErrorKind::BrokenPipe
    {
        // Best effort: the process may have exited meanwhile.
        let _ = child.kill();
        let _ = child.wait();
        return Err(CliError::Runtime(format!("Graphviz input failed: {error}")));
    }
    let deadline = Instant::now() + GRAPHVIZ_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // Best effort: the process may have exited meanwhile.
                let _ = child.kill();
                let _ = child.wait();
                return Err(CliError::Runtime(format!(
                    "Graphviz timed out after {} s",
                    GRAPHVIZ_TIMEOUT.as_secs()
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                return Err(CliError::Runtime(format!("Graphviz failed: {error}")));
            }
        }
    };
    if !status.success() {
        let stderr = stderr
            .and_then(|handle| handle.join().ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        // The last 500 characters, as Python keeps `stderr[-500:]`.
        let tail: String = {
            let chars: Vec<char> = stderr.chars().collect();
            chars[chars.len().saturating_sub(500)..].iter().collect()
        };
        let code = status
            .code()
            .map_or_else(|| "signal".to_string(), |code| code.to_string());
        return Err(CliError::Runtime(format!(
            "Graphviz failed ({code}): {tail}"
        )));
    }
    Ok(())
}

/// The title of a Mermaid diagram's front matter (`---` / `title: ...` /
/// `---`), unquoted (Python's `_mermaid_title`, `code_search.py:305-316`).
fn mermaid_title(source: &str) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    if lines.len() < 3 || lines[0] != "---" {
        return None;
    }
    for line in &lines[1..] {
        if *line == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix("title:") {
            let mut title = rest.trim().to_string();
            if title.chars().count() >= 2 && title.starts_with('"') && title.ends_with('"') {
                title = title[1..title.len() - 1]
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\");
            }
            return (!title.is_empty()).then_some(title);
        }
    }
    None
}

/// Python's `html.escape(text)`: `&`, `<`, `>`, `"` and `'`.
fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            other => out.push(other),
        }
    }
    out
}

/// The page a `.html` diagram is written as (Python's `_HTML_PAGE`,
/// `code_search.py:52-75`).
fn html_page(title: &str, source: &str) -> String {
    let title = html_escape(title);
    let source = html_escape(source);
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
  body {{ margin: 0; padding: 1.5rem; font-family: system-ui, sans-serif; background: #fff; color: #222; }}
  h1 {{ font-size: 1.1rem; font-weight: 600; margin: 0 0 1rem; }}
  .mermaid {{ overflow: auto; }}
  details {{ margin-top: 1.5rem; }}
  pre {{ background: #f6f8fa; padding: 1rem; overflow: auto; font-size: 0.85rem; }}
</style>
<script src="{MERMAID_CDN}"></script>
</head>
<body>
<h1>{title}</h1>
<pre class="mermaid">
{source}
</pre>
<details><summary>Mermaid source</summary><pre>{source}</pre></details>
<script>mermaid.initialize({{ startOnLoad: true, securityLevel: "strict" }});</script>
</body>
</html>
"#
    )
}

/// Honor `--diagram-out` after the results have been printed (Python's
/// `handle_diagram_out`, `code_search.py:319-327`): write the diagram and
/// return the lines to print.
///
/// # Errors
///
/// What [`write_diagram`] raises.
pub fn handle_diagram_out(results: &Value, path: Option<&Path>) -> Result<Vec<String>, CliError> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let written = write_diagram(results, path)?;
    let mut lines = vec![format!("Diagram written to {}", written.display())];
    if HTML_EXTENSIONS.contains(&extension(&written).as_str()) {
        lines.push("Open it in a browser to see the rendered architecture.".to_string());
    }
    Ok(lines)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use serde_json::json;

    use super::*;

    const MERMAID: &str = "---\ntitle: \"architecture: shop\"\n---\nflowchart LR\n    n0[[\"(root)\"]]\n    n1[[\"inventory\"]]\n    n0 -- \"calls x2\" --> n1\n";
    const DOT: &str = "digraph code_graph {\n    rankdir=LR;\n    n0 [label=\"(root)\" shape=component];\n    n1 [label=\"inventory\" shape=component];\n    n0 -> n1 [label=\"calls x2\"];\n}\n";

    fn code_result(format: &str, source: &str) -> Value {
        json!({
            "operation": "architecture",
            "repos": ["shop"],
            "nodes": [{"id": "m0", "kind": "module", "name": "."}],
            "edges": [],
            "stats": {"nodes_total": 2, "nodes_shown": 2, "edges_shown": 1, "truncated": false},
            "diagram": {"format": format, "source": source, "nodes": 2, "edges": 1},
        })
    }

    /// The per-dataset wrapping of a search response.
    fn search_response(result: Value) -> Value {
        json!([{
            "dataset_id": "11111111-1111-1111-1111-111111111111",
            "dataset_name": "shop",
            "search_result": [result],
        }])
    }

    fn options<'a>(
        code_query: Option<&'a str>,
        diagram: Option<DiagramFormatArg>,
        diagram_out: Option<&'a Path>,
    ) -> CodeOptions<'a> {
        CodeOptions {
            code_query,
            diagram,
            diagram_out,
        }
    }

    // --- option assembly (test_cli_code_search.py:73-109) -------------------

    #[test]
    fn build_code_query_is_none_without_code_options() {
        assert_eq!(
            build_code_query(&CodeOptions::default(), true).unwrap(),
            None
        );
        assert_eq!(
            build_code_query(&CodeOptions::default(), false).unwrap(),
            None
        );
    }

    #[test]
    fn build_code_query_parses_json_and_applies_diagram_options() {
        let built = build_code_query(
            &options(
                Some(r#"{"operation": "impact_analysis", "name": "pkg.X"}"#),
                Some(DiagramFormatArg::Dot),
                None,
            ),
            true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            Value::Object(built),
            json!({"operation": "impact_analysis", "name": "pkg.X", "diagram": "dot"})
        );

        // --diagram-out alone implies a diagram, format inferred from the extension.
        for (path, format) in [
            ("arch.html", "mermaid"),
            ("arch.svg", "dot"),
            ("arch.gv", "dot"),
        ] {
            let built = build_code_query(&options(None, None, Some(Path::new(path))), true)
                .unwrap()
                .unwrap();
            assert_eq!(Value::Object(built), json!({"diagram": format}), "{path}");
        }
        // An explicit diagram in the JSON wins over the inferred one.
        let built = build_code_query(
            &options(
                Some(r#"{"operation": "explore", "diagram": false}"#),
                None,
                Some(Path::new("x.html")),
            ),
            true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            Value::Object(built),
            json!({"operation": "explore", "diagram": false})
        );
    }

    #[test]
    fn infer_diagram_format_follows_the_extension() {
        for (extension, expected) in [
            (".mmd", "mermaid"),
            (".dot", "dot"),
            (".png", "dot"),
            (".PDF", "dot"),
            (".html", "mermaid"),
            ("", "mermaid"),
        ] {
            assert_eq!(
                infer_diagram_format(Path::new(&format!("diagram{extension}"))),
                expected,
                "{extension}"
            );
        }
    }

    #[test]
    fn build_code_query_rejects_non_code_types_and_bad_json() {
        let refused =
            |result: Result<Option<Map<String, Value>>, CliError>, needle: &str| match result {
                Err(CliError::Validation(message)) => {
                    assert!(message.contains(needle), "{message}")
                }
                other => panic!("expected a validation error, got {other:?}"),
            };
        refused(
            build_code_query(&options(None, Some(DiagramFormatArg::Mermaid), None), false),
            "require --query-type CODE",
        );
        refused(
            build_code_query(&options(Some("{}"), None, None), false),
            "require --query-type CODE",
        );
        refused(
            build_code_query(&options(Some("{not json"), None, None), true),
            "not valid JSON",
        );
        refused(
            build_code_query(&options(Some(r#"["architecture"]"#), None, None), true),
            "JSON object",
        );
    }

    // --- result discovery (test_cli_code_search.py:115-136) ------------------

    #[test]
    fn iter_code_results_finds_results_in_search_and_recall_shapes() {
        let nested = search_response(code_result("mermaid", MERMAID));
        let operations: Vec<&Value> = iter_code_results(&nested)
            .into_iter()
            .map(|result| &result["operation"])
            .collect();
        assert_eq!(operations, [&json!("architecture")]);
        // recall items: a list mixing sources.
        let recall_like = json!([
            {"source": "session", "content": {"question": "q"}},
            {"source": "code", "content": {"text": "x", "raw": code_result("mermaid", MERMAID)}},
        ]);
        assert_eq!(iter_code_results(&recall_like).len(), 1);
        assert!(iter_code_results(&json!(["just text", 42, null])).is_empty());
        // A result is not searched further: its nested objects are its own.
        let nested_ops = json!({"operation": "ask", "hits": [{"operation": "inner"}]});
        assert_eq!(iter_code_results(&nested_ops).len(), 1);

        assert_eq!(first_diagram(&nested).unwrap()["format"], json!("mermaid"));
        assert_eq!(
            first_diagram(&json!([{"operation": "delta", "diagram": {"source": null}}])),
            None
        );
    }

    #[test]
    fn render_code_results_shows_payload_and_fenced_diagram() {
        let out = render_code_results(&search_response(code_result("mermaid", MERMAID))).unwrap();
        assert!(out.starts_with(
            "Result 1: architecture (nodes_shown=2, edges_shown=1, truncated=False)\n{\n  \"operation\": \"architecture\",\n"
        ), "{out}");
        assert!(out.contains("\"repos\""));
        assert!(out.contains("Diagram (mermaid): 2 node(s), 1 edge(s)\n```mermaid\n---\n"));
        assert!(out.contains("title: \"architecture: shop\""));
        assert!(out.ends_with("n0 -- \"calls x2\" --> n1\n```\n\n"), "{out}");
        // The diagram source is shown once, in the fence, not repeated in the JSON.
        assert_eq!(out.matches("flowchart LR").count(), 1);

        assert_eq!(render_code_results(&json!(["plain", "strings"])), None);
    }

    #[test]
    fn render_code_results_reports_navigation_truncation_and_notes() {
        let subgraph = json!({
            "operation": "subgraph",
            "nodes": [],
            "edges": [],
            "truncated": true,
            "truncation_reason": "max_nodes",
            "stats": {"nodes_visited": 140, "edges_considered": 300, "depth_reached": 2},
        });
        let asked = json!({"operation": "ask", "question": "how?", "truncated": false});
        let path = json!({"operation": "find_path", "found": false, "total": 0,
                          "diagram": {"format": "mermaid", "source": null, "note": "no path to draw"}});
        let out = render_code_results(&json!([subgraph, asked, path])).unwrap();
        assert!(out.contains("Result 1: subgraph (nodes_visited=140, truncated=True (max_nodes))"));
        assert!(out.contains("Result 2: ask (truncated=False)"));
        assert!(out.contains("Result 3: find_path (0 total, no path)"));
        assert!(out.contains("Diagram: no path to draw\n"));
        // A diagram without its format or counts prints Python's `None`.
        let bare = json!({"operation": "explore", "diagram": {"source": "graph TD\n"}});
        let out = render_code_results(&bare).unwrap();
        assert!(
            out.contains("Diagram (None): None node(s), None edge(s)\n```None\ngraph TD\n```\n"),
            "{out}"
        );
        // Non-ASCII is escaped, as Python's json.dumps writes it.
        let out = render_code_results(&json!({"operation": "search", "summary": "é"})).unwrap();
        assert!(out.contains("\"summary\": \"\\u00e9\""), "{out}");
        assert!(out.starts_with("Result 1: search (é)\n"), "{out}");
    }

    // --- writing diagrams (test_cli_code_search.py:139-188) ------------------

    #[test]
    fn write_diagram_raw_and_html() {
        let dir = tempfile::tempdir().unwrap();
        let response = search_response(code_result("mermaid", MERMAID));

        let raw = write_diagram(&response, &dir.path().join("out").join("arch.mmd")).unwrap();
        assert_eq!(std::fs::read_to_string(raw).unwrap(), MERMAID);

        let html = write_diagram(&response, &dir.path().join("arch.html")).unwrap();
        let page = std::fs::read_to_string(html).unwrap();
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("<title>architecture: shop</title>"));
        assert!(page.contains("class=\"mermaid\""));
        assert!(page.contains("mermaid.min.js"));
        assert!(page.contains("mermaid.initialize"));
        // Source is HTML-escaped inside the page; the label quotes survive as entities.
        assert!(page.contains("n0[[&quot;(root)&quot;]]"));

        let dot_response = search_response(code_result("dot", DOT));
        let refused = |result: Result<PathBuf, CliError>, needle: &str| match result {
            Err(CliError::Runtime(message)) => assert!(message.contains(needle), "{message}"),
            other => panic!("expected a runtime error, got {other:?}"),
        };
        refused(
            write_diagram(&dot_response, &dir.path().join("arch.html")),
            "Mermaid format",
        );
        refused(
            write_diagram(
                &json!([{"operation": "delta", "diagram": {"source": null}}]),
                &dir.path().join("x.mmd"),
            ),
            "No diagram",
        );
    }

    #[test]
    fn write_diagram_renders_dot_with_graphviz() {
        let dir = tempfile::tempdir().unwrap();
        let dot_response = search_response(code_result("dot", DOT));

        let raw = write_diagram(&dot_response, &dir.path().join("arch.dot")).unwrap();
        assert_eq!(std::fs::read_to_string(raw).unwrap(), DOT);

        match write_diagram(
            &search_response(code_result("mermaid", MERMAID)),
            &dir.path().join("arch.svg"),
        ) {
            Err(CliError::Runtime(message)) => assert!(message.contains("DOT format"), "{message}"),
            other => panic!("expected a runtime error, got {other:?}"),
        }

        // A program that is not installed is reported as missing Graphviz.
        match render_with_graphviz(
            "cognee-no-such-graphviz-binary",
            DOT,
            "svg",
            &dir.path().join("x.svg"),
        ) {
            Err(CliError::Runtime(message)) => {
                assert!(message.contains("needs Graphviz"), "{message}")
            }
            other => panic!("expected a runtime error, got {other:?}"),
        }

        let svg = dir.path().join("arch.svg");
        match write_diagram(&dot_response, &svg) {
            Ok(written) => {
                let text = std::fs::read_to_string(written).unwrap();
                assert!(text.trim_start().starts_with("<?xml"), "{text}");
            }
            // Graphviz is not installed here: nothing more to check.
            Err(CliError::Runtime(message)) if message.contains("needs Graphviz") => {}
            Err(other) => panic!("Graphviz failed: {other:?}"),
        }
    }

    #[test]
    fn graphviz_failures_carry_its_stderr() {
        let dir = tempfile::tempdir().unwrap();
        match render_with_graphviz("dot", "not a graph {", "svg", &dir.path().join("x.svg")) {
            Err(CliError::Runtime(message)) => assert!(
                message.starts_with("Graphviz failed (") || message.contains("needs Graphviz"),
                "{message}"
            ),
            Ok(()) => panic!("invalid DOT must fail"),
            Err(other) => panic!("expected a runtime error, got {other:?}"),
        }
    }

    #[test]
    fn mermaid_titles_and_html_escaping_match_python() {
        assert_eq!(
            mermaid_title(MERMAID).as_deref(),
            Some("architecture: shop")
        );
        assert_eq!(
            mermaid_title("---\ntitle: \"a \\\"b\\\" \\\\c\"\n---\nflowchart LR\n").as_deref(),
            Some("a \"b\" \\c")
        );
        assert_eq!(
            mermaid_title("---\ntitle: plain\n---\n").as_deref(),
            Some("plain")
        );
        assert_eq!(mermaid_title("---\ntitle:\n---\n"), None);
        assert_eq!(mermaid_title("flowchart LR\n  a --> b\n"), None);
        assert_eq!(mermaid_title("---\n---\ntitle: late\n"), None);
        assert_eq!(
            html_escape("<a href='x'>&\"</a>"),
            "&lt;a href=&#x27;x&#x27;&gt;&amp;&quot;&lt;/a&gt;"
        );
    }

    #[test]
    fn handle_diagram_out_reports_the_written_file() {
        assert!(handle_diagram_out(&json!([]), None).unwrap().is_empty());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("arch.html");
        let lines = handle_diagram_out(
            &search_response(code_result("mermaid", MERMAID)),
            Some(&path),
        )
        .unwrap();
        assert_eq!(
            lines,
            [
                format!("Diagram written to {}", path.display()),
                "Open it in a browser to see the rendered architecture.".to_string()
            ]
        );
        let raw = dir.path().join("arch.mmd");
        let lines = handle_diagram_out(
            &search_response(code_result("mermaid", MERMAID)),
            Some(&raw),
        )
        .unwrap();
        assert_eq!(lines, [format!("Diagram written to {}", raw.display())]);
    }

    /// `test_code_query_help_lists_every_retriever_operation`.
    #[test]
    fn code_query_help_lists_every_retriever_operation() {
        use cognee::cognee_code_graph::query::CodeOperation;
        let listed = CODE_QUERY_HELP
            .split("Operations: ")
            .nth(1)
            .unwrap()
            .split(", ")
            .collect::<std::collections::BTreeSet<_>>();
        let all = CodeOperation::ALL
            .iter()
            .map(|operation| operation.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(listed, all);
    }
}
