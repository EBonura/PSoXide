//! `check-mfc0`: flag MFC0/MFC2/CFC2 load-delay hazards in guest assembly source.
//!
//! On the R3000 the destination of `mfc0`/`mfc2` arrives one instruction
//! late: the very next instruction reads the STALE register. This has shipped
//! twice (psx-rt's `enable_cpu_interrupts` historically, and the demo-disc
//! loader's `quiesce()`, where the stale value landed in SR with BEV set and
//! every interrupt vectored into the ROM). The emulator models the hazard
//! faithfully, but only code that runs under it gets caught; this check reads
//! the source instead.
//!
//! Two checks, inside `.rs` files, for every line containing `mfc0`, `mfc2` or `cfc2` with a
//! register destination (`$t0`, `$8` or an `{operand}`):
//!
//! 1. the next asm-looking line (within three) must not mention the same register. A `nop`, or
//!    any line not touching the register, is fine;
//! 2. the move must not be the last instruction of its `asm!` block. The consumer then sits in
//!    the next block, and the compiler is free to put it in the very next instruction: HL and CS
//!    shipped `asm!("mfc0 $8, $12", lateout("$8") sr)` followed by `sr |= ..` for weeks, and it
//!    set SR.IsC by layout luck. End the block with a `nop`.
//!
//! Scans `sdk/` and `engine/` by default; any directory can be named, so a game checks its own
//! sources with the `check-mfc0` binary of `psoxide-hazard`.

use std::fs;
use std::path::{Component, Path, PathBuf};

use regex::Regex;

/// A move from a coprocessor register and its destination: `$t0`, `$8` or `{operand}`.
const MOVE: &str = r"\b(?:mfc[02]|cfc2)\s+(\$\w+|\{\w*\})";

/// `str.splitlines()`: Python also breaks on `\v`, `\f`, `\x1c`-`\x1e`,
/// NEL and the Unicode line and paragraph separators.
#[must_use]
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if boundary {
            lines.push(&text[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|&(_, next)| next == '\n') {
                chars.next();
                end += 1;
            }
            start = end;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `str.strip()` with no arguments.
#[must_use]
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// The first `"` of `text` that is not escaped.
fn closing_quote(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some(i),
            _ => escaped = false,
        }
    }
    None
}

/// True when the move found at `line[end..]` is the last instruction of its `asm!` block: its
/// string literal closes on this line and no further instruction string follows, in the call or
/// on the next line that is not a comment.
fn ends_asm_block(line: &str, end: usize, following: &[&str]) -> bool {
    // Skip the rest of the operands (`, $12`), then expect the closing quote.
    let Some(close) = closing_quote(&line[end..]) else {
        return false; // a raw or multi-line string: the next line is part of the same text
    };
    let inside = &line[end..end + close];
    if inside.contains(';') || inside.contains("\\n") {
        return false; // more instructions in the same literal
    }
    let rest = line[end + close + 1..]
        .trim_start()
        .trim_start_matches(',')
        .trim_start();
    if !rest.is_empty() && !rest.starts_with("//") {
        // `, lateout("$8") sr)` is operands; `, "nop"` is another instruction.
        return !rest.starts_with('"');
    }
    for next in following {
        let next = py_strip(next);
        if next.is_empty() || next.starts_with("//") {
            continue;
        }
        return !next.starts_with('"');
    }
    true
}

fn check_file(path: &Path, mfc: &Regex) -> Vec<String> {
    let bytes = fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    let lines = splitlines(&text);
    let mut problems = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(found) = mfc.captures(line) else {
            continue;
        };
        let operand = found.get(1).expect("one group");
        if line
            .find("//")
            .is_some_and(|comment| comment < operand.start())
        {
            continue; // prose or a note beside encoded words, not an instruction
        }
        let register = operand.as_str();
        // `{t}` operands are not words, so `\b` after them would never match.
        let pattern = if register.starts_with('{') {
            regex::escape(register)
        } else {
            format!(r"{}\b", regex::escape(register))
        };
        let reads = Regex::new(&pattern).expect("escaped register");
        // The delay slot is the next line that looks like an instruction.
        for next in lines.iter().skip(i + 1).take(3) {
            let stripped = py_strip(next).trim_matches(['"', ',']);
            if stripped.is_empty() || stripped.starts_with("//") || stripped.starts_with('#') {
                continue;
            }
            // A read that is not itself another move's destination operand.
            let read = reads.find_iter(stripped).any(|m| {
                let before = &stripped[..m.start()];
                !(before.ends_with("mfc0 ")
                    || before.ends_with("mfc2 ")
                    || before.ends_with("cfc2 "))
            });
            if read && !stripped.contains("nop") {
                problems.push(format!(
                    "{}:{}: `{}` is followed by `{stripped}` which reads {register} in the load-delay slot",
                    path.display(),
                    i + 1,
                    py_strip(line)
                ));
            }
            break;
        }
        let end = operand.end();
        if line[..operand.start()].contains('"') && ends_asm_block(line, end, &lines[i + 1..]) {
            problems.push(format!(
                "{}:{}: `{}` ends its asm block, so the next block can read {register} in the \
                 load-delay slot; add a `nop`",
                path.display(),
                i + 1,
                py_strip(line)
            ));
        }
    }
    problems
}

/// Every `.rs` file under `root`, sorted part by part as Python sorts
/// `Path`s, without descending into symlinked directories.
fn rust_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") && path.is_file() {
            out.push(path);
        }
    }
}

/// `Path("./sdk/")` prints as `sdk`.
fn normalized(path: &str) -> PathBuf {
    let parts: PathBuf = Path::new(path)
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    if parts.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parts
    }
}

/// Scan `paths` (default `sdk` and `engine`); the exit code is 1 when any
/// hazard is found.
///
/// # Panics
///
/// Never in practice: the only `expect` is on a constant pattern.
#[must_use]
pub fn main(paths: &[String]) -> i32 {
    let roots: Vec<PathBuf> = if paths.is_empty() {
        vec![PathBuf::from("sdk"), PathBuf::from("engine")]
    } else {
        paths.iter().map(|p| normalized(p)).collect()
    };
    let mfc = Regex::new(MOVE).expect("valid regex");
    let mut problems = Vec::new();
    for root in roots {
        let mut files = Vec::new();
        rust_files(&root, &mut files);
        files.sort();
        for path in files {
            let target = path
                .strip_prefix(&root)
                .map(|rel| rel.components().any(|c| c.as_os_str() == "target"))
                .unwrap_or(false)
                || root.components().any(|c| c.as_os_str() == "target");
            // This file's own tests and docs spell the shapes it looks for.
            let own = path.ends_with("psoxide-hazard/src/mfc0.rs");
            if !target && !own {
                problems.extend(check_file(&path, &mfc));
            }
        }
    }
    for problem in &problems {
        println!("{problem}");
    }
    if problems.is_empty() {
        println!("mfc0/mfc2 delay slots clean");
        0
    } else {
        println!(
            "\n{} mfc load-delay hazard(s). Put a nop after the mfc.",
            problems.len()
        );
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(source: &str) -> Vec<String> {
        let dir = std::env::temp_dir().join(format!("xtask-mfc0-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("case{}.rs", source.len()));
        fs::write(&file, source).unwrap();
        let found = check_file(&file, &Regex::new(MOVE).unwrap());
        let _ = fs::remove_file(&file);
        found
    }

    #[test]
    fn flags_a_read_in_the_delay_slot() {
        let found = scan("asm!(\n  \"mfc0 $t0, $12\",\n  \"ori $t0, $t0, 1\",\n);\n");
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with(":2: `\"mfc0 $t0, $12\",` is followed by `ori $t0, $t0, 1` which reads $t0 in the load-delay slot"));
    }

    #[test]
    fn a_nop_comment_or_other_register_is_fine() {
        assert!(scan("\"mfc0 $t0, $12\",\n\"nop\",\n\"ori $t0, $t0, 1\",\n").is_empty());
        assert!(scan("\"mfc0 $t0, $12\",\n// note\n\"addu $t1, $t2, $t3\",\n").is_empty());
        assert!(scan("\"mfc0 $t0, $12\",\n\"mfc0 $t0, $13\",\n\"nop\",\n").is_empty());
        assert!(scan("\"mfc2 $t0, $12\",\n\"or $t01, $t2\",\n").is_empty());
    }

    /// HL's `enable_cop2`: the move ends its block and the `|=` is compiled into the next
    /// instruction, which reads the register one instruction early.
    #[test]
    fn flags_a_move_that_ends_its_asm_block() {
        let old = "unsafe {\n    core::arch::asm!(\"mfc0 $8, $12\", lateout(\"$8\") sr);\n    sr |= 0x4000_0000;\n}\n";
        let found = scan(old);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains(":2:") && found[0].contains("ends its asm block"));
        // The same shape over several lines, and for mfc2 and cfc2.
        let spread = "asm!(\n    \"mfc2 {t}, $0\",\n    t = out(reg) v,\n    options(nomem),\n);\n";
        assert_eq!(scan(spread).len(), 1);
        assert_eq!(scan("asm!(\"cfc2 $t1, $31\", out(\"$t1\") v)").len(), 1);
    }

    #[test]
    fn a_move_followed_by_a_nop_in_its_block_is_fine() {
        assert!(scan("asm!(\"mfc0 $8, $12\", \"nop\", lateout(\"$8\") sr);").is_empty());
        assert!(scan("asm!(\n \"mfc0 {t}, $12\",\n \"nop\",\n t = out(reg) sr,\n);").is_empty());
        assert!(scan("asm!(\"mfc0 $8, $12\\n nop\", lateout(\"$8\") sr);").is_empty());
        assert!(scan("asm!(\"mfc0 $8, $12; nop\", lateout(\"$8\") sr);").is_empty());
        // Assembly in a raw string is judged by the next line only.
        assert!(scan("r#\"\n    mfc0 $t2, $12\n    lui $t3, 1\n\"#").is_empty());
    }

    #[test]
    fn braced_operands_are_checked_in_the_slot() {
        assert_eq!(
            scan("asm!(\n \"mfc0 {t}, $12\",\n \"ori {t}, {t}, 1\",\n)").len(),
            1
        );
    }

    #[test]
    fn splits_lines_like_python() {
        assert_eq!(splitlines("a\r\nb\rc\u{2028}d\n"), ["a", "b", "c", "d"]);
        assert_eq!(splitlines("a\n\nb"), ["a", "", "b"]);
    }
}
