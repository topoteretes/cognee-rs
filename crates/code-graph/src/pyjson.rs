//! Python-compatible text forms of JSON values.
//!
//! `fact_hash` is a SHA-256 over Python's `json.dumps(fields, sort_keys=True,
//! default=str)`, and a fact's generic description joins `str(value)` of its
//! scalar props. Both must come out byte for byte as Python writes them, or a
//! Rust ingestion would report every fact as changed on a Python-written graph
//! (and the other way round). serde_json's own output differs in separators,
//! escaping and float formatting, so the two forms are spelled out here.

use serde_json::{Map, Number, Value};

/// `json.loads(text)` as Python reads numbers: [`zero_negative_integers`]
/// first, then serde_json.
///
/// # Errors
///
/// What serde_json raises for text that is not JSON.
pub fn loads(text: &str) -> serde_json::Result<Value> {
    serde_json::from_str(&zero_negative_integers(text))
}

/// `text` with every bare `-0` number written `0`.
///
/// Python reads the JSON integer `-0` as the int `0`, and writes it back as
/// `0`; serde_json reads it as the float `-0.0`, which Python writes as
/// `-0.0`. So a fact with a `-0` prop would hash and describe differently
/// in the two SDKs. A `-0` that is a float (`-0.0`, `-0e1`) stays a float,
/// as in Python, and a `-0` inside a string is text. The pass is lexical: it
/// leaves invalid JSON as invalid as it was.
pub fn zero_negative_integers(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("-0") {
        return std::borrow::Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut in_string = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            match byte {
                b'\\' => index += 1,
                b'"' => in_string = false,
                _ => {}
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte == b'-'
            && bytes.get(index + 1) == Some(&b'0')
            && !matches!(bytes.get(index + 2), Some(b'.' | b'e' | b'E' | b'0'..=b'9'))
            && !(index > 0 && is_number_byte(bytes[index - 1]))
        {
            // `-` and `0` are ASCII, so `index` is a char boundary.
            out.push_str(&text[copied..index]);
            copied = index + 1;
        }
        index += 1;
    }
    if copied == 0 {
        return std::borrow::Cow::Borrowed(text);
    }
    out.push_str(&text[copied..]);
    std::borrow::Cow::Owned(out)
}

/// A byte that continues a JSON number: a `-` after one is an exponent sign
/// (`1e-0`), not a number of its own.
fn is_number_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-')
}

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

/// `json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",",
/// ":"))`: the compact, key-sorted form Python's CODE retriever uses as its
/// context text and as a sort key for JSON values (`code_retriever._json_key`).
///
/// Unlike [`dumps_sorted`] it keeps every character outside ASCII as is: only
/// `"`, `\` and the control characters below U+0020 are escaped, as Python's
/// `ensure_ascii=False` encoder does. Floats are `float.__repr__`. The
/// workspace builds serde_json with `preserve_order`, so the keys are sorted
/// here explicitly.
pub fn dumps_compact(value: &Value) -> String {
    let mut out = String::new();
    write_compact(value, &mut out);
    out
}

fn write_compact(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number_repr(number)),
        Value::String(text) => write_unicode_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_compact(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            out.push('{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_unicode_string(key, out);
                out.push(':');
                write_compact(item, out);
            }
            out.push('}');
        }
    }
}

/// A JSON string as Python's `ensure_ascii=False` encoder writes it
/// (`py_encode_basestring`): `"`, `\` and the C0 controls escaped, the
/// usual ones with their short escape, the rest as `\u00XX`.
fn write_unicode_string(text: &str, out: &mut String) {
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
            '\u{00}'..='\u{1f}' => out.push_str(&format!("\\u{:04x}", u32::from(character))),
            _ => out.push(character),
        }
    }
    out.push('"');
}

/// Python's `str.casefold()`.
///
/// Rust has no full case folding in `std`, and `to_lowercase` is not it: `ß`
/// lowercases to itself but folds to `ss`, the final sigma lowercases to `ς`
/// but folds to `σ`, and the ligatures and the Greek letters with a
/// subscript iota fold to several characters. Each character is folded on
/// its own (case folding has no context): first through the `F` (full) and
/// `C` entries of Unicode's `CaseFolding.txt` whose folding differs from the
/// character's simple lowercase, then through its lowercase. A character
/// added to `CaseFolding.txt` after Unicode 15.1 (Python 3.12's table) may
/// still fold differently.
pub fn casefold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if let Some(folded) = special_fold(character) {
            out.push_str(folded);
        } else if let Some(folded) = cherokee_fold(character) {
            out.push(folded);
        } else {
            out.extend(character.to_lowercase());
        }
    }
    out
}

/// The Cherokee letters fold to their uppercase forms (`CaseFolding.txt`
/// maps the lowercase block onto the uppercase one, unlike every other
/// script).
fn cherokee_fold(character: char) -> Option<char> {
    let code = u32::from(character);
    let folded = match code {
        0xAB70..=0xABBF => code - 0xAB70 + 0x13A0,
        0x13F8..=0x13FD => code - 0x13F8 + 0x13F0,
        // The uppercase block is already the folded form.
        0x13A0..=0x13F5 => code,
        _ => return None,
    };
    char::from_u32(folded)
}

/// The `CaseFolding.txt` entries (status `C` and `F`) whose folding is not
/// the character's lowercase.
fn special_fold(character: char) -> Option<&'static str> {
    Some(match character {
        // Status F: one character folds to several.
        'ß' | 'ẞ' => "ss",
        'İ' => "i\u{307}",
        'ŉ' => "\u{2bc}n",
        'ǰ' => "j\u{30c}",
        'ΐ' => "\u{3b9}\u{308}\u{301}",
        'ΰ' => "\u{3c5}\u{308}\u{301}",
        'և' => "\u{565}\u{582}",
        'ẖ' => "h\u{331}",
        'ẗ' => "t\u{308}",
        'ẘ' => "w\u{30a}",
        'ẙ' => "y\u{30a}",
        'ẚ' => "a\u{2be}",
        'ὐ' => "\u{3c5}\u{313}",
        'ὒ' => "\u{3c5}\u{313}\u{300}",
        'ὔ' => "\u{3c5}\u{313}\u{301}",
        'ὖ' => "\u{3c5}\u{313}\u{342}",
        'ᾀ' | 'ᾈ' => "\u{1f00}\u{3b9}",
        'ᾁ' | 'ᾉ' => "\u{1f01}\u{3b9}",
        'ᾂ' | 'ᾊ' => "\u{1f02}\u{3b9}",
        'ᾃ' | 'ᾋ' => "\u{1f03}\u{3b9}",
        'ᾄ' | 'ᾌ' => "\u{1f04}\u{3b9}",
        'ᾅ' | 'ᾍ' => "\u{1f05}\u{3b9}",
        'ᾆ' | 'ᾎ' => "\u{1f06}\u{3b9}",
        'ᾇ' | 'ᾏ' => "\u{1f07}\u{3b9}",
        'ᾐ' | 'ᾘ' => "\u{1f20}\u{3b9}",
        'ᾑ' | 'ᾙ' => "\u{1f21}\u{3b9}",
        'ᾒ' | 'ᾚ' => "\u{1f22}\u{3b9}",
        'ᾓ' | 'ᾛ' => "\u{1f23}\u{3b9}",
        'ᾔ' | 'ᾜ' => "\u{1f24}\u{3b9}",
        'ᾕ' | 'ᾝ' => "\u{1f25}\u{3b9}",
        'ᾖ' | 'ᾞ' => "\u{1f26}\u{3b9}",
        'ᾗ' | 'ᾟ' => "\u{1f27}\u{3b9}",
        'ᾠ' | 'ᾨ' => "\u{1f60}\u{3b9}",
        'ᾡ' | 'ᾩ' => "\u{1f61}\u{3b9}",
        'ᾢ' | 'ᾪ' => "\u{1f62}\u{3b9}",
        'ᾣ' | 'ᾫ' => "\u{1f63}\u{3b9}",
        'ᾤ' | 'ᾬ' => "\u{1f64}\u{3b9}",
        'ᾥ' | 'ᾭ' => "\u{1f65}\u{3b9}",
        'ᾦ' | 'ᾮ' => "\u{1f66}\u{3b9}",
        'ᾧ' | 'ᾯ' => "\u{1f67}\u{3b9}",
        'ᾲ' => "\u{1f70}\u{3b9}",
        'ᾳ' | 'ᾼ' => "\u{3b1}\u{3b9}",
        'ᾴ' => "\u{3ac}\u{3b9}",
        'ᾶ' => "\u{3b1}\u{342}",
        'ᾷ' => "\u{3b1}\u{342}\u{3b9}",
        'ῂ' => "\u{1f74}\u{3b9}",
        'ῃ' | 'ῌ' => "\u{3b7}\u{3b9}",
        'ῄ' => "\u{3ae}\u{3b9}",
        'ῆ' => "\u{3b7}\u{342}",
        'ῇ' => "\u{3b7}\u{342}\u{3b9}",
        'ῒ' => "\u{3b9}\u{308}\u{300}",
        '\u{1fd3}' => "\u{3b9}\u{308}\u{301}",
        'ῖ' => "\u{3b9}\u{342}",
        'ῗ' => "\u{3b9}\u{308}\u{342}",
        'ῢ' => "\u{3c5}\u{308}\u{300}",
        '\u{1fe3}' => "\u{3c5}\u{308}\u{301}",
        'ῤ' => "\u{3c1}\u{313}",
        'ῦ' => "\u{3c5}\u{342}",
        'ῧ' => "\u{3c5}\u{308}\u{342}",
        'ῲ' => "\u{1f7c}\u{3b9}",
        'ῳ' | 'ῼ' => "\u{3c9}\u{3b9}",
        'ῴ' => "\u{3ce}\u{3b9}",
        'ῶ' => "\u{3c9}\u{342}",
        'ῷ' => "\u{3c9}\u{342}\u{3b9}",
        'ﬀ' => "ff",
        'ﬁ' => "fi",
        'ﬂ' => "fl",
        'ﬃ' => "ffi",
        'ﬄ' => "ffl",
        'ﬅ' | 'ﬆ' => "st",
        'ﬓ' => "\u{574}\u{576}",
        'ﬔ' => "\u{574}\u{565}",
        'ﬕ' => "\u{574}\u{56b}",
        'ﬖ' => "\u{57e}\u{576}",
        'ﬗ' => "\u{574}\u{56d}",
        // Status C: folds to a character other than its lowercase.
        'µ' => "\u{3bc}",
        'ſ' => "s",
        '\u{345}' | '\u{1fbe}' => "\u{3b9}",
        'ς' => "\u{3c3}",
        'ϐ' => "\u{3b2}",
        'ϑ' => "\u{3b8}",
        'ϕ' => "\u{3c6}",
        'ϖ' => "\u{3c0}",
        'ϰ' => "\u{3ba}",
        'ϱ' => "\u{3c1}",
        'ϵ' => "\u{3b5}",
        'ẛ' => "\u{1e61}",
        '\u{1c80}' => "\u{432}",
        '\u{1c81}' => "\u{434}",
        '\u{1c82}' => "\u{43e}",
        '\u{1c83}' => "\u{441}",
        '\u{1c84}' | '\u{1c85}' => "\u{442}",
        '\u{1c86}' => "\u{44a}",
        '\u{1c87}' => "\u{463}",
        '\u{1c88}' => "\u{a64b}",
        _ => return None,
    })
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

/// `str(value)` of a JSON value as Python prints it: a string bare,
/// anything else as [`py_repr`] writes it (Python's `str` of a list or dict
/// is its `repr`).
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => py_repr(other),
    }
}

/// `repr(value)` of a JSON value as Python holds it: `None`, `True`,
/// `False`, numbers as `repr`, strings as [`py_repr_str`], lists as
/// `[a, b]` and dicts as `{'k': v}` in key order.
pub fn py_repr(value: &Value) -> String {
    let mut out = String::new();
    write_repr(value, &mut out);
    out
}

fn write_repr(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("None"),
        Value::Bool(true) => out.push_str("True"),
        Value::Bool(false) => out.push_str("False"),
        Value::Number(number) => out.push_str(&number_repr(number)),
        Value::String(text) => out.push_str(&py_repr_str(text)),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_repr(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&py_repr_str(key));
                out.push_str(": ");
                write_repr(item, out);
            }
            out.push('}');
        }
    }
}

/// `repr(text)` as Python writes a `str`: single-quoted unless the text
/// holds a `'` and no `"`; `\\`, the quote, `\t`, `\n` and `\r` escaped; any
/// other character that is not printable as `\xNN`, `\uNNNN` or
/// `\UNNNNNNNN`.
pub fn py_repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if character == quote => {
                out.push('\\');
                out.push(character);
            }
            _ if is_printable(character) => out.push(character),
            _ => {
                let code = u32::from(character);
                if code <= 0xff {
                    out.push_str(&format!("\\x{code:02x}"));
                } else if code <= 0xffff {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    out.push_str(&format!("\\U{code:08x}"));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// Python's `str.isprintable()` for one character: the space, and every
/// character outside the categories `Cc`, `Cf`, `Cs`, `Co`, `Cn`, `Zl`,
/// `Zp` and `Zs`. The category table is unicode-properties' (Unicode 17),
/// so a character assigned after the Unicode version of the running Python
/// (15.0 for 3.12) reads as printable here and as `Cn` there.
fn is_printable(character: char) -> bool {
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
    if character == ' ' {
        return true;
    }
    !matches!(
        character.general_category(),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
            | GeneralCategory::SpaceSeparator
    )
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
#[allow(
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
    fn casefold_matches_python() {
        // Expected values are Python 3.12's `str.casefold()`.
        for (text, folded) in [
            ("Straße", "strasse"),
            ("ẞ", "ss"),
            ("ΌΣΟΣ", "όσοσ"),
            ("ὀδυσσεύς", "ὀδυσσεύσ"),
            ("ﬁle", "file"),
            ("İstanbul", "i\u{307}stanbul"),
            ("ᾈ", "\u{1f00}\u{3b9}"),
            ("µ", "\u{3bc}"),
            ("ſ", "s"),
            ("ꭰ", "Ꭰ"),
            ("MixedCase_ID", "mixedcase_id"),
        ] {
            assert_eq!(casefold(text), folded, "{text}");
        }
    }

    #[test]
    fn dumps_compact_matches_python_json_key() {
        // json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
        let value = json!({
            "b": [1, 2.5, true, null],
            "a": {"z": "x", "y": "é\u{1F600}\"\\\n\u{7f}\u{1}"},
        });
        assert_eq!(
            dumps_compact(&value),
            "{\"a\":{\"y\":\"é\u{1F600}\\\"\\\\\\n\u{7f}\\u0001\",\"z\":\"x\"},\"b\":[1,2.5,true,null]}"
        );
        assert_eq!(dumps_compact(&json!(1.0)), "1.0");
        assert_eq!(dumps_compact(&json!({})), "{}");
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

    /// `repr()` and `str()` as Python a59433e6b's interpreter (CPython 3.12,
    /// Unicode 15.0) wrote them for `json.loads(<json>)`: generated by
    /// running Python, not written by hand.
    const PYTHON_REPR_TABLE: &[(&str, &str, Option<&str>)] = &[
        (r##""plain""##, r##"'plain'"##, Some(r##"plain"##)),
        (r##""it's""##, r##""it's""##, Some(r##"it's"##)),
        (
            r##""say \"hi\"""##,
            r##"'say "hi"'"##,
            Some(r##"say "hi""##),
        ),
        (
            r##""both ' and \"""##,
            r##"'both \' and "'"##,
            Some(r##"both ' and ""##),
        ),
        (
            r##""back\\slash""##,
            r##"'back\\slash'"##,
            Some(r##"back\slash"##),
        ),
        (r##""tab\tnl\nret\r""##, r##"'tab\tnl\nret\r'"##, None),
        (
            r##""\u0000\u001f\u007f\u0080\u009f""##,
            r##"'\x00\x1f\x7f\x80\x9f'"##,
            None,
        ),
        (r##""\u00e9t\u00e9""##, r##"'été'"##, Some(r##"été"##)),
        (r##""nb\u00a0sp""##, r##"'nb\xa0sp'"##, None),
        (r##""soft\u00adhyphen""##, r##"'soft\xadhyphen'"##, None),
        (r##""zw\u200bsp""##, r##"'zw\u200bsp'"##, None),
        (r##""line\u2028sep""##, r##"'line\u2028sep'"##, None),
        (r##""para\u2029sep""##, r##"'para\u2029sep'"##, None),
        (r##""ideo\u3000sp""##, r##"'ideo\u3000sp'"##, None),
        (r##""private\ue000use""##, r##"'private\ue000use'"##, None),
        (r##""\ud83d\ude00""##, r##"'😀'"##, Some(r##"😀"##)),
        (r##""tag\udb40\udc01""##, r##"'tag\U000e0001'"##, None),
        (r##""unassigned\u0378""##, r##"'unassigned\u0378'"##, None),
        (r##""bom\ufeff""##, r##"'bom\ufeff'"##, None),
        (r##""\u4e2d\u6587""##, r##"'中文'"##, Some(r##"中文"##)),
        (r##""""##, r##"''"##, Some(r##""##)),
        (r##"null"##, r##"None"##, Some(r##"None"##)),
        (r##"true"##, r##"True"##, Some(r##"True"##)),
        (r##"false"##, r##"False"##, Some(r##"False"##)),
        (r##"0"##, r##"0"##, Some(r##"0"##)),
        (r##"-3"##, r##"-3"##, Some(r##"-3"##)),
        (r##"1.5"##, r##"1.5"##, Some(r##"1.5"##)),
        (r##"1e16"##, r##"1e+16"##, Some(r##"1e+16"##)),
        (r##"-0.0"##, r##"-0.0"##, Some(r##"-0.0"##)),
        (
            r##"12345678901234567890"##,
            r##"12345678901234567890"##,
            Some(r##"12345678901234567890"##),
        ),
        (r##"[]"##, r##"[]"##, Some(r##"[]"##)),
        (r##"{}"##, r##"{}"##, Some(r##"{}"##)),
        (
            r##"[1, "a", null, [true], {"k": "v"}]"##,
            r##"[1, 'a', None, [True], {'k': 'v'}]"##,
            Some(r##"[1, 'a', None, [True], {'k': 'v'}]"##),
        ),
        (
            r##"{"b": 1, "a": ["x", 2.0], "c'": {}}"##,
            r##"{'b': 1, 'a': ['x', 2.0], "c'": {}}"##,
            Some(r##"{'b': 1, 'a': ['x', 2.0], "c'": {}}"##),
        ),
        (
            r##"["it's", "\n"]"##,
            r##"["it's", '\n']"##,
            Some(r##"["it's", '\n']"##),
        ),
    ];

    #[test]
    fn repr_and_str_match_python() {
        for (json_text, repr, str_form) in PYTHON_REPR_TABLE {
            let value: Value = loads(json_text).expect("the table holds valid JSON");
            assert_eq!(&py_repr(&value), repr, "repr({json_text})");
            if let Some(str_form) = str_form {
                assert_eq!(&py_str(&value), str_form, "str({json_text})");
            }
        }
    }

    /// `json.dumps(json.loads(<json>), sort_keys=True)` as Python a59433e6b's
    /// interpreter wrote it (`None`: Python rejects the text): generated by
    /// running Python, not written by hand.
    const PYTHON_NEGATIVE_ZERO_TABLE: &[(&str, Option<&str>)] = &[
        (r##"-0"##, Some(r##"0"##)),
        (
            r##"[-0, -0.0, -0e0, -0E+1, 1e-0, -0.5]"##,
            Some(r##"[0, -0.0, -0.0, -0.0, 1.0, -0.5]"##),
        ),
        (r##"[-00]"##, None),
        (
            r##"{"a": -0, "b": "-0", "c\"-0": -0}"##,
            Some(r##"{"a": 0, "b": "-0", "c\"-0": 0}"##),
        ),
        (r##" -0 "##, Some(r##"0"##)),
        (r##"[-0]"##, Some(r##"[0]"##)),
        (r##"{"x":-0}"##, Some(r##"{"x": 0}"##)),
        (r##"-0.0"##, Some(r##"-0.0"##)),
    ];

    #[test]
    fn negative_zero_integers_read_as_python_reads_them() {
        for (json_text, dumped) in PYTHON_NEGATIVE_ZERO_TABLE {
            let read = loads(json_text).ok();
            assert_eq!(
                read.as_ref().map(dumps_sorted).as_deref(),
                *dumped,
                "json.loads({json_text})"
            );
        }
        // Nothing to rewrite: the text is borrowed as it is.
        assert!(matches!(
            zero_negative_integers(r#"{"a": -0.5, "b": "-0"}"#),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    /// A known difference (README, Known differences): Python's `json.loads`
    /// reads a lone surrogate escape into a `str` (`repr` `'\ud800'`);
    /// serde_json rejects the text, so such a facts line is skipped as
    /// corrupt.
    #[test]
    fn a_lone_surrogate_escape_is_not_read() {
        assert!(loads(r#""\ud800""#).is_err());
        assert!(loads(r#"{"name": "x\udc00y"}"#).is_err());
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
