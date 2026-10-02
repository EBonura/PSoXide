//! JSON written the way Python's `json.dumps` writes it.
//!
//! The site's data files and build records are committed or compared byte
//! for byte, and they were produced by `json.dumps`, so its layout is the
//! format: insertion-ordered keys, `", "`/`": "` separators without an
//! indent, Python's float `repr`, and `ensure_ascii` escaping where the
//! original asked for it. `serde_json` sorts map keys and spells floats and
//! escapes differently, so values are built as [`Json`] instead.

use std::fmt::Write as _;

/// An ordered JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null` (Python `None`).
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A float, written with Python's `repr`.
    Float(f64),
    /// A string.
    Str(String),
    /// A list.
    Arr(Vec<Json>),
    /// An object, in insertion order.
    Obj(Vec<(String, Json)>),
}

impl From<&str> for Json {
    fn from(text: &str) -> Self {
        Json::Str(text.to_string())
    }
}

impl From<String> for Json {
    fn from(text: String) -> Self {
        Json::Str(text)
    }
}

impl From<bool> for Json {
    fn from(flag: bool) -> Self {
        Json::Bool(flag)
    }
}

impl From<i64> for Json {
    fn from(n: i64) -> Self {
        Json::Int(n)
    }
}

impl From<usize> for Json {
    fn from(n: usize) -> Self {
        Json::Int(n as i64)
    }
}

impl From<u64> for Json {
    fn from(n: u64) -> Self {
        Json::Int(n as i64)
    }
}

impl From<f64> for Json {
    fn from(n: f64) -> Self {
        Json::Float(n)
    }
}

impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(value: Option<T>) -> Self {
        value.map_or(Json::Null, Into::into)
    }
}

impl<T: Into<Json>> From<Vec<T>> for Json {
    fn from(items: Vec<T>) -> Self {
        Json::Arr(items.into_iter().map(Into::into).collect())
    }
}

/// Build a [`Json::Obj`] from `key => value` pairs, in order.
#[macro_export]
macro_rules! obj {
    ($($key:expr => $value:expr),* $(,)?) => {
        $crate::pyjson::Json::Obj(vec![$(($key.to_string(), $crate::pyjson::Json::from($value))),*])
    };
}

impl Json {
    /// Look a key up in an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// A TOML value as `tomllib` would hand it to `json.dumps`.
    pub fn from_toml(value: &toml::Value) -> Self {
        match value {
            toml::Value::String(text) => Json::Str(text.clone()),
            toml::Value::Integer(n) => Json::Int(*n),
            toml::Value::Float(n) => Json::Float(*n),
            toml::Value::Boolean(flag) => Json::Bool(*flag),
            toml::Value::Datetime(when) => Json::Str(when.to_string()),
            toml::Value::Array(items) => Json::Arr(items.iter().map(Json::from_toml).collect()),
            toml::Value::Table(table) => Json::Obj(
                table
                    .iter()
                    .map(|(k, v)| (k.clone(), Json::from_toml(v)))
                    .collect(),
            ),
        }
    }

    /// A parsed `serde_json` value (keys arrive sorted).
    pub fn from_serde(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => Json::Null,
            serde_json::Value::Bool(flag) => Json::Bool(*flag),
            serde_json::Value::Number(n) => n
                .as_i64()
                .map_or_else(|| Json::Float(n.as_f64().unwrap_or(f64::NAN)), Json::Int),
            serde_json::Value::String(text) => Json::Str(text.clone()),
            serde_json::Value::Array(items) => {
                Json::Arr(items.iter().map(Json::from_serde).collect())
            }
            serde_json::Value::Object(map) => Json::Obj(
                map.iter()
                    .map(|(k, v)| (k.clone(), Json::from_serde(v)))
                    .collect(),
            ),
        }
    }
}

/// Python's `repr(float)`: the shortest digits that round-trip, written
/// fixed for decimal exponents in -4..16 and in scientific notation outside.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    // `{:e}` gives the shortest round-trip digits, e.g. "5.9826e1".
    let sci = format!("{:e}", x.abs());
    let (mantissa, exponent) = sci.split_once('e').expect("{:e} has an exponent");
    let exponent: i32 = exponent.parse().expect("integer exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if x.is_sign_negative() { "-" } else { "" };
    let body = if (-4..16).contains(&exponent) {
        if exponent >= 0 {
            let point = exponent as usize + 1;
            if digits.len() <= point {
                format!("{digits}{}.0", "0".repeat(point - digits.len()))
            } else {
                format!("{}.{}", &digits[..point], &digits[point..])
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exponent - 1) as usize))
        }
    } else {
        let mantissa = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        let exp_sign = if exponent < 0 { '-' } else { '+' };
        format!("{mantissa}e{exp_sign}{:02}", exponent.abs())
    };
    format!("{sign}{body}")
}

/// Python's `round(x, places)`: the exact binary value rounded half-even to
/// `places` decimals, which is what Rust's fixed-precision formatting does.
pub fn round(x: f64, places: usize) -> f64 {
    format!("{x:.places$}")
        .parse()
        .expect("formatted float parses")
}

fn string(out: &mut String, text: &str, ascii: bool) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (ascii && !(' '..='~').contains(&c)) => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write(out: &mut String, value: &Json, indent: Option<usize>, ascii: bool, level: usize) {
    let newline = |out: &mut String, level: usize| {
        if let Some(width) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(width * level));
        }
    };
    let separator = if indent.is_some() { "," } else { ", " };
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Json::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Json::Float(n) => out.push_str(&float_repr(*n)),
        Json::Str(text) => string(out, text, ascii),
        Json::Arr(items) if items.is_empty() => out.push_str("[]"),
        Json::Obj(entries) if entries.is_empty() => out.push_str("{}"),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(separator);
                }
                newline(out, level + 1);
                write(out, item, indent, ascii, level + 1);
            }
            newline(out, level);
            out.push(']');
        }
        Json::Obj(entries) => {
            out.push('{');
            for (i, (key, item)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push_str(separator);
                }
                newline(out, level + 1);
                string(out, key, ascii);
                out.push_str(": ");
                write(out, item, indent, ascii, level + 1);
            }
            newline(out, level);
            out.push('}');
        }
    }
}

/// `json.dumps(value, indent=indent, ensure_ascii=ascii)`.
pub fn dumps(value: &Json, indent: Option<usize>, ascii: bool) -> String {
    let mut out = String::new();
    write(&mut out, value, indent, ascii, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_print_as_python_repr() {
        for (x, repr) in [
            (59.826, "59.826"),
            (150.0, "150.0"),
            (0.1, "0.1"),
            (0.30000000000000004, "0.30000000000000004"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (123456789012345.6, "123456789012345.6"),
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (-2.5, "-2.5"),
            (1e22, "1e+22"),
        ] {
            assert_eq!(float_repr(x), repr, "{x}");
        }
    }

    #[test]
    fn round_is_half_even_on_the_exact_value() {
        assert_eq!(round(2.675, 2), 2.67); // 2.67499999... in binary
        assert_eq!(round(0.125, 2), 0.12);
        assert_eq!(round(0.375, 2), 0.38);
        assert_eq!(round(12.345678, 1), 12.3);
    }

    #[test]
    fn layout_matches_json_dumps() {
        let value = obj! {"a" => vec![1i64, 2], "b" => Json::Obj(vec![]), "c" => "é\u{7f}"};
        assert_eq!(
            dumps(&value, None, true),
            "{\"a\": [1, 2], \"b\": {}, \"c\": \"\\u00e9\\u007f\"}"
        );
        assert_eq!(
            dumps(&value, Some(1), false),
            "{\n \"a\": [\n  1,\n  2\n ],\n \"b\": {},\n \"c\": \"é\u{7f}\"\n}"
        );
    }
}
