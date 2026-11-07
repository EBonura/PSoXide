//! A minimal stdio MCP client for `psxed-mcp`: run a batch of tool calls in ONE
//! server process (one process = one staged edit batch), print text results, save
//! image results.
//!
//! ```text
//! psoxide-perf mcp-client --project DIR --tools                 # list tools + input schemas
//! psoxide-perf mcp-client --project DIR calls.json [--out DIR]  # calls.json: [{"tool": name, "args": {...}}, ...]
//! ```
//!
//! The transport is newline-delimited JSON-RPC 2.0 over the child's stdin and
//! stdout, written by hand: the same bytes the Python client it replaced sent.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use regex::Regex;

use crate::pyjson::Json;
use crate::util::{join_display, py_repr_str, usage_error, Cli, Error, Result, Token};

/// The repository root: `PSOXIDE_PERF_REPO` when set (tests and fake trees), else
/// the checkout this crate was built from.
pub fn repo_root() -> PathBuf {
    if let Some(root) = std::env::var_os("PSOXIDE_PERF_REPO") {
        return PathBuf::from(root);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tools/psoxide-perf is two levels below the repository root")
        .to_path_buf()
}

/// `target/release/<name>` when built, else the fallback profile directory.
fn built_binary(repo: &Path, name: &str, fallback_profile: &str) -> PathBuf {
    let release = repo.join("target/release").join(name);
    if release.exists() {
        release
    } else {
        repo.join("target").join(fallback_profile).join(name)
    }
}

/// A running MCP server and its request counter.
pub struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    n: i64,
}

impl Client {
    /// Start `psxed-mcp --project PROJECT --frontend FRONTEND` from `repo` and
    /// complete the MCP handshake.
    pub fn for_project(repo: &Path, project: &Path) -> Result<Client> {
        let server = built_binary(repo, "psxed-mcp", "debug");
        let frontend = built_binary(repo, "frontend", "run-fast");
        let mut command = Command::new(server);
        command
            .arg("--project")
            .arg(project)
            .arg("--frontend")
            .arg(frontend)
            .current_dir(repo);
        Client::start(command)
    }

    /// Start an arbitrary server command and complete the MCP handshake.
    pub fn start(mut command: Command) -> Result<Client> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().ok_or("server has no stdout")?);
        let mut client = Client {
            child,
            stdin,
            stdout,
            n: 0,
        };
        client.req(
            "initialize",
            Json::object([
                ("protocolVersion", Json::from("2025-06-18")),
                ("capabilities", Json::Object(Vec::new())),
                (
                    "clientInfo",
                    Json::object([("name", Json::from("mcpc")), ("version", Json::from("0"))]),
                ),
            ]),
        )?;
        client.send(&Json::object([
            ("jsonrpc", Json::from("2.0")),
            ("method", Json::from("notifications/initialized")),
        ]))?;
        Ok(client)
    }

    fn send(&mut self, message: &Json) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or("server stdin is closed")?;
        stdin.write_all(message.dumps(None, false).as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    /// Send a request and wait for the response with its id, skipping any other
    /// message the server writes in between.
    pub fn req(&mut self, method: &str, params: Json) -> Result<Json> {
        self.n += 1;
        let id = self.n;
        self.send(&Json::object([
            ("jsonrpc", Json::from("2.0")),
            ("id", Json::Int(id)),
            ("method", Json::from(method)),
            ("params", params),
        ]))?;
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line)? == 0 {
                return Err(Error("server closed the pipe".to_string()));
            }
            let message = Json::parse(&line)?;
            if message.get("id").and_then(Json::as_i64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(Error(error.dumps(None, false)));
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| Error("KeyError: 'result'".to_string()));
            }
        }
    }

    /// `tools/call` for one tool.
    pub fn call(&mut self, tool: &str, args: Json) -> Result<Json> {
        self.req(
            "tools/call",
            Json::object([("name", Json::from(tool)), ("arguments", args)]),
        )
    }

    /// Close stdin and wait (up to 30 seconds) for the server to exit.
    pub fn close(mut self) -> Result<()> {
        drop(self.stdin.take());
        let started = Instant::now();
        loop {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            if started.elapsed() >= Duration::from_secs(30) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return Err(Error("server did not exit within 30 seconds".to_string()));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Python's `str()` of a decoded JSON value.
fn py_str(value: &Json) -> String {
    match value {
        Json::Str(text) => text.clone(),
        other => py_repr(other),
    }
}

fn py_repr(value: &Json) -> String {
    match value {
        Json::Null => "None".to_string(),
        Json::Bool(true) => "True".to_string(),
        Json::Bool(false) => "False".to_string(),
        Json::Int(n) => n.to_string(),
        Json::Float(f) => crate::util::float_repr(*f),
        Json::Str(text) => py_repr_str(text),
        Json::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Json::Object(pairs) => format!(
            "{{{}}}",
            pairs
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Standard base64, ignoring bytes outside the alphabet (as `b64decode` does).
pub fn base64_decode(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut bits, mut count) = (0u32, 0u32);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    out
}

/// The first text block of a tool result (`result['content'][0]['text']`).
pub fn first_text(result: &Json) -> Result<String> {
    result
        .get("content")
        .and_then(Json::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error("tool result has no text content".to_string()))
}

fn count_lights(listing: &str) -> usize {
    Regex::new(r#"(?m)^- ""#)
        .expect("static pattern")
        .find_iter(listing)
        .count()
}

fn delete_point_lights(client: &mut Client, count: usize) -> Result<()> {
    for _ in 0..count {
        client.call(
            "delete_node",
            Json::object([("node", Json::from("Point Light"))]),
        )?;
    }
    Ok(())
}

fn list_tools(client: &mut Client, out: &mut dyn Write) -> Result<()> {
    let listing = client.req("tools/list", Json::Object(Vec::new()))?;
    let tools = listing
        .get("tools")
        .and_then(Json::as_array)
        .ok_or_else(|| Error("KeyError: 'tools'".to_string()))?;
    for tool in tools {
        let name = tool.get("name").map_or(String::new(), py_str);
        let description = tool.get("description").map_or(String::new(), py_str);
        writeln!(out, "## {name}: {}", description.trim())?;
        let properties = tool
            .get("inputSchema")
            .and_then(|s| s.get("properties"))
            .and_then(Json::as_object)
            .unwrap_or_default();
        for (key, value) in properties {
            let kind = value.get("type").map_or(String::new(), py_str);
            let help = value.get("description").map_or(String::new(), py_str);
            writeln!(out, "{}", format!("   - {key}: {kind} {help}").trim_end())?;
        }
    }
    Ok(())
}

fn run_calls(
    client: &mut Client,
    calls: &[Json],
    out_dir: &Path,
    out: &mut dyn Write,
) -> Result<i32> {
    let light_names = Regex::new(r#"(?m)^- "([^"]+)""#).expect("static pattern");
    let brush_count = Regex::new(r"(\d+) brushes").expect("static pattern");
    for (i, call) in calls.iter().enumerate() {
        let tool = call
            .get("tool")
            .and_then(Json::as_str)
            .ok_or_else(|| Error("KeyError: 'tool'".to_string()))?;
        if tool == "_clear_lights" {
            // Delete every light: the named ones first, one by one.
            let listing = first_text(&client.call("lights", Json::Object(Vec::new()))?)?;
            let count = count_lights(&listing);
            delete_point_lights(client, count)?;
            for captures in light_names.captures_iter(&listing) {
                if &captures[1] != "Point Light" {
                    client.call(
                        "delete_node",
                        Json::object([("node", Json::from(&captures[1]))]),
                    )?;
                }
            }
            writeln!(out, "=== [{i}] _clear_lights: {count} lights")?;
            continue;
        }
        if tool == "_clear" {
            // Delete every brush: each round rebuilds from zero.
            let status = first_text(&client.call("status", Json::Object(Vec::new()))?)?;
            let brushes = brush_count
                .captures(&status)
                .ok_or("'NoneType' object has no attribute 'group'")?[1]
                .parse::<i64>()
                .map_err(|e| Error(e.to_string()))?;
            if brushes != 0 {
                client.call(
                    "delete",
                    Json::object([("first", Json::Int(0)), ("count", Json::Int(brushes))]),
                )?;
            }
            // and every point light: the loop owns all lighting in its test project
            let listing = first_text(&client.call("lights", Json::Object(Vec::new()))?)?;
            let count = count_lights(&listing);
            delete_point_lights(client, count)?;
            writeln!(
                out,
                "=== [{i}] _clear: deleted {brushes} brushes, {count} lights"
            )?;
            continue;
        }
        let stop_on_error = call.get("stop_on_error").is_none_or(Json::truthy);
        let args = call
            .get("args")
            .cloned()
            .unwrap_or(Json::Object(Vec::new()));
        let result = match client.call(tool, args) {
            Ok(result) => result,
            Err(error) if !stop_on_error && is_rpc_failure(&error) => {
                writeln!(out, "=== [{i}] {tool} SKIPPED: {error}")?;
                continue;
            }
            Err(error) => return Err(error),
        };
        let is_error = result.get("isError").is_some_and(Json::truthy);
        writeln!(
            out,
            "=== [{i}] {tool}{}",
            if is_error { " ERROR" } else { "" }
        )?;
        let blocks = result
            .get("content")
            .and_then(Json::as_array)
            .unwrap_or_default();
        for (k, block) in blocks.iter().enumerate() {
            match block.get("type").and_then(Json::as_str) {
                Some("text") => {
                    writeln!(out, "{}", block.get("text").map_or(String::new(), py_str))?
                }
                Some("image") => {
                    let stem = match call.get("save") {
                        Some(save) => py_str(save),
                        None => format!("{i:02}_{tool}_{k}"),
                    };
                    let name = format!("{stem}.png");
                    let data = block.get("data").and_then(Json::as_str).unwrap_or("");
                    std::fs::write(out_dir.join(&name), base64_decode(data))?;
                    writeln!(out, "[image saved {}]", join_display(out_dir, &name))?;
                }
                _ => {}
            }
        }
        if is_error && stop_on_error {
            return Ok(1);
        }
    }
    Ok(0)
}

/// Server-side failures (`RuntimeError` in the Python client): a JSON-RPC error
/// object or a closed pipe, as opposed to a local I/O problem.
fn is_rpc_failure(error: &Error) -> bool {
    error.0 == "server closed the pipe" || error.0.starts_with('{')
}

const USAGE: &str = "psoxide-perf mcp-client --project DIR (--tools | CALLS_JSON|- [--out DIR])";

/// `psoxide-perf mcp-client --project DIR [--tools] [--out DIR] [CALLS_JSON|-]`.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let (mut project, mut tools, mut out_dir, mut calls) = (None, false, ".".to_string(), None);
    let mut cli = Cli::new(args);
    while let Some(token) = cli.next_token() {
        let step: std::result::Result<(), String> = match token {
            Token::Flag(flag) => match flag.as_str() {
                "--project" => cli.value(&flag).map(|v| project = Some(v)),
                "--tools" => {
                    tools = true;
                    Ok(())
                }
                "--out" => cli.value(&flag).map(|v| out_dir = v),
                other => Err(format!("unrecognized arguments: {other}")),
            },
            Token::Positional(text) if calls.is_none() => {
                calls = Some(text);
                Ok(())
            }
            Token::Positional(text) => Err(format!("unrecognized arguments: {text}")),
        };
        if let Err(message) = step {
            return Ok(usage_error(USAGE, &message));
        }
    }
    let Some(project) = project else {
        return Ok(usage_error(
            USAGE,
            "the following arguments are required: --project",
        ));
    };
    let mut client = Client::for_project(&repo_root(), Path::new(&project))?;
    let outcome = (|| -> Result<i32> {
        if tools {
            list_tools(&mut client, out)?;
            return Ok(0);
        }
        let source = match calls.as_deref() {
            Some("-") => {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                text
            }
            Some(path) => {
                std::fs::read_to_string(path).map_err(|e| Error(format!("{path}: {e}")))?
            }
            None => {
                return Err(Error(
                    "expected str, bytes or os.PathLike object, not NoneType".to_string(),
                ))
            }
        };
        let parsed = Json::parse(&source)?;
        let list = parsed.as_array().ok_or("calls must be a JSON list")?;
        std::fs::create_dir_all(&out_dir)?;
        run_calls(&mut client, list, Path::new(&out_dir), out)
    })();
    let closed = client.close();
    let code = outcome?;
    closed?;
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64_decode("aGVsbG8="), b"hello");
        assert_eq!(base64_decode("aGVsbG8"), b"hello");
        assert_eq!(base64_decode("AAEC/w=="), [0, 1, 2, 255]);
        assert_eq!(base64_decode("aGVs\nbG8="), b"hello");
    }

    #[test]
    fn python_reprs_of_decoded_json() {
        let value = Json::parse(r#"{"a": ["x", null, true, 1.5], "it's": "q"}"#).unwrap();
        assert_eq!(
            py_repr(&value),
            r#"{'a': ['x', None, True, 1.5], "it's": 'q'}"#
        );
        assert_eq!(py_str(&Json::from("plain")), "plain");
    }

    fn scripted_server(script: &str) -> Client {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        Client::start(command).expect("handshake")
    }

    #[test]
    fn handshake_and_call_skip_unrelated_messages() {
        let mut client = scripted_server(
            r#"read l; echo '{"jsonrpc":"2.0","id":1,"result":{}}'
read l
read l
echo '{"jsonrpc":"2.0","method":"notifications/message"}'
echo '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"hi"}]}}'
read l
echo '{"jsonrpc":"2.0","id":3,"error":{"code":-32603,"message":"nope"}}'"#,
        );
        let result = client.call("status", Json::Object(Vec::new())).unwrap();
        assert_eq!(first_text(&result).unwrap(), "hi");
        let error = client.call("boom", Json::Object(Vec::new())).unwrap_err();
        assert_eq!(error.0, r#"{"code": -32603, "message": "nope"}"#);
        assert!(is_rpc_failure(&error));
        client.close().unwrap();
    }

    #[test]
    fn a_closed_pipe_is_reported() {
        let mut client =
            scripted_server("read l; echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'; read l");
        let error = client.call("status", Json::Object(Vec::new())).unwrap_err();
        assert_eq!(error.0, "server closed the pipe");
        client.close().unwrap();
    }

    #[test]
    fn light_listings_are_counted_per_line() {
        assert_eq!(
            count_lights("- \"Point Light\"\n- \"Sun\"\ntext - \"no\"\n"),
            2
        );
    }
}
