//! Python-compatible text forms of JSON values.
//!
//! `fact_hash` is a SHA-256 over Python's `json.dumps(fields, sort_keys=True,
//! default=str)`, and a fact's generic description joins `str(value)` of its
//! scalar props. Both must come out byte for byte as Python writes them, or a
//! Rust ingestion would report every fact as changed on a Python-written graph
//! (and the other way round). serde_json's own output differs in separators,
//! escaping and float formatting, so the two forms are spelled out here.

use serde_json::{Map, Number, Value};

/// `json.dumps(value, sort_keys=True)` with Python's defaults: `", "` and
/// `": "` separators, `ensure_ascii=True` escaping and `float.__repr__`
/// floats. Keys are sorted by code point, as Python sorts `str`.
pub fn dumps_sorted(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number_repr(number)),
        Value::String(text) => write_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => write_object(map, out),
    }
}

fn write_object(map: &Map<String, Value>, out: &mut String) {
    // Rust's `str` ordering is byte order, which for UTF-8 is code point
    // order: the order Python's `sorted()` gives `str` keys.
    let mut entries: Vec<(&String, &Value)> = map.iter().collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    out.push('{');
    for (index, (key, item)) in entries.into_iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        write_string(key, out);
        out.push_str(": ");
        write_value(item, out);
    }
    out.push('}');
}

/// `json.dumps(text)` for one string: Python's `ensure_ascii=True` escaping,
/// double-quoted.
pub fn dumps_str(text: &str) -> String {
    let mut out = String::new();
    write_string(text, &mut out);
    out
}

/// A JSON string as Python's `ensure_ascii=True` encoder writes it: every
/// character outside printable ASCII is a `\uXXXX` escape (a surrogate pair
/// above the BMP), with the short escapes for the usual control characters.
fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ' '..='~' => out.push(character),
            _ => {
                let mut units = [0u16; 2];
                for unit in character.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// `repr()` of a JSON number as Python holds it: integers verbatim, floats as
/// `float.__repr__` writes them.
pub fn number_repr(number: &Number) -> String {
    if let Some(integer) = number.as_i64() {
        return integer.to_string();
    }
    if let Some(integer) = number.as_u64() {
        return integer.to_string();
    }
    number.as_f64().map(float_repr).unwrap_or_default()
}

/// `float.__repr__`: the shortest digits that round-trip, written in fixed
/// notation when the decimal exponent is in `-4..16` and in scientific
/// notation (`1e-05`, `1.5e+16`) otherwise, always with a `.0` or a fraction
/// in fixed notation.
pub fn float_repr(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    // `{:e}` is the shortest round-trip form, e.g. `1.5e-5`, `-0e0`.
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .unwrap_or((scientific.as_str(), "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let (negative, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, mantissa),
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    // Python's `decpt`: the value is 0.<digits> * 10**decpt.
    let decpt = exponent + 1;
    let sign = if negative { "-" } else { "" };

    if (-4 < decpt) && (decpt <= 16) {
        let digit_count = digits.len() as i32;
        let body = if decpt <= 0 {
            format!("0.{}{}", "0".repeat((-decpt) as usize), digits)
        } else if decpt >= digit_count {
            format!("{}{}.0", digits, "0".repeat((decpt - digit_count) as usize))
        } else {
            let (whole, fraction) = digits.split_at(decpt as usize);
            format!("{whole}.{fraction}")
        };
        format!("{sign}{body}")
    } else {
        let (first, rest) = digits.split_at(1.min(digits.len()));
        let mantissa = if rest.is_empty() {
            first.to_string()
        } else {
            format!("{first}.{rest}")
        };
        let exponent_sign = if exponent < 0 { "-" } else { "+" };
        format!("{sign}{mantissa}e{exponent_sign}{:02}", exponent.abs())
    }
}

/// `str(value)` of a scalar JSON value as Python prints it: strings bare,
/// `True`/`False`, `None`, numbers as `repr`. Containers fall back to JSON.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => py_repr(other),
    }
}

/// An approximation of `repr(value)` for messages: strings single-quoted,
/// `True`/`False`/`None`, numbers as Python writes them.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => number_repr(number),
        Value::String(text) => format!("'{text}'"),
        other => other.to_string(),
    }
}

/// Python's `str.strip()`: trims what `str.isspace()` calls whitespace, which
/// is Unicode `White_Space` (Rust's `char::is_whitespace`) plus the ASCII
/// separators `\x1c`..=`\x1f`.
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(|character: char| {
        character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
    })
}

/// Python truthiness of a JSON value.
pub fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// What `for item in value or []` yields in Python: a list's items, a dict's
/// keys, a string's characters. Anything else (a falsy value, or a number
/// Python could not iterate) yields nothing.
pub fn py_iter(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::Object(map)) => map.keys().cloned().map(Value::String).collect(),
        Some(Value::String(text)) => text
            .chars()
            .map(|character| Value::String(character.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_match_python_repr() {
        let cases = [
            (1.0, "1.0"),
            (0.5, "0.5"),
            (0.7, "0.7"),
            (-0.0, "-0.0"),
            (100.0, "100.0"),
            (1e-4, "0.0001"),
            (1e-5, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.2345e16, "1.2345e+16"),
            (123.456, "123.456"),
            (1e100, "1e+100"),
            (0.1 + 0.2, "0.30000000000000004"),
        ];
        for (value, expected) in cases {
            assert_eq!(float_repr(value), expected, "{value}");
        }
    }

    #[test]
    fn dumps_matches_python_json_dumps_sorted() {
        let value = json!({
            "b": [1, 2.5, true, null],
            "a": {"z": "x", "y": "é\u{1F600}\"\\\n\u{7f}"},
        });
        assert_eq!(
            dumps_sorted(&value),
            r#"{"a": {"y": "\u00e9\ud83d\ude00\"\\\n\u007f", "z": "x"}, "b": [1, 2.5, true, null]}"#
        );
        assert_eq!(dumps_sorted(&json!({})), "{}");
        assert_eq!(dumps_sorted(&json!([])), "[]");
    }

    #[test]
    fn python_iteration_and_truthiness() {
        assert_eq!(py_iter(Some(&json!("ab"))), vec![json!("a"), json!("b")]);
        assert_eq!(py_iter(Some(&json!({"k": 1}))), vec![json!("k")]);
        assert!(py_iter(Some(&json!(3))).is_empty());
        assert!(!is_truthy(&json!(0)));
        assert!(!is_truthy(&json!("")));
        assert!(is_truthy(&json!(5)));
    }

    #[test]
    fn strip_matches_python_isspace() {
        assert_eq!(
            py_strip(" \t\n\r\u{0b}\u{0c}x\u{1c}\u{1f}\u{85}\u{a0}"),
            "x"
        );
        assert_eq!(py_strip("\u{2003}doc\u{3000}"), "doc");
        assert_eq!(py_strip("\u{200b}"), "\u{200b}");
        assert_eq!(py_strip(""), "");
    }
}
