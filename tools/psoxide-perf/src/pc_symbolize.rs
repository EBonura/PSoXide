//! Aggregate `frontend launch --pc-sample-log` output into guest functions.
//!
//! The editor-playtest guest links with `--oformat=binary`, so it has no symbols.
//! Relink the same sources into an ELF (drop `--oformat=binary`, keep every other
//! RUSTFLAG) and pass that ELF here together with the sample CSV:
//!
//! ```text
//! psoxide-perf pc-symbolize --elf <elf> --samples <pc-csv> [--top 40]
//! ```
//!
//! Addresses that fall inside no symbol are reported as `<unmapped>`.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::util::{
    add_count, dict_rows, parse_hex, parse_int, read_text, usage_error, Cli, Error, OrderedMap,
    Result, Token,
};

const NM: &str = "mipsel-none-elf-nm";

/// Python's `line.split(maxsplit=2)`: up to three fields, the last keeping
/// everything after the second separator run.
fn split_max_two(line: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = line.trim_start();
    while parts.len() < 2 && !rest.is_empty() {
        match rest.find(char::is_whitespace) {
            Some(end) => {
                parts.push(&rest[..end]);
                rest = rest[end..].trim_start();
            }
            None => {
                parts.push(rest);
                rest = "";
            }
        }
    }
    if !rest.is_empty() {
        parts.push(rest);
    }
    parts
}

/// Parse `nm -n --defined-only` output into sorted `(address, name)` pairs.
pub fn parse_nm(output: &str) -> Result<Vec<(u64, String)>> {
    let mut symbols = Vec::new();
    for line in crate::util::splitlines(output) {
        let parts = split_max_two(line);
        if parts.len() != 3 {
            continue;
        }
        // 'A' is an absolute linker constant (RAM_BASE etc), not code.
        if parts[1].to_uppercase() == "A" {
            continue;
        }
        symbols.push((parse_hex(parts[0])? as u64, parts[2].to_string()));
    }
    symbols.sort();
    Ok(symbols)
}

fn load_symbols(elf: &str) -> Result<Vec<(u64, String)>> {
    let output = Command::new(NM)
        .args(["-n", "--defined-only", elf])
        .output()
        .map_err(|e| Error(format!("{NM}: {e}")))?;
    if !output.status.success() {
        return Err(Error(format!("{NM} exited with {}", output.status)));
    }
    parse_nm(&String::from_utf8_lossy(&output.stdout))
}

/// Demangle through `rustfilt` when it is installed; otherwise names stay as is.
fn demangle(names: &[String]) -> HashMap<String, String> {
    if names.is_empty() {
        return HashMap::new();
    }
    let attempt = || -> Option<HashMap<String, String>> {
        let mut child = Command::new("rustfilt")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        child
            .stdin
            .take()?
            .write_all(names.join("\n").as_bytes())
            .ok()?;
        let output = child.wait_with_output().ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        Some(
            names
                .iter()
                .cloned()
                .zip(crate::util::splitlines(&text).into_iter().map(String::from))
                .collect(),
        )
    };
    attempt().unwrap_or_default()
}

/// One sample row: `(pc, samples, window_start_tick if present)`.
struct Sample {
    pc: u64,
    samples: i64,
    window_start: Option<i64>,
}

fn read_samples(path: &str, windowed_required: bool) -> Result<Vec<Sample>> {
    let (header, rows) = dict_rows(&read_text(std::path::Path::new(path))?)?;
    let has_window = header.iter().any(|name| name == "window_start_tick");
    if windowed_required && !has_window {
        return Err(Error(
            "--min-window-start requires a windowed PC sample CSV".to_string(),
        ));
    }
    let mut samples = Vec::new();
    for row in &rows {
        let pc = row
            .get("pc")
            .ok_or_else(|| Error("KeyError: 'pc'".to_string()))?;
        let count = row
            .get("samples")
            .ok_or_else(|| Error("KeyError: 'samples'".to_string()))?;
        let window_start = if has_window {
            Some(parse_int(row.get("window_start_tick").unwrap_or(""))?)
        } else {
            None
        };
        samples.push(Sample {
            pc: parse_hex(pc)? as u64,
            samples: parse_int(count)?,
            window_start,
        });
    }
    Ok(samples)
}

/// Aggregate samples onto symbols. Returns `(grand_total, ranked rows)` where each
/// row is `(symbol, samples, hot_pc)`, ranked by samples (ties by first appearance).
pub fn aggregate(symbols: &[(u64, String)], samples: &[Sample2], top: usize) -> Result<Aggregate> {
    let addrs: Vec<u64> = symbols.iter().map(|s| s.0).collect();
    let mut totals: OrderedMap<String, i64> = OrderedMap::new();
    let mut hottest: HashMap<String, (i64, u64)> = HashMap::new();
    let mut grand = 0;
    for &(pc, n) in samples {
        grand += n;
        let right = addrs.partition_point(|&a| a <= pc);
        let symbol = if right > 0 {
            symbols[right - 1].1.clone()
        } else {
            "<unmapped>".to_string()
        };
        add_count(&mut totals, symbol.clone(), n);
        let best = hottest.get(&symbol).map_or(0, |h| h.0);
        if n > best {
            hottest.insert(symbol, (n, pc));
        }
    }
    let distinct = totals.len();
    let mut ranked: Vec<(String, i64)> = totals.iter().cloned().collect();
    ranked.sort_by_key(|item| -item.1);
    ranked.truncate(top);
    let mut rows = Vec::new();
    for (symbol, n) in ranked {
        let (_, pc) = hottest
            .get(&symbol)
            .copied()
            .ok_or_else(|| Error(format!("KeyError: {symbol:?}")))?;
        rows.push((symbol, n, pc));
    }
    Ok((grand, distinct, rows))
}

/// The result of [`aggregate`]: grand total, distinct symbols, ranked rows.
pub type Aggregate = (i64, usize, Vec<(String, i64, u64)>);

/// `(pc, samples)` pair fed to [`aggregate`].
pub type Sample2 = (u64, i64);

const USAGE: &str =
    "psoxide-perf pc-symbolize --elf ELF --samples CSV [--top N] [--min-window-start TICK]";

struct Options {
    elf: String,
    samples: String,
    top: usize,
    min_window: Option<i64>,
}

fn parse_args(args: &[String]) -> std::result::Result<Options, String> {
    let (mut elf, mut samples, mut top, mut min_window) = (None, None, 40usize, None);
    let mut cli = Cli::new(args);
    while let Some(token) = cli.next_token() {
        match token {
            Token::Flag(flag) => match flag.as_str() {
                "--elf" => elf = Some(cli.value(&flag)?),
                "--samples" => samples = Some(cli.value(&flag)?),
                "--top" => top = cli.int(&flag)?,
                "--min-window-start" => min_window = Some(cli.int(&flag)?),
                other => return Err(format!("unrecognized arguments: {other}")),
            },
            Token::Positional(text) => return Err(format!("unrecognized arguments: {text}")),
        }
    }
    match (elf, samples) {
        (Some(elf), Some(samples)) => Ok(Options {
            elf,
            samples,
            top,
            min_window,
        }),
        _ => Err("the following arguments are required: --elf, --samples".to_string()),
    }
}

/// `psoxide-perf pc-symbolize --elf ELF --samples CSV [--top N] [--min-window-start TICK]`.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => return Ok(usage_error(USAGE, &message)),
    };
    let Options {
        elf,
        samples: samples_path,
        top,
        min_window,
    } = options;
    let symbols = load_symbols(&elf)?;
    if symbols.is_empty() {
        return Err(Error(format!("{NM} found no symbols in {elf}")));
    }
    let samples = match read_samples(&samples_path, min_window.is_some()) {
        Ok(samples) => samples,
        Err(error) if min_window.is_some() && error.0.contains("--min-window-start") => {
            eprintln!("error: {error}");
            return Ok(2);
        }
        Err(error) => return Err(error),
    };
    let kept: Vec<Sample2> = samples
        .iter()
        .filter(|s| match (min_window, s.window_start) {
            (Some(min), Some(start)) => start >= min,
            _ => true,
        })
        .map(|s| (s.pc, s.samples))
        .collect();
    let (grand, distinct, ranked) = aggregate(&symbols, &kept, top)?;
    let names: Vec<String> = ranked.iter().map(|r| r.0.clone()).collect();
    let pretty = demangle(&names);

    writeln!(out, "{grand} samples over {distinct} symbols")?;
    writeln!(
        out,
        "{:>8} {:>7}  {:>10}  symbol",
        "samples", "pct", "hot pc"
    )?;
    for (symbol, n, pc) in ranked {
        if grand == 0 {
            return Err(Error("float division by zero".to_string()));
        }
        let shown = pretty.get(&symbol).unwrap_or(&symbol);
        writeln!(
            out,
            "{n:>8} {:>6.2}%  0x{pc:08x}  {shown}",
            100.0 * n as f64 / grand as f64
        )?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nm_output_skips_absolute_constants_and_sorts() {
        let text = "80010020 T beta\n80010000 T alpha\n00000800 A RAM_BASE\n80010010 t gamma fn\n";
        let symbols = parse_nm(text).unwrap();
        assert_eq!(
            symbols,
            vec![
                (0x8001_0000, "alpha".to_string()),
                (0x8001_0010, "gamma fn".to_string()),
                (0x8001_0020, "beta".to_string())
            ]
        );
    }

    #[test]
    fn samples_land_on_the_nearest_lower_symbol() {
        let symbols = vec![(0x100, "a".to_string()), (0x200, "b".to_string())];
        let (grand, distinct, rows) = aggregate(
            &symbols,
            &[(0x104, 3), (0x108, 5), (0x204, 4), (0x10, 1)],
            10,
        )
        .unwrap();
        assert_eq!((grand, distinct), (13, 3));
        assert_eq!(rows[0], ("a".to_string(), 8, 0x108));
        assert_eq!(rows[1], ("b".to_string(), 4, 0x204));
        assert_eq!(rows[2], ("<unmapped>".to_string(), 1, 0x10));
    }
}
