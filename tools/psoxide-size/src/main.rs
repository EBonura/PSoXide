//! psoxide-size: where the bytes of a PS1 guest image go.
//!
//! ```text
//! psoxide-size audit  MAP [--exe EXE] [--top N] [--json] [--budget CLASS=BYTES]...
//! psoxide-size diff   BEFORE.map AFTER.map
//! psoxide-size faults MAP RAM.bin
//! psoxide-size faults MAP RAM.bin
//! ```
//!
//! `audit` reads the lld link map every guest build writes (and, when given,
//! the PS-X EXE for string, `Location` and duplicate-constant detection) and
//! prints bytes per class and kind, the largest symbols and statics, strings,
//! monomorphization groups, alignment padding and duplicated constants.
//! `--budget panic_machinery=2048` (or any class name) makes it exit 1 when a
//! class exceeds the budget, so a CI step can hold a size line.

use psoxide_size::classify::{classify, generic_base, Class, Item, Kind};
use psoxide_size::fault;
use psoxide_size::map::{self, parse, Image, LinkMap};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::process::ExitCode;

const KINDS: [Kind; 4] = [Kind::Text, Kind::Rodata, Kind::Data, Kind::Bss];

struct Audit<'a> {
    items: Vec<Item<'a>>,
    resident: Vec<usize>,
    overlay_bytes: u64,
}

fn is_overlay(out: &str) -> bool {
    out.contains("overlay")
}

fn audit<'a>(map: &'a LinkMap, image: Option<&Image>) -> Audit<'a> {
    let mut items = Vec::new();
    let mut resident = Vec::new();
    let mut overlay_bytes = 0u64;
    for sec in &map.sections {
        if sec.size == 0 {
            continue;
        }
        let item = classify(sec, image);
        if is_overlay(&sec.out) {
            overlay_bytes += sec.size as u64;
        } else if item.kind != Kind::Other {
            resident.push(items.len());
        }
        items.push(item);
    }
    Audit {
        items,
        resident,
        overlay_bytes,
    }
}

type Matrix = BTreeMap<Class, [u64; 4]>;

fn matrix(a: &Audit<'_>) -> Matrix {
    let mut m = Matrix::new();
    for &i in &a.resident {
        let it = &a.items[i];
        let col = KINDS.iter().position(|&k| k == it.kind).unwrap();
        m.entry(it.class).or_insert([0; 4])[col] += it.sec.size as u64;
    }
    m
}

fn class_total(m: &Matrix, c: Class) -> u64 {
    m.get(&c).map(|r| r.iter().sum()).unwrap_or(0)
}

fn machinery_total(m: &Matrix) -> u64 {
    m.iter()
        .filter(|(c, _)| c.is_panic_machinery())
        .map(|(_, r)| r.iter().sum::<u64>())
        .sum()
}

/// Bytes lost between consecutive input sections of one output section.
fn padding(map: &LinkMap) -> BTreeMap<String, (u64, u64)> {
    let mut by_out: BTreeMap<&str, Vec<&map::Section>> = BTreeMap::new();
    for s in &map.sections {
        if s.size > 0 && !is_overlay(&s.out) {
            by_out.entry(&s.out).or_default().push(s);
        }
    }
    let mut out = BTreeMap::new();
    for (name, mut secs) in by_out {
        secs.sort_by_key(|s| s.vma);
        let (mut gaps, mut count) = (0u64, 0u64);
        for w in secs.windows(2) {
            let end = w[0].vma as u64 + w[0].size as u64;
            let next = w[1].vma as u64;
            if next > end && next - end < 4096 {
                gaps += next - end;
                count += 1;
            }
        }
        out.insert(name.to_string(), (gaps, count));
    }
    out
}

fn kib(b: u64) -> String {
    format!("{b:>8} B ({:.1} KiB)", b as f64 / 1024.0)
}

fn report(map: &LinkMap, image: Option<&Image>, top: usize, json: bool) -> (String, Matrix) {
    let a = audit(map, image);
    let m = matrix(&a);
    let mut s = String::new();
    let sum_kind = |col: usize| m.values().map(|r| r[col]).sum::<u64>();
    let pad = padding(map);
    if json {
        let mut classes = Vec::new();
        for (c, r) in &m {
            classes.push(format!(
                "\"{}\":{{\"text\":{},\"rodata\":{},\"data\":{},\"bss\":{}}}",
                c.name(),
                r[0],
                r[1],
                r[2],
                r[3]
            ));
        }
        let _ = writeln!(
            s,
            "{{\"heap_free\":{},\"text\":{},\"rodata\":{},\"data\":{},\"bss\":{},\"overlay\":{},\"panic_machinery\":{},\"padding\":{},\"classes\":{{{}}}}}",
            map.heap_free().map(|v| v.to_string()).unwrap_or("null".into()),
            sum_kind(0),
            sum_kind(1),
            sum_kind(2),
            sum_kind(3),
            a.overlay_bytes,
            machinery_total(&m),
            pad.values().map(|p| p.0).sum::<u64>(),
            classes.join(",")
        );
        return (s, m);
    }
    let _ = writeln!(s, "== image");
    if let Some(f) = map.heap_free() {
        let _ = writeln!(s, "heap_free      {}", kib(f as u64));
    }
    for (name, col) in [("text", 0), ("rodata", 1), ("data", 2), ("bss", 3)] {
        let _ = writeln!(s, "{name:<14} {}", kib(sum_kind(col)));
    }
    if a.overlay_bytes > 0 {
        let _ = writeln!(s, "overlay        {} (not resident)", kib(a.overlay_bytes));
    }
    let total: u64 = (0..4).map(sum_kind).sum();
    let _ = writeln!(s, "\n== bytes by class (resident)");
    let _ = writeln!(
        s,
        "{:<16}{:>10}{:>10}{:>10}{:>10}{:>11}{:>7}",
        "class", "text", "rodata", "data", "bss", "total", "%"
    );
    let mut rows: Vec<_> = m.iter().collect();
    rows.sort_by_key(|(_, r)| std::cmp::Reverse(r.iter().sum::<u64>()));
    for (c, r) in rows {
        let t: u64 = r.iter().sum();
        let _ = writeln!(
            s,
            "{:<16}{:>10}{:>10}{:>10}{:>10}{:>11}{:>6.1}%",
            c.name(),
            r[0],
            r[1],
            r[2],
            r[3],
            t,
            t as f64 * 100.0 / total.max(1) as f64
        );
    }
    let mach = machinery_total(&m);
    let _ = writeln!(
        s,
        "\npanic_machinery (fmt + panic + str_char + panic_location + file_name): {}",
        kib(mach)
    );
    let _ = writeln!(
        s,
        "  -Cpanic=immediate-abort removes most of this and the Location/file-name data; \
         debug_impl ({}) and string ({}) stay unless the code is edited.",
        class_total(&m, Class::DebugImpl),
        class_total(&m, Class::Str)
    );
    let _ = writeln!(s, "\n== alignment padding between input sections");
    for (name, (bytes, n)) in pad.iter().filter(|(_, (bytes, _))| *bytes > 0) {
        let _ = writeln!(s, "{name:<16}{bytes:>8} B in {n} gaps");
    }

    let mut ranked: Vec<&Item<'_>> = a.resident.iter().map(|&i| &a.items[i]).collect();
    ranked.sort_by_key(|it| std::cmp::Reverse(it.sec.size));
    let _ = writeln!(s, "\n== top {top} input sections");
    for it in ranked.iter().take(top) {
        let _ = writeln!(
            s,
            "{:>8}  {:<6} {:<14} {}",
            it.sec.size,
            it.kind.name(),
            it.class.name(),
            clip(&it.name, 110)
        );
    }
    let _ = writeln!(s, "\n== top {top} non-code statics (rodata, data, bss)");
    for it in ranked.iter().filter(|it| it.kind != Kind::Text).take(top) {
        let _ = writeln!(
            s,
            "{:>8}  {:<6} {:<14} align {:<3} {}",
            it.sec.size,
            it.kind.name(),
            it.class.name(),
            it.sec.align,
            clip(&it.name, 100)
        );
    }

    let mut by_crate: HashMap<&str, u64> = HashMap::new();
    for it in &ranked {
        *by_crate.entry(&it.krate).or_default() += it.sec.size as u64;
    }
    let mut crates: Vec<_> = by_crate.into_iter().collect();
    crates.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
    let _ = writeln!(s, "\n== bytes by crate (first path segment)");
    for (c, b) in crates.iter().take(top.min(15)) {
        let _ = writeln!(s, "{b:>8}  {c}");
    }

    let strings: Vec<_> = ranked.iter().filter(|it| it.class == Class::Str).collect();
    if !strings.is_empty() {
        let total: u64 = strings.iter().map(|it| it.sec.size as u64).sum();
        let _ = writeln!(
            s,
            "\n== string literals: {} sections, {} B (largest {})",
            strings.len(),
            total,
            top.min(10)
        );
        for it in strings.iter().take(top.min(10)) {
            let _ = writeln!(
                s,
                "{:>8}  {:?}",
                it.sec.size,
                it.text.as_deref().unwrap_or("")
            );
        }
    }

    // Monomorphization groups: code sections that differ only in generic args.
    let mut groups: HashMap<String, (u64, u32)> = HashMap::new();
    for it in &ranked {
        if it.kind == Kind::Text && !it.name.starts_with("<anon") {
            let e = groups.entry(generic_base(&it.name)).or_default();
            e.0 += it.sec.size as u64;
            e.1 += 1;
        }
    }
    let mut multi: Vec<_> = groups.into_iter().filter(|(_, (_, n))| *n >= 2).collect();
    multi.sort_by_key(|(_, (b, _))| std::cmp::Reverse(*b));
    let pool: u64 = multi.iter().map(|(_, (b, _))| *b).sum();
    let _ = writeln!(
        s,
        "\n== generic instances: {} groups with 2+ copies hold {} B of code (candidates, not a gain)",
        multi.len(),
        pool
    );
    for (name, (bytes, n)) in multi.iter().take(top) {
        let _ = writeln!(s, "{bytes:>8}  x{n:<4} {}", clip(name, 100));
    }

    if let Some(img) = image {
        let mut seen: HashMap<Vec<u8>, (u32, u32, String)> = HashMap::new();
        let mut dup: HashMap<Vec<u8>, (u32, u32, String)> = HashMap::new();
        for it in &ranked {
            if it.kind != Kind::Rodata || it.sec.size < 16 {
                continue;
            }
            let Some(bytes) = img.at(it.sec.vma, it.sec.size) else {
                continue;
            };
            if bytes.iter().all(|&b| b == 0) {
                continue;
            }
            match seen.get(bytes) {
                Some(_) => {
                    let e = dup
                        .entry(bytes.to_vec())
                        .or_insert((it.sec.size, 1, it.name.clone()));
                    e.1 += 1;
                }
                None => {
                    seen.insert(bytes.to_vec(), (it.sec.size, 1, it.name.clone()));
                }
            }
        }
        let mut dups: Vec<_> = dup.into_values().collect();
        dups.sort_by_key(|(sz, n, _)| std::cmp::Reverse(*sz as u64 * *n as u64));
        let wasted: u64 = dups.iter().map(|(sz, n, _)| *sz as u64 * *n as u64).sum();
        let _ = writeln!(
            s,
            "\n== duplicated constants (identical read-only bytes, >= 16 B): {} groups, {} B repeated copies",
            dups.len(),
            wasted
        );
        for (sz, n, name) in dups.iter().take(top.min(10)) {
            let _ = writeln!(
                s,
                "{:>8}  {} extra copies of {} B  {}",
                *sz as u64 * *n as u64,
                n,
                sz,
                clip(name, 80)
            );
        }
        let zero_data: u64 = ranked
            .iter()
            .filter(|it| it.kind == Kind::Data)
            .filter_map(|it| img.at(it.sec.vma, it.sec.size).map(|b| (it, b)))
            .filter(|(_, b)| b.iter().all(|&x| x == 0))
            .map(|(it, _)| it.sec.size as u64)
            .sum();
        let _ = writeln!(
            s,
            "\n== all-zero .data: {} B (could live in .bss and shrink the loaded image)",
            zero_data
        );
    } else {
        let _ = writeln!(
            s,
            "\n(no --exe: string, Location and duplicate-constant detection skipped)"
        );
    }
    (s, m)
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let head: String = s.chars().take(n).collect();
        format!("{head}...")
    }
}

fn load(path: &str) -> Result<LinkMap, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let m = parse(&text);
    if m.sections.is_empty() {
        return Err(format!("{path}: no input sections (is it an lld map?)"));
    }
    Ok(m)
}

fn run() -> Result<ExitCode, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: psoxide-size audit MAP [--exe EXE] [--top N] [--json] [--list CLASS] [--budget CLASS=BYTES]...\n       psoxide-size diff BEFORE.map AFTER.map\n       psoxide-size panic-flags [--toml]   (the shared panic policy; PSOXIDE_PANIC_MESSAGES=1 restores messages)\n       psoxide-size faults MAP RAM.bin   (exit 1 when psx-rt recorded a CPU exception)";
    match args.first().map(String::as_str) {
        Some("audit") => {
            let mut map_path = None;
            let (mut exe, mut top, mut json) = (None, 15usize, false);
            let mut list: Option<String> = None;
            let mut budgets: Vec<(String, u64)> = Vec::new();
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--exe" => exe = it.next().cloned(),
                    "--top" => top = it.next().and_then(|v| v.parse().ok()).ok_or(usage)?,
                    "--json" => json = true,
                    "--list" => list = it.next().cloned(),
                    "--budget" => {
                        let v = it.next().ok_or(usage)?;
                        let (k, n) = v.split_once('=').ok_or(usage)?;
                        budgets.push((k.to_string(), n.parse().map_err(|_| usage.to_string())?));
                    }
                    other if map_path.is_none() => map_path = Some(other.to_string()),
                    _ => return Err(usage.into()),
                }
            }
            let map = load(&map_path.ok_or(usage)?)?;
            let image = match exe {
                Some(p) => Some(Image::parse(
                    &std::fs::read(&p).map_err(|e| format!("{p}: {e}"))?,
                )?),
                None => None,
            };
            let (text, m) = report(&map, image.as_ref(), top, json);
            print!("{text}");
            if let Some(class) = list {
                let a = audit(&map, image.as_ref());
                let mut rows: Vec<_> = a
                    .resident
                    .iter()
                    .map(|&i| &a.items[i])
                    .filter(|it| it.class.name() == class)
                    .collect();
                rows.sort_by_key(|it| std::cmp::Reverse(it.sec.size));
                println!("\n== {class}: {} sections", rows.len());
                for it in rows {
                    let shown = it
                        .text
                        .as_deref()
                        .map_or_else(|| clip(&it.name, 120), |t| format!("{t:?}"));
                    println!("{:>8}  {:<6} {}", it.sec.size, it.kind.name(), shown);
                }
            }
            let mut failed = false;
            for (name, limit) in budgets {
                let have = if name == "panic_machinery" {
                    machinery_total(&m)
                } else {
                    m.iter()
                        .filter(|(c, _)| c.name() == name)
                        .map(|(_, r)| r.iter().sum::<u64>())
                        .sum()
                };
                if have > limit {
                    eprintln!("budget exceeded: {name} is {have} B, budget {limit} B");
                    failed = true;
                }
            }
            Ok(if failed {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Some("panic-flags") => {
            // `--toml` prints the flags as a cargo `--config` array body.
            let flags = psoxide_size::policy::rustflags_from_env();
            if args.get(1).map(String::as_str) == Some("--toml") {
                let quoted: Vec<String> = flags.iter().map(|f| format!("\"{f}\"")).collect();
                println!("{}", quoted.join(","));
            } else {
                println!("{}", flags.join(" "));
            }
            Ok(ExitCode::SUCCESS)
        }
        Some("faults") if args.len() == 3 => {
            let map = load(&args[1])?;
            let ram = std::fs::read(&args[2]).map_err(|e| format!("{}: {e}", args[2]))?;
            let record = fault::read(&map, &ram)?;
            println!("cpu_exceptions={}", record.count);
            match fault::describe(&map, &record) {
                Some(line) => {
                    println!("{line}");
                    Ok(ExitCode::from(1))
                }
                None => Ok(ExitCode::SUCCESS),
            }
        }
        Some("diff") if args.len() == 3 => {
            let (a, b) = (load(&args[1])?, load(&args[2])?);
            let (ma, mb) = (matrix(&audit(&a, None)), matrix(&audit(&b, None)));
            let mut classes: Vec<Class> = ma.keys().chain(mb.keys()).copied().collect();
            classes.sort();
            classes.dedup();
            println!(
                "{:<16}{:>12}{:>12}{:>12}",
                "class", "before", "after", "delta"
            );
            for c in classes {
                let (x, y) = (class_total(&ma, c) as i64, class_total(&mb, c) as i64);
                println!("{:<16}{:>12}{:>12}{:>+12}", c.name(), x, y, y - x);
            }
            if let (Some(x), Some(y)) = (a.heap_free(), b.heap_free()) {
                println!(
                    "{:<16}{:>12}{:>12}{:>+12}",
                    "heap_free",
                    x,
                    y,
                    y as i64 - x as i64
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(usage.into()),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
