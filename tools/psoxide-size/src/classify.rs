//! Put every input section of a link map into one class.
//!
//! The classes answer "what is this byte for": formatting and panic
//! machinery that `-Cpanic=immediate-abort` removes, Debug/Display impls a
//! game derived, strings, tables, SDK code, game code.

use crate::map::{Image, Section};
use rustc_demangle::demangle;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Text,
    Rodata,
    Data,
    Bss,
    Other,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Rodata => "rodata",
            Kind::Data => "data",
            Kind::Bss => "bss",
            Kind::Other => "other",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    /// `core::fmt` and `alloc::fmt` (Display, Debug, `pad_integral`, `Arguments`).
    Fmt,
    /// `core::panicking`, `unwrap_failed`, bounds-check and overflow panics, the SDK panic reporter.
    Panic,
    /// `core::str` validation and `core::char` helpers pulled in by formatting.
    StrChar,
    /// `Vec` and `RawVec` growth paths.
    AllocGrow,
    /// `Debug`/`Display` impls on game or SDK types.
    DebugImpl,
    /// `compiler_builtins` and the hand-written memcpy family.
    Builtins,
    /// A 16-byte `core::panic::Location` record.
    PanicLocation,
    /// A source file name string (`src/foo.rs`).
    FileName,
    /// Any other string literal.
    Str,
    /// An anonymous constant (promoted array, jump table, vtable).
    AnonConst,
    /// A named static or function of an SDK crate (`psx_*`).
    Sdk,
    /// Everything else: the game's own code and statics.
    Game,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Fmt => "fmt",
            Class::Panic => "panic",
            Class::StrChar => "str_char",
            Class::AllocGrow => "alloc_grow",
            Class::DebugImpl => "debug_impl",
            Class::Builtins => "builtins",
            Class::PanicLocation => "panic_location",
            Class::FileName => "file_name",
            Class::Str => "string",
            Class::AnonConst => "anon_const",
            Class::Sdk => "sdk",
            Class::Game => "game",
        }
    }

    /// Classes that `-Cpanic=immediate-abort` removes (or most of): the
    /// formatting and panic machinery and its locations and file names.
    pub fn is_panic_machinery(self) -> bool {
        matches!(
            self,
            Class::Fmt | Class::Panic | Class::StrChar | Class::PanicLocation | Class::FileName
        )
    }
}

pub struct Item<'a> {
    pub sec: &'a Section,
    pub kind: Kind,
    pub class: Class,
    /// The crate the item belongs to (first path segment), `?` when anonymous.
    pub krate: String,
    /// Human name: demangled first symbol, else the demangled section name.
    pub name: String,
    /// The text of a string item (lossy, trimmed), for the report.
    pub text: Option<String>,
}

pub fn kind_of(sec: &Section) -> Kind {
    let n = sec.name.as_str();
    let by_name = |p: &str| n.contains(p);
    if by_name(".text") {
        Kind::Text
    } else if by_name(".rodata") || by_name(".data.rel.ro") {
        Kind::Rodata
    } else if by_name(".bss") || by_name(".sbss") {
        Kind::Bss
    } else if by_name(".data") || by_name(".sdata") {
        Kind::Data
    } else {
        match sec.out.as_str() {
            o if o.contains("text") => Kind::Text,
            o if o.contains("bss") => Kind::Bss,
            o if o.contains("data") => Kind::Data,
            _ => Kind::Other,
        }
    }
}

/// Identifier segments of a (possibly mangled) Rust name, in order.
pub fn tokens(name: &str) -> Vec<String> {
    let shown = format!("{:#}", demangle(name));
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in shown.chars() {
        if c.is_alphanumeric() || c == '_' {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn has(tokens: &[String], w: &str) -> bool {
    tokens.iter().any(|t| t == w)
}

const PANIC_WORDS: &[&str] = &[
    "panicking",
    "panic_fmt",
    "panic_bounds_check",
    "unwrap_failed",
    "expect_failed",
    "slice_index_fail",
    "slice_end_index_len_fail",
    "slice_start_index_len_fail",
    "slice_index_order_fail",
    "slice_error_fail",
    "str_index_overflow_fail",
    "capacity_overflow",
    "handle_alloc_error",
    "assert_failed",
    "report_panic",
    "panic_nounwind",
    "panic_const",
    "unreachable_display",
    "option_expect_failed",
    "result_unwrap_failed",
    "len_mismatch_fail",
];

const STR_WORDS: &[&str] = &[
    "from_utf8",
    "validations",
    "run_utf8_validation",
    "utf8_char_width",
    "next_code_point",
    "from_utf8_unchecked",
    "escape_debug",
    "escape_unicode",
];

const GROW_WORDS: &[&str] = &[
    "finish_grow",
    "grow_one",
    "grow_amortized",
    "reserve_for_push",
    "RawVec",
];

/// Class of a code or named-static item from its name tokens.
fn class_of_named(tokens: &[String], is_code: bool) -> Class {
    let krate = tokens.first().map(String::as_str).unwrap_or("");
    let core_like = matches!(krate, "core" | "alloc" | "compiler_builtins");
    if PANIC_WORDS.iter().any(|w| has(tokens, w)) {
        return Class::Panic;
    }
    if has(tokens, "fmt") && is_code {
        let formatter_trait = [
            "Debug", "Display", "LowerHex", "UpperHex", "Binary", "Octal", "Write",
        ]
        .iter()
        .any(|w| has(tokens, w));
        // `<game::T as core::fmt::Debug>::fmt` starts with the game's crate;
        // `<usize as core::fmt::Display>::fmt` starts with a primitive.
        let first_is_primitive = matches!(
            krate,
            "usize"
                | "isize"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "i8"
                | "i16"
                | "i32"
                | "i64"
                | "str"
                | "bool"
                | "char"
        );
        if has(tokens, "core") || has(tokens, "alloc") {
            if formatter_trait && !core_like && !first_is_primitive {
                return Class::DebugImpl;
            }
            return Class::Fmt;
        }
        if formatter_trait {
            return Class::DebugImpl;
        }
    }
    if is_code && STR_WORDS.iter().any(|w| has(tokens, w)) {
        return Class::StrChar;
    }
    if is_code && GROW_WORDS.iter().any(|w| has(tokens, w)) {
        return Class::AllocGrow;
    }
    if krate == "compiler_builtins" {
        return Class::Builtins;
    }
    if krate.starts_with("psx_") {
        return Class::Sdk;
    }
    Class::Game
}

fn printable(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes
            .iter()
            .all(|&b| b == b'\n' || b == b'\t' || (0x20..0x7f).contains(&b))
}

/// First path segment of a name, skipping `<`, `&`, `mut`, `dyn` noise.
fn crate_of(tokens: &[String]) -> String {
    tokens
        .iter()
        .find(|t| !matches!(t.as_str(), "mut" | "dyn" | "const" | "impl"))
        .cloned()
        .unwrap_or_else(|| "?".into())
}

pub fn classify<'a>(sec: &'a Section, image: Option<&Image>) -> Item<'a> {
    let kind = kind_of(sec);
    let is_code = kind == Kind::Text;
    // Section names look like `.text._ZN...` / `.rodata..Lanon.<hash>.<n>` /
    // `.data.rel.ro._R...`. Demangle the part after the section kind.
    let mangled = sec
        .name
        .find("._R")
        .or_else(|| sec.name.find("._ZN"))
        .map(|at| &sec.name[at + 1..])
        .unwrap_or("");
    let anon = sec.name.contains(".Lanon") || sec.name.contains("anon.") || mangled.is_empty();
    let sym_name = sec
        .syms
        .iter()
        .find(|s| s.size > 0 && !s.name.starts_with(".L"))
        .map(|s| s.name.clone());
    let display = sym_name.clone().unwrap_or_else(|| {
        if anon {
            format!("<anon {}>", sec.name)
        } else {
            format!("{:#}", demangle(mangled))
        }
    });
    let toks = match (&sym_name, anon) {
        (Some(_), _) => tokens(&display),
        (None, false) => tokens(mangled),
        (None, true) => Vec::new(),
    };
    let mut krate = if toks.is_empty() {
        "?".to_string()
    } else {
        crate_of(&toks)
    };
    let mut text = None;
    let class = if anon && kind != Kind::Text && kind != Kind::Bss {
        classify_anon(sec, image, &mut text)
    } else if toks.is_empty() {
        Class::Game
    } else {
        class_of_named(&toks, is_code)
    };
    if class.is_panic_machinery() && krate == "?" {
        krate = "core".into();
    }
    Item {
        sec,
        kind,
        class,
        krate,
        name: display,
        text,
    }
}

fn classify_anon(sec: &Section, image: Option<&Image>, text: &mut Option<String>) -> Class {
    let Some(img) = image else {
        // No bytes: a 16-byte word-aligned anonymous constant is most often a
        // `Location`, but only the EXE can say.
        return Class::AnonConst;
    };
    let Some(bytes) = img.at(sec.vma, sec.size) else {
        return Class::AnonConst;
    };
    // `core::panic::Location`: { file ptr, file len, line, col }.
    if sec.size == 16 {
        let w = |i: usize| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        let (ptr, len) = (w(0), w(1));
        if (0x8001_0000..0x8020_0000).contains(&ptr)
            && len > 3
            && len < 200
            && w(2) < 1_000_000
            && w(3) < 4000
        {
            if let Some(name) = img.at(ptr, len) {
                if name.ends_with(b".rs") && printable(name) {
                    return Class::PanicLocation;
                }
            }
        }
    }
    // Merged string sections hold several NUL separated strings.
    let trimmed: Vec<u8> = bytes.iter().copied().filter(|&b| b != 0).collect();
    if sec.align <= 4
        && printable(&trimmed)
        && trimmed.len() * 10 >= bytes.len() * 9
        && trimmed.len() >= 2
    {
        let s = String::from_utf8_lossy(bytes).replace('\0', "|");
        let snippet: String = s.chars().take(64).collect();
        *text = Some(snippet);
        if s.contains(".rs") && s.contains('/') && s.len() < 160 {
            return Class::FileName;
        }
        return Class::Str;
    }
    Class::AnonConst
}

/// Group key for monomorphization duplicates: the demangled name with every
/// turbofish and type-argument list removed. A leading `<Type as Trait>`
/// qualifier keeps its first level (so impls of different types stay apart)
/// but loses nested arguments.
pub fn generic_base(name: &str) -> String {
    let qualified = name.starts_with('<');
    let mut depth = 0u32;
    let mut seen_first_group = false;
    let mut out = String::new();
    for c in name.chars() {
        match c {
            '<' => {
                depth += 1;
                if qualified && depth == 1 && !seen_first_group {
                    out.push(c);
                }
            }
            '>' if depth > 0 => {
                if qualified && depth == 1 && !seen_first_group {
                    out.push(c);
                    seen_first_group = true;
                }
                depth -= 1;
            }
            _ if depth == 0 => out.push(c),
            _ if qualified && depth == 1 && !seen_first_group => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(name: &str, syms: &[&str]) -> Section {
        Section {
            out: ".text".into(),
            object: "x.o".into(),
            name: name.into(),
            vma: 0x8001_0000,
            size: 64,
            align: 4,
            syms: syms
                .iter()
                .map(|s| crate::map::Sym {
                    name: s.to_string(),
                    vma: 0x8001_0000,
                    size: 64,
                })
                .collect(),
        }
    }

    #[test]
    fn core_fmt_and_panic_are_machinery() {
        let a = sec(
            ".text._ZN4core3fmt9Formatter12pad_integral17h0123456789abcdefE",
            &["core::fmt::Formatter::pad_integral"],
        );
        assert_eq!(classify(&a, None).class, Class::Fmt);
        let b = sec(".text.x", &["core::panicking::panic_bounds_check"]);
        assert_eq!(classify(&b, None).class, Class::Panic);
        let c = sec(".text.x", &["core::str::validations::next_code_point"]);
        assert_eq!(classify(&c, None).class, Class::StrChar);
    }

    #[test]
    fn game_debug_impl_is_not_core_fmt() {
        let a = sec(".text.x", &["<hl_psx::Weapon as core::fmt::Debug>::fmt"]);
        assert_eq!(classify(&a, None).class, Class::DebugImpl);
        let b = sec(".text.x", &["<usize as core::fmt::Display>::fmt"]);
        assert_eq!(classify(&b, None).class, Class::Fmt);
    }

    #[test]
    fn generic_arguments_are_stripped() {
        assert_eq!(
            generic_base("<alloc::vec::Vec<hl::T>>::push::<u8>"),
            "<alloc::vec::Vec>::push::"
        );
    }

    #[test]
    fn sdk_and_game_split_on_crate() {
        assert_eq!(
            classify(&sec(".text.x", &["psx_gpu::draw"]), None).class,
            Class::Sdk
        );
        assert_eq!(
            classify(&sec(".text.x", &["hl_psx::play"]), None).class,
            Class::Game
        );
    }
}
