//! Small text helpers with the exact semantics the checks rely on.
//!
//! The detector reads objdump-syntax operand text (`lw a0,4(sp)`), so the
//! way it splits, strips and parses that text is part of what it detects.
//! These helpers keep those rules in one place: comma splitting that keeps
//! empty fields, whitespace stripping, integer literals with an optional
//! `0x` prefix, and the few fixed patterns the tools match.

/// True for the characters `str.strip()` removes in the original tools
/// (Unicode whitespace plus the ASCII separators 0x1c..0x1f).
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Strip leading and trailing whitespace.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// Split on commas, keeping empty fields, and strip each field.
pub fn fields(s: &str) -> Vec<&str> {
    s.split(',').map(strip).collect()
}

/// The text with every space removed.
pub fn squeeze(s: &str) -> String {
    s.replace(' ', "")
}

/// An integer literal: optional sign, then `0x`/`0o`/`0b` and digits, or a
/// decimal with no leading zero (`0` itself excepted); underscores may sit
/// between digits. `None` where the original `int(text, 0)` raised.
pub fn int_auto(text: &str) -> Option<i64> {
    let s = strip(text);
    let (negative, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let lower = body.to_ascii_lowercase();
    let (radix, digits) = if let Some(rest) = lower.strip_prefix("0x") {
        (16, rest.strip_prefix('_').unwrap_or(rest))
    } else if let Some(rest) = lower.strip_prefix("0o") {
        (8, rest.strip_prefix('_').unwrap_or(rest))
    } else if let Some(rest) = lower.strip_prefix("0b") {
        (2, rest.strip_prefix('_').unwrap_or(rest))
    } else {
        if lower.len() > 1
            && lower.starts_with('0')
            && lower.bytes().any(|b| b != b'0' && b != b'_')
        {
            return None;
        }
        (10, lower.as_str())
    };
    let value = digits_value(digits, radix)?;
    Some(if negative { -value } else { value })
}

/// Digits in `radix` with single underscores allowed between them.
fn digits_value(digits: &str, radix: u32) -> Option<i64> {
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let mut value: i64 = 0;
    for c in digits.chars().filter(|&c| c != '_') {
        let digit = c.to_digit(radix)?;
        value = value
            .checked_mul(i64::from(radix))?
            .checked_add(i64::from(digit))?;
    }
    Some(value)
}

/// An integer in base 16, with an optional sign and `0x` prefix.
pub fn int_hex(text: &str) -> Option<i64> {
    let s = strip(text);
    let (negative, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let lower = body.to_ascii_lowercase();
    let digits = lower.strip_prefix("0x").map_or(lower.as_str(), |rest| {
        rest.strip_prefix('_').unwrap_or(rest)
    });
    let value = digits_value(digits, 16)?;
    Some(if negative { -value } else { value })
}

/// The hex number at the end of `args` after the leftmost `0x` that
/// starts one (`0x([0-9a-f]+)$`): a branch or jump target.
pub fn trailing_hex(args: &str) -> Option<i64> {
    let bytes = args.as_bytes();
    for i in 0..bytes.len().saturating_sub(2) {
        if &bytes[i..i + 2] == b"0x" {
            let rest = &args[i + 2..];
            if !rest.is_empty()
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return i64::from_str_radix(rest, 16).ok();
            }
        }
    }
    None
}

/// The whole text as `0x` and lowercase hex digits (`0x([0-9a-f]+)`).
pub fn whole_hex(text: &str) -> Option<i64> {
    let rest = text.strip_prefix("0x")?;
    if rest.is_empty()
        || !rest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    i64::from_str_radix(rest, 16).ok()
}

/// True for `[a-z0-9]+`.
fn is_reg_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// The register inside the leftmost `(reg)` of an operand (`4(sp)` gives
/// `sp`), `\(([a-z0-9]+)\)`.
pub fn paren_register(source: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(open) = source[from..].find('(') {
        let start = from + open + 1;
        let name_len = source[start..]
            .bytes()
            .take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            .count();
        if name_len > 0 && source[start + name_len..].starts_with(')') {
            return Some(&source[start..start + name_len]);
        }
        from = start;
    }
    None
}

/// `[a-z0-9]+,(-?\d+)\(([a-z0-9]+)\)` over the whole text: a load's
/// offset and base register.
pub fn load_operands(text: &str) -> Option<(i64, &str)> {
    let (dest, rest) = text.split_once(',')?;
    if !is_reg_name(dest) {
        return None;
    }
    let (offset, rest) = rest.split_once('(')?;
    let base = rest.strip_suffix(')')?;
    if !is_reg_name(base) || !is_decimal(offset) {
        return None;
    }
    Some((offset.parse().ok()?, base))
}

/// `-?\d+` over the whole text.
pub fn is_decimal(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// `sp,sp,(-?\d+)` over the whole text: an `addiu` frame adjustment.
pub fn sp_adjust(text: &str) -> Option<i64> {
    let amount = text.strip_prefix("sp,sp,")?;
    if is_decimal(amount) {
        amount.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_literals() {
        assert_eq!(int_auto("0x8001"), Some(0x8001));
        assert_eq!(int_auto("-24"), Some(-24));
        assert_eq!(int_auto(" 7 "), Some(7));
        assert_eq!(int_auto("0"), Some(0));
        assert_eq!(int_auto("00"), Some(0));
        assert_eq!(int_auto("012"), None);
        assert_eq!(int_auto("zero"), None);
        assert_eq!(int_auto(""), None);
        assert_eq!(int_hex("80012298"), Some(0x8001_2298));
        assert_eq!(int_hex("0x10"), Some(16));
    }

    #[test]
    fn patterns() {
        assert_eq!(trailing_hex("a2,0x80012345"), Some(0x8001_2345));
        assert_eq!(trailing_hex("0x0x12"), Some(0x12));
        assert_eq!(trailing_hex("ra"), None);
        assert_eq!(paren_register("4(sp)"), Some("sp"));
        assert_eq!(paren_register("$9"), None);
        assert_eq!(load_operands("at,-32(v1)"), Some((-32, "v1")));
        assert_eq!(load_operands("$9,0(at)"), None);
        assert_eq!(sp_adjust("sp,sp,-24"), Some(-24));
        assert_eq!(fields(""), [""]);
        assert_eq!(fields("a, b"), ["a", "b"]);
    }
}
