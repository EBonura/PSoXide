//! Content-verified local replay store. No build, download, or game mutation.
//!
//! A case names hashed inputs, a fixed emulator command line and a completion
//! contract. `run` rehashes the current inputs, takes a per-key lock, replays
//! only on a miss, and seals the result with a receipt that `get` and `compare`
//! re-verify byte by byte. See `benchmarks/performance-suite/README.md`.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use regex::Regex;
use sha2::{Digest, Sha256};

use crate::cortex_bench_report::{parse_launch_summary, CYCLE_COLUMNS};
use crate::pyjson::Json;
use crate::util::{dict_rows, parse_int, read_text, usage_error, Cli, Token};

/// Receipt schema version.
pub const SCHEMA: i64 = 1;

/// The bytes that stand in for the tool's identity in a cache key: this very
/// source file and the launch-summary parser it shares with the Cortex report.
const TOOL_SOURCE: &[u8] = include_bytes!("performance_suite.rs");
const PARSER_SOURCE: &[u8] = include_bytes!("cortex_bench_report.rs");

/// Failure of a store operation. `Value` mirrors the Python tool's
/// `ValueError`, `Io` its `OSError` family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuiteError {
    /// A contract violation: bad case, tampered result, failed completion.
    Value(String),
    /// A file-system or process error.
    Io(String),
}

impl fmt::Display for SuiteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuiteError::Value(message) | SuiteError::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SuiteError {}

impl From<std::io::Error> for SuiteError {
    fn from(error: std::io::Error) -> Self {
        SuiteError::Io(error.to_string())
    }
}

impl From<crate::util::Error> for SuiteError {
    fn from(error: crate::util::Error) -> Self {
        SuiteError::Value(error.0)
    }
}

/// Result alias for this module.
pub type Result<T> = std::result::Result<T, SuiteError>;

fn value_error<T>(message: impl Into<String>) -> Result<T> {
    Err(SuiteError::Value(message.into()))
}

/// SHA-256 of a file's bytes, lowercase hex.
pub fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut block = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut block)?;
        if read == 0 {
            break;
        }
        hasher.update(&block[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `json.dumps(indent=2, sort_keys=True) + "\n"` written to `path`.
pub fn write_json(path: &Path, value: &Json) -> Result<()> {
    let mut text = value.dumps(Some(2), true);
    text.push('\n');
    fs::write(path, text)?;
    Ok(())
}

fn read_json(path: &Path) -> Result<Json> {
    Ok(Json::parse(&fs::read_to_string(path)?)?)
}

fn key_of(identity: &Json) -> String {
    digest_bytes(identity.compact().as_bytes())
}

fn walk(root: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(root)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    entries.sort();
    for path in entries {
        found.push(path.clone());
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            walk(&path, found)?;
        }
    }
    Ok(())
}

/// Hash every regular file under `root`: `{relative path: {sha256, bytes}}`.
pub fn files(root: &Path) -> Result<Json> {
    let mut entries = Vec::new();
    walk(root, &mut entries)?;
    if entries
        .iter()
        .any(|p| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
    {
        return value_error("artifact symlinks are not supported");
    }
    let mut listing: BTreeMap<String, Json> = BTreeMap::new();
    for path in entries {
        if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|e| SuiteError::Io(e.to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            listing.insert(
                relative,
                Json::object([
                    ("sha256", Json::Str(digest(&path)?)),
                    ("bytes", Json::from(fs::metadata(&path)?.len())),
                ]),
            );
        }
    }
    Ok(Json::Object(listing.into_iter().collect()))
}

/// POSIX `shlex.split`.
pub fn shlex_split(line: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return value_error("No closing quotation"),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('"' | '\\')) => word.push(escaped),
                            Some(other) => {
                                word.push('\\');
                                word.push(other);
                            }
                            None => return value_error("No escaped character"),
                        },
                        Some(c) => word.push(c),
                        None => return value_error("No closing quotation"),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(escaped) => word.push(escaped),
                    None => return value_error("No escaped character"),
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// Hash the complete disc dependency closure, including multi-file CUEs.
pub fn cue_files(path: &Path) -> Result<Vec<PathBuf>> {
    let is_cue = path
        .extension()
        .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("cue"));
    if !is_cue {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for line in crate::util::splitlines(&read_text(path)?) {
        let parts = shlex_split(line)?;
        if parts.first().is_some_and(|p| p.to_uppercase() == "FILE") {
            if parts.len() != 3 {
                return value_error("malformed CUE FILE line");
            }
            let parent = path.parent().unwrap_or(Path::new("."));
            result.push(fs::canonicalize(parent.join(&parts[1]))?);
        }
    }
    if result.is_empty() {
        return value_error("CUE has no FILE inputs");
    }
    Ok(result)
}

fn expanduser(text: &str) -> PathBuf {
    if text == "~" || text.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(text.trim_start_matches('~').trim_start_matches('/'));
        }
    }
    PathBuf::from(text)
}

type Inputs = Vec<(String, String)>;

fn input_path<'a>(inputs: &'a Inputs, name: &str) -> Option<&'a str> {
    inputs
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, p)| p.as_str())
}

/// Resolve each `$binding` input of a case to an absolute existing path.
pub fn materialize(case: &Json, bindings: &Json) -> Result<Inputs> {
    let mut inputs = Vec::new();
    let declared = case
        .get("inputs")
        .and_then(Json::as_object)
        .ok_or_else(|| SuiteError::Value("KeyError: 'inputs'".to_string()))?;
    for (name, value) in declared {
        let Some(binding) = value.as_str().and_then(|v| v.strip_prefix('$')) else {
            return value_error("input paths must use external $bindings");
        };
        let bound = bindings
            .get(binding)
            .and_then(Json::as_str)
            .ok_or_else(|| SuiteError::Value(format!("KeyError: '{binding}'")))?;
        inputs.push((
            name.clone(),
            fs::canonicalize(expanduser(bound))?
                .to_string_lossy()
                .into_owned(),
        ));
    }
    Ok(inputs)
}

// Deliberately bounded to the documented suite lanes. Add new flags here with
// their input/output semantics instead of permitting invisible file inputs.
const INPUT_FLAGS: [&str; 4] = ["--path", "--disc", "--input-tape", "--load-state"];
const OUTPUT_FLAGS: [&str; 18] = [
    "--config-dir",
    "--route-log",
    "--cpu-cycle-profile-log",
    "--gpu-frame-stats-log",
    "--cd-command-log",
    "--profile-log",
    "--counter-log",
    "--route-screenshot-dir",
    "--dump-display",
    "--dump-ram",
    "--dump-spu-ram",
    "--dump-audio",
    "--dump-vram",
    "--pc-line-log",
    "--pc-sample-log",
    "--icache-event-log",
    "--instruction-class-log",
    "--stack-profile-log",
];
const SCALAR_FLAGS: [&str; 9] = [
    "--steps",
    "--stop-at-poll",
    "--guest-frames",
    "--route-screenshot-interval",
    "--pc-sample-instructions",
    "--pc-line-start-route-tick",
    "--icache-event-start-route-tick",
    "--stack-profile-root-pc",
    "--press",
];
const BOOL_FLAGS: [&str; 5] = [
    "--embedded-playtest",
    "--digital-pad",
    "--dump-hash",
    "--guest-debug-log",
    "--dump-guest-profile",
];

fn string_list(value: &Json) -> Result<Vec<String>> {
    value
        .as_array()
        .ok_or_else(|| SuiteError::Value("expected a list of strings".to_string()))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| SuiteError::Value("expected a list of strings".to_string()))
        })
        .collect()
}

fn has_parent_component(value: &str) -> bool {
    value.split('/').any(|part| part == "..")
}

/// Validate the emulator argument vector against the declared flag semantics.
pub fn check_arguments(argv: &[String], inputs: &Inputs) -> Result<()> {
    if argv.is_empty() || (argv[0] != "launch" && argv[0] != "{input.driver}") {
        return value_error("expected launch or an explicitly hashed test driver");
    }
    let reference = Regex::new(r"^\{input\.([a-zA-Z0-9_]+)\}$").expect("static pattern");
    let mut index = 1;
    while index < argv.len() {
        let flag = argv[index].as_str();
        index += 1;
        if BOOL_FLAGS.contains(&flag) {
            continue;
        }
        if !(INPUT_FLAGS.contains(&flag)
            || OUTPUT_FLAGS.contains(&flag)
            || SCALAR_FLAGS.contains(&flag))
        {
            return value_error(format!(
                "unsupported option (declare its file semantics first): {flag}"
            ));
        }
        if index == argv.len() {
            return value_error(format!("missing value for {flag}"));
        }
        let value = argv[index].as_str();
        index += 1;
        if INPUT_FLAGS.contains(&flag) {
            match reference.captures(value) {
                Some(c) if input_path(inputs, &c[1]).is_some() => {}
                _ => return value_error(format!("{flag} must reference one hashed input")),
            }
        } else if OUTPUT_FLAGS.contains(&flag)
            && (!value.starts_with("{out}/") || has_parent_component(value))
        {
            return value_error(format!(
                "{flag} must remain under the isolated output directory"
            ));
        }
    }
    Ok(())
}

/// Conservative poll completion supports exact binary v2 tapes only.
pub fn poll_tape_end(path: &str) -> Result<i64> {
    let raw = fs::read(path)?;
    if raw.len() < 16 || &raw[..8] != b"PXITAPE2" {
        return value_error(
            "poll completion requires a PXITAPE2 tape; use a semantic adapter otherwise",
        );
    }
    let count = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]) as usize;
    let start = u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]) as i64;
    if raw.len() != 16 + count * 6 {
        return value_error("truncated or malformed poll tape");
    }
    Ok(start + count as i64)
}

fn argv_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    argv.iter()
        .position(|a| a == flag)
        .and_then(|i| argv.get(i + 1))
        .map(String::as_str)
}

fn uname(flag: &str) -> String {
    Command::new("uname")
        .arg(flag)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// A validated, hashed case.
#[derive(Debug)]
pub struct Plan {
    /// The cache key: sha256 of the compact identity record.
    pub key: String,
    /// Everything that determines the result.
    pub identity: Json,
    /// Resolved input paths by name.
    pub inputs: Inputs,
}

/// Validate a case against its bindings and compute its identity and key.
pub fn plan(case: &Json, bindings: &Json) -> Result<Plan> {
    let lane = case.get("lane").and_then(Json::as_str);
    if lane != Some("quick") && lane != Some("acceptance") {
        return value_error("lane must be quick or acceptance");
    }
    let inputs = materialize(case, bindings)?;
    if input_path(&inputs, "emulator").is_none() {
        return value_error("an explicit emulator binary is required");
    }
    let argv = string_list(
        case.get("argv")
            .ok_or_else(|| SuiteError::Value("KeyError: 'argv'".to_string()))?,
    )?;
    check_arguments(&argv, &inputs)?;
    if !case.get("build_provenance").is_some_and(Json::truthy) {
        return value_error(
            "record build directory, flags and source provenance in build_provenance",
        );
    }
    let required = string_list(
        case.get("required_outputs")
            .ok_or_else(|| SuiteError::Value("KeyError: 'required_outputs'".to_string()))?,
    )?;
    for name in &required {
        if name.starts_with('/') || has_parent_component(name) {
            return value_error("required outputs must remain inside the result directory");
        }
    }
    for flag in ["--config-dir", "--steps"] {
        if !argv.iter().any(|a| a == flag) {
            return value_error(format!("missing {flag}"));
        }
    }
    if argv_value(&argv, "--config-dir") != Some("{out}/config") {
        return value_error("config-dir must be fresh {out}/config");
    }
    if !argv.iter().any(|a| a == "--path") {
        return value_error("explicit --path is required; library lookup is not reproducible");
    }
    if argv.iter().any(|a| a == "--input-tape")
        && argv_value(&argv, "--input-tape") != Some("{input.tape}")
    {
        return value_error("tape must be a hashed tape input");
    }
    if !argv_value(&argv, "--path")
        .unwrap_or("")
        .starts_with("{input.")
    {
        return value_error("guest path must reference a hashed input");
    }
    if argv.iter().any(|a| a == "--disc")
        && !argv_value(&argv, "--disc")
            .unwrap_or("")
            .starts_with("{input.")
    {
        return value_error("disc must reference a hashed input");
    }
    let completion = case
        .get("completion")
        .ok_or_else(|| SuiteError::Value("KeyError: 'completion'".to_string()))?;
    if let Some(poll) = completion.get("poll") {
        if argv.iter().any(|a| a == "--guest-frames") {
            return value_error("poll completion forbids competing frame limits");
        }
        let maximum = completion
            .get("max_poll")
            .unwrap_or(poll)
            .as_i64()
            .unwrap_or(0);
        let tape_ok = match input_path(&inputs, "tape") {
            Some(tape) => poll_tape_end(tape)? > maximum,
            None => false,
        };
        if !tape_ok {
            return value_error("tape must extend strictly beyond maximum completion poll");
        }
    }
    let environment = case
        .get("environment")
        .cloned()
        .unwrap_or(Json::Object(Vec::new()));
    if let Some(pairs) = environment.as_object() {
        if pairs.iter().any(|(key, _)| !key.starts_with("PSOXIDE_")) {
            return value_error("only explicit PSOXIDE_* runtime overrides are supported");
        }
    }
    let mut records = Json::Object(Vec::new());
    for (name, value) in &inputs {
        let path = Path::new(value);
        if !path.is_file() {
            return value_error(format!("{name} is not a file"));
        }
        let mut dependencies = Vec::new();
        for dependency in cue_files(path)? {
            dependencies.push(Json::object([
                ("path", Json::Str(dependency.to_string_lossy().into_owned())),
                ("sha256", Json::Str(digest(&dependency)?)),
                ("bytes", Json::from(fs::metadata(&dependency)?.len())),
            ]));
        }
        records.set(
            name.clone(),
            Json::object([
                ("path", Json::Str(value.clone())),
                ("sha256", Json::Str(digest(path)?)),
                ("bytes", Json::from(fs::metadata(path)?.len())),
                ("cue_dependencies", Json::Array(dependencies)),
            ]),
        );
    }
    // Scripts used as executables must declare their interpreter. No opaque shell wrappers.
    let mut magic = [0u8; 2];
    let emulator = input_path(&inputs, "emulator").expect("checked above");
    let read = fs::File::open(emulator)?.read(&mut magic)?;
    if &magic[..read] == b"#!" {
        return value_error("emulator must be a real binary, not an unhashed wrapper");
    }
    let identity = Json::object([
        ("schema", Json::Int(SCHEMA)),
        ("case", case.clone()),
        ("inputs", records),
        ("tool_sha256", Json::Str(digest_bytes(TOOL_SOURCE))),
        ("parser_sha256", Json::Str(digest_bytes(PARSER_SOURCE))),
        ("runtime_environment", environment),
        (
            "config_policy",
            Json::from("fresh empty directory; absent memory cards"),
        ),
        (
            "host_platform",
            Json::Str(format!("{}/{}", uname("-s"), uname("-m"))),
        ),
    ]);
    Ok(Plan {
        key: key_of(&identity),
        identity,
        inputs,
    })
}

/// Substitute `{out}` and `{input.NAME}` placeholders.
pub fn expand(argv: &[String], inputs: &Inputs, out: &Path) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for word in argv {
        let mut word = word.replace("{out}", &out.to_string_lossy());
        for (name, path) in inputs {
            word = word.replace(&format!("{{input.{name}}}"), path);
        }
        if word.contains("{input.") || word.contains("{out}") {
            return value_error("unresolved command placeholder");
        }
        result.push(word);
    }
    Ok(result)
}

/// Parse the launch summary and, when present, the cycle-profile totals.
pub fn parse_summary(out: &Path) -> Result<Json> {
    let text = String::from_utf8_lossy(&fs::read(out.join("stdout.txt"))?).into_owned();
    let mut summary = parse_launch_summary(&text)?;
    let cycle_file = out.join("cycles.csv");
    if cycle_file.is_file() {
        let wanted: Vec<&str> = CYCLE_COLUMNS
            .iter()
            .copied()
            .chain([
                "profiled_cpu_cycles",
                "uncached_fetch_stall_cycles",
                "other_cycles",
            ])
            .collect();
        let (header, rows) = dict_rows(&read_text(&cycle_file)?)?;
        let mut totals = Json::Object(Vec::new());
        for row in &rows {
            for key in &header {
                let value = row.get(key).unwrap_or("");
                if wanted.contains(&key.as_str()) && !value.is_empty() {
                    let current = totals.get(key).and_then(Json::as_i64).unwrap_or(0);
                    totals.set(key.clone(), Json::Int(current + parse_int(value)?));
                }
            }
        }
        summary.set("guest_cycle_columns", totals);
        summary.set(
            "cycle_note",
            Json::from("stack_ram_load_stall_cycles is a subset, never add it twice"),
        );
    }
    Ok(summary)
}

fn run_with_timeout(command: &mut Command, limit: Option<Duration>) -> Result<i32> {
    let mut child = command.spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code().unwrap_or(-1));
        }
        if limit.is_some_and(|limit| started.elapsed() >= limit) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SuiteError::Io(format!(
                "Command timed out after {} seconds",
                limit.map_or(0.0, |l| l.as_secs_f64())
            )));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Decide whether the replay completed, using the case's completion contract.
pub fn completion(case: &Json, inputs: &Inputs, out: &Path, summary: &Json) -> Result<Json> {
    let text = String::from_utf8_lossy(&fs::read(out.join("stdout.txt"))?).into_owned();
    let fault =
        Regex::new(r"\[cli\] (?:step \d+ failed|guest exception at step)").expect("static pattern");
    if fault.is_match(&text) {
        return value_error("guest fault, not successful completion");
    }
    let argv = string_list(
        case.get("argv")
            .ok_or_else(|| SuiteError::Value("KeyError: 'argv'".to_string()))?,
    )?;
    let cap: i64 = argv_value(&argv, "--steps")
        .ok_or_else(|| SuiteError::Value("'--steps' is not in list".to_string()))?
        .parse()
        .map_err(|_| SuiteError::Value("invalid literal for int()".to_string()))?;
    let instructions = summary
        .get("instructions")
        .and_then(Json::as_i64)
        .unwrap_or(0);
    if instructions >= cap {
        return value_error("instruction hard cap reached, not a completed route");
    }
    let expected = case
        .get("completion")
        .ok_or_else(|| SuiteError::Value("KeyError: 'completion'".to_string()))?;
    if let Some(target) = expected.get("poll").and_then(Json::as_i64) {
        match summary.get("stopped_at").and_then(Json::as_i64) {
            Some(stopped) if stopped < cap => {}
            _ => return value_error("missing successful early-stop marker"),
        }
        let stop_at = argv_value(&argv, "--stop-at-poll").and_then(|v| v.parse::<i64>().ok());
        if stop_at != Some(target) {
            return value_error("completion poll must match --stop-at-poll");
        }
        let maximum = expected
            .get("max_poll")
            .and_then(Json::as_i64)
            .unwrap_or(target);
        let observed = summary
            .get("port1_polls")
            .and_then(Json::as_i64)
            .ok_or_else(|| SuiteError::Value("KeyError: 'port1_polls'".to_string()))?;
        if !(target <= observed && observed <= maximum) {
            return value_error(format!(
                "completion poll outside explicit bounds: {observed}"
            ));
        }
        return Ok(Json::object([
            ("complete", Json::Bool(true)),
            ("observed_poll", Json::Int(observed)),
            ("target", Json::Int(target)),
            (
                "note",
                Json::from(
                    "CLI completes after the next display-origin change; bounds are case-specific",
                ),
            ),
        ]));
    }
    let adapter_name = expected
        .get("adapter")
        .and_then(Json::as_str)
        .ok_or_else(|| SuiteError::Value("KeyError: 'adapter'".to_string()))?;
    let adapter = input_path(inputs, adapter_name)
        .ok_or_else(|| SuiteError::Value(format!("KeyError: '{adapter_name}'")))?;
    let resolved = Json::Object(
        inputs
            .iter()
            .map(|(n, p)| (n.clone(), Json::Str(p.clone())))
            .collect(),
    );
    write_json(&out.join("resolved-inputs.json"), &resolved)?;
    let resolved_path = out.join("resolved-inputs.json");
    // A script adapter names its interpreter; a native adapter is run directly.
    let mut command = match expected.get("interpreter").and_then(Json::as_str) {
        Some(name) => {
            let interpreter = input_path(inputs, name)
                .ok_or_else(|| SuiteError::Value(format!("KeyError: '{name}'")))?;
            let mut command = Command::new(interpreter);
            command.arg(adapter);
            command
        }
        None => Command::new(adapter),
    };
    command.arg(out).arg(&resolved_path);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SuiteError::Io(
                "completion adapter timed out after 30 seconds".to_string(),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_string(&mut stdout)?;
    }
    if !status.success() {
        return Err(SuiteError::Io(format!(
            "Command '{}' returned non-zero exit status {}.",
            adapter,
            status.code().unwrap_or(-1)
        )));
    }
    let proof = Json::parse(&stdout)?;
    if !matches!(proof.get("complete"), Some(Json::Bool(true))) {
        return value_error("semantic completion adapter did not confirm completion");
    }
    Ok(proof)
}

/// Rehash every recorded artifact; metadata/mtime alone never proves a hit.
pub fn validate_result(entry: &Path, expected_key: Option<&str>) -> Result<Json> {
    let receipt = read_json(&entry.join("receipt.json"))?;
    if receipt.get("status").and_then(Json::as_str) != Some("complete")
        || receipt.get("schema").and_then(Json::as_i64) != Some(SCHEMA)
    {
        return value_error("partial or unknown result");
    }
    let identity = receipt
        .get("identity")
        .ok_or_else(|| SuiteError::Value("KeyError: 'identity'".to_string()))?;
    let key = key_of(identity);
    let recorded = receipt.get("key").and_then(Json::as_str);
    if recorded != Some(key.as_str()) || expected_key.is_some_and(|expected| key != expected) {
        return value_error("result identity mismatch");
    }
    let complete = receipt
        .get("completion")
        .and_then(|c| c.get("complete"))
        .is_some_and(Json::truthy);
    if !complete || receipt.get("returncode").and_then(Json::as_i64) != Some(0) {
        return value_error("result has no successful completion proof");
    }
    let actual = files(&entry.join("artifacts"))?;
    let recorded_artifacts = receipt
        .get("artifacts")
        .ok_or_else(|| SuiteError::Value("KeyError: 'artifacts'".to_string()))?;
    if !actual.py_eq(recorded_artifacts) {
        return value_error("cached artifact missing, changed, or added");
    }
    for name in ["summary", "completion"] {
        let sealed = read_json(&entry.join("artifacts").join(format!("{name}.json")))?;
        match receipt.get(name) {
            Some(recorded) if recorded.py_eq(&sealed) => {}
            _ => return value_error("receipt disagrees with sealed artifact proof"),
        }
    }
    Ok(receipt)
}

fn with_key_lock<T>(store: &Path, key: &str, body: impl FnOnce() -> Result<T>) -> Result<T> {
    fs::create_dir_all(store.join("locks"))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store.join("locks").join(format!("{key}.lock")))?;
    lock.lock()?;
    let result = body();
    drop(lock);
    result
}

fn seconds_since(start: Instant) -> Json {
    Json::Float(start.elapsed().as_secs_f64())
}

/// Replay one case into the store, or return the verified cached result.
pub fn run_case(case: &Json, bindings: &Json, store: &Path) -> Result<Json> {
    run_case_with(case, bindings, store, &|| {})
}

/// [`run_case`] with a hook that runs right after the per-key lock is taken
/// (tests use it to change an input while a second runner waits).
pub fn run_case_with(
    case: &Json,
    bindings: &Json,
    store: &Path,
    after_lock: &dyn Fn(),
) -> Result<Json> {
    let started = Instant::now();
    let prepared = plan(case, bindings)?;
    let key = prepared.key.clone();
    let entry = store.join("results").join(&key);
    let case_id = case.get("id").cloned().unwrap_or(Json::Null);
    with_key_lock(store, &key, || {
        after_lock();
        if plan(case, bindings)?.key != key {
            return value_error(
                "input changed while waiting for cache lock; retry with stable inputs",
            );
        }
        if entry.exists() {
            let receipt = validate_result(&entry, Some(&key))?;
            return Ok(Json::object([
                ("case", case_id.clone()),
                ("key", Json::Str(key.clone())),
                ("cache", Json::from("hit")),
                ("lookup_seconds", seconds_since(started)),
                ("result", Json::Str(entry.to_string_lossy().into_owned())),
                (
                    "summary",
                    receipt.get("summary").cloned().unwrap_or(Json::Null),
                ),
            ]));
        }
        let parent = entry.parent().expect("results/KEY has a parent");
        fs::create_dir_all(parent)?;
        let temp = tempfile::Builder::new()
            .prefix(&format!("{key}."))
            .rand_bytes(8)
            .tempdir_in(parent)?
            .keep();
        let out = temp.join("artifacts");
        fs::create_dir(&out)?;
        fs::create_dir(out.join("config"))?;
        let argv = string_list(case.get("argv").expect("validated by plan"))?;
        let emulator = input_path(&prepared.inputs, "emulator").expect("validated by plan");
        let mut command = vec![emulator.to_string()];
        command.extend(expand(&argv, &prepared.inputs, &out)?);
        let env = replay_environment(case, &out);
        fs::create_dir(out.join("home"))?;
        fs::create_dir(out.join("tmp"))?;
        write_json(
            &out.join("command.json"),
            &Json::object([
                (
                    "argv",
                    Json::Array(command.iter().cloned().map(Json::Str).collect()),
                ),
                (
                    "environment",
                    Json::Object(
                        env.iter()
                            .map(|(n, v)| (n.clone(), Json::Str(v.clone())))
                            .collect(),
                    ),
                ),
            ]),
        )?;
        let attempt = replay(
            case, &prepared, &key, &temp, &out, &entry, bindings, &command, &env,
        );
        match attempt {
            Ok(summary) => Ok(Json::object([
                ("case", case_id.clone()),
                ("key", Json::Str(key.clone())),
                ("cache", Json::from("miss")),
                ("lookup_seconds", seconds_since(started)),
                ("result", Json::Str(entry.to_string_lossy().into_owned())),
                ("summary", summary),
            ])),
            Err(error) => {
                write_json(
                    &temp.join("failure.json"),
                    &Json::object([
                        ("status", Json::from("failed")),
                        ("error", Json::Str(error.to_string())),
                        ("key", Json::Str(key.clone())),
                    ]),
                )?;
                let failed = store.join("failures");
                match fs::create_dir(&failed) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e.into()),
                }
                fs::rename(
                    &temp,
                    failed.join(temp.file_name().expect("temp has a name")),
                )?;
                Err(error)
            }
        }
    })
}

/// The scrubbed environment of a replay: no inherited `PSOXIDE_*` options, a
/// private HOME and TMPDIR, plus the case's declared overrides.
fn replay_environment(case: &Json, out: &Path) -> Vec<(String, String)> {
    // Avoid hidden HOME settings, inherited PSOXIDE_* experiments, and inherited memory cards.
    let mut env: Vec<(String, String)> = vec![
        ("PATH".to_string(), "/bin:/usr/bin".to_string()),
        (
            "HOME".to_string(),
            out.join("home").to_string_lossy().into_owned(),
        ),
        (
            "TMPDIR".to_string(),
            out.join("tmp").to_string_lossy().into_owned(),
        ),
        ("LANG".to_string(), "C".to_string()),
    ];
    if let Some(pairs) = case.get("environment").and_then(Json::as_object) {
        for (name, value) in pairs {
            let text = value.as_str().unwrap_or("").to_string();
            match env.iter_mut().find(|(n, _)| n == name) {
                Some(slot) => slot.1 = text,
                None => env.push((name.clone(), text)),
            }
        }
    }
    env
}

fn replay(
    case: &Json,
    prepared: &Plan,
    key: &str,
    temp: &Path,
    out: &Path,
    entry: &Path,
    bindings: &Json,
    command: &[String],
    env: &[(String, String)],
) -> Result<Json> {
    let wall = Instant::now();
    let log = fs::File::create(out.join("stdout.txt"))?;
    let mut process = Command::new(&command[0]);
    process
        .args(&command[1..])
        .current_dir(out)
        .env_clear()
        .envs(env.iter().map(|(n, v)| (n.as_str(), v.as_str())))
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    let limit = case
        .get("timeout_seconds")
        .map(|v| match v {
            Json::Int(n) => *n as f64,
            Json::Float(f) => *f,
            _ => 900.0,
        })
        .unwrap_or(900.0);
    let returncode = run_with_timeout(&mut process, Some(Duration::from_secs_f64(limit)))?;
    let elapsed = wall.elapsed().as_secs_f64();
    if returncode != 0 {
        return value_error(format!("emulator exited {returncode}"));
    }
    let required = string_list(case.get("required_outputs").expect("validated by plan"))?;
    for name in &required {
        let path = out.join(name);
        if !path.is_file() || fs::metadata(&path)?.len() == 0 {
            return value_error(format!("missing required artifact: {name}"));
        }
    }
    let summary = parse_summary(out)?;
    let proof = completion(case, &prepared.inputs, out, &summary)?;
    // Catch inputs changed during execution rather than caching a mixed run.
    if plan(case, bindings)?.key != key {
        return value_error("input changed during replay");
    }
    write_json(&out.join("summary.json"), &summary)?;
    write_json(&out.join("completion.json"), &proof)?;
    let receipt = Json::object([
        ("schema", Json::Int(SCHEMA)),
        ("status", Json::from("complete")),
        ("key", Json::from(key)),
        ("identity", prepared.identity.clone()),
        ("returncode", Json::Int(i64::from(returncode))),
        ("completion", proof),
        ("summary", summary.clone()),
        ("host_wall_seconds", Json::Float(elapsed)),
        ("artifacts", files(out)?),
    ]);
    write_json(&temp.join("receipt.json"), &receipt)?;
    fs::rename(temp, entry)?;
    validate_result(entry, Some(key))?;
    Ok(summary)
}

/// `Path.match`: match `pattern` from the right against the components of `name`.
fn path_match(name: &str, pattern: &str) -> bool {
    let parts: Vec<&str> = name.split('/').filter(|p| !p.is_empty()).collect();
    let pats: Vec<&str> = pattern.split('/').filter(|p| !p.is_empty()).collect();
    if pats.is_empty() || pats.len() > parts.len() {
        return false;
    }
    parts[parts.len() - pats.len()..]
        .iter()
        .zip(&pats)
        .all(|(part, pat)| fnmatch(part, pat))
}

/// `fnmatch.fnmatchcase` for `*`, `?` and `[...]`.
pub fn fnmatch(name: &str, pattern: &str) -> bool {
    fn go(name: &[char], pattern: &[char]) -> bool {
        let Some((&p, rest)) = pattern.split_first() else {
            return name.is_empty();
        };
        match p {
            '*' => (0..=name.len()).any(|skip| go(&name[skip..], rest)),
            '?' => !name.is_empty() && go(&name[1..], rest),
            '[' => {
                let Some(close) = rest.iter().skip(1).position(|&c| c == ']').map(|i| i + 1) else {
                    return !name.is_empty() && name[0] == '[' && go(&name[1..], rest);
                };
                let mut set = &rest[..close];
                let negate = set.first().is_some_and(|&c| c == '!');
                if negate {
                    set = &set[1..];
                }
                let Some(&first) = name.first() else {
                    return false;
                };
                let mut matched = false;
                let mut i = 0;
                while i < set.len() {
                    if i + 2 < set.len() && set[i + 1] == '-' {
                        matched |= set[i] <= first && first <= set[i + 2];
                        i += 3;
                    } else {
                        matched |= set[i] == first;
                        i += 1;
                    }
                }
                matched != negate && go(&name[1..], &rest[close + 1..])
            }
            c => name.first() == Some(&c) && go(&name[1..], rest),
        }
    }
    let name: Vec<char> = name.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    go(&name, &pattern)
}

fn artifact_set(receipt: &Json, patterns: &[String]) -> Json {
    let pairs = receipt
        .get("artifacts")
        .and_then(Json::as_object)
        .unwrap_or_default();
    Json::Object(
        pairs
            .iter()
            .filter(|(name, _)| patterns.iter().any(|p| path_match(name, p)))
            .cloned()
            .collect(),
    )
}

fn candidate_inputs(case: &Json) -> Vec<String> {
    case.get("candidate_inputs")
        .and_then(Json::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Compare two sealed results as a baseline and a candidate.
pub fn compare(left: &Path, right: &Path) -> Result<Json> {
    let a = validate_result(left, None)?;
    let b = validate_result(right, None)?;
    let ia = a.get("identity").expect("validated");
    let ib = b.get("identity").expect("validated");
    let case_a = ia.get("case").expect("identity has a case");
    let case_b = ib.get("case").expect("identity has a case");
    let allowed = candidate_inputs(case_a);
    let mut allowed_sorted = allowed.clone();
    allowed_sorted.sort();
    let mut other = candidate_inputs(case_b);
    other.sort();
    allowed_sorted.dedup();
    other.dedup();
    if allowed_sorted != other {
        return value_error("A/B allowed candidate inputs differ");
    }
    if allowed
        .iter()
        .any(|n| ["emulator", "tape", "python", "driver"].contains(&n.as_str()))
    {
        return value_error("runtime/control inputs cannot vary across A/B");
    }
    for case in [case_a, case_b] {
        let spec = case.get("completion");
        let names = ["adapter", "interpreter"]
            .iter()
            .filter_map(|k| spec.and_then(|s| s.get(k)).and_then(Json::as_str));
        for name in names {
            if allowed.iter().any(|n| n == name) {
                return value_error("completion implementation cannot vary across A/B");
            }
        }
    }
    let inputs_a = ia
        .get("inputs")
        .and_then(Json::as_object)
        .unwrap_or_default();
    let inputs_b = ib
        .get("inputs")
        .and_then(Json::as_object)
        .unwrap_or_default();
    let mut names: Vec<&String> = inputs_a
        .iter()
        .chain(inputs_b.iter())
        .map(|(n, _)| n)
        .collect();
    names.sort();
    names.dedup();
    for name in names {
        if allowed.iter().any(|n| n == name) {
            continue;
        }
        let find = |pairs: &[(String, Json)]| {
            pairs
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        };
        let (x, y) = (find(inputs_a), find(inputs_b));
        let equal = match (&x, &y) {
            (Some(x), Some(y)) => x.py_eq(y),
            (None, None) => true,
            _ => false,
        };
        if !equal {
            return value_error(format!("A/B non-candidate input {name} differs"));
        }
    }
    for field in [
        "tool_sha256",
        "parser_sha256",
        "runtime_environment",
        "config_policy",
    ] {
        let equal = match (ia.get(field), ib.get(field)) {
            (Some(x), Some(y)) => x.py_eq(y),
            (None, None) => true,
            _ => false,
        };
        if !equal {
            return value_error(format!("A/B {field} differs"));
        }
    }
    for field in ["argv", "completion", "lane", "quality"] {
        let equal = match (case_a.get(field), case_b.get(field)) {
            (Some(x), Some(y)) => x.py_eq(y),
            (None, None) => true,
            _ => false,
        };
        if !equal {
            return value_error(format!("A/B {field} differs"));
        }
    }
    let quick = case_a.get("lane").and_then(Json::as_str) == Some("quick");
    let mut gates = Json::Object(Vec::new());
    for dimension in ["visual", "gameplay", "audio"] {
        let spec = case_a.get("quality").and_then(|q| q.get(dimension));
        let Some(spec) = spec.filter(|s| s.truthy()).filter(|_| !quick) else {
            gates.set(
                dimension,
                Json::object([
                    ("status", Json::from("unavailable")),
                    (
                        "reason",
                        Json::from(if quick {
                            "quick timing lane"
                        } else {
                            "no adapter evidence declared"
                        }),
                    ),
                ]),
            );
            continue;
        };
        let patterns = string_list(
            spec.get("patterns")
                .ok_or_else(|| SuiteError::Value("KeyError: 'patterns'".to_string()))?,
        )?;
        let baseline = artifact_set(&a, &patterns);
        let candidate = artifact_set(&b, &patterns);
        let floor = if dimension == "visual" { 2 } else { 1 };
        let minimum = match spec.get("minimum_count") {
            None => floor,
            Some(Json::Int(n)) => *n,
            Some(_) => -1,
        };
        if minimum < floor {
            return value_error("quality minimum_count is below its evidence floor");
        }
        let count = |set: &Json| set.as_object().map_or(0, <[_]>::len) as i64;
        if count(&baseline).min(count(&candidate)) < minimum {
            gates.set(
                dimension,
                Json::object([
                    ("status", Json::from("unavailable")),
                    ("reason", Json::from("insufficient recorded checkpoints")),
                ]),
            );
        } else {
            gates.set(
                dimension,
                Json::object([
                    (
                        "status",
                        Json::from(if baseline.py_eq(&candidate) {
                            "pass"
                        } else {
                            "different"
                        }),
                    ),
                    ("method", Json::from("byte-exact declared artifacts")),
                    (
                        "scope",
                        spec.get("scope")
                            .cloned()
                            .ok_or_else(|| SuiteError::Value("KeyError: 'scope'".to_string()))?,
                    ),
                    ("baseline_count", Json::Int(count(&baseline))),
                    ("candidate_count", Json::Int(count(&candidate))),
                ]),
            );
        }
    }
    let all_pass = gates
        .as_object()
        .unwrap_or_default()
        .iter()
        .all(|(_, g)| g.get("status").and_then(Json::as_str) == Some("pass"));
    Ok(Json::object([
        ("baseline", a.get("key").cloned().unwrap_or(Json::Null)),
        ("candidate", b.get("key").cloned().unwrap_or(Json::Null)),
        ("quality", gates),
        (
            "guest",
            Json::object([
                ("baseline", a.get("summary").cloned().unwrap_or(Json::Null)),
                ("candidate", b.get("summary").cloned().unwrap_or(Json::Null)),
            ]),
        ),
        (
            "host_wall_seconds",
            Json::object([
                (
                    "baseline",
                    a.get("host_wall_seconds").cloned().unwrap_or(Json::Null),
                ),
                (
                    "candidate",
                    b.get("host_wall_seconds").cloned().unwrap_or(Json::Null),
                ),
                (
                    "note",
                    Json::from("host elapsed time is not guest performance"),
                ),
            ]),
        ),
        (
            "acceptance",
            Json::from(if all_pass { "pass" } else { "not established" }),
        ),
        (
            "noise",
            Json::from(
                "No universal threshold; compare a separately recorded behavior-neutral control.",
            ),
        ),
    ]))
}

const USAGE: &str = "psoxide-perf performance-suite run MANIFEST --bindings FILE --store DIR [--jobs N] [--case ID]... [--report FILE]\n       psoxide-perf performance-suite get RESULT_DIR\n       psoxide-perf performance-suite compare BASELINE CANDIDATE";

enum CommandError {
    Usage(String),
    Failed(String),
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        CommandError::Usage(message)
    }
}

fn run_command(args: &[String]) -> std::result::Result<Json, CommandError> {
    let (mut manifest, mut bindings, mut store, mut jobs) = (None, None, None, 1usize);
    let (mut selected, mut report): (Vec<String>, Option<PathBuf>) = (Vec::new(), None);
    let mut cli = Cli::new(args);
    while let Some(token) = cli.next_token() {
        match token {
            Token::Flag(flag) => match flag.as_str() {
                "--bindings" => bindings = Some(PathBuf::from(cli.value(&flag)?)),
                "--store" => store = Some(PathBuf::from(cli.value(&flag)?)),
                "--jobs" => jobs = cli.int(&flag)?,
                "--case" => selected.push(cli.value(&flag)?),
                "--report" => report = Some(PathBuf::from(cli.value(&flag)?)),
                other => return Err(format!("unrecognized arguments: {other}").into()),
            },
            Token::Positional(text) if manifest.is_none() => manifest = Some(PathBuf::from(text)),
            Token::Positional(text) => return Err(format!("unrecognized arguments: {text}").into()),
        }
    }
    let (Some(manifest), Some(bindings), Some(store)) = (manifest, bindings, store) else {
        return Err(
            "the following arguments are required: manifest, --bindings, --store"
                .to_string()
                .into(),
        );
    };
    let load = |path: &Path| -> std::result::Result<Json, CommandError> {
        let text = fs::read_to_string(path)
            .map_err(|e| CommandError::Failed(format!("{}: {e}", path.display())))?;
        Json::parse(&text).map_err(|e| CommandError::Failed(e.to_string()))
    };
    let manifest = load(&manifest)?;
    let bindings = load(&bindings)?;
    if manifest.get("schema").and_then(Json::as_i64) != Some(SCHEMA) || !(1..=8).contains(&jobs) {
        return Err("schema must be 1; jobs must be between 1 and 8"
            .to_string()
            .into());
    }
    let all_cases = manifest
        .get("cases")
        .and_then(Json::as_array)
        .ok_or_else(|| CommandError::Failed("KeyError: 'cases'".to_string()))?;
    let cases: Vec<&Json> = all_cases
        .iter()
        .filter(|c| {
            selected.is_empty()
                || c.get("id")
                    .and_then(Json::as_str)
                    .is_some_and(|id| selected.contains(&id.to_string()))
        })
        .collect();
    let mut chosen: Vec<&str> = cases
        .iter()
        .filter_map(|c| c.get("id").and_then(Json::as_str))
        .collect();
    chosen.sort();
    chosen.dedup();
    let mut wanted: Vec<&str> = selected.iter().map(String::as_str).collect();
    wanted.sort();
    wanted.dedup();
    if cases.is_empty() || (!selected.is_empty() && wanted != chosen) {
        return Err("unknown or empty case selection".to_string().into());
    }
    let store = fs::canonicalize(&store)
        .or_else(|_| std::path::absolute(&store))
        .unwrap_or(store);
    let queue = std::sync::Mutex::new(0usize);
    let results: Vec<std::sync::Mutex<Option<Result<Json>>>> =
        cases.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| loop {
                let index = {
                    let mut next = queue.lock().expect("queue lock");
                    let index = *next;
                    *next += 1;
                    index
                };
                let Some(case) = cases.get(index) else { break };
                let outcome = run_case(case, &bindings, &store);
                *results[index].lock().expect("result lock") = Some(outcome);
            });
        }
    });
    let mut collected = Vec::new();
    for slot in results {
        match slot
            .into_inner()
            .expect("result lock")
            .expect("every case ran")
        {
            Ok(value) => collected.push(value),
            Err(error) => return Err(CommandError::Failed(error.to_string())),
        }
    }
    let result = Json::Array(collected);
    if let Some(report) = report {
        write_json(&report, &result).map_err(|e| CommandError::Failed(e.to_string()))?;
    }
    Ok(result)
}

/// `psoxide-perf performance-suite run|get|compare ...`.
pub fn run(args: &[String], out: &mut dyn Write) -> crate::util::Result<i32> {
    let Some(action) = args.first() else {
        return Ok(usage_error(
            USAGE,
            "the following arguments are required: action",
        ));
    };
    let rest = &args[1..];
    let result = match action.as_str() {
        "run" => match run_command(rest) {
            Ok(result) => result,
            Err(CommandError::Usage(message)) => return Ok(usage_error(USAGE, &message)),
            Err(CommandError::Failed(message)) => return Err(crate::util::Error(message)),
        },
        "get" => {
            let [result_dir] = rest else {
                return Ok(usage_error(USAGE, "get takes exactly one result directory"));
            };
            let started = Instant::now();
            let receipt = validate_result(Path::new(result_dir), None)
                .map_err(|e| crate::util::Error(e.to_string()))?;
            Json::object([
                ("key", receipt.get("key").cloned().unwrap_or(Json::Null)),
                (
                    "summary",
                    receipt.get("summary").cloned().unwrap_or(Json::Null),
                ),
                (
                    "artifacts",
                    receipt.get("artifacts").cloned().unwrap_or(Json::Null),
                ),
                ("verification_seconds", seconds_since(started)),
                (
                    "input_verification",
                    Json::from("historical identity only; use run to hash current inputs"),
                ),
                (
                    "artifact_root",
                    Json::Str(
                        Path::new(result_dir)
                            .join("artifacts")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                ),
            ])
        }
        "compare" => {
            let [baseline, candidate] = rest else {
                return Ok(usage_error(
                    USAGE,
                    "compare takes a baseline and a candidate directory",
                ));
            };
            compare(Path::new(baseline), Path::new(candidate))
                .map_err(|e| crate::util::Error(e.to_string()))?
        }
        other => return Ok(usage_error(USAGE, &format!("invalid choice: '{other}'"))),
    };
    writeln!(out, "{}", result.dumps(Some(2), false))?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shlex_splits_like_posix_shells() {
        assert_eq!(
            shlex_split("FILE \"my disc.bin\" BINARY").unwrap(),
            ["FILE", "my disc.bin", "BINARY"]
        );
        assert_eq!(
            shlex_split(r"a\ b 'c d' e\\f").unwrap(),
            ["a b", "c d", r"e\f"]
        );
        assert!(shlex_split("FILE \"open").is_err());
    }

    #[test]
    fn path_matching_is_anchored_on_the_right() {
        assert!(path_match("frames/a.ppm", "frames/*.ppm"));
        assert!(path_match("x/frames/a.ppm", "frames/*.ppm"));
        assert!(path_match("final.ppm", "final.ppm"));
        assert!(!path_match("frames/a.png", "frames/*.ppm"));
        assert!(!path_match("a.ppm", "frames/*.ppm"));
        assert!(fnmatch("run-3.txt", "run-[0-9].t?t"));
        assert!(!fnmatch("run-x.txt", "run-[0-9].txt"));
    }
}
