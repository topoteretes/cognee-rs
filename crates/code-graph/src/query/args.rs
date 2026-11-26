//! Argument parsing of the CODE operations: Python's module-level validators
//! and `CodeRetriever`'s argument helpers (`code_retriever.py:303-398,
//! 2023-2027, 2175-2215`), with their exact messages and `error_param`s.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::error::{CodeQueryError, CodeQueryResult};
use super::snapshot::FACT_TYPE_BY_KIND;
use crate::pyjson::{is_truthy, py_str, py_strip};

/// The configuration of one CODE call: the request's
/// `retriever_specific_config` with its `code_query` merged over it.
pub type CodeConfig = Map<String, Value>;

/// Python's `type(value).__name__` of a JSON value.
pub(crate) fn python_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) if number.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// An integer that is not a boolean, as Python's `isinstance(value, int)
/// and not isinstance(value, bool)` sees it: `Some(Ok(n))` for one that fits
/// `i64`, `Some(Err(()))` for a larger positive one, `None` otherwise.
fn as_int(value: &Value) -> Option<Result<i64, ()>> {
    match value {
        Value::Number(number) if !number.is_f64() => Some(number.as_i64().ok_or(())),
        _ => None,
    }
}

/// `_as_strings`: a string or a list of non-empty strings, deduplicated in
/// order. A missing value, `null` and `""` are empty.
pub fn as_strings(value: Option<&Value>, field: &str) -> CodeQueryResult<Vec<String>> {
    let items = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(text)) if text.is_empty() => return Ok(Vec::new()),
        Some(Value::String(text)) => return Ok(vec![text.clone()]),
        Some(Value::Array(items)) => items,
        Some(_) => {
            return Err(CodeQueryError::invalid(
                format!("CODE {field} must be a string or a list of strings."),
                field,
            ));
        }
    };
    let mut strings = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::String(text) if !text.is_empty() => strings.push(text.clone()),
            _ => {
                return Err(CodeQueryError::invalid(
                    format!("CODE {field} must contain only non-empty strings."),
                    field,
                ));
            }
        }
    }
    let mut seen = HashSet::new();
    strings.retain(|text| seen.insert(text.clone()));
    Ok(strings)
}

/// `_bounded_int`: `default` for a missing value or one at most zero, capped
/// at `maximum`.
pub fn bounded_int(
    value: Option<&Value>,
    field: &str,
    default: usize,
    maximum: usize,
) -> CodeQueryResult<usize> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(default);
    };
    match as_int(value) {
        Some(Ok(number)) if number <= 0 => Ok(default),
        Some(Ok(number)) => Ok(usize::try_from(number).unwrap_or(usize::MAX).min(maximum)),
        Some(Err(())) => Ok(maximum),
        None => Err(CodeQueryError::invalid(
            format!("CODE {field} must be an integer."),
            field,
        )),
    }
}

/// `_count`: a limit of the navigation operations, like [`bounded_int`] but a
/// negative value is an error.
pub fn count(
    value: Option<&Value>,
    field: &str,
    default: usize,
    maximum: usize,
) -> CodeQueryResult<usize> {
    if let Some(Some(Ok(number))) = value.map(as_int)
        && number < 0
    {
        return Err(CodeQueryError::invalid(
            format!("CODE {field} must be a non-negative integer."),
            field,
        ));
    }
    bounded_int(value, field, default, maximum)
}

/// `_offset`: a non-negative integer, 0 when missing.
pub fn offset(value: Option<&Value>) -> CodeQueryResult<usize> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(0);
    };
    match as_int(value) {
        Some(Ok(number)) if number >= 0 => Ok(usize::try_from(number).unwrap_or(usize::MAX)),
        Some(Err(())) => Ok(usize::MAX),
        _ => Err(CodeQueryError::invalid(
            "CODE offset must be a non-negative integer.",
            "offset",
        )),
    }
}

/// `_direction`: `forward`, `reverse` or `both` (any case), `default` when
/// the value is falsy.
pub fn direction(value: Option<&Value>, default: &str) -> CodeQueryResult<Direction> {
    let text = match value {
        Some(value) if is_truthy(value) => py_str(value).to_lowercase(),
        _ => default.to_string(),
    };
    match text.as_str() {
        "forward" => Ok(Direction::Forward),
        "reverse" => Ok(Direction::Reverse),
        "both" => Ok(Direction::Both),
        _ => Err(CodeQueryError::invalid(
            "CODE direction must be 'forward', 'reverse', or 'both'.",
            "direction",
        )),
    }
}

/// Which edges of a node a walk follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Outgoing edges.
    Forward,
    /// Incoming edges.
    Reverse,
    /// Both.
    Both,
}

impl Direction {
    /// Whether outgoing edges are followed.
    pub fn forward(self) -> bool {
        matches!(self, Self::Forward | Self::Both)
    }

    /// Whether incoming edges are followed.
    pub fn reverse(self) -> bool {
        matches!(self, Self::Reverse | Self::Both)
    }
}

/// `_node_type`: a fact kind (`symbol`) or node type (`CodeSymbol`, any
/// case) as its node type.
pub fn node_type(value: &str, field: &str) -> CodeQueryResult<String> {
    if FACT_TYPE_BY_KIND
        .iter()
        .any(|(_, node_type)| *node_type == value)
    {
        return Ok(value.to_string());
    }
    let normalized = py_strip(value).to_lowercase();
    if let Some((_, node_type)) = FACT_TYPE_BY_KIND
        .iter()
        .find(|(kind, _)| *kind == normalized)
    {
        return Ok((*node_type).to_string());
    }
    if let Some((_, node_type)) = FACT_TYPE_BY_KIND
        .iter()
        .find(|(_, node_type)| node_type.to_lowercase() == normalized)
    {
        return Ok((*node_type).to_string());
    }
    let kinds: Vec<&str> = FACT_TYPE_BY_KIND.iter().map(|(kind, _)| *kind).collect();
    Err(CodeQueryError::invalid(
        format!(
            "Unsupported CODE node kind/type {}; expected one of {}.",
            crate::pyjson::py_repr_str(value),
            kinds.join(", ")
        ),
        field,
    ))
}

/// `_node_types`: [`as_strings`] read as node types; `None` when empty.
pub fn node_types(value: Option<&Value>, field: &str) -> CodeQueryResult<Option<HashSet<String>>> {
    let values = as_strings(value, field)?;
    if values.is_empty() {
        return Ok(None);
    }
    values
        .iter()
        .map(|item| node_type(item, field))
        .collect::<CodeQueryResult<HashSet<String>>>()
        .map(Some)
}

/// `_relation_types`: the union of `relation_types`, `relation_kinds` and
/// `relation`; `None` when all are empty.
pub fn relation_types(config: &CodeConfig) -> CodeQueryResult<Option<HashSet<String>>> {
    let mut values = HashSet::new();
    for field in ["relation_types", "relation_kinds", "relation"] {
        values.extend(as_strings(config.get(field), field)?);
    }
    Ok((!values.is_empty()).then_some(values))
}

/// `_identifier_arg`: the first non-blank id, name or repo among `fields`,
/// stripped. Anything but a string is a usage error (`invalid_argument`),
/// not an identifier that fails to resolve: `not_found` is softened into a
/// per-dataset miss, which would hide the caller's mistake.
pub fn identifier_arg(config: &CodeConfig, fields: &[&str]) -> CodeQueryResult<Option<String>> {
    for field in fields {
        let Some(value) = config.get(*field).filter(|value| !value.is_null()) else {
            continue;
        };
        let Value::String(text) = value else {
            return Err(CodeQueryError::invalid(
                format!(
                    "CODE {field} must be a string, got {}.",
                    python_type_name(value)
                ),
                field,
            ));
        };
        let stripped = py_strip(text);
        if !stripped.is_empty() {
            return Ok(Some(stripped.to_string()));
        }
    }
    Ok(None)
}

/// `str(config[field])` when the field is truthy (`if self.config.get(field):
/// ...append(str(self.config[field]))`).
pub fn truthy_text(config: &CodeConfig, field: &str) -> Option<String> {
    config
        .get(field)
        .filter(|value| is_truthy(value))
        .map(py_str)
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

    #[test]
    fn as_strings_matches_python() {
        assert!(as_strings(None, "names").unwrap().is_empty());
        assert!(as_strings(Some(&json!("")), "names").unwrap().is_empty());
        assert_eq!(as_strings(Some(&json!("a")), "names").unwrap(), vec!["a"]);
        assert_eq!(
            as_strings(Some(&json!(["b", "a", "b"])), "names").unwrap(),
            vec!["b", "a"]
        );
        let error = as_strings(Some(&json!(5)), "names").unwrap_err();
        assert_eq!(
            error.message,
            "CODE names must be a string or a list of strings."
        );
        let error = as_strings(Some(&json!(["a", ""])), "names").unwrap_err();
        assert_eq!(
            error.message,
            "CODE names must contain only non-empty strings."
        );
        assert_eq!(error.param.as_deref(), Some("names"));
    }

    #[test]
    fn integers_match_python() {
        assert_eq!(bounded_int(None, "limit", 10, 50).unwrap(), 10);
        assert_eq!(bounded_int(Some(&json!(0)), "limit", 10, 50).unwrap(), 10);
        assert_eq!(bounded_int(Some(&json!(-3)), "limit", 10, 50).unwrap(), 10);
        assert_eq!(bounded_int(Some(&json!(70)), "limit", 10, 50).unwrap(), 50);
        assert_eq!(
            bounded_int(Some(&json!(u64::MAX)), "limit", 10, 50).unwrap(),
            50
        );
        for bad in [json!(true), json!(1.5), json!("3")] {
            assert_eq!(
                bounded_int(Some(&bad), "limit", 10, 50)
                    .unwrap_err()
                    .message,
                "CODE limit must be an integer."
            );
        }
        assert_eq!(
            count(Some(&json!(-1)), "limit", 10, 50)
                .unwrap_err()
                .message,
            "CODE limit must be a non-negative integer."
        );
        assert_eq!(count(Some(&json!(0)), "limit", 10, 50).unwrap(), 10);
        assert_eq!(offset(None).unwrap(), 0);
        assert_eq!(offset(Some(&json!(4))).unwrap(), 4);
        for bad in [json!(-1), json!(false), json!(2.0)] {
            assert_eq!(
                offset(Some(&bad)).unwrap_err().message,
                "CODE offset must be a non-negative integer."
            );
        }
    }

    #[test]
    fn direction_and_node_types() {
        assert_eq!(direction(None, "both").unwrap(), Direction::Both);
        assert_eq!(
            direction(Some(&json!("REVERSE")), "both").unwrap(),
            Direction::Reverse
        );
        assert_eq!(
            direction(Some(&json!("")), "forward").unwrap(),
            Direction::Forward
        );
        assert!(direction(Some(&json!(5)), "both").is_err());
        assert_eq!(node_type("symbol", "kinds").unwrap(), "CodeSymbol");
        assert_eq!(node_type(" Route ", "kinds").unwrap(), "ApiEndpoint");
        assert_eq!(node_type("codemodule", "kinds").unwrap(), "CodeModule");
        assert_eq!(node_type("CodeModule", "kinds").unwrap(), "CodeModule");
        assert!(node_type("flow", "kinds").is_err());
    }

    #[test]
    fn identifiers_must_be_strings() {
        let config: CodeConfig = serde_json::from_value(json!({
            "name": " a ", "id": 5, "repo": ["x"], "blank": "  ", "flag": true,
        }))
        .unwrap();
        assert_eq!(
            identifier_arg(&config, &["name"]).unwrap().as_deref(),
            Some("a")
        );
        assert_eq!(identifier_arg(&config, &["blank"]).unwrap(), None);
        assert_eq!(identifier_arg(&config, &["missing"]).unwrap(), None);
        assert_eq!(
            identifier_arg(&config, &["id"]).unwrap_err().message,
            "CODE id must be a string, got int."
        );
        assert_eq!(
            identifier_arg(&config, &["repo"]).unwrap_err().message,
            "CODE repo must be a string, got list."
        );
        assert_eq!(
            identifier_arg(&config, &["flag"]).unwrap_err().message,
            "CODE flag must be a string, got bool."
        );
    }
}
