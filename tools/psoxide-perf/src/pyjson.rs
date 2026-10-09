//! An insertion-ordered JSON value with a reader and a writer that reproduce
//! Python's `json.loads` / `json.dumps` byte for byte (key order, `indent`,
//! `sort_keys`, `ensure_ascii`, float `repr`). The replay store hashes the
//! compact encoding of its identity records and prints the pretty one, so both
//! need the exact bytes the Python tool wrote.

use crate::util::{float_repr, Error, Result};

/// A JSON value. Objects keep the order their keys were first written.
#[derive(Debug, Clone)]
pub enum Json {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// An integer literal.
    Int(i64),
    /// A literal with a fraction or exponent.
    Float(f64),
    /// A string.
    Str(String),
    /// An array.
    Array(Vec<Json>),
    /// An object as ordered `(key, value)` pairs.
    Object(Vec<(String, Json)>),
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

impl From<i64> for Json {
    fn from(value: i64) -> Self {
        Json::Int(value)
    }
}

impl From<u64> for Json {
    fn from(value: u64) -> Self {
        Json::Int(value as i64)
    }
}

impl From<usize> for Json {
    fn from(value: usize) -> Self {
        Json::Int(value as i64)
    }
}

impl From<f64> for Json {
    fn from(value: f64) -> Self {
        Json::Float(value)
    }
}

impl From<bool> for Json {
    fn from(value: bool) -> Self {
        Json::Bool(value)
    }
}

impl Json {
    /// An object from `(key, value)` pairs (later duplicates replace earlier ones).
    pub fn object<K: Into<String>, V: Into<Json>>(pairs: impl IntoIterator<Item = (K, V)>) -> Json {
        let mut object = Json::Object(Vec::new());
        for (key, value) in pairs {
            object.set(key, value.into());
        }
        object
    }

    /// Set `key` in an object, keeping its original position when it exists.
    pub fn set(&mut self, key: impl Into<String>, value: Json) {
        let key = key.into();
        if let Json::Object(pairs) = self {
            match pairs.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => pairs.push((key, value)),
            }
        }
    }

    /// The value for `key` when this is an object that has it.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Mutable access to the value for `key` of an object.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        match self {
            Json::Object(pairs) => pairs.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Mutable access to the elements of an array.
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Json>> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The string, when this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(text) => Some(text),
            _ => None,
        }
    }

    /// The integer, when this is one.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// The elements, when this is an array.
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The pairs, when this is an object.
    pub fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(pairs) => Some(pairs),
            _ => None,
        }
    }

    /// Python truthiness (`bool(value)`).
    pub fn truthy(&self) -> bool {
        match self {
            Json::Null => false,
            Json::Bool(value) => *value,
            Json::Int(value) => *value != 0,
            Json::Float(value) => *value != 0.0,
            Json::Str(text) => !text.is_empty(),
            Json::Array(items) => !items.is_empty(),
            Json::Object(pairs) => !pairs.is_empty(),
        }
    }

    /// Python `==`: objects compare without regard to order and `1 == 1.0`.
    pub fn py_eq(&self, other: &Json) -> bool {
        match (self, other) {
            (Json::Null, Json::Null) => true,
            (Json::Bool(a), Json::Bool(b)) => a == b,
            (Json::Bool(a), Json::Int(b)) | (Json::Int(b), Json::Bool(a)) => i64::from(*a) == *b,
            (Json::Int(a), Json::Int(b)) => a == b,
            (Json::Int(a), Json::Float(b)) | (Json::Float(b), Json::Int(a)) => (*a as f64) == *b,
            (Json::Float(a), Json::Float(b)) => a == b,
            (Json::Str(a), Json::Str(b)) => a == b,
            (Json::Array(a), Json::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.py_eq(y))
            }
            (Json::Object(a), Json::Object(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(key, value)| b.iter().any(|(k, v)| k == key && value.py_eq(v)))
            }
            _ => false,
        }
    }

    /// Python `json.dumps(self, indent=indent, sort_keys=sort_keys)`.
    pub fn dumps(&self, indent: Option<usize>, sort_keys: bool) -> String {
        let mut out = String::new();
        let separators = if indent.is_some() {
            (",", ": ")
        } else {
            (", ", ": ")
        };
        self.write(&mut out, indent, sort_keys, separators, 0);
        out
    }

    /// The compact, key-sorted encoding used for hashing:
    /// `json.dumps(value, sort_keys=True, separators=(',', ':'))`.
    pub fn compact(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, None, true, (",", ":"), 0);
        out
    }

    fn write(
        &self,
        out: &mut String,
        indent: Option<usize>,
        sort_keys: bool,
        sep: (&str, &str),
        depth: usize,
    ) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Int(value) => out.push_str(&value.to_string()),
            Json::Float(value) => out.push_str(&float_json(*value)),
            Json::Str(text) => write_string(out, text),
            Json::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(sep.0);
                    }
                    newline(out, indent, depth + 1);
                    item.write(out, indent, sort_keys, sep, depth + 1);
                }
                newline(out, indent, depth);
                out.push(']');
            }
            Json::Object(pairs) => {
                if pairs.is_empty() {
                    out.push_str("{}");
                    return;
                }
                let mut ordered: Vec<&(String, Json)> = pairs.iter().collect();
                if sort_keys {
                    ordered.sort_by(|a, b| a.0.cmp(&b.0));
                }
                out.push('{');
                for (i, (key, value)) in ordered.into_iter().enumerate() {
                    if i > 0 {
                        out.push_str(sep.0);
                    }
                    newline(out, indent, depth + 1);
                    write_string(out, key);
                    out.push_str(sep.1);
                    value.write(out, indent, sort_keys, sep, depth + 1);
                }
                newline(out, indent, depth);
                out.push('}');
            }
        }
    }

    /// Python `json.loads`.
    pub fn parse(text: &str) -> Result<Json> {
        let mut parser = Parser {
            text: text.as_bytes(),
            position: 0,
        };
        parser.skip_whitespace();
        let value = parser.value()?;
        parser.skip_whitespace();
        if parser.position != parser.text.len() {
            return Err(parser.error("Extra data"));
        }
        Ok(value)
    }
}

fn newline(out: &mut String, indent: Option<usize>, depth: usize) {
    if let Some(width) = indent {
        out.push('\n');
        out.push_str(&" ".repeat(width * depth));
    }
}

fn float_json(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value.is_infinite() {
        if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
    } else {
        float_repr(value)
    }
}

/// Python's `ensure_ascii=True` string encoding.
fn write_string(out: &mut String, text: &str) {
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
            c if (c as u32) < 0x20 || (c as u32) > 0x7f => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    text: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> Error {
        Error(format!("{what}: char {}", self.position))
    }

    fn skip_whitespace(&mut self) {
        while matches!(
            self.text.get(self.position),
            Some(b' ' | b'\t' | b'\n' | b'\r')
        ) {
            self.position += 1;
        }
    }

    fn eat(&mut self, literal: &str) -> bool {
        if self.text[self.position..].starts_with(literal.as_bytes()) {
            self.position += literal.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Json> {
        match self.text.get(self.position) {
            None => Err(self.error("Expecting value")),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(_) if self.eat("null") => Ok(Json::Null),
            Some(_) if self.eat("true") => Ok(Json::Bool(true)),
            Some(_) if self.eat("false") => Ok(Json::Bool(false)),
            Some(_) if self.eat("NaN") => Ok(Json::Float(f64::NAN)),
            Some(_) if self.eat("Infinity") => Ok(Json::Float(f64::INFINITY)),
            Some(_) if self.eat("-Infinity") => Ok(Json::Float(f64::NEG_INFINITY)),
            Some(_) => self.number(),
        }
    }

    fn number(&mut self) -> Result<Json> {
        let start = self.position;
        let mut float = false;
        while let Some(&b) = self.text.get(self.position) {
            match b {
                b'0'..=b'9' | b'-' | b'+' => {}
                b'.' | b'e' | b'E' => float = true,
                _ => break,
            }
            self.position += 1;
        }
        let literal = std::str::from_utf8(&self.text[start..self.position]).unwrap_or("");
        if literal.is_empty() || literal == "-" {
            self.position = start;
            return Err(self.error("Expecting value"));
        }
        if float {
            literal
                .parse::<f64>()
                .map(Json::Float)
                .map_err(|_| self.error("Invalid number"))
        } else {
            literal
                .parse::<i64>()
                .map(Json::Int)
                .map_err(|_| self.error("Invalid number"))
        }
    }

    fn string(&mut self) -> Result<String> {
        self.position += 1; // opening quote
        let mut out: Vec<u16> = Vec::new();
        let mut run = String::new();
        loop {
            let Some(&b) = self.text.get(self.position) else {
                return Err(self.error("Unterminated string"));
            };
            match b {
                b'"' => {
                    self.position += 1;
                    break;
                }
                b'\\' => {
                    flush(&mut run, &mut out);
                    self.position += 1;
                    let Some(&escape) = self.text.get(self.position) else {
                        return Err(self.error("Unterminated string"));
                    };
                    self.position += 1;
                    match escape {
                        b'"' => out.push(u16::from(b'"')),
                        b'\\' => out.push(u16::from(b'\\')),
                        b'/' => out.push(u16::from(b'/')),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(10),
                        b'r' => out.push(13),
                        b't' => out.push(9),
                        b'u' => {
                            let hex = self
                                .text
                                .get(self.position..self.position + 4)
                                .and_then(|h| std::str::from_utf8(h).ok())
                                .and_then(|h| u16::from_str_radix(h, 16).ok())
                                .ok_or_else(|| self.error("Invalid \\uXXXX escape"))?;
                            self.position += 4;
                            out.push(hex);
                        }
                        _ => return Err(self.error("Invalid \\escape")),
                    }
                }
                0..=0x1f => return Err(self.error("Invalid control character at")),
                _ => {
                    // Copy one UTF-8 scalar.
                    let width = match b {
                        0..=0x7f => 1,
                        0xc0..=0xdf => 2,
                        0xe0..=0xef => 3,
                        _ => 4,
                    };
                    let chunk = self
                        .text
                        .get(self.position..self.position + width)
                        .and_then(|c| std::str::from_utf8(c).ok())
                        .ok_or_else(|| self.error("Invalid UTF-8"))?;
                    run.push_str(chunk);
                    self.position += width;
                }
            }
        }
        flush(&mut run, &mut out);
        Ok(String::from_utf16_lossy(&out))
    }

    fn array(&mut self) -> Result<Json> {
        self.position += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.eat("]") {
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value()?);
            self.skip_whitespace();
            if self.eat(",") {
                continue;
            }
            if self.eat("]") {
                return Ok(Json::Array(items));
            }
            return Err(self.error("Expecting ',' delimiter"));
        }
    }

    fn object(&mut self) -> Result<Json> {
        self.position += 1;
        let mut object = Json::Object(Vec::new());
        self.skip_whitespace();
        if self.eat("}") {
            return Ok(object);
        }
        loop {
            self.skip_whitespace();
            if self.text.get(self.position) != Some(&b'"') {
                return Err(self.error("Expecting property name enclosed in double quotes"));
            }
            let key = self.string()?;
            self.skip_whitespace();
            if !self.eat(":") {
                return Err(self.error("Expecting ':' delimiter"));
            }
            self.skip_whitespace();
            let value = self.value()?;
            object.set(key, value);
            self.skip_whitespace();
            if self.eat(",") {
                continue;
            }
            if self.eat("}") {
                return Ok(object);
            }
            return Err(self.error("Expecting ',' delimiter"));
        }
    }
}

/// Move accumulated literal text into the UTF-16 buffer (so surrogate-pair
/// escapes combine across `\uXXXX` units).
fn flush(run: &mut String, out: &mut Vec<u16>) {
    out.extend(run.encode_utf16());
    run.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dumps_matches_python_layouts() {
        let value = Json::object([
            ("b", Json::from(1i64)),
            (
                "a",
                Json::Array(vec![Json::from(2.5), Json::Null, Json::from("x\u{e9}\n")]),
            ),
            ("c", Json::object::<&str, Json>([])),
        ]);
        assert_eq!(
            value.dumps(None, true),
            "{\"a\": [2.5, null, \"x\\u00e9\\n\"], \"b\": 1, \"c\": {}}"
        );
        assert_eq!(
            value.compact(),
            "{\"a\":[2.5,null,\"x\\u00e9\\n\"],\"b\":1,\"c\":{}}"
        );
        assert_eq!(
            value.dumps(Some(2), false),
            "{\n  \"b\": 1,\n  \"a\": [\n    2.5,\n    null,\n    \"x\\u00e9\\n\"\n  ],\n  \"c\": {}\n}"
        );
    }

    #[test]
    fn parse_keeps_order_and_duplicate_key_semantics() {
        let value = Json::parse(r#"{"z": 1, "a": [1, 2.0, "😀"], "z": 3}"#).unwrap();
        assert_eq!(
            value.dumps(None, false),
            "{\"z\": 3, \"a\": [1, 2.0, \"\\ud83d\\ude00\"]}"
        );
        assert!(Json::parse("{").is_err());
        assert!(Json::parse("1 2").is_err());
    }

    #[test]
    fn equality_ignores_object_order() {
        let a = Json::parse(r#"{"x": 1, "y": {"p": 2}}"#).unwrap();
        let b = Json::parse(r#"{"y": {"p": 2.0}, "x": 1}"#).unwrap();
        assert!(a.py_eq(&b));
        assert!(!a.py_eq(&Json::parse(r#"{"x": 1}"#).unwrap()));
    }
}
