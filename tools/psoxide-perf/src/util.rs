//! Shared helpers: the error type, an insertion-ordered counter map, a CSV
//! reader with `csv.DictReader` semantics, and Python-compatible number
//! formatting so the reports stay byte-identical with the scripts they replaced.

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;

/// A message for a human; the binary prints it and exits non-zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

/// Result alias used across the crate.
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error(error.to_string())
    }
}

impl From<&str> for Error {
    fn from(text: &str) -> Self {
        Error(text.to_string())
    }
}

impl From<String> for Error {
    fn from(text: String) -> Self {
        Error(text)
    }
}

/// `return Err(Error(format!(...)))`.
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::util::Error(format!($($arg)*)))
    };
}

/// A map that remembers first-insertion order, like a Python `dict`. The
/// reports sort with stable sorts, so ties resolve by the order a key first
/// appeared, exactly as they did in the scripts.
#[derive(Debug, Clone)]
pub struct OrderedMap<K, V> {
    index: HashMap<K, usize>,
    entries: Vec<(K, V)>,
}

impl<K: Hash + Eq + Clone, V> Default for OrderedMap<K, V> {
    fn default() -> Self {
        Self {
            index: HashMap::new(),
            entries: Vec::new(),
        }
    }
}

impl<K: Hash + Eq + Clone, V> OrderedMap<K, V> {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// The value for `key`, inserting `make()` first when absent.
    pub fn entry_or_insert_with(&mut self, key: K, make: impl FnOnce() -> V) -> &mut V {
        let position = match self.index.get(&key) {
            Some(&position) => position,
            None => {
                let position = self.entries.len();
                self.index.insert(key.clone(), position);
                self.entries.push((key, make()));
                position
            }
        };
        &mut self.entries[position].1
    }

    /// The value for `key`, if present.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.index
            .get(key)
            .map(|&position| &self.entries[position].1)
    }

    /// Entries in first-insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &(K, V)> {
        self.entries.iter()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the map holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Add `amount` to the counter for `key` (a `defaultdict(int)` update).
pub fn add_count<K: Hash + Eq + Clone>(map: &mut OrderedMap<K, i64>, key: K, amount: i64) {
    *map.entry_or_insert_with(key, || 0) += amount;
}

/// Split `text` into records the way `csv.reader` does for the dialects these
/// tools read: comma separated, `"` quoting with `""` escapes, LF or CRLF line
/// ends. A blank line yields an empty record.
pub fn csv_records(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut field_started = false;
    let mut at_field_start = true;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if at_field_start => {
                in_quotes = true;
                field_started = true;
                at_field_start = false;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                field_started = true;
                at_field_start = true;
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                if field_started || !field.is_empty() || !record.is_empty() {
                    record.push(std::mem::take(&mut field));
                }
                records.push(std::mem::take(&mut record));
                field_started = false;
                at_field_start = true;
            }
            _ => {
                field.push(c);
                field_started = true;
                at_field_start = false;
            }
        }
    }
    if field_started || !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

/// One `csv.DictReader` row: values by header name; a short row leaves the
/// trailing columns empty (Python's `None`).
#[derive(Debug, Clone)]
pub struct DictRow {
    names: std::rc::Rc<Vec<String>>,
    values: Vec<String>,
}

impl DictRow {
    /// The raw text of column `name`; an absent trailing column reads as empty.
    pub fn get(&self, name: &str) -> Option<&str> {
        let position = self.names.iter().position(|n| n == name)?;
        Some(self.values.get(position).map_or("", String::as_str))
    }

    /// Column names in header order.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// True when the row has no cell for column `name` at all (a short row).
    pub fn is_missing(&self, name: &str) -> bool {
        self.names
            .iter()
            .position(|n| n == name)
            .is_none_or(|position| position >= self.values.len())
    }
}

/// Read a CSV file as `csv.DictReader` does: the first record names the
/// columns, blank records are skipped. Returns the header and the rows.
pub fn dict_rows(text: &str) -> Result<(Vec<String>, Vec<DictRow>)> {
    let mut records = csv_records(text).into_iter();
    let Some(header) = records.next() else {
        return Ok((Vec::new(), Vec::new()));
    };
    let names = std::rc::Rc::new(header.clone());
    let mut rows = Vec::new();
    for values in records {
        if values.is_empty() {
            continue;
        }
        if values.len() > names.len() {
            return Err(Error(format!(
                "row has {} fields but the header names {}",
                values.len(),
                names.len()
            )));
        }
        rows.push(DictRow {
            names: names.clone(),
            values,
        });
    }
    Ok((header, rows))
}

/// Python's `int(text)` for the plain forms these logs use: optional sign and
/// decimal digits, surrounding whitespace allowed.
pub fn parse_int(text: &str) -> Result<i64> {
    text.trim()
        .parse::<i64>()
        .map_err(|_| Error(format!("invalid literal for int(): {text:?}")))
}

/// Python's `int(text, 16)`: optional sign, optional `0x` prefix.
pub fn parse_hex(text: &str) -> Result<i64> {
    let trimmed = text.trim();
    let (negative, body) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let digits = body
        .strip_prefix("0x")
        .or_else(|| body.strip_prefix("0X"))
        .unwrap_or(body);
    let value = i64::from_str_radix(digits, 16)
        .map_err(|_| Error(format!("invalid literal for int() with base 16: {text:?}")))?;
    Ok(if negative { -value } else { value })
}

/// `f"{value:,}"` for an integer.
pub fn group_thousands(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    if value < 0 {
        format!("-{grouped}")
    } else {
        grouped
    }
}

/// `f"{value:,.{places}f}"`.
pub fn group_float(value: f64, places: usize) -> String {
    let text = format!("{value:.places$}");
    let (sign, body) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text.as_str()),
    };
    let (whole, fraction) = match body.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (body, None),
    };
    let mut grouped = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    match fraction {
        Some(fraction) => format!("{sign}{grouped}.{fraction}"),
        None => format!("{sign}{grouped}"),
    }
}

/// Python's `round(value, places)`: the correctly rounded decimal of the exact
/// binary value, ties to even, parsed back to a float.
pub fn round_places(value: f64, places: usize) -> f64 {
    format!("{value:.places$}").parse().unwrap_or(value)
}

/// Python's `round(value)` (ties to even) as an integer.
pub fn round_int(value: f64) -> i64 {
    value.round_ties_even() as i64
}

/// Python's `repr(float)`: the shortest round-trip digits, with an exponent
/// when the decimal exponent is below -4 or at least 16.
pub fn float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_string();
    }
    // `{:e}` yields the shortest digits as `d.ddde<exp>`.
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    if (-4..16).contains(&exponent) {
        let position = exponent + 1;
        let body = if position <= 0 {
            format!("0.{}{}", "0".repeat((-position) as usize), digits)
        } else if (position as usize) >= digits.len() {
            format!(
                "{}{}.0",
                digits,
                "0".repeat(position as usize - digits.len())
            )
        } else {
            format!(
                "{}.{}",
                &digits[..position as usize],
                &digits[position as usize..]
            )
        };
        format!("{sign}{body}")
    } else {
        let mut head = digits[..1].to_string();
        if digits.len() > 1 {
            head.push('.');
            head.push_str(&digits[1..]);
        }
        let exp_sign = if exponent < 0 { '-' } else { '+' };
        format!("{sign}{head}e{exp_sign}{:02}", exponent.abs())
    }
}

/// One command-line token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// `--flag` (the `=value` form is split off and kept for [`Cli::value`]).
    Flag(String),
    /// A bare argument.
    Positional(String),
}

/// A minimal `argparse`-style command line: `--flag value`, `--flag=value`
/// and positionals, with usage errors reported as `Err(message)`.
pub struct Cli<'a> {
    args: &'a [String],
    position: usize,
    inline: Option<String>,
}

impl<'a> Cli<'a> {
    /// Start reading `args`.
    pub fn new(args: &'a [String]) -> Self {
        Self {
            args,
            position: 0,
            inline: None,
        }
    }

    /// The next token, if any.
    pub fn next_token(&mut self) -> Option<Token> {
        let arg = self.args.get(self.position)?;
        self.position += 1;
        self.inline = None;
        if let Some(rest) = arg.strip_prefix("--") {
            if let Some((name, value)) = rest.split_once('=') {
                self.inline = Some(value.to_string());
                return Some(Token::Flag(format!("--{name}")));
            }
            return Some(Token::Flag(arg.clone()));
        }
        Some(Token::Positional(arg.clone()))
    }

    /// The value of the flag just returned by [`Cli::next_token`].
    pub fn value(&mut self, flag: &str) -> std::result::Result<String, String> {
        if let Some(value) = self.inline.take() {
            return Ok(value);
        }
        let value = self
            .args
            .get(self.position)
            .cloned()
            .ok_or_else(|| format!("argument {flag}: expected one argument"))?;
        self.position += 1;
        Ok(value)
    }

    /// The flag's value parsed as an integer.
    pub fn int<T: std::str::FromStr>(&mut self, flag: &str) -> std::result::Result<T, String> {
        let text = self.value(flag)?;
        text.parse()
            .map_err(|_| format!("argument {flag}: invalid int value: '{text}'"))
    }
}

/// Print a usage error the way `argparse` does and return its exit status.
pub fn usage_error(usage: &str, message: &str) -> i32 {
    eprintln!("usage: {usage}\nerror: {message}");
    2
}

/// Python's `str.splitlines()` (without line ends): breaks on LF, CR, CRLF and
/// the other Unicode line boundaries.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut iter = text.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        let is_break = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        lines.push(&text[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = iter.peek() {
                iter.next();
                end = j + 1;
            }
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Read a whole file as UTF-8 text, naming the path on failure.
pub fn read_text(path: &std::path::Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| Error(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_python() {
        assert_eq!(float_repr(1.0), "1.0");
        assert_eq!(float_repr(0.5), "0.5");
        assert_eq!(float_repr(100.0), "100.0");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1.5e16), "1.5e+16");
        assert_eq!(float_repr(123456789012345.0), "123456789012345.0");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(0.00001), "1e-05");
        assert_eq!(float_repr(0.000123), "0.000123");
        assert_eq!(float_repr(-2.5), "-2.5");
        assert_eq!(float_repr(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(float_repr(59.94), "59.94");
        assert_eq!(float_repr(1234.5678), "1234.5678");
    }

    #[test]
    fn grouping_matches_python() {
        assert_eq!(group_thousands(1234567), "1,234,567");
        assert_eq!(group_thousands(-999), "-999");
        assert_eq!(group_thousands(1000), "1,000");
        assert_eq!(group_float(1234567.5, 0), "1,234,568");
        assert_eq!(group_float(999.4, 0), "999");
        assert_eq!(group_float(1000.0, 1), "1,000.0");
    }

    #[test]
    fn rounding_is_half_even_on_exact_ties() {
        assert_eq!(round_places(0.125, 2), 0.12);
        assert_eq!(round_places(2.675, 2), 2.67);
        assert_eq!(round_int(2.5), 2);
        assert_eq!(round_int(3.5), 4);
    }

    #[test]
    fn csv_reader_handles_quotes_and_blank_lines() {
        let records = csv_records("a,b\r\n\"x,1\",\"he said \"\"hi\"\"\"\n\n3,4");
        assert_eq!(records[0], ["a", "b"]);
        assert_eq!(records[1], ["x,1", "he said \"hi\""]);
        assert!(records[2].is_empty());
        assert_eq!(records[3], ["3", "4"]);
    }

    #[test]
    fn dict_rows_leave_short_rows_empty() {
        let (header, rows) = dict_rows("a,b,c\n1,2\n\n4,5,6\n").unwrap();
        assert_eq!(header, ["a", "b", "c"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("c"), Some(""));
        assert!(rows[0].is_missing("c"));
        assert_eq!(rows[1].get("c"), Some("6"));
    }
}
