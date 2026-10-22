//! A small XML reader for the SVG subset: elements and attributes only.
//!
//! Text nodes, comments, processing instructions, the doctype and CDATA are
//! skipped. Entity references in attribute values are decoded for the five
//! predefined entities and numeric references.

use std::fmt;

/// One element with its attributes and child elements.
#[derive(Clone, Debug, Default)]
pub struct Element {
    /// Tag name with any `svg:` prefix removed.
    pub name: String,
    /// Attributes in source order, values entity-decoded.
    pub attrs: Vec<(String, String)>,
    /// Child elements in source order.
    pub children: Vec<Element>,
}

impl Element {
    /// The value of attribute `name`.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A syntax error with the byte offset it was found at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XmlError {
    /// Byte offset into the source.
    pub offset: usize,
    /// What was wrong.
    pub message: String,
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "XML error at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for XmlError {}

struct Reader<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn err<T>(&self, message: &str) -> Result<T, XmlError> {
        Err(XmlError {
            offset: self.pos,
            message: message.to_string(),
        })
    }

    fn starts_with(&self, s: &str) -> bool {
        self.bytes[self.pos..].starts_with(s.as_bytes())
    }

    fn skip_until(&mut self, end: &str) -> Result<(), XmlError> {
        match self.src[self.pos..].find(end) {
            Some(i) => {
                self.pos += i + end.len();
                Ok(())
            }
            None => self.err(&format!("unterminated construct, expected `{end}`")),
        }
    }

    fn skip_space(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn name(&mut self) -> Result<String, XmlError> {
        let start = self.pos;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.') {
                self.pos += 1;
            } else {
                break;
            }
        }
        if start == self.pos {
            return self.err("expected a name");
        }
        Ok(self.src[start..self.pos].to_string())
    }

    /// Skip text and non-element markup up to the next `<` that opens or
    /// closes an element.
    fn skip_misc(&mut self) -> Result<(), XmlError> {
        loop {
            while self.pos < self.bytes.len() && self.bytes[self.pos] != b'<' {
                self.pos += 1;
            }
            if self.starts_with("<!--") {
                self.skip_until("-->")?;
            } else if self.starts_with("<![CDATA[") {
                self.skip_until("]]>")?;
            } else if self.starts_with("<?") {
                self.skip_until("?>")?;
            } else if self.starts_with("<!") {
                // DOCTYPE, possibly with an internal subset in brackets.
                let mut depth = 0i32;
                while self.pos < self.bytes.len() {
                    match self.bytes[self.pos] {
                        b'[' => depth += 1,
                        b']' => depth -= 1,
                        b'>' if depth <= 0 => {
                            self.pos += 1;
                            break;
                        }
                        _ => {}
                    }
                    self.pos += 1;
                }
            } else {
                return Ok(());
            }
        }
    }

    fn element(&mut self) -> Result<Element, XmlError> {
        self.pos += 1; // '<'
        let raw = self.name()?;
        let mut el = Element {
            name: raw.strip_prefix("svg:").unwrap_or(&raw).to_string(),
            ..Element::default()
        };
        loop {
            self.skip_space();
            if self.pos >= self.bytes.len() {
                return self.err("unexpected end of input in a tag");
            }
            match self.bytes[self.pos] {
                b'/' => {
                    self.pos += 1;
                    if self.bytes.get(self.pos) != Some(&b'>') {
                        return self.err("expected `>` after `/`");
                    }
                    self.pos += 1;
                    return Ok(el);
                }
                b'>' => {
                    self.pos += 1;
                    break;
                }
                _ => {
                    let key = self.name()?;
                    self.skip_space();
                    if self.bytes.get(self.pos) != Some(&b'=') {
                        return self.err("expected `=` after an attribute name");
                    }
                    self.pos += 1;
                    self.skip_space();
                    let quote = match self.bytes.get(self.pos) {
                        Some(&q) if q == b'"' || q == b'\'' => q,
                        _ => return self.err("expected a quoted attribute value"),
                    };
                    self.pos += 1;
                    let start = self.pos;
                    while self.pos < self.bytes.len() && self.bytes[self.pos] != quote {
                        self.pos += 1;
                    }
                    if self.pos >= self.bytes.len() {
                        return self.err("unterminated attribute value");
                    }
                    let value = decode_entities(&self.src[start..self.pos]);
                    self.pos += 1;
                    el.attrs.push((key, value));
                }
            }
        }
        loop {
            self.skip_misc()?;
            if self.pos >= self.bytes.len() {
                return self.err(&format!("element <{}> is not closed", el.name));
            }
            if self.starts_with("</") {
                self.pos += 2;
                let close = self.name()?;
                let close = close.strip_prefix("svg:").unwrap_or(&close);
                if close != el.name {
                    return self.err(&format!("</{close}> closes <{}>", el.name));
                }
                self.skip_space();
                if self.bytes.get(self.pos) != Some(&b'>') {
                    return self.err("expected `>` in a closing tag");
                }
                self.pos += 1;
                return Ok(el);
            }
            let child = self.element()?;
            el.children.push(child);
        }
    }
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else { break };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Parse `src` and return its root element.
pub fn parse(src: &str) -> Result<Element, XmlError> {
    let mut reader = Reader {
        src,
        bytes: src.as_bytes(),
        pos: 0,
    };
    reader.skip_misc()?;
    if reader.pos >= reader.bytes.len() {
        return reader.err("no root element");
    }
    reader.element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_elements_attributes_and_entities() {
        let root = parse(
            "<?xml version=\"1.0\"?><!-- c --><svg a='1'><g id=\"x&amp;y\"><path d=\"M0 0\"/></g></svg>",
        )
        .unwrap();
        assert_eq!(root.name, "svg");
        assert_eq!(root.attr("a"), Some("1"));
        assert_eq!(root.children[0].attr("id"), Some("x&y"));
        assert_eq!(root.children[0].children[0].name, "path");
    }

    #[test]
    fn rejects_mismatched_tags() {
        assert!(parse("<a><b></a></b>").is_err());
    }
}
