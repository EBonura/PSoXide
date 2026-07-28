use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const ONE_VBLANK_CYCLES: u64 = 564_480;
const CPU_HZ: f64 = 33_868_800.0;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let Some(command) = args.first().cloned() else {
        return Err(help());
    };
    args.remove(0);
    match command.as_str() {
        "lint-policy-guard" => lint_policy_guard(),
        "runtime-numeric-guard" => runtime_numeric_guard(),
        "bake-spectrum" => bake_spectrum(&args),
        "duckstation-harness" => duckstation_harness(&args),
        "emulator-inventory" => emulator_inventory(&args),
        "external-emulator-smoke" => external_emulator_smoke(&args),
        "cortex-bringup-report" => cortex_bringup_report(&args),
        "cortex-stream-guard" => cortex_stream_guard(&args),
        "vblank-chart" => vblank_chart(&args),
        "gen-tones" => gen_tones(),
        "gen-fonts" => gen_fonts(),
        "-h" | "--help" | "help" => Err(help()),
        other => Err(format!(
            "unknown psoxide-dev command `{other}`\n\n{}",
            help()
        )),
    }
}

fn help() -> String {
    "usage: psoxide-dev <command> [args]\n\
     commands:\n\
       lint-policy-guard\n\
       runtime-numeric-guard\n\
       bake-spectrum <input.wav> -o <output.bin> [--fps N] [--bands N] [--seconds S]\n\
       duckstation-harness --cue <disc.cue> [--expect TEXT] [--bios-boot]\n\
       emulator-inventory [--require NAME]\n\
       external-emulator-smoke --emulator mednafen|retroarch|ares --cue <disc.cue> [--bios BIOS] [--fail-on TEXT] [--screenshot PNG]\n\
       cortex-bringup-report [--out REPORT.md] [--fail-on-warn]\n\
       cortex-stream-guard [--profile profile.csv] [--cdda-log cdda.log] [--boot-flow-log boot.log]\n\
       vblank-chart --in <profile.csv> --out <chart.html> [--title TITLE]\n\
       gen-tones\n\
       gen-fonts"
        .to_string()
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tools/psoxide-dev is two levels below repo root")
        .to_path_buf()
}

fn resolve_from_cwd(path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .unwrap_or_else(|_| repo_root())
            .join(path)
    }
}

fn relative_to_root(path: &Path) -> String {
    path.strip_prefix(repo_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn lint_policy_guard() -> Result<(), String> {
    let root = repo_root();
    // The three real workspace roots. editor/ and emu/ merged into the
    // root workspace (abeb9fef) -- their crates inherit root's lints.
    let manifests = [
        root.join("Cargo.toml"),
        root.join("engine/Cargo.toml"),
        root.join("sdk/Cargo.toml"),
    ];
    let sections = ["workspace.lints.rust", "workspace.lints.clippy"];
    let mut violations = Vec::new();
    let reference_path = &manifests[0];
    let mut reference = HashMap::new();
    for section in sections {
        let body = extract_toml_section(reference_path, section)?;
        if body.is_none() {
            violations.push(format!(
                "{} missing [{section}]",
                relative_to_root(reference_path)
            ));
        }
        reference.insert(section, body);
    }
    for manifest in manifests.iter().skip(1) {
        for section in sections {
            let expected = reference.get(section).and_then(|value| value.as_ref());
            let actual = extract_toml_section(manifest, section)?;
            match (expected, actual.as_ref()) {
                (_, None) => violations.push(format!(
                    "{} missing [{section}]",
                    relative_to_root(manifest)
                )),
                (Some(expected), Some(actual)) if expected != actual => violations.push(format!(
                    "{} [{section}] differs from {}",
                    relative_to_root(manifest),
                    relative_to_root(reference_path)
                )),
                _ => {}
            }
        }
    }
    if !violations.is_empty() {
        eprintln!("lint policy guard failed.");
        eprintln!(
            "Keep [workspace.lints.rust] and [workspace.lints.clippy] identical in every Cargo workspace manifest.\n"
        );
        for violation in violations {
            eprintln!("{violation}");
        }
        return Err("lint policy guard failed".to_string());
    }
    println!("lint policy guard: ok ({} manifests)", manifests.len());
    Ok(())
}

fn extract_toml_section(path: &Path, section: &str) -> Result<Option<Vec<String>>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let header = format!("[{section}]");
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim() != header {
            continue;
        }
        let mut body = Vec::new();
        for candidate in lines.by_ref() {
            if candidate.starts_with('[') {
                break;
            }
            if !candidate.trim().is_empty() {
                body.push(candidate.trim_end().to_string());
            }
        }
        return Ok(Some(body));
    }
    Ok(None)
}

fn runtime_numeric_guard() -> Result<(), String> {
    let root = repo_root();
    let mut runtime_roots = vec![
        root.join("engine/crates/psx-engine/src"),
        root.join("engine/crates/psx-game-runtime/src"),
        root.join("engine/crates/psx-level/src"),
    ];
    push_child_src_dirs(&mut runtime_roots, &root.join("engine/examples"))?;
    for entry in sorted_dirs(&root.join("sdk/crates"))? {
        if entry.file_name().and_then(|s| s.to_str()) == Some("psx-gte-core") {
            continue;
        }
        runtime_roots.push(entry.join("src"));
    }

    // Whole-file exemptions: files whose PURPOSE is 64-bit math.
    // psx-rt/builtins.rs implements the __divdi3/__moddi3 compiler
    // shims (their signatures are i64 by ABI definition); psx-math's
    // fmt.rs is the u64 decimal formatter. Line markers inside them
    // would just repeat the file header.
    let exempt_files = [
        root.join("sdk/crates/psx-rt/src/builtins.rs"),
        root.join("sdk/crates/psx-math/src/fmt.rs"),
    ];

    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for runtime_root in runtime_roots {
        collect_rs_files(&runtime_root, &mut files, &mut seen)?;
    }
    files.retain(|f| !exempt_files.contains(f));
    files.sort();

    let mut violations = Vec::new();
    let mut allowed_hits = 0usize;
    for path in &files {
        let src = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let code = blank_comments_and_literals(&src);
        let allowed = allowed_lines(&src);
        scan_forbidden_types(
            path,
            &src,
            &code,
            &allowed,
            &mut allowed_hits,
            &mut violations,
        );
        scan_float_literals(
            path,
            &src,
            &code,
            &allowed,
            &mut allowed_hits,
            &mut violations,
        );
    }

    if !violations.is_empty() {
        eprintln!("PS1 runtime numeric guard failed.");
        eprintln!(
            "Runtime Rust must avoid f32/f64, float literals, and 64/128-bit integer types unless explicitly allowlisted."
        );
        eprintln!("Use fixed-point 16/32-bit integer math, or move host-only code out of runtime source roots.\n");
        eprintln!(
            "Use `// psx-numeric-allow-next-line: reason` only for a reviewed exception that cannot move out of runtime code.\n"
        );
        for violation in violations {
            eprintln!("{violation}");
        }
        return Err("runtime numeric guard failed".to_string());
    }
    println!(
        "runtime numeric guard: ok ({} files, {} line-level allow hits)",
        files.len(),
        allowed_hits
    );
    Ok(())
}

fn push_child_src_dirs(out: &mut Vec<PathBuf>, parent: &Path) -> Result<(), String> {
    for dir in sorted_dirs(parent)? {
        out.push(dir.join("src"));
    }
    Ok(())
}

fn sorted_dirs(parent: &Path) -> Result<Vec<PathBuf>, String> {
    if !parent.exists() {
        return Ok(Vec::new());
    }
    let mut dirs = Vec::new();
    for entry in fs::read_dir(parent).map_err(|e| format!("{}: {e}", parent.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn collect_rs_files(
    root: &Path,
    files: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root).map_err(|e| format!("{}: {e}", root.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            collect_rs_files(&path, files, seen)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs")
            && seen.insert(path.clone())
        {
            files.push(path);
        }
    }
    Ok(())
}

fn allowed_lines(src: &str) -> HashSet<usize> {
    let mut allowed = HashSet::new();
    for (idx, line) in src.lines().enumerate() {
        let number = idx + 1;
        if line.contains("psx-numeric-allow-line") {
            allowed.insert(number);
        }
        if line.contains("psx-numeric-allow-next-line") {
            allowed.insert(number + 1);
        }
    }
    allowed
}

fn blank_comments_and_literals(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = bytes.to_vec();
    let mut idx = 0usize;
    while idx < bytes.len() {
        if bytes[idx] == b'r' {
            if let Some(end) = raw_string_end(bytes, idx) {
                blank_range(&mut out, idx, end);
                idx = end;
                continue;
            }
        }
        if bytes[idx..].starts_with(b"//") {
            let end = bytes[idx..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| idx + p)
                .unwrap_or(bytes.len());
            blank_range(&mut out, idx, end);
            idx = end;
            continue;
        }
        if bytes[idx..].starts_with(b"/*") {
            let mut depth = 1usize;
            let mut end = idx + 2;
            while end < bytes.len() && depth > 0 {
                if bytes[end..].starts_with(b"/*") {
                    depth += 1;
                    end += 2;
                } else if bytes[end..].starts_with(b"*/") {
                    depth -= 1;
                    end += 2;
                } else {
                    end += 1;
                }
            }
            blank_range(&mut out, idx, end);
            idx = end;
            continue;
        }
        if bytes[idx] == b'"' {
            let mut end = idx + 1;
            let mut escaped = false;
            while end < bytes.len() {
                let current = bytes[end];
                if escaped {
                    escaped = false;
                } else if current == b'\\' {
                    escaped = true;
                } else if current == b'"' {
                    end += 1;
                    break;
                }
                end += 1;
            }
            blank_range(&mut out, idx, end);
            idx = end;
            continue;
        }
        if bytes[idx] == b'\'' {
            if let Some(end) = char_literal_end(bytes, idx) {
                blank_range(&mut out, idx, end);
                idx = end;
                continue;
            }
        }
        idx += 1;
    }
    String::from_utf8(out).expect("source bytes stay utf8")
}

fn raw_string_end(src: &[u8], start: usize) -> Option<usize> {
    if src.get(start) != Some(&b'r') {
        return None;
    }
    let mut idx = start + 1;
    while src.get(idx) == Some(&b'#') {
        idx += 1;
    }
    if src.get(idx) != Some(&b'"') {
        return None;
    }
    let hashes = idx - start - 1;
    let mut marker = vec![b'"'];
    marker.extend(std::iter::repeat_n(b'#', hashes));
    find_bytes(&src[idx + 1..], &marker).map(|pos| idx + 1 + pos + marker.len())
}

fn char_literal_end(src: &[u8], start: usize) -> Option<usize> {
    if start + 1 >= src.len() {
        return None;
    }
    let next = src[start + 1];
    if next.is_ascii_alphabetic() || next == b'_' {
        return None;
    }
    let mut idx = start + 1;
    if src[idx] == b'\\' {
        idx += 2;
    } else {
        idx += 1;
    }
    (src.get(idx) == Some(&b'\'')).then_some(idx + 1)
}

fn blank_range(out: &mut [u8], start: usize, end: usize) {
    let end = end.min(out.len());
    for byte in &mut out[start..end] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (idx, ch) in src.char_indices() {
        if idx >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn report_context(src: &str, line: usize) -> String {
    src.lines()
        .nth(line.saturating_sub(1))
        .unwrap_or("")
        .trim()
        .to_string()
}

fn scan_forbidden_types(
    path: &Path,
    src: &str,
    code: &str,
    allowed: &HashSet<usize>,
    allowed_hits: &mut usize,
    violations: &mut Vec<String>,
) {
    let forbidden = ["f32", "f64", "u64", "i64", "u128", "i128"];
    for token in forbidden {
        let mut start = 0usize;
        while let Some(pos) = code[start..].find(token) {
            let offset = start + pos;
            let before = offset
                .checked_sub(1)
                .and_then(|i| code.as_bytes().get(i).copied());
            let after = code.as_bytes().get(offset + token.len()).copied();
            if before.is_none_or(|b| !is_ident_byte(b)) && after.is_none_or(|b| !is_ident_byte(b)) {
                let (line, col) = line_col(code, offset);
                if allowed.contains(&line) {
                    *allowed_hits += 1;
                } else {
                    let kind = if token.starts_with('f') {
                        "float type"
                    } else {
                        "wide integer type"
                    };
                    violations.push(format!(
                        "{}:{line}:{col}: forbidden {kind} `{token}`\n    {}",
                        relative_to_root(path),
                        report_context(src, line)
                    ));
                }
            }
            start = offset + token.len();
        }
    }
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn scan_float_literals(
    path: &Path,
    src: &str,
    code: &str,
    allowed: &HashSet<usize>,
    allowed_hits: &mut usize,
    violations: &mut Vec<String>,
) {
    let bytes = code.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        if !bytes[idx].is_ascii_digit()
            || idx > 0 && (is_ident_byte(bytes[idx - 1]) || bytes[idx - 1] == b'.')
        {
            idx += 1;
            continue;
        }
        let start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        let mut has_float = false;
        if idx < bytes.len()
            && bytes[idx] == b'.'
            && idx + 1 < bytes.len()
            && bytes[idx + 1].is_ascii_digit()
        {
            has_float = true;
            idx += 1;
            while idx < bytes.len() && bytes[idx].is_ascii_digit() {
                idx += 1;
            }
        }
        if idx < bytes.len() && matches!(bytes[idx], b'e' | b'E') {
            has_float = true;
            idx += 1;
            if idx < bytes.len() && matches!(bytes[idx], b'+' | b'-') {
                idx += 1;
            }
            while idx < bytes.len() && bytes[idx].is_ascii_digit() {
                idx += 1;
            }
        }
        if has_float {
            if idx < bytes.len() && bytes[idx] == b'_' {
                idx += 1;
            }
            if idx + 2 <= bytes.len() && &bytes[idx..idx + 1] == b"f" {
                idx += 1;
                while idx < bytes.len() && bytes[idx].is_ascii_digit() {
                    idx += 1;
                }
            }
            let literal = &code[start..idx];
            let (line, col) = line_col(code, start);
            if allowed.contains(&line) {
                *allowed_hits += 1;
            } else {
                violations.push(format!(
                    "{}:{line}:{col}: forbidden float literal `{literal}`\n    {}",
                    relative_to_root(path),
                    report_context(src, line)
                ));
            }
        }
        idx = idx.max(start + 1);
    }
}

fn bake_spectrum(args: &[String]) -> Result<(), String> {
    let mut input = None;
    let mut output = None;
    let mut fps = 30usize;
    let mut bands = 16usize;
    let mut seconds = None;
    let mut window = 512usize;
    let mut min_hz = 80.0f64;
    let mut max_hz = 8000.0f64;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                output = args.get(i).cloned();
            }
            "--fps" => {
                i += 1;
                fps = parse_arg(args.get(i), "--fps")?;
            }
            "--bands" => {
                i += 1;
                bands = parse_arg(args.get(i), "--bands")?;
            }
            "--seconds" => {
                i += 1;
                seconds = Some(parse_arg(args.get(i), "--seconds")?);
            }
            "--window" => {
                i += 1;
                window = parse_arg(args.get(i), "--window")?;
            }
            "--min-hz" => {
                i += 1;
                min_hz = parse_arg(args.get(i), "--min-hz")?;
            }
            "--max-hz" => {
                i += 1;
                max_hz = parse_arg(args.get(i), "--max-hz")?;
            }
            value if !value.starts_with('-') && input.is_none() => input = Some(value.to_string()),
            other => return Err(format!("unknown bake-spectrum arg `{other}`")),
        }
        i += 1;
    }
    let input = input.ok_or_else(|| "bake-spectrum requires an input WAV".to_string())?;
    let output = output.ok_or_else(|| "bake-spectrum requires -o <output>".to_string())?;
    if fps == 0 || bands == 0 || window <= 8 || !window.is_power_of_two() {
        return Err(
            "--fps/--bands must be positive and --window must be a power of two > 8".into(),
        );
    }
    if min_hz <= 0.0 || max_hz <= min_hz {
        return Err("--min-hz/--max-hz must form a positive range".into());
    }
    let input_path = resolve_from_cwd(&input);
    let output_path = resolve_from_cwd(&output);
    let wav = read_wav_pcm16(&input_path)?;
    let seconds = seconds.unwrap_or(wav.frames as f64 / wav.rate as f64);
    let frame_count = (seconds * fps as f64).floor().max(1.0) as usize;
    let hop = wav.rate as f64 / fps as f64;
    let freqs = log_spaced_frequencies(bands, min_hz, max_hz);
    let hann: Vec<f64> = (0..window)
        .map(|i| 0.5 - 0.5 * ((2.0 * std::f64::consts::PI * i as f64) / (window - 1) as f64).cos())
        .collect();
    let mut powers = Vec::with_capacity(frame_count);
    for frame in 0..frame_count {
        let start = (frame as f64 * hop).floor() as usize;
        let samples = frame_samples(&wav, start, &hann);
        powers.push(
            freqs
                .iter()
                .map(|&freq| goertzel_power(&samples, wav.rate, freq))
                .collect::<Vec<_>>(),
        );
    }
    let baked = normalize_and_smooth(&powers);
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(&output_path, &baked).map_err(|e| format!("{}: {e}", output_path.display()))?;
    println!(
        "[bake-spectrum] {} -> {} ({} frames x {} bands @ {} Hz, {} bytes)",
        input_path.display(),
        output_path.display(),
        frame_count,
        bands,
        fps,
        baked.len()
    );
    Ok(())
}

fn parse_arg<T: std::str::FromStr>(value: Option<&String>, name: &str) -> Result<T, String> {
    value
        .ok_or_else(|| format!("{name} requires a value"))?
        .parse()
        .map_err(|_| format!("invalid value for {name}"))
}

#[derive(Clone)]
struct EmulatorInventoryRow {
    key: &'static str,
    label: &'static str,
    binary: Option<PathBuf>,
    core: Option<PathBuf>,
    coverage: &'static str,
    install_hint: &'static str,
}

fn emulator_inventory(args: &[String]) -> Result<(), String> {
    let mut requires = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--require" => {
                i += 1;
                requires.push(
                    args.get(i)
                        .cloned()
                        .ok_or("--require needs an emulator key")?,
                );
            }
            other => return Err(format!("unknown emulator-inventory arg `{other}`")),
        }
        i += 1;
    }

    let rows = emulator_inventory_rows();
    println!(
        "{:<13} {:<14} {:<10} {:<38} Coverage",
        "Key", "Emulator", "Available", "Binary/core"
    );
    for row in &rows {
        let available = row.binary.is_some() && (row.key != "retroarch" || row.core.is_some());
        let location = match (&row.binary, &row.core) {
            (Some(binary), Some(core)) => {
                format!("{} + {}", binary.display(), core.display())
            }
            (Some(binary), None) => binary.display().to_string(),
            _ => row.install_hint.to_string(),
        };
        println!(
            "{:<13} {:<14} {:<10} {:<38} {}",
            row.key,
            row.label,
            if available { "yes" } else { "no" },
            truncate_middle(&location, 38),
            row.coverage
        );
    }

    let missing: Vec<_> = requires
        .iter()
        .filter(|required| {
            !rows.iter().any(|row| {
                row.key == required.as_str()
                    && row.binary.is_some()
                    && (row.key != "retroarch" || row.core.is_some())
            })
        })
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "required emulator(s) unavailable: {}",
            missing.join(", ")
        ));
    }
    Ok(())
}

fn emulator_inventory_rows() -> Vec<EmulatorInventoryRow> {
    let retroarch = discover_retroarch(None).ok();
    let retroarch_core = find_retroarch_psx_core();
    vec![
        EmulatorInventoryRow {
            key: "duckstation",
            label: "DuckStation",
            binary: discover_duckstation(None).ok(),
            core: None,
            coverage: "TTY marker harness",
            install_hint: "install DuckStation.app or set DUCKSTATION_BIN",
        },
        EmulatorInventoryRow {
            key: "redux",
            label: "PCSX-Redux",
            binary: discover_pcsx_redux(None).ok(),
            core: None,
            coverage: "parity-oracle BIOS smoke",
            install_hint: "install PCSX-Redux.app or set PSOXIDE_REDUX_BIN",
        },
        EmulatorInventoryRow {
            key: "mednafen",
            label: "Mednafen",
            binary: discover_mednafen(None).ok(),
            core: None,
            coverage: "optional BIOS/TOC launch smoke",
            install_hint: "brew install mednafen or set MEDNAFEN_BIN",
        },
        EmulatorInventoryRow {
            key: "retroarch",
            label: "RetroArch",
            binary: retroarch,
            core: retroarch_core,
            coverage: "optional launch smoke + screenshot",
            install_hint: "brew install --cask retroarch-metal plus a PSX core",
        },
        EmulatorInventoryRow {
            key: "ares",
            label: "ares",
            binary: discover_ares(None).ok(),
            core: None,
            coverage: "optional TOC launch smoke",
            install_hint: "install ares.app or set ARES_BIN",
        },
    ]
}

fn external_emulator_smoke(args: &[String]) -> Result<(), String> {
    let mut emulator = None;
    let mut cue = None;
    let mut bios = env::var("PSOXIDE_BIOS").ok();
    let mut timeout = 12.0f64;
    let mut log = Some("build/external-emulator-smoke/emulator.log".to_string());
    let mut fail_on = Vec::new();
    let mut screenshot = None;
    let mut screenshot_frames = 360u64;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--emulator" => {
                i += 1;
                emulator = args.get(i).cloned();
            }
            "--cue" => {
                i += 1;
                cue = args.get(i).cloned();
            }
            "--bios" => {
                i += 1;
                bios = args.get(i).cloned();
            }
            "--timeout" => {
                i += 1;
                timeout = parse_arg(args.get(i), "--timeout")?;
            }
            "--log" => {
                i += 1;
                log = args.get(i).cloned();
            }
            "--fail-on" => {
                i += 1;
                fail_on.push(args.get(i).cloned().ok_or("--fail-on requires value")?);
            }
            "--screenshot" => {
                i += 1;
                screenshot = args.get(i).cloned();
            }
            "--screenshot-frames" => {
                i += 1;
                screenshot_frames = parse_arg(args.get(i), "--screenshot-frames")?;
            }
            other => return Err(format!("unknown external-emulator-smoke arg `{other}`")),
        }
        i += 1;
    }

    let emulator = emulator.ok_or("external-emulator-smoke needs --emulator")?;
    let cue = resolve_from_cwd(
        cue.as_deref()
            .ok_or("external-emulator-smoke needs --cue")?,
    );
    if !cue.is_file() {
        return Err(format!("disc cue not found: {}", cue.display()));
    }
    let log_path = resolve_from_cwd(
        log.as_deref()
            .unwrap_or("build/external-emulator-smoke/emulator.log"),
    );
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let screenshot_path = screenshot.as_deref().map(resolve_from_cwd);
    if screenshot_path.is_some() && emulator != "retroarch" {
        return Err("--screenshot is currently supported only for RetroArch".to_string());
    }
    if let Some(parent) = screenshot_path.as_deref().and_then(Path::parent) {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }

    let mut command = match emulator.as_str() {
        "mednafen" => mednafen_command(&cue, bios.as_deref())?,
        "retroarch" => retroarch_command(
            &cue,
            bios.as_deref(),
            screenshot_path.as_deref(),
            screenshot_frames,
        )?,
        "ares" => ares_command(&cue, bios.as_deref())?,
        other => {
            return Err(format!(
                "external-emulator-smoke does not know how to launch `{other}`"
            ));
        }
    };
    let fatal_patterns = external_emulator_fatal_patterns(&emulator, &fail_on);
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("SDL_AUDIODRIVER", "dummy");

    println!("Emulator: {emulator}");
    println!("Disc:     {}", cue.display());
    println!("Log:      {}", log_path.display());
    if let Some(path) = screenshot_path.as_deref() {
        println!("Shot:     {}", path.display());
    }
    println!("Command:  {:?}", command);

    let mut child = command
        .current_dir(repo_root())
        .spawn()
        .map_err(|e| format!("launch {emulator}: {e}"))?;
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel();
    spawn_line_reader(stdout, tx.clone());
    spawn_line_reader(stderr, tx);
    let mut log_file =
        fs::File::create(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    let mut lines = Vec::new();
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            while let Ok(line) = rx.try_recv() {
                writeln!(log_file, "{line}").map_err(|e| e.to_string())?;
                if let Some(pattern) = matching_pattern(&line, &fatal_patterns) {
                    let _ = child.kill();
                    let _ = child.wait();
                    eprintln!("\n{emulator} emitted fatal launch pattern `{pattern}`");
                    eprintln!("{line}");
                    return Err(format!("{emulator} launch smoke failed"));
                }
                lines.push(line);
            }
            if !status.success() {
                eprintln!("\n{emulator} exited early with {status}");
                for line in important_tail(&lines) {
                    eprintln!("{line}");
                }
                return Err(format!("{emulator} launch smoke failed"));
            }
            println!("{emulator} exited successfully during launch smoke");
            verify_optional_screenshot(screenshot_path.as_deref())?;
            return Ok(());
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                writeln!(log_file, "{line}").map_err(|e| e.to_string())?;
                if let Some(pattern) = matching_pattern(&line, &fatal_patterns) {
                    let _ = child.kill();
                    let _ = child.wait();
                    eprintln!("\n{emulator} emitted fatal launch pattern `{pattern}`");
                    eprintln!("{line}");
                    return Err(format!("{emulator} launch smoke failed"));
                }
                lines.push(line);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
    }
    if child.try_wait().map_err(|e| e.to_string())?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    println!("{emulator} stayed alive for {timeout:.1}s; launch smoke passed");
    verify_optional_screenshot(screenshot_path.as_deref())?;
    Ok(())
}

fn verify_optional_screenshot(path: Option<&Path>) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    if png_file_status(path) == "PASS" {
        println!("Screenshot: {}", path.display());
        Ok(())
    } else {
        Err(format!(
            "observable screenshot was not created as a valid PNG: {}",
            path.display()
        ))
    }
}

fn external_emulator_fatal_patterns(emulator: &str, extra: &[String]) -> Vec<String> {
    let mut patterns = match emulator {
        "retroarch" => vec![
            "Firmware is missing".to_string(),
            "Failed to load content".to_string(),
        ],
        _ => Vec::new(),
    };
    patterns.extend(extra.iter().cloned());
    patterns
}

fn matching_pattern<'a>(line: &str, patterns: &'a [String]) -> Option<&'a str> {
    patterns
        .iter()
        .find(|pattern| !pattern.is_empty() && line.contains(pattern.as_str()))
        .map(String::as_str)
}

fn cortex_stream_guard(args: &[String]) -> Result<(), String> {
    let mut profile = Some("build/preburn/cortex_ignition_v1/profile.csv".to_string());
    let mut cdda_log = Some("build/preburn/cortex_ignition_v1/cdda-probe.log".to_string());
    let mut boot_flow_log = Some("build/preburn/cortex_ignition_v1/boot-flow.log".to_string());
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--profile" => {
                i += 1;
                profile = args.get(i).cloned();
            }
            "--cdda-log" => {
                i += 1;
                cdda_log = args.get(i).cloned();
            }
            "--boot-flow-log" => {
                i += 1;
                boot_flow_log = args.get(i).cloned();
            }
            other => return Err(format!("unknown cortex-stream-guard arg `{other}`")),
        }
        i += 1;
    }

    let profile = resolve_from_cwd(
        profile
            .as_deref()
            .unwrap_or("build/preburn/cortex_ignition_v1/profile.csv"),
    );
    let cdda_log = resolve_from_cwd(
        cdda_log
            .as_deref()
            .unwrap_or("build/preburn/cortex_ignition_v1/cdda-probe.log"),
    );
    let boot_flow_log = resolve_from_cwd(
        boot_flow_log
            .as_deref()
            .unwrap_or("build/preburn/cortex_ignition_v1/boot-flow.log"),
    );

    let mut failures = Vec::new();
    let mut signals = Vec::new();

    match read_optional_log(&cdda_log)? {
        Some(text) if log_status(Some(&text), &["Nonzero:", "Peak:", "Play="]) == "PASS" => {
            signals.push(format!(
                "cdda log: {}",
                summarize_matching_lines(&text, &["Nonzero:", "Peak:", "CD CMDS:"], 4)
            ));
        }
        Some(text) => {
            failures.push(format!(
                "failure: CD-DA probe lacks required audio/play signal ({})",
                summarize_matching_lines(&text, &["Nonzero:", "Peak:", "CD CMDS:"], 4)
            ));
        }
        None => failures.push(format!(
            "failure: CD-DA probe log missing: {}",
            cdda_log.display()
        )),
    }

    match read_optional_log(&boot_flow_log)? {
        Some(text) if log_status(Some(&text), &["fifo pops:", "audio:", "Play"]) == "PASS" => {
            signals.push(format!(
                "boot-flow log: {}",
                summarize_matching_lines(&text, &["display:", "fifo pops:", "audio:", "Play"], 5)
            ));
        }
        Some(text) if text.contains("skip BIOS boot-flow probe:") => {
            signals.push(format!(
                "boot-flow log: {}",
                summarize_matching_lines(&text, &["skip BIOS boot-flow probe:"], 1)
            ));
        }
        Some(text) => {
            failures.push(format!(
                "failure: BIOS boot-flow log lacks CD data plus CD-DA signal ({})",
                summarize_matching_lines(&text, &["display:", "fifo pops:", "audio:", "Play"], 5)
            ));
        }
        None => failures.push(format!(
            "failure: BIOS boot-flow log missing: {}",
            boot_flow_log.display()
        )),
    }

    match streaming_profile_summary(&profile) {
        Ok(summary) => {
            signals.push(format!(
                "profile rows={} loads={} bytes={} sectors={} status={} requests={} misses={} failed_loads={} pending_last={} missing_mask_max=0x{:x}",
                summary.rows,
                summary.max_cd_room_chunk_loads,
                summary.max_cd_room_chunk_bytes,
                summary.max_cd_room_chunk_sectors,
                summary.max_cd_room_chunk_status,
                summary.max_room_stream_requests,
                summary.max_room_stream_misses,
                summary.max_room_stream_failed_loads,
                summary.last_room_stream_pending_loads,
                summary.max_missing_mask
            ));
            if !summary.missing_columns.is_empty() {
                failures.push(format!(
                    "failure: profile is missing guard column(s): {}",
                    summary.missing_columns.join(", ")
                ));
            }
            if summary.rows == 0 {
                failures.push("failure: profile has no guest-frame rows".to_string());
            }
            if summary.rows > 0 && summary.missing_columns.is_empty() {
                if summary.max_cd_room_chunk_loads == 0 {
                    failures
                        .push("failure: no CD room chunk loads observed in profile".to_string());
                }
                if summary.max_cd_room_chunk_bytes == 0 || summary.max_cd_room_chunk_sectors == 0 {
                    failures.push(
                        "failure: room streaming did not report CD bytes/sectors".to_string(),
                    );
                }
                if summary.max_cd_room_chunk_status != 0 {
                    let status = summary.max_cd_room_chunk_status;
                    let label = if status == 5 {
                        "STATUS_CD_ERROR"
                    } else {
                        "nonzero status"
                    };
                    failures.push(format!(
                        "failure: CD room chunk status reached {status} ({label})"
                    ));
                }
                if summary.max_room_stream_failed_loads != 0 {
                    failures.push(format!(
                        "failure: room stream failed-load counter reached {}",
                        summary.max_room_stream_failed_loads
                    ));
                }
                if summary.max_room_stream_requests == 0 {
                    failures.push("failure: no room stream requests observed".to_string());
                }
                if summary.max_missing_mask != 0 {
                    failures.push(format!(
                        "failure: portal/room missing mask became nonzero: 0x{:x}",
                        summary.max_missing_mask
                    ));
                }
            }
        }
        Err(error) => failures.push(format!("failure: {error}")),
    }

    for signal in &signals {
        println!("{signal}");
    }
    if failures.is_empty() {
        println!("streaming guard: PASS");
        return Ok(());
    }
    for failure in &failures {
        eprintln!("{failure}");
    }
    eprintln!("streaming guard: FAIL");
    Err("cortex stream guard failed".to_string())
}

#[derive(Default)]
struct StreamingProfileSummary {
    rows: usize,
    missing_columns: Vec<String>,
    max_cd_room_chunk_loads: u64,
    max_cd_room_chunk_bytes: u64,
    max_cd_room_chunk_sectors: u64,
    max_cd_room_chunk_status: u64,
    max_room_stream_requests: u64,
    max_room_stream_misses: u64,
    max_room_stream_failed_loads: u64,
    last_room_stream_pending_loads: u64,
    max_missing_mask: u64,
}

fn streaming_profile_summary(path: &Path) -> Result<StreamingProfileSummary, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header = lines
        .next()
        .ok_or_else(|| format!("profile is empty: {}", path.display()))?;
    let headers: Vec<&str> = header.split(',').map(str::trim).collect();
    let required = [
        "cd_room_chunk_loads",
        "cd_room_chunk_bytes",
        "cd_room_chunk_sectors",
        "cd_room_chunk_status",
        "room_stream_requests",
        "room_stream_misses",
        "room_stream_pending_loads",
        "room_stream_failed_loads",
        "missing_mask",
    ];
    let mut columns = HashMap::new();
    let mut missing = Vec::new();
    for name in required {
        if let Some(index) = headers.iter().position(|header| *header == name) {
            columns.insert(name, index);
        } else {
            missing.push(name.to_string());
        }
    }

    let mut summary = StreamingProfileSummary {
        missing_columns: missing,
        ..StreamingProfileSummary::default()
    };
    for line in lines {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        summary.rows += 1;
        summary.max_cd_room_chunk_loads = summary.max_cd_room_chunk_loads.max(csv_field_u64(
            &fields,
            columns.get("cd_room_chunk_loads").copied(),
        ));
        summary.max_cd_room_chunk_bytes = summary.max_cd_room_chunk_bytes.max(csv_field_u64(
            &fields,
            columns.get("cd_room_chunk_bytes").copied(),
        ));
        summary.max_cd_room_chunk_sectors = summary.max_cd_room_chunk_sectors.max(csv_field_u64(
            &fields,
            columns.get("cd_room_chunk_sectors").copied(),
        ));
        summary.max_cd_room_chunk_status = summary.max_cd_room_chunk_status.max(csv_field_u64(
            &fields,
            columns.get("cd_room_chunk_status").copied(),
        ));
        summary.max_room_stream_requests = summary.max_room_stream_requests.max(csv_field_u64(
            &fields,
            columns.get("room_stream_requests").copied(),
        ));
        summary.max_room_stream_misses = summary.max_room_stream_misses.max(csv_field_u64(
            &fields,
            columns.get("room_stream_misses").copied(),
        ));
        summary.max_room_stream_failed_loads = summary.max_room_stream_failed_loads.max(
            csv_field_u64(&fields, columns.get("room_stream_failed_loads").copied()),
        );
        summary.last_room_stream_pending_loads =
            csv_field_u64(&fields, columns.get("room_stream_pending_loads").copied());
        summary.max_missing_mask = summary
            .max_missing_mask
            .max(csv_field_u64(&fields, columns.get("missing_mask").copied()));
    }
    Ok(summary)
}

fn csv_field_u64(fields: &[&str], index: Option<usize>) -> u64 {
    index
        .and_then(|index| fields.get(index))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

fn cortex_bringup_report(args: &[String]) -> Result<(), String> {
    let mut out = Some("build/preburn/cortex_ignition_v1/BRINGUP_REPORT.md".to_string());
    let mut preburn_dir = Some("build/preburn/cortex_ignition_v1".to_string());
    let mut duckstation_log =
        Some("build/duckstation-harness/cortex_ignition_v1-bios.log".to_string());
    let mut external_dir = Some("build/external-emulator-smoke".to_string());
    let mut fail_on_warn = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out = args.get(i).cloned();
            }
            "--preburn-dir" => {
                i += 1;
                preburn_dir = args.get(i).cloned();
            }
            "--duckstation-log" => {
                i += 1;
                duckstation_log = args.get(i).cloned();
            }
            "--external-dir" => {
                i += 1;
                external_dir = args.get(i).cloned();
            }
            "--fail-on-warn" => {
                fail_on_warn = true;
            }
            other => return Err(format!("unknown cortex-bringup-report arg `{other}`")),
        }
        i += 1;
    }

    let out_path = resolve_from_cwd(
        out.as_deref()
            .unwrap_or("build/preburn/cortex_ignition_v1/BRINGUP_REPORT.md"),
    );
    let preburn_dir = resolve_from_cwd(
        preburn_dir
            .as_deref()
            .unwrap_or("build/preburn/cortex_ignition_v1"),
    );
    let duckstation_log = resolve_from_cwd(
        duckstation_log
            .as_deref()
            .unwrap_or("build/duckstation-harness/cortex_ignition_v1-bios.log"),
    );
    let external_dir = resolve_from_cwd(
        external_dir
            .as_deref()
            .unwrap_or("build/external-emulator-smoke"),
    );

    let mut report = String::new();
    writeln!(
        report,
        "# Cortex Ignition V1 Bringup Report\n\n\
         Generated from local logs. This is a data ledger, not a proof of real-hardware success: \
         emulator rows are only as strong as the signal listed in the table.\n"
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        report,
        "## Evidence Matrix\n\n\
         | Surface | Source | Status | Signal | Follow-up |\n\
         | --- | --- | --- | --- | --- |"
    )
    .map_err(|e| e.to_string())?;

    let disc_reads = read_optional_log(&preburn_dir.join("disc-reads.log"))?;
    report_row(
        &mut report,
        "Disc image structure",
        &preburn_dir.join("disc-reads.log"),
        log_status(disc_reads.as_deref(), &["CD001", "PS-X EXE"]),
        disc_reads
            .as_deref()
            .map(|text| summarize_matching_lines(text, &["CD001", "PS-X EXE"], 3))
            .unwrap_or_else(|| "log missing".to_string()),
        "Keep this as the first burn gate: bad SYSTEM.CNF/EXE/ISO layout must fail before CD-R.",
    )?;

    let internal_hle = read_optional_log(&preburn_dir.join("internal-hle.log"))?;
    report_row(
        &mut report,
        "PSoXide internal boot/render",
        &preburn_dir.join("internal-hle.log"),
        log_status(
            internal_hle.as_deref(),
            &["display_fnv1a_64", "vram_fnv1a_64"],
        ),
        internal_hle
            .as_deref()
            .map(|text| {
                summarize_matching_lines(text, &["display_fnv1a_64", "vram_fnv1a_64", "pc="], 4)
            })
            .unwrap_or_else(|| "log missing".to_string()),
        "Pin these hashes when a hardware-visible checkpoint changes unexpectedly.",
    )?;

    let bios_cdrom = read_optional_log(&preburn_dir.join("bios-cdrom-probe.log"))?;
    report_row(
        &mut report,
        "BIOS CD-ROM command flow",
        &preburn_dir.join("bios-cdrom-probe.log"),
        log_status(bios_cdrom.as_deref(), &["ReadTOC", "ReadN", "Pause"]),
        bios_cdrom
            .as_deref()
            .map(|text| {
                if text.contains("skip BIOS CD-ROM probe:") {
                    summarize_matching_lines(text, &["skip BIOS CD-ROM probe:"], 1)
                } else {
                    summarize_command_names(
                        text,
                        &[
                            "Test", "GetID", "ReadTOC", "SetLoc", "SeekL", "SetMode", "ReadN",
                            "Pause",
                        ],
                    )
                }
            })
            .unwrap_or_else(|| "log missing".to_string()),
        "Use command-order deltas here to improve emulator CD-ROM timing and BIOS boot behavior.",
    )?;

    let boot_flow = read_optional_log(&preburn_dir.join("boot-flow.log"))?;
    report_row(
        &mut report,
        "BIOS boot flow, CD data, CD-DA",
        &preburn_dir.join("boot-flow.log"),
        log_status(boot_flow.as_deref(), &["fifo pops:", "audio:", "Play"]),
        boot_flow
            .as_deref()
            .map(|text| {
                if text.contains("skip BIOS boot-flow probe:") {
                    summarize_matching_lines(text, &["skip BIOS boot-flow probe:"], 1)
                } else {
                    summarize_matching_lines(text, &["display:", "fifo pops:", "audio:", "Play"], 5)
                }
            })
            .unwrap_or_else(|| "log missing".to_string()),
        "This is the closest local proxy for the real CD path; failures should become emulator or engine fixes.",
    )?;

    let cdda = read_optional_log(&preburn_dir.join("cdda-probe.log"))?;
    report_row(
        &mut report,
        "CD-DA audio probe",
        &preburn_dir.join("cdda-probe.log"),
        log_status(cdda.as_deref(), &["Nonzero:", "Peak:", "Play="]),
        cdda.as_deref()
            .map(|text| summarize_matching_lines(text, &["Nonzero:", "Peak:", "CD CMDS:"], 4))
            .unwrap_or_else(|| "log missing".to_string()),
        "Keep CD-DA and streamed data ownership regressions visible; they are central to the hardware issue.",
    )?;

    let streaming_guard = read_optional_log(&preburn_dir.join("streaming-guard.log"))?;
    report_row(
        &mut report,
        "CD-DA + room streaming guard",
        &preburn_dir.join("streaming-guard.log"),
        log_status(streaming_guard.as_deref(), &["streaming guard: PASS"]),
        streaming_guard
            .as_deref()
            .map(|text| {
                summarize_matching_lines(
                    text,
                    &["streaming guard:", "profile rows=", "failure:"],
                    6,
                )
            })
            .unwrap_or_else(|| "log missing".to_string()),
        "This red gate must cover menu CD-DA and gameplay room streaming before a burn candidate is trusted.",
    )?;

    let duck = read_optional_log(&duckstation_log)?;
    let duck_markers = [
        "psx-rt: main",
        "editor-playtest: init ok",
        "psx-engine: scene init ok",
        "psx-engine: cdda setmode ok",
        "psx-engine: cdda demute ok",
        "psx-engine: cdda play ok",
    ];
    report_row(
        &mut report,
        "DuckStation BIOS TTY markers",
        &duckstation_log,
        log_status(duck.as_deref(), &duck_markers),
        duck.as_deref()
            .map(|text| {
                format!(
                    "{}/{} markers observed",
                    count_matching_needles(text, &duck_markers),
                    duck_markers.len()
                )
            })
            .unwrap_or_else(|| "log missing".to_string()),
        "TTY markers are semantic assertions; keep adding markers for real-CD stop points.",
    )?;

    external_report_row(
        &mut report,
        "Mednafen BIOS/TOC smoke",
        &external_dir.join("cortex_ignition_v1-mednafen.log"),
        &["Using module: psx", "Region:"],
        &[],
        "OBSERVED",
        "BIOS core selection and disc TOC are visible, but there is still no semantic boot-progress assertion.",
    )?;
    external_report_row(
        &mut report,
        "RetroArch launch smoke",
        &external_dir.join("cortex_ignition_v1-retroarch.log"),
        &["[INFO]", "Audio"],
        &["Firmware is missing", "Failed to load content"],
        "OBSERVED",
        "A missing firmware warning invalidates the signal; configure BIOS before using this row as coverage.",
    )?;
    let retroarch_screenshot = external_dir.join("cortex_ignition_v1-retroarch.png");
    report_row(
        &mut report,
        "RetroArch frame screenshot",
        &retroarch_screenshot,
        png_file_status(&retroarch_screenshot),
        png_file_signal(&retroarch_screenshot),
        "Frame capture proves the core advanced far enough to render a host-side image; pin hashes later if this becomes stable.",
    )?;
    external_report_row(
        &mut report,
        "ares TOC smoke",
        &external_dir.join("cortex_ignition_v1-ares.log"),
        &["session", "track01", "track02"],
        &[],
        "OBSERVED",
        "ares currently gives TOC/process-liveness coverage, not boot-progress assertions.",
    )?;

    writeln!(
        report,
        "\n## Hardware Finding Rule\n\n\
         Every real-CD observation should land in one of three places:\n\n\
         - a `hardware-boot-visual` checkpoint or text label when the finding is visible on a TV;\n\
         - a preburn/emulator assertion when the finding is reproducible without hardware;\n\
         - a tracked emulator, engine, or SDK fix when the finding explains a behavioral gap.\n\n\
         Record real CD-R observations in local hardware notes and decode color/text stops \
         with `docs/hardware-visual-checkpoints.md`. \
         The current visible checkpoint sources are `engine/crates/psx-engine/src/app.rs` and \
         `engine/crates/psx-engine/src/game_app.rs`. If the next CD stops on a color/text code, \
         preserve that code here and convert it into a named gate before burning again.\n"
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        report,
        "## Focused Probe Routing\n\n\
         When hardware and emulators disagree, route active red rows to one narrow owner at a time: \
         CD-ROM IRQ/data timing, \
         CD-DA/data arbitration, SIO/pad timing, GPU display area, or engine/SDK disc layout.\n"
    )
    .map_err(|e| e.to_string())?;

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(&out_path, report).map_err(|e| format!("{}: {e}", out_path.display()))?;
    println!("wrote {}", relative_to_root(&out_path));
    if fail_on_warn {
        let text =
            fs::read_to_string(&out_path).map_err(|e| format!("{}: {e}", out_path.display()))?;
        if text.contains("| WARN |") || text.contains("| MISSING |") {
            return Err(format!(
                "bringup report contains WARN/MISSING rows: {}",
                relative_to_root(&out_path)
            ));
        }
    }
    Ok(())
}

fn read_optional_log(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn png_file_status(path: &Path) -> &'static str {
    match fs::read(path) {
        Ok(bytes) if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() > 8 => "PASS",
        Ok(_) => "WARN",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "MISSING",
        Err(_) => "WARN",
    }
}

fn png_file_signal(path: &Path) -> String {
    match fs::read(path) {
        Ok(bytes) if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() > 8 => {
            format!("valid PNG, {} bytes", bytes.len())
        }
        Ok(bytes) => format!("not a valid PNG, {} bytes", bytes.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "file missing".to_string(),
        Err(error) => format!("read error: {error}"),
    }
}

fn log_status(text: Option<&str>, required: &[&str]) -> &'static str {
    match text {
        None => "MISSING",
        Some(text) if required.iter().all(|needle| text.contains(needle)) => "PASS",
        Some(_) => "WARN",
    }
}

fn external_report_row(
    report: &mut String,
    surface: &str,
    path: &Path,
    positive: &[&str],
    negative: &[&str],
    success_status: &'static str,
    follow_up: &str,
) -> Result<(), String> {
    let text = read_optional_log(path)?;
    let status = match text.as_deref() {
        None => "MISSING",
        Some(text) if negative.iter().any(|needle| text.contains(needle)) => "WARN",
        Some(text) if positive.iter().all(|needle| text.contains(needle)) => success_status,
        Some(_) => "WARN",
    };
    let signal = text
        .as_deref()
        .map(|text| {
            if negative.iter().any(|needle| text.contains(needle)) {
                summarize_matching_lines(text, negative, 5)
            } else {
                summarize_matching_lines(text, positive, 5)
            }
        })
        .unwrap_or_else(|| "log missing".to_string());
    report_row(report, surface, path, status, signal, follow_up)
}

fn report_row(
    report: &mut String,
    surface: &str,
    path: &Path,
    status: &str,
    signal: String,
    follow_up: &str,
) -> Result<(), String> {
    writeln!(
        report,
        "| {} | `{}` | {} | {} | {} |",
        markdown_cell(surface),
        markdown_cell(&relative_to_root(path)),
        markdown_cell(status),
        markdown_cell(&signal),
        markdown_cell(follow_up)
    )
    .map_err(|e| e.to_string())
}

fn markdown_cell(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace('\n', " ")
        .trim()
        .to_string()
}

fn summarize_matching_lines(text: &str, needles: &[&str], max_lines: usize) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        if needles
            .iter()
            .any(|needle| !needle.is_empty() && line.contains(needle))
        {
            lines.push(truncate_middle(line.trim(), 120));
            if lines.len() == max_lines {
                break;
            }
        }
    }
    if lines.is_empty() {
        "no matching signal lines".to_string()
    } else {
        lines.join("; ")
    }
}

fn summarize_command_names(text: &str, names: &[&str]) -> String {
    let found: Vec<_> = names
        .iter()
        .filter(|name| text.contains(**name))
        .copied()
        .collect();
    if found.is_empty() {
        "no command names found".to_string()
    } else {
        found.join(", ")
    }
}

fn count_matching_needles(text: &str, needles: &[&str]) -> usize {
    needles
        .iter()
        .filter(|needle| text.contains(**needle))
        .count()
}

fn mednafen_command(cue: &Path, bios: Option<&str>) -> Result<Command, String> {
    let binary = discover_mednafen(None)?;
    let mut command = Command::new(binary);
    if let Some(bios) = bios.filter(|s| !s.is_empty()) {
        let bios = resolve_from_cwd(bios);
        if bios.is_file() {
            let bios = bios.to_string_lossy().to_string();
            command
                .arg("-psx.bios_na")
                .arg(&bios)
                .arg("-psx.bios_jp")
                .arg(&bios)
                .arg("-psx.bios_eu")
                .arg(&bios);
        }
    }
    command.arg("-force_module").arg("psx").arg(cue);
    Ok(command)
}

fn retroarch_command(
    cue: &Path,
    bios: Option<&str>,
    screenshot: Option<&Path>,
    screenshot_frames: u64,
) -> Result<Command, String> {
    let binary = discover_retroarch(None)?;
    let core = find_retroarch_psx_core().ok_or(
        "RetroArch found, but no PSX libretro core found. Set RETROARCH_PSX_CORE=/path/to/core.",
    )?;
    let mut command = Command::new(binary);
    command.arg("--verbose");
    if let Some(config) = retroarch_bios_append_config(bios)? {
        command.arg(format!("--appendconfig={}", config.display()));
    }
    if let Some(screenshot) = screenshot {
        command
            .arg(format!("--max-frames={screenshot_frames}"))
            .arg("--max-frames-ss")
            .arg(format!("--max-frames-ss-path={}", screenshot.display()));
    }
    command.arg("-L").arg(core).arg(cue);
    Ok(command)
}

fn retroarch_bios_append_config(bios: Option<&str>) -> Result<Option<PathBuf>, String> {
    let Some(bios) = bios.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let bios = resolve_from_cwd(bios);
    let system_dir = if bios.is_dir() {
        bios
    } else if bios.is_file() {
        bios.parent()
            .ok_or_else(|| format!("BIOS has no parent directory: {}", bios.display()))?
            .to_path_buf()
    } else {
        return Ok(None);
    };
    let config = repo_root().join("build/external-emulator-smoke/retroarch-system.cfg");
    if let Some(parent) = config.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let system_dir = retroarch_config_string(&system_dir.to_string_lossy());
    fs::write(&config, format!("system_directory = {system_dir}\n"))
        .map_err(|e| format!("{}: {e}", config.display()))?;
    Ok(Some(config))
}

fn retroarch_config_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn ares_command(cue: &Path, bios: Option<&str>) -> Result<Command, String> {
    let binary = discover_ares(None)?;
    let mut command = Command::new(binary);
    command
        .arg("--system")
        .arg("PlayStation")
        .arg("--no-file-prompt");
    if let Some(bios) = bios.filter(|s| !s.is_empty()) {
        let bios = resolve_from_cwd(bios);
        if bios.is_file() {
            let bios = bios.to_string_lossy().to_string();
            for region in ["BIOS.US", "BIOS.Japan", "BIOS.Europe"] {
                command
                    .arg("--setting")
                    .arg(format!("PlayStation/Firmware/{region}={bios}"));
            }
        }
    }
    command.arg(cue);
    Ok(command)
}

fn truncate_middle(text: &str, max_len: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_len {
        return text.to_string();
    }
    if max_len <= 3 {
        return ".".repeat(max_len);
    }
    let keep = (max_len - 3) / 2;
    let tail = max_len - 3 - keep;
    let head: String = chars.iter().take(keep).collect();
    let tail: String = chars.iter().skip(chars.len() - tail).collect();
    format!("{head}...{tail}")
}

struct WavPcm {
    samples: Vec<i16>,
    channels: usize,
    rate: usize,
    frames: usize,
}

fn read_wav_pcm16(path: &Path) -> Result<WavPcm, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err(format!("{}: expected RIFF/WAVE", path.display()));
    }
    let mut offset = 12usize;
    let mut channels = None;
    let mut rate = None;
    let mut bits = None;
    let mut data = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let len = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
        let end = offset.saturating_add(len).min(bytes.len());
        match id {
            b"fmt " if len >= 16 => {
                let format = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
                if format != 1 {
                    return Err(format!("{}: expected PCM WAV format", path.display()));
                }
                channels = Some(u16::from_le_bytes(
                    bytes[offset + 2..offset + 4].try_into().unwrap(),
                ) as usize);
                rate = Some(
                    u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize,
                );
                bits = Some(u16::from_le_bytes(
                    bytes[offset + 14..offset + 16].try_into().unwrap(),
                ));
            }
            b"data" => data = Some(bytes[offset..end].to_vec()),
            _ => {}
        }
        offset = end + (len & 1);
    }
    let channels = channels.ok_or_else(|| format!("{}: missing fmt chunk", path.display()))?;
    let rate = rate.ok_or_else(|| format!("{}: missing sample rate", path.display()))?;
    if bits != Some(16) {
        return Err(format!("{}: expected 16-bit PCM WAV", path.display()));
    }
    let data = data.ok_or_else(|| format!("{}: missing data chunk", path.display()))?;
    let mut samples = Vec::with_capacity(data.len() / 2);
    for chunk in data.chunks_exact(2) {
        samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
    }
    let frames = samples.len() / channels.max(1);
    Ok(WavPcm {
        samples,
        channels,
        rate,
        frames,
    })
}

fn log_spaced_frequencies(count: usize, min_hz: f64, max_hz: f64) -> Vec<f64> {
    if count == 1 {
        return vec![(min_hz + max_hz) * 0.5];
    }
    let ratio = max_hz / min_hz;
    (0..count)
        .map(|i| min_hz * ratio.powf(i as f64 / (count - 1) as f64))
        .collect()
}

fn frame_samples(wav: &WavPcm, start: usize, window: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(window.len());
    for (i, &weight) in window.iter().enumerate() {
        let src = start + i;
        if src >= wav.frames {
            out.push(0.0);
            continue;
        }
        let base = src * wav.channels;
        let sample = if wav.channels == 1 {
            wav.samples[base] as f64
        } else {
            (wav.samples[base] as f64 + wav.samples[base + 1] as f64) * 0.5
        };
        out.push((sample / 32768.0) * weight);
    }
    out
}

fn goertzel_power(samples: &[f64], rate: usize, freq: f64) -> f64 {
    let n = samples.len();
    let k = ((n as f64 * freq) / rate as f64)
        .round()
        .clamp(1.0, (n / 2 - 1) as f64);
    let omega = (2.0 * std::f64::consts::PI * k) / n as f64;
    let coeff = 2.0 * omega.cos();
    let mut s_prev = 0.0;
    let mut s_prev2 = 0.0;
    for &sample in samples {
        let s = sample + coeff * s_prev - s_prev2;
        s_prev2 = s_prev;
        s_prev = s;
    }
    s_prev2 * s_prev2 + s_prev * s_prev - coeff * s_prev * s_prev2
}

fn percentile_f64(values: &mut [f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let idx = ((values.len() - 1) as f64 * q).round() as usize;
    values[idx.min(values.len() - 1)]
}

fn normalize_and_smooth(powers: &[Vec<f64>]) -> Vec<u8> {
    if powers.is_empty() {
        return Vec::new();
    }
    let frames = powers.len();
    let bands = powers[0].len();
    let mut floors = vec![0.0; bands];
    let mut ceilings = vec![1.0; bands];
    for band in 0..bands {
        let mut values: Vec<f64> = (0..frames)
            .map(|frame| powers[frame][band].ln_1p())
            .collect();
        let mut values_hi = values.clone();
        floors[band] = percentile_f64(&mut values, 0.10);
        ceilings[band] = (floors[band] + 0.001).max(percentile_f64(&mut values_hi, 0.96));
    }
    let mut prev = vec![0.0; bands];
    let mut out = vec![0; frames * bands];
    for frame in 0..frames {
        for band in 0..bands {
            let value = powers[frame][band].ln_1p();
            let norm = ((value - floors[band]) / (ceilings[band] - floors[band])).clamp(0.0, 1.0);
            let target = norm.sqrt();
            let smoothed = if target > prev[band] {
                prev[band] * 0.35 + target * 0.65
            } else {
                prev[band] * 0.78 + target * 0.22
            };
            prev[band] = smoothed;
            out[frame * bands + band] = (smoothed * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn duckstation_harness(args: &[String]) -> Result<(), String> {
    let mut cue = None;
    let mut duckstation = env::var("DUCKSTATION_BIN").ok();
    let mut timeout = 45.0f64;
    let mut settle = 0.75f64;
    let mut log = Some("build/duckstation-harness/game-magikaaaaaarp-pong.log".to_string());
    let mut expects = Vec::new();
    let mut no_default_expect = false;
    let mut gui = false;
    let mut bios_boot = false;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--cue" => {
                i += 1;
                cue = args.get(i).cloned();
            }
            "--duckstation" => {
                i += 1;
                duckstation = args.get(i).cloned();
            }
            "--timeout" => {
                i += 1;
                timeout = parse_arg(args.get(i), "--timeout")?;
            }
            "--settle" => {
                i += 1;
                settle = parse_arg(args.get(i), "--settle")?;
            }
            "--log" => {
                i += 1;
                log = args.get(i).cloned();
            }
            "--expect" => {
                i += 1;
                expects.push(args.get(i).cloned().ok_or("--expect requires value")?);
            }
            "--no-default-expect" => no_default_expect = true,
            "--gui" => gui = true,
            "--bios-boot" => bios_boot = true,
            other => return Err(format!("unknown duckstation-harness arg `{other}`")),
        }
        i += 1;
    }
    let cue_path = resolve_from_cwd(
        cue.as_deref()
            .unwrap_or("build/examples/mipsel-sony-psx/release/game-magikaaaaaarp-pong.cue"),
    );
    if !cue_path.is_file() {
        return Err(format!("Disc cue not found: {}", cue_path.display()));
    }
    let duckstation = discover_duckstation(duckstation.as_deref())?;
    let log_path = resolve_from_cwd(
        log.as_deref()
            .unwrap_or("build/duckstation-harness/game.log"),
    );
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let mut expected = if no_default_expect {
        Vec::new()
    } else {
        vec![
            "psx-rt: main".to_string(),
            "magikarp: init ok".to_string(),
            "psx-engine: present ok".to_string(),
            "magikarp: cdda setmode".to_string(),
            "magikarp: cdda setmode ack".to_string(),
            "magikarp: cdda demute".to_string(),
            "magikarp: cdda demute ack".to_string(),
            "magikarp: cdda play".to_string(),
            "magikarp: cdda play ack".to_string(),
            "magikarp: cdda ok".to_string(),
        ]
    };
    expected.extend(expects);
    let mut command = Command::new(&duckstation);
    command
        .arg("-batch")
        .arg("-nofullscreen")
        .arg("-earlyconsole")
        .arg(if bios_boot { "-slowboot" } else { "-fastboot" });
    if !gui {
        command.arg("-nogui");
    }
    command.arg("--").arg(&cue_path);
    println!("DuckStation: {}", duckstation.display());
    println!("Disc:        {}", cue_path.display());
    println!("Log:         {}", log_path.display());
    println!("Command:     {:?}", command);
    let mut child = command
        .current_dir(repo_root())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("launch DuckStation: {e}"))?;
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel();
    spawn_line_reader(stdout, tx.clone());
    spawn_line_reader(stderr, tx);
    let mut log_file =
        fs::File::create(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
    let mut seen = HashSet::new();
    let mut all_lines = Vec::new();
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    let mut complete_deadline = None;
    while Instant::now() < deadline {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() && rx.try_recv().is_err() {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                writeln!(log_file, "{line}").map_err(|e| e.to_string())?;
                for marker in &expected {
                    if line.contains(marker) {
                        seen.insert(marker.clone());
                    }
                }
                all_lines.push(line);
                if !expected.is_empty()
                    && seen.len() == expected.len()
                    && complete_deadline.is_none()
                {
                    complete_deadline = Some(Instant::now() + Duration::from_secs_f64(settle));
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if complete_deadline.is_some_and(|when| Instant::now() >= when) {
            break;
        }
    }
    if child.try_wait().map_err(|e| e.to_string())?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let missing: Vec<_> = expected
        .iter()
        .filter(|marker| !seen.contains(*marker))
        .collect();
    if !missing.is_empty() {
        eprintln!("\nMissing DuckStation markers:");
        for marker in missing {
            eprintln!("  - {marker}");
        }
        eprintln!("\nImportant log tail:");
        for line in important_tail(&all_lines) {
            eprintln!("{line}");
        }
        return Err("DuckStation markers missing".into());
    }
    println!("\nDuckStation markers observed:");
    for marker in expected {
        println!("  ok {marker}");
    }
    Ok(())
}

fn discover_duckstation(explicit: Option<&str>) -> Result<PathBuf, String> {
    if let Some(explicit) = explicit.filter(|s| !s.is_empty()) {
        let candidate = PathBuf::from(explicit);
        if candidate.extension().and_then(|s| s.to_str()) == Some("app") {
            if let Some(binary) = duckstation_binary_from_app(&candidate) {
                return Ok(binary);
            }
        }
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(format!(
            "DuckStation binary not executable: {}",
            candidate.display()
        ));
    }
    for name in ["DuckStation", "duckstation-qt", "duckstation"] {
        if let Some(path) = find_on_path(name) {
            return Ok(path);
        }
    }
    for app in [
        PathBuf::from("/Applications/DuckStation.app"),
        dirs_home().join("Applications/DuckStation.app"),
        dirs_home().join("Downloads/DuckStation.app"),
    ] {
        if let Some(binary) = duckstation_binary_from_app(&app) {
            return Ok(binary);
        }
    }
    Err("DuckStation not found. Set DUCKSTATION_BIN=/path/to/DuckStation.".into())
}

fn duckstation_binary_from_app(app: &Path) -> Option<PathBuf> {
    mac_app_binary_from_app(app, &["DuckStation", "duckstation-qt", "duckstation"])
}

fn discover_pcsx_redux(explicit: Option<&str>) -> Result<PathBuf, String> {
    let explicit = explicit
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| env::var("PSOXIDE_REDUX_BIN").ok().filter(|s| !s.is_empty()));
    if let Some(explicit) = explicit {
        let candidate = PathBuf::from(explicit);
        if candidate.extension().and_then(|s| s.to_str()) == Some("app") {
            if let Some(binary) = mac_app_binary_from_app(&candidate, &["PCSX-Redux", "pcsx-redux"])
            {
                return Ok(binary);
            }
        }
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(format!(
            "PCSX-Redux binary not found: {}",
            candidate.display()
        ));
    }
    for name in ["pcsx-redux", "PCSX-Redux"] {
        if let Some(path) = find_on_path(name) {
            return Ok(path);
        }
    }
    for app in [
        PathBuf::from("/Applications/PCSX-Redux.app"),
        dirs_home().join("Applications/PCSX-Redux.app"),
        dirs_home().join("Downloads/PCSX-Redux.app"),
    ] {
        if let Some(binary) = mac_app_binary_from_app(&app, &["PCSX-Redux", "pcsx-redux"]) {
            return Ok(binary);
        }
    }
    Err("PCSX-Redux not found. Set PSOXIDE_REDUX_BIN=/path/to/pcsx-redux.".into())
}

fn discover_mednafen(explicit: Option<&str>) -> Result<PathBuf, String> {
    let explicit = explicit
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| env::var("MEDNAFEN_BIN").ok().filter(|s| !s.is_empty()));
    if let Some(explicit) = explicit {
        let candidate = PathBuf::from(explicit);
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(format!(
            "Mednafen binary not found: {}",
            candidate.display()
        ));
    }
    find_on_path("mednafen").ok_or_else(|| {
        "Mednafen not found. Install with `brew install mednafen` or set MEDNAFEN_BIN.".into()
    })
}

fn discover_retroarch(explicit: Option<&str>) -> Result<PathBuf, String> {
    let explicit = explicit
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| env::var("RETROARCH_BIN").ok().filter(|s| !s.is_empty()));
    if let Some(explicit) = explicit {
        let candidate = PathBuf::from(explicit);
        if candidate.extension().and_then(|s| s.to_str()) == Some("app") {
            if let Some(binary) = retroarch_binary_from_app(&candidate) {
                return Ok(binary);
            }
        }
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(format!(
            "RetroArch binary not found: {}",
            candidate.display()
        ));
    }
    for name in ["retroarch", "RetroArch", "RetroArch-Metal"] {
        if let Some(path) = find_on_path(name) {
            return Ok(path);
        }
    }
    for app in [
        PathBuf::from("/Applications/RetroArch.app"),
        PathBuf::from("/Applications/RetroArch Metal.app"),
        dirs_home().join("Applications/RetroArch.app"),
        dirs_home().join("Downloads/RetroArch.app"),
    ] {
        if let Some(binary) = retroarch_binary_from_app(&app) {
            return Ok(binary);
        }
    }
    Err("RetroArch not found. Install with `brew install --cask retroarch-metal` or set RETROARCH_BIN.".into())
}

fn retroarch_binary_from_app(app: &Path) -> Option<PathBuf> {
    mac_app_binary_from_app(app, &["RetroArch", "RetroArch-Metal", "retroarch"])
}

fn find_retroarch_psx_core() -> Option<PathBuf> {
    if let Ok(core) = env::var("RETROARCH_PSX_CORE") {
        let path = PathBuf::from(core);
        if path.is_file() {
            return Some(path);
        }
    }
    let names = [
        "mednafen_psx_hw_libretro.dylib",
        "mednafen_psx_libretro.dylib",
        "beetle_psx_hw_libretro.dylib",
        "beetle_psx_libretro.dylib",
        "swanstation_libretro.dylib",
        "pcsx_rearmed_libretro.dylib",
    ];
    let dirs = [
        PathBuf::from("/Applications/RetroArch.app/Contents/Resources/cores"),
        PathBuf::from("/Applications/RetroArch Metal.app/Contents/Resources/cores"),
        dirs_home().join("Library/Application Support/RetroArch/cores"),
        dirs_home().join(".config/retroarch/cores"),
        PathBuf::from("/opt/homebrew/lib/libretro"),
        PathBuf::from("/usr/local/lib/libretro"),
    ];
    for dir in dirs {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn discover_ares(explicit: Option<&str>) -> Result<PathBuf, String> {
    let explicit = explicit
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| env::var("ARES_BIN").ok().filter(|s| !s.is_empty()));
    if let Some(explicit) = explicit {
        let candidate = PathBuf::from(explicit);
        if candidate.extension().and_then(|s| s.to_str()) == Some("app") {
            if let Some(binary) = mac_app_binary_from_app(&candidate, &["ares", "Ares"]) {
                return Ok(binary);
            }
        }
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(format!("ares binary not found: {}", candidate.display()));
    }
    for name in ["ares", "Ares"] {
        if let Some(path) = find_on_path(name) {
            return Ok(path);
        }
    }
    for app in [
        PathBuf::from("/Applications/ares.app"),
        PathBuf::from("/Applications/Ares.app"),
        dirs_home().join("Applications/ares.app"),
        dirs_home().join("Downloads/ares.app"),
    ] {
        if let Some(binary) = mac_app_binary_from_app(&app, &["ares", "Ares"]) {
            return Ok(binary);
        }
    }
    Err("ares not found. Install ares.app or set ARES_BIN.".into())
}

fn mac_app_binary_from_app(app: &Path, names: &[&str]) -> Option<PathBuf> {
    let macos = app.join("Contents/MacOS");
    for name in names {
        let candidate = macos.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn dirs_home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn spawn_line_reader<R: Read + Send + 'static>(reader: R, tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
}

fn important_tail(lines: &[String]) -> Vec<&str> {
    let patterns = ["I/TTY:", "E(", "W(", "V/PerfMon:", "V/AudioStream:"];
    let mut important: Vec<&str> = lines
        .iter()
        .filter(|line| patterns.iter().any(|pattern| line.contains(pattern)))
        .map(String::as_str)
        .collect();
    if important.len() > 80 {
        important.drain(0..important.len() - 80);
    }
    important
}

fn vblank_chart(args: &[String]) -> Result<(), String> {
    let mut input = None;
    let mut output = None;
    let mut title = None;
    let mut budget = ONE_VBLANK_CYCLES;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--in" => {
                i += 1;
                input = args.get(i).cloned();
            }
            "--out" => {
                i += 1;
                output = args.get(i).cloned();
            }
            "--title" => {
                i += 1;
                title = args.get(i).cloned();
            }
            "--budget" => {
                i += 1;
                budget = parse_arg(args.get(i), "--budget")?;
            }
            other => return Err(format!("unknown vblank-chart arg `{other}`")),
        }
        i += 1;
    }
    let input = resolve_from_cwd(input.as_deref().ok_or("vblank-chart requires --in")?);
    let output = resolve_from_cwd(output.as_deref().ok_or("vblank-chart requires --out")?);
    let csv = fs::read_to_string(&input).map_err(|e| format!("{}: {e}", input.display()))?;
    let mut rows = csv.lines();
    let header: Vec<String> = rows
        .next()
        .ok_or("profile CSV is empty")?
        .split(',')
        .map(str::to_string)
        .collect();
    let row_values: Vec<Vec<String>> = rows
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split(',').map(str::to_string).collect())
        .collect();
    let frame_cycles_idx =
        csv_col(&header, "frame_cycles").ok_or("CSV has no frame_cycles column")?;
    let render_idx = csv_col(&header, "render");
    let present_idx = csv_col(&header, "present");
    let miss_idx = csv_col(&header, "visual_deadline_misses");
    let bands = [
        ("update", "sim: update", "#1f6feb"),
        ("frame_clear", "frame clear", "#6e7681"),
        ("room", "room", "#2f81f7"),
        ("sky", "sky", "#58a6ff"),
        ("far_vista", "far vista", "#79c0ff"),
        ("image_props", "props (image/box)", "#2ea043"),
        ("model_instances", "models", "#3fb950"),
        ("player", "player", "#56d364"),
        ("equipment", "equipment", "#2dd4bf"),
        ("world_flush", "world flush/sort", "#db61a2"),
        ("ot_submit", "ot submit", "#e3a008"),
        ("ot_wait", "gpu draw (ot wait)", "#e3633a"),
        ("render_other", "render glue", "#768390"),
        ("present", "present", "#8957e5"),
        ("frame_other", "idle / loop", "#373e47"),
    ];
    let mut bars = Vec::new();
    let mut render_cyc = Vec::new();
    let mut sim_cyc = Vec::new();
    let mut misses = 0u64;
    for row in &row_values {
        let fc = csv_num(row, Some(frame_cycles_idx));
        let render = csv_num(row, render_idx);
        let present = csv_num(row, present_idx);
        let mut values = Vec::new();
        let mut render_leaf_sum = 0u64;
        for (key, _, _) in bands.iter().take(12) {
            let value = csv_num(row, csv_col(&header, key));
            if *key != "update" {
                render_leaf_sum = render_leaf_sum.saturating_add(value);
            }
            values.push(value);
        }
        let render_other = render.saturating_sub(render_leaf_sum);
        let frame_other = fc
            .saturating_sub(values[0])
            .saturating_sub(render)
            .saturating_sub(present);
        values.push(render_other);
        values.push(present);
        values.push(frame_other);
        let is_render = render > 0;
        if is_render {
            render_cyc.push(fc);
        } else {
            sim_cyc.push(fc);
        }
        let miss = csv_num(row, miss_idx);
        misses = misses.saturating_add(miss);
        bars.push((values, fc, is_render, miss));
    }
    let title = title.unwrap_or_else(|| {
        format!(
            "per-vblank work - {}",
            input
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("profile")
        )
    });
    let html = render_chart_html(&title, &bands, &bars, budget);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(&output, html).map_err(|e| format!("{}: {e}", output.display()))?;
    render_cyc.sort_unstable();
    sim_cyc.sort_unstable();
    let render_avg = avg_u64(&render_cyc);
    let sim_avg = avg_u64(&sim_cyc);
    let all_avg = avg_u64(
        &render_cyc
            .iter()
            .chain(sim_cyc.iter())
            .copied()
            .collect::<Vec<_>>(),
    );
    println!("wrote {}  ({} vblanks)", output.display(), bars.len());
    println!(
        "  render vblanks : {:>4}  avg {}  p50 {}  over-budget {}/{}",
        render_cyc.len(),
        fmt_budget(render_avg, budget),
        fmt_budget(percentile_u64(&render_cyc, 50), budget),
        render_cyc.iter().filter(|&&v| v > budget).count(),
        render_cyc.len()
    );
    println!(
        "  sim-only       : {:>4}  avg {}  p50 {}",
        sim_cyc.len(),
        fmt_budget(sim_avg, budget),
        fmt_budget(percentile_u64(&sim_cyc, 50), budget)
    );
    println!(
        "  overall avg    : {}   <- perfectly-spread target",
        fmt_budget(all_avg, budget)
    );
    println!("  deadline misses: {misses}");
    Ok(())
}

fn csv_col(header: &[String], name: &str) -> Option<usize> {
    header.iter().position(|col| col == name)
}

fn csv_num(row: &[String], idx: Option<usize>) -> u64 {
    idx.and_then(|i| row.get(i))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

fn avg_u64(values: &[u64]) -> u64 {
    if values.is_empty() {
        0
    } else {
        values.iter().sum::<u64>() / values.len() as u64
    }
}

fn percentile_u64(values: &[u64], p: u64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let idx = ((values.len() as u64 - 1) * p / 100) as usize;
    values[idx]
}

fn fmt_budget(value: u64, budget: u64) -> String {
    format!(
        "{value:>10} ({:5.1}% of 1 vblank)",
        value as f64 / budget as f64 * 100.0
    )
}

fn render_chart_html(
    title: &str,
    bands: &[(&str, &str, &str)],
    bars: &[(Vec<u64>, u64, bool, u64)],
    budget: u64,
) -> String {
    let labels = bands
        .iter()
        .map(|(_, label, _)| json_string(label))
        .collect::<Vec<_>>()
        .join(",");
    let colors = bands
        .iter()
        .map(|(_, _, color)| json_string(color))
        .collect::<Vec<_>>()
        .join(",");
    let max_fc = bars.iter().map(|(_, fc, _, _)| *fc).max().unwrap_or(budget);
    let cap = (budget * 2).max(max_fc);
    let bar_json = bars
        .iter()
        .map(|(values, fc, render, miss)| {
            format!(
                "{{\"s\":[{}],\"fc\":{},\"r\":{},\"m\":{}}}",
                values
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
                fc,
                if *render { 1 } else { 0 },
                miss
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"<!doctype html><meta charset="utf-8"><title>{title}</title>
<style>body{{margin:0;background:#0d1117;color:#c9d1d9;font:13px -apple-system,Segoe UI,sans-serif}}#hdr,#legend{{padding:12px 16px;border-bottom:1px solid #21262d}}canvas{{display:block;width:100%;height:420px}}.sw{{display:inline-block;width:11px;height:11px;margin:0 4px 0 12px}}</style>
<div id="hdr"><h1>{title}</h1><div id="stats"></div><div id="legend"></div></div><canvas id="c"></canvas>
<script>
const labels=[{labels}],colors=[{colors}],bars=[{bar_json}],budget={budget},cap={cap},hz={CPU_HZ};
document.getElementById('legend').innerHTML=labels.map((l,i)=>`<span class=sw style="background:${{colors[i]}}"></span>${{l}}`).join('');
const c=document.getElementById('c'),ctx=c.getContext('2d');function resize(){{c.width=c.clientWidth*devicePixelRatio;c.height=420*devicePixelRatio;ctx.setTransform(devicePixelRatio,0,0,devicePixelRatio,0,0);draw();}}addEventListener('resize',resize);
function ms(cyc){{return (cyc/hz*1000).toFixed(2)}}function draw(){{const w=c.clientWidth,h=420,plot=386,bw=w/Math.max(1,bars.length);ctx.clearRect(0,0,w,h);const y=v=>10+plot-(v/cap)*plot;for(let i=0;i<bars.length;i++){{let acc=0,x=i*bw;for(let s=0;s<bars[i].s.length;s++){{let v=bars[i].s[s];if(!v)continue;ctx.fillStyle=colors[s];ctx.fillRect(x+0.5,y(acc+v),Math.max(1,bw-1),Math.max(0,y(acc)-y(acc+v)));acc+=v;}}if(bars[i].m){{ctx.fillStyle='#f85149';ctx.fillRect(x,400,Math.max(1,bw-1),4)}}}}ctx.strokeStyle='#a8b1bb';ctx.setLineDash([4,4]);ctx.beginPath();ctx.moveTo(0,y(budget));ctx.lineTo(w,y(budget));ctx.stroke();}}
const render=bars.filter(b=>b.r),sim=bars.filter(b=>!b.r);const avg=a=>a.length?Math.round(a.reduce((s,b)=>s+b.fc,0)/a.length):0;document.getElementById('stats').textContent=`vblanks ${{bars.length}} · render ${{render.length}} avg ${{ms(avg(render))}}ms · sim ${{sim.length}} avg ${{ms(avg(sim))}}ms · 1 vblank ${{ms(budget)}}ms`;resize();
</script>"#,
        title = html_escape(title),
        labels = labels,
        colors = colors,
        bar_json = bar_json,
        budget = budget,
        cap = cap,
        CPU_HZ = CPU_HZ
    )
}

fn json_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn gen_tones() -> Result<(), String> {
    let crate_root = repo_root().join("sdk/crates/psx-spu");
    let vendor = crate_root.join("vendor");
    let out = crate_root.join("src/tones.rs");
    fs::create_dir_all(&vendor).map_err(|e| format!("{}: {e}", vendor.display()))?;
    let tones = [
        (
            "SINE",
            "Pure sine wave. Smooth, tonal - the default tuning reference.",
            make_sine_samples(),
        ),
        (
            "SQUARE",
            "Hard square wave. Buzzier, classic 8-bit timbre.",
            make_square_samples(),
        ),
        (
            "TRIANGLE",
            "Triangle wave. Softer than square, richer than sine.",
            make_triangle_samples(),
        ),
        (
            "SAWTOOTH",
            "Sawtooth ramp. Bright, full-spectrum - good for chiptune leads.",
            make_sawtooth_samples(),
        ),
    ];
    let mut rust = String::from(
        "//! Built-in ADPCM tone samples.\n//!\n//! Each tone is a single 16-byte ADPCM block with self-loop flags\n//! set (0x07 = loop end + repeat + loop start), so the SPU\n//! sustains the waveform until [`crate::Voice::key_off`]. Native\n//! playback frequency at [`crate::Pitch::UNITY`] is ~1575 Hz\n//! (44100 / 28 samples per loop); use [`crate::Pitch::for_frequency`]\n//! to tune to a specific note.\n//!\n//! Generated by `psoxide-dev gen-tones` from hand-rolled waveform\n//! definitions - no external dependency on an ADPCM encoder.\n\n",
    );
    rust.push_str("/// Native playback frequency (Hz) when a single-block\n/// tone is played at [`crate::Pitch::UNITY`]. The 28-sample\n/// loop at 44100 Hz gives 44100 / 28 = 1575 Hz.\npub const NATIVE_HZ: u32 = 1575;\n\n");
    for (name, doc, samples) in tones {
        let block = encode_adpcm_block(&samples, 8, 0, 0x07);
        let path = vendor.join(format!("tone_{}.adpcm", name.to_ascii_lowercase()));
        fs::write(&path, block).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("wrote {} - 16 bytes", relative_to_root(&path));
        rust.push_str(&format!(
            "/// {doc}\npub const {name}: &[u8; 16] = include_bytes!(\"../vendor/tone_{}.adpcm\");\n\n",
            name.to_ascii_lowercase()
        ));
    }
    rust.pop();
    fs::write(&out, rust).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("wrote {}", relative_to_root(&out));
    Ok(())
}

fn make_sine_samples() -> Vec<i8> {
    (0..28)
        .map(|i| {
            (f64::sin(2.0 * std::f64::consts::PI * i as f64 / 28.0) * 7.0)
                .round()
                .clamp(-8.0, 7.0) as i8
        })
        .collect()
}

fn make_square_samples() -> Vec<i8> {
    (0..28).map(|i| if i < 14 { 7 } else { -7 }).collect()
}

fn make_triangle_samples() -> Vec<i8> {
    (0..28)
        .map(|i| {
            let v = if i < 14 {
                -7.0 + (14.0 * i as f64 / 13.0).round()
            } else {
                7.0 - (14.0 * (i - 14) as f64 / 13.0).round()
            };
            v.clamp(-7.0, 7.0) as i8
        })
        .collect()
}

fn make_sawtooth_samples() -> Vec<i8> {
    (0..28)
        .map(|i| (-7.0 + (14.0 * i as f64 / 27.0).round()).clamp(-7.0, 7.0) as i8)
        .collect()
}

fn encode_adpcm_block(samples: &[i8], shift: u8, filter: u8, flags: u8) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[0] = (shift & 0x0f) | ((filter & 0x0f) << 4);
    out[1] = flags;
    for (idx, &sample) in samples.iter().take(28).enumerate() {
        let nibble = sample as u8 & 0x0f;
        let byte = 2 + idx / 2;
        if idx % 2 == 0 {
            out[byte] |= nibble;
        } else {
            out[byte] |= nibble << 4;
        }
    }
    out
}

fn gen_fonts() -> Result<(), String> {
    let crate_root = repo_root().join("sdk/crates/psx-font");
    let vendor = crate_root.join("vendor");
    let out_dir = crate_root.join("src/fonts");
    fs::create_dir_all(&out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let fonts = font_entries();
    let mut mod_rs = String::from(
        "//! Built-in fonts.\n//!\n//! Mostly from dhepper/font8x8 (8x8 monochrome), the\n//! canonical IBM VGA 8x16 BIOS font, and a curated set of\n//! commercially usable TTFs rasterized into compact PS1 cells.\n//! See `vendor/PROVENANCE.md` for the full chain.\n//!\n//! Generated by `psoxide-dev gen-fonts`. Add new entries there\n//! rather than editing this file by hand.\n\n",
    );
    for entry in fonts {
        let mut entry = entry;
        let glyphs = match entry.format {
            FontFormat::CHeader8x8 => parse_c_header_8x8(&vendor.join(entry.source), entry.count)?,
            FontFormat::IbmBin => {
                parse_ibm_bin(&vendor.join(entry.source), entry.height, entry.count)?
            }
            FontFormat::Ttf => parse_ttf_font(&vendor.join(entry.source), &mut entry)?,
        };
        let collisions = colliding_letters(&entry, &glyphs);
        if !collisions.is_empty() {
            let report = collisions
                .iter()
                .map(|group| group.iter().collect::<String>())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "{}: letters rasterize to identical bitmaps at {}x{}: {report}.\n\
                 A reader cannot tell them apart, so text in this font is wrong rather \n\
                 than merely ugly. Either raise the cell size until the letterforms \n\
                 separate, or drop the font: some display faces distinguish letters with \n\
                 a notch a pixel or two wide and simply have no legible small size.",
                entry.module, entry.width, entry.height
            ));
        }
        let module = emit_font_module(&entry, &glyphs);
        let path = out_dir.join(format!("{}.rs", entry.module));
        fs::write(&path, module).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("wrote {} - {} glyphs", relative_to_root(&path), entry.count);
        let last_cp = entry.first_cp + entry.count as u32 - 1;
        mod_rs.push_str(&format!(
            "/// Built-in font: `{}` (U+{:04X}..U+{:04X}, {} glyphs).\npub mod {};\n",
            entry.module, entry.first_cp, last_cp, entry.count, entry.module
        ));
    }
    mod_rs
        .push_str("\n// Flat re-exports so call sites read as\n// `psx_font::fonts::BASIC` etc.\n");
    for entry in font_entries() {
        mod_rs.push_str(&format!("pub use {}::{};\n", entry.module, entry.prefix));
    }
    fs::write(out_dir.join("mod.rs"), mod_rs).map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Clone, Copy)]
enum FontFormat {
    CHeader8x8,
    IbmBin,
    Ttf,
}

#[derive(Clone)]
struct FontEntry {
    source: &'static str,
    format: FontFormat,
    module: &'static str,
    prefix: &'static str,
    license: &'static str,
    first_cp: u32,
    count: usize,
    doc: &'static str,
    width: usize,
    height: usize,
    advance: usize,
    line_height: usize,
    max_cell_w: usize,
    max_cell_h: usize,
    preferred_px: f32,
    min_px: f32,
    advances: Option<Vec<u8>>,
}

fn font_entries() -> Vec<FontEntry> {
    let mut entries = vec![
        font_entry(
            "font8x8_basic.h",
            FontFormat::CHeader8x8,
            "basic",
            "BASIC",
            "Public Domain",
            0x00,
            128,
            "ASCII 0x00..0x7F.",
            8,
            8,
        ),
        font_entry(
            "font8x8_ext_latin.h",
            FontFormat::CHeader8x8,
            "ext_latin",
            "EXT_LATIN",
            "Public Domain",
            0xA0,
            96,
            "Latin-1 supplement, U+00A0..U+00FF.",
            8,
            8,
        ),
        font_entry(
            "font8x8_box.h",
            FontFormat::CHeader8x8,
            "boxdraw",
            "BOXDRAW",
            "Public Domain",
            0x2500,
            128,
            "Box-drawing U+2500..U+257F.",
            8,
            8,
        ),
        font_entry(
            "IBM_VGA_8x16.bin",
            FontFormat::IbmBin,
            "basic_8x16",
            "BASIC_8X16",
            "Public Domain",
            0x00,
            128,
            "IBM VGA 8x16 BIOS console font.",
            8,
            16,
        ),
    ];
    for (source, module, prefix, license, doc) in [
        (
            "kenney-fonts/Kenney Blocks.ttf",
            "kenney_blocks",
            "KENNEY_BLOCKS",
            "CC0-1.0",
            "Kenney Blocks, rasterized from Kenney Fonts.",
        ),
        (
            "kenney-fonts/Kenney Future.ttf",
            "kenney_future",
            "KENNEY_FUTURE",
            "CC0-1.0",
            "Kenney Future, rasterized from Kenney Fonts.",
        ),
        (
            "kenney-fonts/Kenney Future Narrow.ttf",
            "kenney_future_narrow",
            "KENNEY_FUTURE_NARROW",
            "CC0-1.0",
            "Kenney Future Narrow, rasterized from Kenney Fonts.",
        ),
        (
            "kenney-fonts/Kenney High.ttf",
            "kenney_high",
            "KENNEY_HIGH",
            "CC0-1.0",
            "Kenney High, rasterized from Kenney Fonts.",
        ),
        (
            "kenney-fonts/Kenney High Square.ttf",
            "kenney_high_square",
            "KENNEY_HIGH_SQUARE",
            "CC0-1.0",
            "Kenney High Square, rasterized from Kenney Fonts.",
        ),
        (
            "kenney-fonts/Kenney Mini.ttf",
            "kenney_mini",
            "KENNEY_MINI",
            "CC0-1.0",
            "Kenney Mini, rasterized from Kenney Fonts.",
        ),
        (
            "kenney-fonts/Kenney Mini Square.ttf",
            "kenney_mini_square",
            "KENNEY_MINI_SQUARE",
            "CC0-1.0",
            "Kenney Mini Square, rasterized from Kenney Fonts.",
        ),
        (
            "kenney-fonts/Kenney Mini Square Mono.ttf",
            "kenney_mini_square_mono",
            "KENNEY_MINI_SQUARE_MONO",
            "CC0-1.0",
            "Kenney Mini Square Mono, rasterized from Kenney Fonts.",
        ),
        (
            "kenney-fonts/Kenney Pixel.ttf",
            "kenney_pixel",
            "KENNEY_PIXEL",
            "CC0-1.0",
            "Kenney Pixel, rasterized from Kenney Fonts.",
        ),
        (
            "kenney-fonts/Kenney Pixel Square.ttf",
            "kenney_pixel_square",
            "KENNEY_PIXEL_SQUARE",
            "CC0-1.0",
            "Kenney Pixel Square, rasterized from Kenney Fonts.",
        ),
        (
            "kenney-fonts/Kenney Rocket.ttf",
            "kenney_rocket",
            "KENNEY_ROCKET",
            "CC0-1.0",
            "Kenney Rocket, rasterized from Kenney Fonts.",
        ),
        (
            "kenney-fonts/Kenney Rocket Square.ttf",
            "kenney_rocket_square",
            "KENNEY_ROCKET_SQUARE",
            "CC0-1.0",
            "Kenney Rocket Square, rasterized from Kenney Fonts.",
        ),
        (
            "google-fonts/pressstart2p/PressStart2P-Regular.ttf",
            "press_start_2p",
            "PRESS_START_2P",
            "SIL OFL 1.1",
            "Press Start 2P, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/silkscreen/Silkscreen-Regular.ttf",
            "silkscreen",
            "SILKSCREEN",
            "SIL OFL 1.1",
            "Silkscreen, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/pixelifysans/PixelifySans[wght].ttf",
            "pixelify_sans",
            "PIXELIFY_SANS",
            "SIL OFL 1.1",
            "Pixelify Sans, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/orbitron/Orbitron[wght].ttf",
            "orbitron",
            "ORBITRON",
            "SIL OFL 1.1",
            "Orbitron, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/audiowide/Audiowide-Regular.ttf",
            "audiowide",
            "AUDIOWIDE",
            "SIL OFL 1.1",
            "Audiowide, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/michroma/Michroma-Regular.ttf",
            "michroma",
            "MICHROMA",
            "SIL OFL 1.1",
            "Michroma, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/electrolize/Electrolize-Regular.ttf",
            "electrolize",
            "ELECTROLIZE",
            "SIL OFL 1.1",
            "Electrolize, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/oxanium/Oxanium[wght].ttf",
            "oxanium",
            "OXANIUM",
            "SIL OFL 1.1",
            "Oxanium, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/rajdhani/Rajdhani-Regular.ttf",
            "rajdhani",
            "RAJDHANI",
            "SIL OFL 1.1",
            "Rajdhani, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/chakrapetch/ChakraPetch-Regular.ttf",
            "chakra_petch",
            "CHAKRA_PETCH",
            "SIL OFL 1.1",
            "Chakra Petch, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/tektur/Tektur[wdth,wght].ttf",
            "tektur",
            "TEKTUR",
            "SIL OFL 1.1",
            "Tektur, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/tomorrow/Tomorrow-Regular.ttf",
            "tomorrow",
            "TOMORROW",
            "SIL OFL 1.1",
            "Tomorrow, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/zendots/ZenDots-Regular.ttf",
            "zen_dots",
            "ZEN_DOTS",
            "SIL OFL 1.1",
            "Zen Dots, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/turretroad/TurretRoad-Regular.ttf",
            "turret_road",
            "TURRET_ROAD",
            "SIL OFL 1.1",
            "Turret Road, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/tiny5/Tiny5-Regular.ttf",
            "tiny5",
            "TINY5",
            "SIL OFL 1.1",
            "Tiny5, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/jersey10/Jersey10-Regular.ttf",
            "jersey_10",
            "JERSEY_10",
            "SIL OFL 1.1",
            "Jersey 10, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/spacemono/SpaceMono-Regular.ttf",
            "space_mono",
            "SPACE_MONO",
            "SIL OFL 1.1",
            "Space Mono, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/brunoace/BrunoAce-Regular.ttf",
            "bruno_ace",
            "BRUNO_ACE",
            "SIL OFL 1.1",
            "Bruno Ace, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/aldrich/Aldrich-Regular.ttf",
            "aldrich",
            "ALDRICH",
            "SIL OFL 1.1",
            "Aldrich, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/syncopate/Syncopate-Regular.ttf",
            "syncopate",
            "SYNCOPATE",
            "Apache-2.0",
            "Syncopate, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/sharetechmono/ShareTechMono-Regular.ttf",
            "share_tech_mono",
            "SHARE_TECH_MONO",
            "SIL OFL 1.1",
            "Share Tech Mono, rasterized from Google Fonts.",
        ),
        (
            "google-fonts/jura/Jura[wght].ttf",
            "jura",
            "JURA",
            "SIL OFL 1.1",
            "Jura, rasterized from Google Fonts.",
        ),
    ] {
        let mut entry = font_entry(
            source,
            FontFormat::Ttf,
            module,
            prefix,
            license,
            0x20,
            96,
            doc,
            0,
            0,
        );
        if let Some((cell_w, cell_h, px)) = LARGE_CELL_TTF_FONTS
            .iter()
            .find(|(name, ..)| *name == module)
            .map(|(_, w, h, px)| (*w, *h, *px))
        {
            entry.max_cell_w = cell_w;
            entry.max_cell_h = cell_h;
            entry.preferred_px = px;
            // Never fall back below this: the whole point is that the small
            // sizes are the broken ones.
            entry.min_px = px;
        }
        entries.push(entry);
    }
    entries
}

/// Display faces that need a bigger cell than the default to stay readable.
///
/// Most of these TTFs survive a 16-pixel cell. A few tell letters apart with a
/// notch one or two pixels wide at that size, which the rasterizer's coverage
/// threshold then removes, leaving K, X and H as the same bitmap and turning
/// `PRESS X` into `PRESS H`. Giving them room costs VRAM and rules them out
/// for body text, but a large legible font beats a small wrong one.
const LARGE_CELL_TTF_FONTS: &[(&str, usize, usize, f32)] = &[
    ("kenney_future", 32, 28, 24.0),
    ("kenney_future_narrow", 32, 28, 24.0),
    // A variable font whose default instance is thin: at 16 pixels its
    // horizontal bars fall under the coverage threshold and vanish, leaving E
    // and L, F and I as bare stems.
    ("jura", 32, 28, 26.0),
];

#[allow(clippy::too_many_arguments)]
fn font_entry(
    source: &'static str,
    format: FontFormat,
    module: &'static str,
    prefix: &'static str,
    license: &'static str,
    first_cp: u32,
    count: usize,
    doc: &'static str,
    width: usize,
    height: usize,
) -> FontEntry {
    FontEntry {
        source,
        format,
        module,
        prefix,
        license,
        first_cp,
        count,
        doc,
        width,
        height,
        advance: width,
        line_height: height,
        max_cell_w: 32,
        max_cell_h: 16,
        preferred_px: 16.0,
        min_px: 8.0,
        advances: None,
    }
}

fn parse_c_header_8x8(path: &Path, expected: usize) -> Result<Vec<Vec<u8>>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut glyphs = Vec::new();
    for segment in text.split('{') {
        let Some((body, _)) = segment.split_once('}') else {
            continue;
        };
        let values: Vec<u8> = body
            .split(',')
            .filter_map(|part| {
                let part = part.trim();
                part.strip_prefix("0x")
                    .or_else(|| part.strip_prefix("0X"))
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            })
            .collect();
        if values.len() == 8 {
            glyphs.push(values);
        }
    }
    if glyphs.len() != expected {
        return Err(format!(
            "{}: parsed {} glyphs, expected {expected}",
            path.display(),
            glyphs.len()
        ));
    }
    Ok(glyphs)
}

fn parse_ibm_bin(path: &Path, height: usize, expected: usize) -> Result<Vec<Vec<u8>>, String> {
    let data = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if data.len() / height < expected {
        return Err(format!("{}: not enough glyphs", path.display()));
    }
    Ok((0..expected)
        .map(|idx| data[idx * height..idx * height + height].to_vec())
        .collect())
}

fn parse_ttf_font(path: &Path, entry: &mut FontEntry) -> Result<Vec<Vec<u8>>, String> {
    let data = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let font = fontdue::Font::from_bytes(data, fontdue::FontSettings::default())
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut chosen_px = None;
    for px in (entry.min_px as u32..=entry.preferred_px as u32).rev() {
        let mut cell_w = 0usize;
        let mut cell_h = 0usize;
        for cp in entry.first_cp..entry.first_cp + entry.count as u32 {
            if cp == 0x20 {
                continue;
            }
            let ch = char::from_u32(cp).unwrap_or(' ');
            let (metrics, _) = font.rasterize(ch, px as f32);
            cell_w = cell_w.max(metrics.width + 2);
            cell_h = cell_h.max(metrics.height + 2);
        }
        if cell_w <= entry.max_cell_w && cell_h <= entry.max_cell_h {
            chosen_px = Some((px as f32, cell_w.max(1), cell_h.max(1)));
            break;
        }
    }
    let (px, cell_w, cell_h) = chosen_px.ok_or_else(|| {
        format!(
            "{}: could not fit glyphs into {}x{} cells",
            path.display(),
            entry.max_cell_w,
            entry.max_cell_h
        )
    })?;
    entry.width = cell_w;
    entry.height = cell_h;
    entry.advance = cell_w;
    entry.line_height = cell_h;
    let row_bytes = cell_w.div_ceil(8);
    let mut glyphs = Vec::new();
    let mut advances = Vec::new();
    for cp in entry.first_cp..entry.first_cp + entry.count as u32 {
        let ch = char::from_u32(cp).unwrap_or(' ');
        let (metrics, bitmap) = font.rasterize(ch, px);
        let mut rows = vec![0u8; row_bytes * cell_h];
        let offset_x = 1usize;
        let offset_y = 1usize;
        for y in 0..metrics.height.min(cell_h.saturating_sub(offset_y)) {
            for x in 0..metrics.width.min(cell_w.saturating_sub(offset_x)) {
                if bitmap[y * metrics.width + x] >= 128 {
                    let dx = offset_x + x;
                    rows[(offset_y + y) * row_bytes + dx / 8] |= 1 << (dx % 8);
                }
            }
        }
        advances.push(metrics.advance_width.ceil().clamp(1.0, 255.0) as u8);
        glyphs.push(rows);
    }
    entry.advances = Some(advances);
    Ok(glyphs)
}


/// Letters of the Latin alphabet that came out as the same bitmap.
///
/// A font is not merely ugly when this happens, it is wrong: `PRESS X` renders
/// as `PRESS H` and no amount of squinting recovers it. Worth checking at
/// generation time because the failure is invisible in the glyph table and only
/// shows up as misspelled words on screen, a long way from here.
///
/// Only A-Z: punctuation and box-drawing glyphs legitimately repeat, and a
/// font of pure block shapes has every reason to.
fn colliding_letters(entry: &FontEntry, glyphs: &[Vec<u8>]) -> Vec<Vec<char>> {
    use std::collections::BTreeMap;
    let mut by_bitmap: BTreeMap<&[u8], Vec<char>> = BTreeMap::new();
    for letter in 'A'..='Z' {
        let Some(index) = (letter as u32).checked_sub(entry.first_cp) else {
            continue;
        };
        let Some(glyph) = glyphs.get(index as usize) else {
            continue;
        };
        if glyph.iter().all(|byte| *byte == 0) {
            continue; // a font without letters at all is a different problem
        }
        by_bitmap.entry(glyph.as_slice()).or_default().push(letter);
    }
    by_bitmap
        .into_values()
        .filter(|group| group.len() > 1)
        .collect()
}

fn emit_font_module(entry: &FontEntry, glyphs: &[Vec<u8>]) -> String {
    let row_bytes = entry.width.div_ceil(8);
    let total_bytes = entry.count * row_bytes * entry.height;
    let bit_order = match entry.format {
        FontFormat::IbmBin => "Msb",
        _ => "Lsb",
    };
    let mut out = format!(
        "// This file is GENERATED by `psoxide-dev gen-fonts` from\n// `vendor/{}`. Do NOT edit by hand - re-run the\n// generator if the upstream font source changes.\n//\n// License: {} (see `vendor/PROVENANCE.md`).\n\nuse crate::{{BitOrder, BitmapFont}};\n\n",
        entry.source, entry.license
    );
    out.push_str(&format!(
        "/// {}\n///\n/// {}x{} pixels per glyph, 1 bit per pixel, {} = leftmost.\npub const {}_BITMAP: [u8; {}] = [\n",
        entry.doc,
        entry.width,
        entry.height,
        if bit_order == "Lsb" { "LSB" } else { "MSB" },
        entry.prefix,
        total_bytes
    ));
    for (idx, rows) in glyphs.iter().enumerate() {
        let cp = entry.first_cp + idx as u32;
        out.push_str("    ");
        out.push_str(
            &rows
                .iter()
                .map(|b| format!("0x{b:02x}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        out.push_str(&format!(", // U+{cp:04X}\n"));
    }
    out.push_str("];\n\n");
    let advances_name = if let Some(advances) = entry.advances.as_ref() {
        out.push_str(&format!("/// Per-glyph pixel advances for the `{}` bitmap above.\npub const {}_ADVANCES: [u8; {}] = [\n", entry.module, entry.prefix, advances.len()));
        for chunk in advances.chunks(16) {
            out.push_str("    ");
            out.push_str(
                &chunk
                    .iter()
                    .map(u8::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            out.push_str(",\n");
        }
        out.push_str("];\n\n");
        format!("Some(&{}_ADVANCES)", entry.prefix)
    } else {
        "None".to_string()
    };
    out.push_str(&format!(
        "/// [`BitmapFont`] descriptor for the `{}` bitmap above.\n/// Pass into [`crate::FontAtlas::upload`] to install it in VRAM.\npub const {}: BitmapFont = BitmapFont {{\n    glyph_w: {},\n    glyph_h: {},\n    first_char: 0x{:04x},\n    glyph_count: {},\n    bitmap: &{}_BITMAP,\n    glyph_advances: {},\n    advance_x: {},\n    line_height: {},\n    bit_order: BitOrder::{},\n}};\n",
        entry.module,
        entry.prefix,
        entry.width,
        entry.height,
        entry.first_cp,
        entry.count,
        entry.prefix,
        advances_name,
        entry.advance,
        entry.line_height,
        bit_order
    ));
    out
}
