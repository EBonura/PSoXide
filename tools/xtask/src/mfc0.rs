//! `check-mfc0`: flag MFC0/MFC2 load-delay hazards in guest assembly.
//!
//! On the R3000 the destination of `mfc0`/`mfc2` arrives one instruction
//! late: the very next instruction reads the STALE register. This has shipped
//! twice (psx-rt's `enable_cpu_interrupts` historically, and the demo-disc
//! loader's `quiesce()`, where the stale value landed in SR with BEV set and
//! every interrupt vectored into the ROM). The emulator models the hazard
//! faithfully, but only code that runs under it gets caught; this check reads
//! the source instead.
//!
//! Heuristic: inside `.rs` files, for every line containing `mfc0`/`mfc2`
//! with a register destination, look at the next asm-looking line (within
//! three); if it mentions the same register, fail. A `nop`, or any line not
//! touching the register, is fine. Scans `sdk/` and `engine/` by default.

use std::fs;
use std::path::{Component, Path, PathBuf};

use regex::Regex;

/// `str.splitlines()`: Python also breaks on `\v`, `\f`, `\x1c`-`\x1e`,
/// NEL and the Unicode line and paragraph separators.
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
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
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
        let register = &found[1];
        let reads =
            Regex::new(&format!(r"{}\b", regex::escape(register))).expect("escaped register");
        // The delay slot is the next line that looks like an instruction.
        for next in lines.iter().skip(i + 1).take(3) {
            let stripped = py_strip(next).trim_matches(['"', ',']);
            if stripped.is_empty() || stripped.starts_with("//") || stripped.starts_with('#') {
                continue;
            }
            // `(?<!mfc0 )`: a read that is not itself another mfc0's operand.
            let read = reads
                .find_iter(stripped)
                .any(|m| !stripped[..m.start()].ends_with("mfc0 "));
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
pub fn main(paths: &[String]) -> i32 {
    let roots: Vec<PathBuf> = if paths.is_empty() {
        vec![PathBuf::from("sdk"), PathBuf::from("engine")]
    } else {
        paths.iter().map(|p| normalized(p)).collect()
    };
    let mfc = Regex::new(r"\bmfc[02]\s+(\$\w+)").expect("valid regex");
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
            if !target {
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
        let found = check_file(&file, &Regex::new(r"\bmfc[02]\s+(\$\w+)").unwrap());
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
        assert!(scan("\"mfc0 $t0, $12\",\n\"mfc0 $t0, $13\",\n").is_empty());
        assert!(scan("\"mfc2 $t0, $12\",\n\"or $t01, $t2\",\n").is_empty());
    }

    #[test]
    fn splits_lines_like_python() {
        assert_eq!(splitlines("a\r\nb\rc\u{2028}d\n"), ["a", "b", "c", "d"]);
        assert_eq!(splitlines("a\n\nb"), ["a", "", "b"]);
    }
}
