//! Small shared helpers: CRC-32, strict base64, Python-style line splitting and
//! the error type the report tools print.

use std::fmt;

/// A message for a human; the tools print it prefixed with their name and exit
/// with status 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

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

/// `return Err(Error(format!(...)))`.
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::util::Error(format!($($arg)*)))
    };
}

/// `if !cond { bail!(...) }`.
#[macro_export]
macro_rules! ensure {
    ($cond:expr, $($arg:tt)*) => {
        if !($cond) {
            $crate::bail!($($arg)*);
        }
    };
}

/// CRC-32 (IEEE 802.3, as `binascii.crc32`).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Standard-alphabet base64 with padding, as `base64.b64decode(validate=True)`:
/// any character outside the alphabet, misplaced padding or a length that is
/// not a multiple of four is an error.
pub fn base64_decode(text: &str) -> Result<Vec<u8>> {
    let bytes = text.as_bytes();
    let padding = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    let body = &bytes[..bytes.len() - padding];
    if padding > 2
        || body
            .iter()
            .any(|b| !(b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/'))
    {
        bail!("Non-base64 digit found");
    }
    if !bytes.len().is_multiple_of(4) {
        bail!("Incorrect padding");
    }
    let value = |b: u8| -> u32 {
        match b {
            b'A'..=b'Z' => u32::from(b - b'A'),
            b'a'..=b'z' => u32::from(b - b'a') + 26,
            b'0'..=b'9' => u32::from(b - b'0') + 52,
            b'+' => 62,
            _ => 63,
        }
    };
    let mut out = Vec::with_capacity(body.len() / 4 * 3 + 2);
    for chunk in body.chunks(4) {
        let mut acc = 0u32;
        for &b in chunk {
            acc = acc << 6 | value(b);
        }
        acc <<= 6 * (4 - chunk.len()) as u32;
        let produced = chunk.len() * 6 / 8;
        for i in 0..produced {
            out.push((acc >> (16 - 8 * i)) as u8);
        }
    }
    Ok(out)
}

pub fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let acc = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(acc >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Python's `str.splitlines()`: every line break the standard library honours,
/// with the break dropped (`\r\n` is one break).
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if matches!(
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
        ) {
            lines.push(&text[start..at]);
            start = at + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
                chars.next();
                start += 1;
            }
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Python's `str.split()` with no argument: runs of whitespace separate words.
pub fn split_words(text: &str) -> Vec<&str> {
    text.split(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
        .filter(|word| !word.is_empty())
        .collect()
}

/// Python's `int(text, 16)` for the plain forms the payloads contain.
pub fn parse_hex(text: &str) -> Result<u64> {
    let trimmed = text.trim();
    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(digits, 16)
        .map_err(|_| Error(format!("invalid literal for int() with base 16: {text:?}")))
}

/// A cursor over little-endian fields with Python `struct`-style bounds errors.
pub struct Reader<'a> {
    data: &'a [u8],
    pub offset: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], offset: usize) -> Self {
        Reader { data, offset }
    }

    pub fn bytes(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self.offset + count;
        ensure!(
            end <= self.data.len(),
            "unpack_from requires a buffer of at least {end} bytes for unpacking {count} bytes at offset {} (actual buffer size is {})",
            self.offset,
            self.data.len()
        );
        let slice = &self.data[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }

    pub fn u32s(&mut self, count: usize) -> Result<Vec<u32>> {
        (0..count).map(|_| self.u32()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn base64_round_trips_and_rejects_malformed_text() {
        for data in [&b""[..], b"a", b"ab", b"abc", b"abcd", &[0xFF, 0xFE, 0xFD, 0x00]] {
            assert_eq!(base64_decode(&base64_encode(data)).unwrap(), data);
        }
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert!(base64_decode("TWE").is_err());
        assert!(base64_decode("T*E=").is_err());
        assert!(base64_decode("====").is_err());
        assert!(base64_decode("TW=E").is_err());
    }

    #[test]
    fn python_line_and_word_splitting() {
        assert_eq!(splitlines("a\r\nb\rc\n"), ["a", "b", "c"]);
        assert_eq!(split_words("  x\ty  z\n"), ["x", "y", "z"]);
        assert_eq!(parse_hex("0xFF").unwrap(), 255);
        assert!(parse_hex("zz").is_err());
    }
}
