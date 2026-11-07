//! Attribute exact PC-line or PC-word counts to linker-map symbols and cache sets.
//!
//! Accepts any frontend histogram: `--pc-line-log` (instructions) or a stall-line
//! log (`--mmio-stall-line-log`, `--ram-load-stall-line-log`,
//! `--icache-stall-line-log`). Per-function totals are exact only for logs written
//! with `--pc-log-words` (column `pc`). A 16-byte line log (column `line_pc`) bills
//! a whole line to one symbol even when the line also holds the head of the next
//! function, so a hot callee such as memcpy leaks into whatever precedes it and a
//! code-size change shows per-function deltas that are pure placement.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::util;
use crate::util::{
    add_count, dict_rows, parse_hex, parse_int, read_text, splitlines, usage_error, Cli, Error,
    OrderedMap, Result, Token,
};

/// One linker-map symbol: start, end (exclusive) and name.
pub type Symbol = (u64, u64, String);

/// Load the sized, addressable symbols of a linker map, sorted by address.
pub fn load_symbols(path: &Path) -> Result<Vec<Symbol>> {
    let row = Regex::new(r"^\s*([0-9a-fA-F]+)\s+[0-9a-fA-F]+\s+([0-9a-fA-F]+)\s+\d+\s+(.+?)\s*$")
        .expect("static pattern");
    let mut symbols: Vec<Symbol> = Vec::new();
    for line in splitlines(&read_text(path)?) {
        let Some(captures) = row.captures(line) else {
            continue;
        };
        let address = u64::from_str_radix(&captures[1], 16).map_err(|e| Error(e.to_string()))?;
        let size = u64::from_str_radix(&captures[2], 16).map_err(|e| Error(e.to_string()))?;
        let name = captures[3].to_string();
        if size == 0
            || !(0x8000_0000..=0xBFFF_FFFF).contains(&address)
            || name.contains('/')
            || name.contains(":(")
            || [".", "BYTE(", "LONG(", "QUAD(", "*fill*"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            || name.contains(" = ")
        {
            continue;
        }
        symbols.push((address, address + size, name));
    }
    symbols.sort_by_key(|a| (a.0, a.1));
    Ok(symbols)
}

/// A loaded frontend histogram.
pub struct Counts {
    /// `(pc, value)` rows in file order.
    pub rows: Vec<(u64, i64)>,
    /// True for a per-word (`pc`) log, false for a 16-byte line log.
    pub words: bool,
    /// The second column's name (`instructions`, `stall_cycles`, ...).
    pub value_name: String,
}

/// Load a `pc` / `line_pc` histogram.
pub fn load_counts(path: &Path) -> Result<Counts> {
    let text = read_text(path)?;
    let mut records = util::csv_records(&text).into_iter();
    let header = records.next().unwrap_or_default();
    if header.len() < 2 {
        bail!(
            "{}: expected a pc or line_pc column and a value column",
            path.display()
        );
    }
    let key = header[0].clone();
    let value_name = header[1].clone();
    if key != "pc" && key != "line_pc" {
        bail!(
            "{}: expected a pc or line_pc column, got '{}'",
            path.display(),
            key
        );
    }
    let mut rows = Vec::new();
    for record in records {
        if record.is_empty() {
            continue;
        }
        if record.len() < 2 {
            bail!("{}: short row {:?}", path.display(), record);
        }
        rows.push((parse_hex(&record[0])? as u64, parse_int(&record[1])?));
    }
    Ok(Counts {
        rows,
        words: key == "pc",
        value_name,
    })
}

/// Sum counts per owning symbol, in first-appearance order.
pub fn per_symbol(
    counts: &[(u64, i64)],
    symbols: &[Symbol],
    starts: &[u64],
) -> OrderedMap<String, i64> {
    let mut totals = OrderedMap::new();
    for &(pc, count) in counts {
        add_count(&mut totals, symbol_for(pc, symbols, starts), count);
    }
    totals
}

/// Strip a trailing LLVM clone suffix (` (.630)`).
fn strip_clone_suffix(name: &str) -> &str {
    if let Some(stripped) = name.strip_suffix(')') {
        if let Some(open) = stripped.rfind(" (.") {
            let digits = &stripped[open + 3..];
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                return &stripped[..open];
            }
        }
    }
    name
}

/// Fold LLVM clone suffixes (`name (.630)`), which renumber between builds.
pub fn merge_clone_suffixes(totals: &OrderedMap<String, i64>) -> OrderedMap<String, i64> {
    let mut merged = OrderedMap::new();
    for (name, count) in totals.iter() {
        add_count(&mut merged, strip_clone_suffix(name).to_string(), *count);
    }
    merged
}

/// Lines of a line log whose 16 bytes belong to more than one symbol.
pub fn straddled(counts: &[(u64, i64)], symbols: &[Symbol], starts: &[u64]) -> (i64, i64) {
    let mut lines = 0;
    let mut value = 0;
    for &(pc, count) in counts {
        let owners: BTreeSet<String> = (0..16)
            .step_by(4)
            .map(|offset| symbol_for(pc + offset, symbols, starts))
            .collect();
        if owners.len() > 1 {
            lines += 1;
            value += count;
        }
    }
    (lines, value)
}

/// Map a physical code line to the conventional cached/BIOS alias.
pub fn canonical_code_line(physical: u64) -> u64 {
    if (0x1FC0_0000..0x1FC8_0000).contains(&physical) {
        physical | 0xA000_0000
    } else {
        physical | 0x8000_0000
    }
}

/// `(victim, incoming, set, events, stalls)` temporal replacements.
pub type EvictionPair = (u64, u64, u64, i64, i64);

/// Aggregate the exact refill CSV into temporal replacement pairs.
pub fn load_eviction_pairs(path: &Path) -> Result<Vec<EvictionPair>> {
    let text = read_text(path)?;
    let (_, rows) = dict_rows(&text)?;
    let mut totals: OrderedMap<(u64, u64, u64), (i64, i64)> = OrderedMap::new();
    let field = |row: &util::DictRow, name: &str| -> Result<String> {
        match row.get(name) {
            Some(value) => Ok(value.to_string()),
            None => Err(Error(format!("missing column {name}"))),
        }
    };
    for row in &rows {
        if field(row, "miss_kind")? != "tag" || parse_hex(&field(row, "victim_valid_mask")?)? == 0 {
            continue;
        }
        let victim = canonical_code_line(parse_hex(&field(row, "victim_line")?)? as u64);
        let incoming = canonical_code_line(parse_hex(&field(row, "incoming_line")?)? as u64);
        let cache_set = parse_hex(&field(row, "cache_set")?)? as u64;
        let stall = parse_int(&field(row, "stall_cycles")?)?;
        let aggregate = totals.entry_or_insert_with((victim, incoming, cache_set), || (0, 0));
        aggregate.0 += 1;
        aggregate.1 += stall;
    }
    Ok(totals
        .iter()
        .map(|((victim, incoming, cache_set), (events, stalls))| {
            (*victim, *incoming, *cache_set, *events, *stalls)
        })
        .collect())
}

/// The symbol owning `pc`, or `<unattributed>`.
pub fn symbol_for(pc: u64, symbols: &[Symbol], starts: &[u64]) -> String {
    // bisect_right: the number of starts <= pc.
    let right = starts.partition_point(|&start| start <= pc);
    let mut index = right as i64 - 1;
    while index >= 0 && starts[index as usize] == starts[right.wrapping_sub(1)] {
        let (start, end, name) = &symbols[index as usize];
        if *start <= pc && pc < *end {
            return name.clone();
        }
        index -= 1;
    }
    if index >= 0 {
        let (start, end, name) = &symbols[index as usize];
        if *start <= pc && pc < *end {
            return name.clone();
        }
    }
    "<unattributed>".to_string()
}

struct Options {
    pc_lines: PathBuf,
    linker_map: PathBuf,
    limit: usize,
    compare: Option<(PathBuf, PathBuf)>,
    icache_events: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> std::result::Result<Options, String> {
    let mut positional: Vec<PathBuf> = Vec::new();
    let mut limit = 30usize;
    let mut compare = None;
    let mut icache_events = None;
    let mut cli = Cli::new(args);
    while let Some(token) = cli.next_token() {
        match token {
            Token::Flag(flag) => match flag.as_str() {
                "--limit" => limit = cli.int("--limit")?,
                "--compare" => {
                    let base_log = cli.value("--compare")?;
                    let base_map = cli
                        .value("--compare")
                        .map_err(|_| "argument --compare: expected 2 arguments".to_string())?;
                    compare = Some((PathBuf::from(base_log), PathBuf::from(base_map)));
                }
                "--icache-events" => icache_events = Some(PathBuf::from(cli.value(&flag)?)),
                other => return Err(format!("unrecognized arguments: {other}")),
            },
            Token::Positional(text) => positional.push(PathBuf::from(text)),
        }
    }
    if positional.len() != 2 {
        return Err("the following arguments are required: pc_lines, linker_map".to_string());
    }
    let linker_map = positional.pop().expect("two positionals");
    let pc_lines = positional.pop().expect("two positionals");
    Ok(Options {
        pc_lines,
        linker_map,
        limit,
        compare,
        icache_events,
    })
}

/// `psoxide-perf pc-line-attribution PC_LINES LINKER_MAP [--limit N]
/// [--compare BASE_LOG BASE_MAP] [--icache-events CSV]`.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            return Ok(usage_error(
                "psoxide-perf pc-line-attribution PC_LINES LINKER_MAP [--limit N] [--compare BASE_LOG BASE_MAP] [--icache-events CSV]",
                &message,
            ));
        }
    };
    report(&options, out)?;
    Ok(0)
}

fn report(options: &Options, out: &mut dyn Write) -> Result<()> {
    let loaded = load_counts(&options.pc_lines)?;
    let (counts, words, value_name) = (&loaded.rows, loaded.words, loaded.value_name.as_str());
    let symbols = load_symbols(&options.linker_map)?;
    let starts: Vec<u64> = symbols.iter().map(|symbol| symbol.0).collect();
    let total_raw: i64 = counts.iter().map(|(_, count)| count).sum();
    let total = if total_raw == 0 { 1 } else { total_raw } as f64;
    let limit = options.limit;

    if !words {
        let (shared_lines, shared_value) = straddled(counts, &symbols, &starts);
        if shared_lines != 0 {
            writeln!(
                out,
                "warning: {shared_lines} lines holding {shared_value} {value_name} ({:.4}%) span more than one symbol and are billed to one of them; rerun with --pc-log-words for exact totals\n",
                shared_value as f64 * 100.0 / total
            )?;
        }
    }

    let by_symbol = per_symbol(counts, &symbols, &starts);

    // Line-level views: fold words into their I-cache line, naming every owner.
    let mut line_totals: OrderedMap<u64, i64> = OrderedMap::new();
    let mut line_names: OrderedMap<u64, Vec<String>> = OrderedMap::new();
    let mut sorted_counts = counts.clone();
    sorted_counts.sort();
    for &(pc, count) in &sorted_counts {
        let line = pc & !0xF;
        add_count(&mut line_totals, line, count);
        let name = symbol_for(pc, &symbols, &starts);
        let names = line_names.entry_or_insert_with(line, Vec::new);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let mut by_set: OrderedMap<u64, Vec<(u64, i64, String)>> = OrderedMap::new();
    let mut attributed: Vec<(u64, i64, String)> = Vec::new();
    for (pc, count) in line_totals.iter() {
        let name = line_names.get(pc).expect("every line has owners").join("|");
        by_set
            .entry_or_insert_with((pc >> 4) & 0xFF, Vec::new)
            .push((*pc, *count, name.clone()));
        attributed.push((*pc, *count, name));
    }

    writeln!(
        out,
        "hot functions ({})",
        if words {
            "exact, per word"
        } else {
            "per 16-byte line"
        }
    )?;
    writeln!(out, "{value_name},percent,symbol")?;
    let mut ranked: Vec<&(String, i64)> = by_symbol.iter().collect();
    ranked.sort_by_key(|item| -item.1);
    for (name, count) in ranked.into_iter().take(limit) {
        writeln!(out, "{count},{:.4},{name}", *count as f64 * 100.0 / total)?;
    }

    if let Some((base_log, base_map)) = &options.compare {
        let base_loaded = load_counts(base_log)?;
        let base_symbols = load_symbols(base_map)?;
        let base_starts: Vec<u64> = base_symbols.iter().map(|symbol| symbol.0).collect();
        let base =
            merge_clone_suffixes(&per_symbol(&base_loaded.rows, &base_symbols, &base_starts));
        let current = merge_clone_suffixes(&by_symbol);
        if !(words && base_loaded.words) {
            writeln!(
                out,
                "\nwarning: a line log on either side makes these deltas placement-sensitive"
            )?;
        }
        writeln!(out, "\nfunction deltas (this log minus BASE_LOG)")?;
        writeln!(
            out,
            "delta,percent_change,base_{value_name},{value_name},symbol"
        )?;
        let names: BTreeSet<&String> = base
            .iter()
            .chain(current.iter())
            .map(|(name, _)| name)
            .collect();
        let lookup =
            |map: &OrderedMap<String, i64>, name: &String| map.get(name).copied().unwrap_or(0);
        let mut ranked: Vec<&String> = names.into_iter().collect();
        ranked.sort_by(|a, b| {
            let delta = |name: &String| (lookup(&current, name) - lookup(&base, name)).abs();
            (-delta(a), a.as_str()).cmp(&(-delta(b), b.as_str()))
        });
        for name in ranked.into_iter().take(limit) {
            let (old, new) = (lookup(&base, name), lookup(&current, name));
            let change = if old != 0 {
                format!("{:+.2}", (new - old) as f64 * 100.0 / old as f64)
            } else {
                "new".to_string()
            };
            writeln!(out, "{:+},{change},{old},{new},{name}", new - old)?;
        }
    }

    writeln!(out, "\nhot lines")?;
    writeln!(out, "{value_name},percent,line_pc,cache_set,symbol")?;
    let mut hot = attributed.clone();
    hot.sort_by_key(|item| -item.1);
    for (pc, count, name) in hot.into_iter().take(limit) {
        writeln!(
            out,
            "{count},{:.4},0x{pc:08x},0x{:02x},{name}",
            count as f64 * 100.0 / total,
            (pc >> 4) & 0xff
        )?;
    }

    type Pressure = (i64, i64, u64, Vec<(u64, i64, String)>);
    let mut pressures: Vec<Pressure> = Vec::new();
    for (cache_set, entries) in by_set.iter() {
        let mut entries = entries.clone();
        entries.sort_by_key(|item| -item.1);
        let set_total: i64 = entries.iter().map(|(_, count, _)| count).sum();
        let eviction_pressure = set_total - entries[0].1;
        pressures.push((eviction_pressure, set_total, *cache_set, entries));
    }
    // `sort(reverse=True)`: descending by (pressure, total, set, entries), stable.
    pressures.sort_by(|a, b| {
        (b.0, b.1, b.2)
            .cmp(&(a.0, a.1, a.2))
            .then_with(|| b.3.cmp(&a.3))
    });

    writeln!(out, "\nhot direct-map conflicts")?;
    writeln!(out, "pressure,total,cache_set,distinct_lines,top_lines")?;
    for (pressure, set_total, cache_set, entries) in pressures.iter().take(limit) {
        let top = entries
            .iter()
            .take(3)
            .map(|(pc, count, name)| format!("0x{pc:08x}:{count}:{name}"))
            .collect::<Vec<_>>()
            .join("; ");
        writeln!(
            out,
            "{pressure},{set_total},0x{cache_set:02x},{},{top}",
            entries.len()
        )?;
    }

    if let Some(events) = &options.icache_events {
        let mut pairs = load_eviction_pairs(events)?;
        writeln!(out, "\nexact temporal eviction pairs")?;
        writeln!(
            out,
            "stall_cycles,events,cache_set,victim_line,victim_symbol,incoming_line,incoming_symbol"
        )?;
        pairs.sort_by_key(|a| (-a.4, -a.3, a.0, a.1));
        for (victim, incoming, cache_set, events, stalls) in pairs.into_iter().take(limit) {
            let victim_symbol = symbol_for(victim, &symbols, &starts);
            let incoming_symbol = symbol_for(incoming, &symbols, &starts);
            writeln!(
                out,
                "{stalls},{events},0x{cache_set:02x},0x{victim:08x},{victim_symbol},0x{incoming:08x},{incoming_symbol}"
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Regression tests for the attribution tool.
    //!
    //! The case that matters: one 16-byte I-cache line holding the tail of a cold
    //! function and the head of a hot one. A line log bills the whole line to the
    //! cold function; a word log (frontend `--pc-log-words`) must not.

    use super::*;

    // `cold` owns 0x80010000..0x80010008, `hot` starts mid-line at 0x80010008.
    const MAP: &str = "     VMA      LMA     Size Align Out     In      Symbol\n\
80010000 80010000        8     4                 cold\n\
80010008 80010008       18     4                 hot (.12)\n";
    const WORDS: &str =
        "pc,instructions,percent\n0x80010008,90,0\n0x80010004,10,0\n0x80010010,5,0\n";
    const LINES: &str = "line_pc,instructions,percent\n0x80010000,100,0\n0x80010010,5,0\n";

    struct Fixture {
        _dir: tempfile::TempDir,
        map: PathBuf,
        words: PathBuf,
        lines: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let map = dir.path().join("link.map");
        let words = dir.path().join("words.csv");
        let lines = dir.path().join("lines.csv");
        std::fs::write(&map, MAP).unwrap();
        std::fs::write(&words, WORDS).unwrap();
        std::fs::write(&lines, LINES).unwrap();
        Fixture {
            _dir: dir,
            map,
            words,
            lines,
        }
    }

    fn run_tool(args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mut out = Vec::new();
        assert_eq!(run(&args, &mut out).unwrap(), 0);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn word_log_splits_a_shared_line_exactly() {
        let f = fixture();
        let symbols = load_symbols(&f.map).unwrap();
        let counts = load_counts(&f.words).unwrap();
        assert!(counts.words);
        assert_eq!(counts.value_name, "instructions");
        let starts: Vec<u64> = symbols.iter().map(|s| s.0).collect();
        let totals = per_symbol(&counts.rows, &symbols, &starts);
        assert_eq!(totals.get(&"cold".to_string()), Some(&10));
        assert_eq!(totals.get(&"hot (.12)".to_string()), Some(&95));
    }

    #[test]
    fn line_log_is_flagged_as_straddling() {
        let f = fixture();
        let output = run_tool(&[f.lines.to_str().unwrap(), f.map.to_str().unwrap()]);
        assert!(
            output.contains("warning: 1 lines holding 100 instructions"),
            "{output}"
        );
        assert!(output.contains("100,95.2381,cold"), "{output}");
    }

    #[test]
    fn compare_reports_the_line_log_phantom() {
        let f = fixture();
        let (words, lines, map) = (
            f.words.to_str().unwrap(),
            f.lines.to_str().unwrap(),
            f.map.to_str().unwrap(),
        );
        let output = run_tool(&[words, map, "--compare", lines, map]);
        assert!(output.contains("+90,+1800.00,5,95,hot"), "{output}");
        assert!(output.contains("-90,-90.00,100,10,cold"), "{output}");
        assert!(
            output.contains("0x80010000,0x00,cold|hot (.12)"),
            "{output}"
        );
    }

    #[test]
    fn clone_suffixes_are_stripped() {
        assert_eq!(strip_clone_suffix("f (.630)"), "f");
        assert_eq!(strip_clone_suffix("f (.)"), "f (.)");
        assert_eq!(strip_clone_suffix("f (.1a)"), "f (.1a)");
        assert_eq!(strip_clone_suffix("plain"), "plain");
    }
}
