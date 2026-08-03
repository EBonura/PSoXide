//! Live MCP debug server, embedded in the running GUI.
//!
//! Lets an agent drive the emulator over the Model Context Protocol
//! (official `rmcp` SDK, streamable HTTP at `/mcp`): screenshot, read/write
//! RAM+VRAM, step frames, toggle wireframe, load a game, reset.
//!
//! ## Threading
//! The emulator (`AppState.bus`) is single-threaded and non-`Send` (wgpu), so
//! it never leaves the winit main loop. The HTTP server runs on its own thread
//! with a private tokio runtime; tool calls become [`Cmd`]s pushed over a tokio
//! mpsc channel, drained and executed against `&mut AppState` in the redraw
//! loop ([`McpBridge::drain`]). Only plain data (PNG bytes, RAM slices) crosses
//! the thread boundary, so the non-`Send` emulator and the tokio side never
//! actually share memory.
//!
//! Native-only and behind the `mcp` feature: rmcp/tokio/axum never enter the
//! wasm dep graph (see `Cargo.toml`), and the module is `cfg`-gated off wasm.

use base64::Engine as _;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpService,
};
use rmcp::{ErrorData, ServerHandler};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::app::{self, AppState};

/// Default TCP port. Override with `PSOXIDE_MCP_PORT`.
const DEFAULT_PORT: u16 = 7355;

type Reply<T> = oneshot::Sender<Result<T, String>>;

/// A tool call, sent from the HTTP thread to the winit main loop.
enum Cmd {
    Screenshot(Reply<Vec<u8>>),
    DumpVram(Reply<Vec<u8>>),
    ReadRam {
        addr: u32,
        len: u32,
        reply: Reply<Vec<u8>>,
    },
    ReadWord {
        addr: u32,
        reply: Reply<u32>,
    },
    WriteRam {
        addr: u32,
        bytes: Vec<u8>,
        reply: Reply<usize>,
    },
    Pause(Reply<String>),
    Resume(Reply<String>),
    Step {
        frames: u32,
        reply: Reply<String>,
    },
    ToggleWireframe {
        on: Option<bool>,
        reply: Reply<bool>,
    },
    LoadGame {
        path: String,
        reply: Reply<String>,
    },
    Reset(Reply<String>),
    Status(Reply<String>),
}

/// Receiver end, owned by the main loop. Drained every redraw.
pub struct McpBridge {
    rx: mpsc::UnboundedReceiver<Cmd>,
}

impl McpBridge {
    /// Execute every queued tool call against the live emulator. Non-blocking:
    /// returns once the queue is empty. Called from the top of `RedrawRequested`.
    pub fn drain(&mut self, state: &mut AppState) {
        while let Ok(cmd) = self.rx.try_recv() {
            handle(cmd, state);
        }
    }
}

/// Start the MCP server on a background thread. Returns the bridge to store on
/// the shell, or `None` if the port could not be bound (GUI still runs).
pub fn start() -> Option<McpBridge> {
    let port = std::env::var("PSOXIDE_MCP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let (tx, rx) = mpsc::unbounded_channel();

    let spawned = std::thread::Builder::new()
        .name("psx-mcp".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[mcp] runtime build failed: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                let addr = format!("127.0.0.1:{port}");
                match tokio::net::TcpListener::bind(&addr).await {
                    Ok(listener) => {
                        eprintln!("[mcp] listening on http://{addr}/mcp");
                        serve(listener, tx).await;
                    }
                    Err(e) => eprintln!("[mcp] bind {addr} failed: {e} (server disabled)"),
                }
            });
        });

    match spawned {
        Ok(_) => Some(McpBridge { rx }),
        Err(e) => {
            eprintln!("[mcp] thread spawn failed: {e}");
            None
        }
    }
}

/// Mount the MCP service on an axum router and serve it until the listener dies.
async fn serve(listener: tokio::net::TcpListener, tx: mpsc::UnboundedSender<Cmd>) {
    let service = StreamableHttpService::new(
        move || Ok(PsxMcp { tx: tx.clone() }),
        Arc::new(LocalSessionManager::default()),
        Default::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    if let Err(e) = axum::serve(listener, router).await {
        eprintln!("[mcp] serve error: {e}");
    }
}

// ---------------------------------------------------------------------------
// Main-loop command execution (runs with &mut AppState, on the winit thread).
// ---------------------------------------------------------------------------

fn handle(cmd: Cmd, state: &mut AppState) {
    match cmd {
        Cmd::Screenshot(reply) => {
            let _ = reply.send(screenshot(state));
        }
        Cmd::DumpVram(reply) => {
            let _ = reply.send(dump_vram(state));
        }
        Cmd::ReadRam { addr, len, reply } => {
            let _ = reply.send(read_ram(state, addr, len));
        }
        Cmd::ReadWord { addr, reply } => {
            // read_ram clamps at the end of the RAM window, so a read near
            // the top can return fewer than 4 bytes -- error instead of
            // indexing past the slice (a panic here kills the GUI thread).
            let _ = reply.send(read_ram(state, addr, 4).and_then(|b| {
                let b: [u8; 4] = b
                    .try_into()
                    .map_err(|_| "address too close to the end of RAM".to_string())?;
                Ok(u32::from_le_bytes(b))
            }));
        }
        Cmd::WriteRam { addr, bytes, reply } => {
            let _ = reply.send(write_ram(state, addr, &bytes));
        }
        Cmd::Pause(reply) => {
            state.running = false;
            state.menu.sync_run_label(false);
            let _ = reply.send(Ok("paused".into()));
        }
        Cmd::Resume(reply) => {
            let msg = if state.bus.is_some() {
                state.running = true;
                state.menu.sync_run_label(true);
                "running".to_string()
            } else {
                "no game loaded".to_string()
            };
            let _ = reply.send(Ok(msg));
        }
        Cmd::Step { frames, reply } => {
            let _ = reply.send(step(state, frames));
        }
        Cmd::ToggleWireframe { on, reply } => {
            let _ = reply.send(match state.bus.as_mut() {
                Some(bus) => {
                    let next = on.unwrap_or(!bus.gpu.wireframe_enabled);
                    bus.gpu.wireframe_enabled = next;
                    Ok(next)
                }
                None => Err("no game loaded".into()),
            });
        }
        Cmd::LoadGame { path, reply } => {
            let _ = reply.send(load_game(state, &path));
        }
        Cmd::Reset(reply) => {
            let _ = reply.send(match state.current_game.clone() {
                Some(entry) => state
                    .launch_entry(&entry)
                    .map(|_| format!("reset {}", entry.title)),
                None => Err("no current game to reset".into()),
            });
        }
        Cmd::Status(reply) => {
            let _ = reply.send(Ok(status(state)));
        }
    }
}

fn screenshot(state: &AppState) -> Result<Vec<u8>, String> {
    let bus = state.bus.as_ref().ok_or("no game loaded")?;
    let (rgba, w, h) = bus.gpu.display_rgba8();
    png_from_rgba(&rgba, w, h)
}

fn dump_vram(state: &AppState) -> Result<Vec<u8>, String> {
    let bus = state.bus.as_ref().ok_or("no game loaded")?;
    let words = bus.gpu.vram.words();
    const W: u32 = 1024;
    const H: u32 = 512;
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for &px in words.iter().take((W * H) as usize) {
        let (r, g, b) = bgr555_to_rgb8(px);
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    png_from_rgba(&rgba, W, H)
}

/// Reads go through the raw RAM slice (no MMIO side effects). `addr` is masked
/// to the 2 MB main-RAM window, so KUSEG/KSEG0/KSEG1 mirrors all resolve.
fn read_ram(state: &AppState, addr: u32, len: u32) -> Result<Vec<u8>, String> {
    let bus = state.bus.as_ref().ok_or("no game loaded")?;
    let ram = bus.ram();
    let start = (addr as usize) & (ram.len() - 1);
    let len = (len as usize).min(64 * 1024); // ponytail: cap a single read at 64 KiB
    let end = (start + len).min(ram.len());
    Ok(ram[start..end].to_vec())
}

/// Writes go through `bus.write8` so mirrors and any write-side bookkeeping are
/// honored. A RAM address writes RAM; an I/O address would hit MMIO (caller's
/// responsibility, same as a real debugger).
fn write_ram(state: &mut AppState, addr: u32, bytes: &[u8]) -> Result<usize, String> {
    let bus = state.bus.as_mut().ok_or("no game loaded")?;
    for (i, &b) in bytes.iter().enumerate() {
        bus.write8(addr.wrapping_add(i as u32), b);
    }
    Ok(bytes.len())
}

fn step(state: &mut AppState, frames: u32) -> Result<String, String> {
    if state.bus.is_none() {
        return Err("no game loaded".into());
    }
    // Step even while paused: this is the debugger advance, independent of the
    // run loop. Cap to keep a runaway request from stalling the UI for minutes.
    let frames = frames.clamp(1, 6000); // ponytail: ~100s at 60fps ceiling
    let mut instructions = 0u64;
    let mut vblanks = 0u64;
    for _ in 0..frames {
        let r = app::step_one_frame(state);
        instructions += r.instructions;
        vblanks += r.vblanks;
    }
    Ok(format!(
        "stepped {frames} frame(s): {instructions} instructions, {vblanks} vblanks"
    ))
}

fn load_game(state: &mut AppState, path: &str) -> Result<String, String> {
    use psoxide_settings::library::{GameKind, LibraryEntry, Region};
    use std::path::PathBuf;

    let path = PathBuf::from(path);
    let meta = std::fs::metadata(&path).map_err(|e| format!("stat {}: {e}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let kind = match ext.as_str() {
        "exe" => GameKind::Exe,
        "cue" => GameKind::DiscCue,
        "ccd" => GameKind::DiscCcd,
        "iso" => GameKind::DiscIso,
        "bin" | "img" => GameKind::DiscBin,
        other => return Err(format!("unsupported extension: .{other}")),
    };
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mcp-game")
        .to_string();
    // The id only names the per-game save dir; the file path is the identity a
    // debug session cares about, so a path-derived id is stable enough here.
    let id = format!("mcp-{:016x}", fnv1a(path.to_string_lossy().as_bytes()));
    let entry = LibraryEntry {
        id,
        path: path.clone(),
        kind,
        title: stem.clone(),
        region: Region::Unknown,
        size: meta.len(),
        mtime: 0,
        diagnostic: None,
    };
    state.launch_entry(&entry)?;
    // The GUI launch path closes the Menu overlay in `apply_menu_action::
    // LaunchGame`; mirror that here so an MCP-loaded game is actually
    // visible in the window (and the paused redraw scheduler doesn't hold
    // the active tick for an overlay nobody opened).
    state.menu.open = false;
    Ok(format!("loaded {stem}"))
}

fn status(state: &AppState) -> String {
    match state.bus.as_ref() {
        Some(bus) => {
            let (_, w, h) = bus.gpu.display_rgba8();
            format!(
                "{{\"running\":{},\"pc\":\"0x{:08x}\",\"cycles\":{},\"wireframe\":{},\"display\":\"{w}x{h}\",\"game\":{:?}}}",
                state.running,
                state.cpu.pc(),
                bus.cycles(),
                bus.gpu.wireframe_enabled,
                state.current_game.as_ref().map(|e| e.title.as_str()),
            )
        }
        None => "{\"running\":false,\"game\":null}".into(),
    }
}

// --- small helpers ---------------------------------------------------------

fn bgr555_to_rgb8(px: u16) -> (u8, u8, u8) {
    let r5 = (px & 0x1F) as u8;
    let g5 = ((px >> 5) & 0x1F) as u8;
    let b5 = ((px >> 10) & 0x1F) as u8;
    let ex = |c: u8| (c << 3) | (c >> 2);
    (ex(r5), ex(g5), ex(b5))
}

fn png_from_rgba(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())
        .ok_or("rgba buffer does not match dimensions")?;
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| format!("png encode: {e}"))?;
    Ok(png.into_inner())
}

use psx_hw::hash::fnv1a_64 as fnv1a;

// ---------------------------------------------------------------------------
// rmcp server: tool surface. Each tool sends a Cmd and awaits the reply.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct PsxMcp {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl PsxMcp {
    /// Send a command to the main loop and await its reply, mapping the transport
    /// and tool errors into rmcp's error type.
    async fn call<T, F>(&self, make: F) -> Result<T, ErrorData>
    where
        F: FnOnce(Reply<T>) -> Cmd,
    {
        let (rtx, rrx) = oneshot::channel();
        self.tx
            .send(make(rtx))
            .map_err(|_| ErrorData::internal_error("emulator loop is gone", None))?;
        rrx.await
            .map_err(|_| ErrorData::internal_error("emulator dropped the request", None))?
            .map_err(|e| ErrorData::internal_error(e, None))
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ReadRamReq {
    /// Guest address (masked to the 2 MB main-RAM window).
    addr: u32,
    /// Number of bytes to read (max 65536).
    len: u32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct AddrReq {
    /// Guest address (masked to the 2 MB main-RAM window).
    addr: u32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct WriteRamReq {
    /// Guest address to start writing at.
    addr: u32,
    /// Bytes to write, as a hex string (e.g. "deadbeef" or "de ad be ef").
    hex: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct StepReq {
    /// Number of video frames to advance (1..=6000).
    frames: u32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ToggleWireframeReq {
    /// Desired state. Omit to toggle the current value.
    on: Option<bool>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct LoadGameReq {
    /// Absolute path to a disc image (.cue/.bin/.iso/.ccd) or PSX-EXE (.exe).
    path: String,
}

fn png_result(png: Vec<u8>) -> CallToolResult {
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    CallToolResult::success(vec![ContentBlock::image(b64, "image/png")])
}

fn text_result(s: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(s.into())])
}

#[rmcp::tool_router]
impl PsxMcp {
    #[rmcp::tool(description = "PNG screenshot of the current PSX display output.")]
    async fn screenshot(&self) -> Result<CallToolResult, ErrorData> {
        Ok(png_result(self.call(Cmd::Screenshot).await?))
    }

    #[rmcp::tool(description = "PNG dump of the full 1024x512 VRAM (BGR555 -> RGB).")]
    async fn dump_vram(&self) -> Result<CallToolResult, ErrorData> {
        Ok(png_result(self.call(Cmd::DumpVram).await?))
    }

    #[rmcp::tool(description = "Read bytes from main RAM. Returns a hex string.")]
    async fn read_ram(
        &self,
        Parameters(ReadRamReq { addr, len }): Parameters<ReadRamReq>,
    ) -> Result<CallToolResult, ErrorData> {
        let bytes = self.call(|reply| Cmd::ReadRam { addr, len, reply }).await?;
        Ok(text_result(hex(&bytes)))
    }

    #[rmcp::tool(description = "Read a 32-bit little-endian word from main RAM.")]
    async fn read_word(
        &self,
        Parameters(AddrReq { addr }): Parameters<AddrReq>,
    ) -> Result<CallToolResult, ErrorData> {
        let word = self.call(|reply| Cmd::ReadWord { addr, reply }).await?;
        Ok(text_result(format!("0x{word:08x}")))
    }

    #[rmcp::tool(description = "Write hex bytes into main RAM at addr.")]
    async fn write_ram(
        &self,
        Parameters(WriteRamReq { addr, hex }): Parameters<WriteRamReq>,
    ) -> Result<CallToolResult, ErrorData> {
        let bytes = parse_hex(&hex).map_err(|e| ErrorData::invalid_params(e, None))?;
        let n = self
            .call(|reply| Cmd::WriteRam { addr, bytes, reply })
            .await?;
        Ok(text_result(format!("wrote {n} bytes at 0x{addr:08x}")))
    }

    #[rmcp::tool(description = "Pause emulation (stop the run loop).")]
    async fn pause(&self) -> Result<CallToolResult, ErrorData> {
        Ok(text_result(self.call(Cmd::Pause).await?))
    }

    #[rmcp::tool(description = "Resume emulation at full speed.")]
    async fn resume(&self) -> Result<CallToolResult, ErrorData> {
        Ok(text_result(self.call(Cmd::Resume).await?))
    }

    #[rmcp::tool(description = "Advance emulation by N video frames, even while paused.")]
    async fn step(
        &self,
        Parameters(StepReq { frames }): Parameters<StepReq>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(text_result(
            self.call(|reply| Cmd::Step { frames, reply }).await?,
        ))
    }

    #[rmcp::tool(description = "Toggle the GPU wireframe render mode. Omit `on` to flip it.")]
    async fn toggle_wireframe(
        &self,
        Parameters(ToggleWireframeReq { on }): Parameters<ToggleWireframeReq>,
    ) -> Result<CallToolResult, ErrorData> {
        let now = self
            .call(|reply| Cmd::ToggleWireframe { on, reply })
            .await?;
        Ok(text_result(format!("wireframe = {now}")))
    }

    #[rmcp::tool(description = "Boot a disc image (.cue/.bin/.iso/.ccd) or PSX-EXE by path.")]
    async fn load_game(
        &self,
        Parameters(LoadGameReq { path }): Parameters<LoadGameReq>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(text_result(
            self.call(|reply| Cmd::LoadGame { path, reply }).await?,
        ))
    }

    #[rmcp::tool(description = "Reboot the currently loaded game.")]
    async fn reset(&self) -> Result<CallToolResult, ErrorData> {
        Ok(text_result(self.call(Cmd::Reset).await?))
    }

    #[rmcp::tool(description = "Emulator status: run state, PC, cycles, wireframe, display, game.")]
    async fn status(&self) -> Result<CallToolResult, ErrorData> {
        Ok(text_result(self.call(Cmd::Status).await?))
    }
}

#[rmcp::tool_handler(
    name = "psoxide",
    version = "0.1.0",
    instructions = "Drive the live PSoXide PS1 emulator: screenshot, dump_vram, read_ram/read_word/write_ram, pause/resume/step, toggle_wireframe, load_game, reset, status."
)]
impl ServerHandler for PsxMcp {}

// --- hex helpers -----------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !cleaned.len().is_multiple_of(2) {
        return Err("hex string must have an even number of digits".into());
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_roundtrips_and_ignores_whitespace() {
        assert_eq!(parse_hex("deadbeef").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(
            parse_hex("de ad be ef").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
        assert_eq!(hex(&[0xde, 0xad]), "dead");
        assert!(parse_hex("abc").is_err()); // odd length
        assert!(parse_hex("zz").is_err()); // non-hex
    }

    #[test]
    fn bgr555_endpoints() {
        assert_eq!(bgr555_to_rgb8(0x0000), (0, 0, 0));
        assert_eq!(bgr555_to_rgb8(0x7fff), (255, 255, 255)); // all 5-bit channels max
        assert_eq!(bgr555_to_rgb8(0x001f), (255, 0, 0)); // red channel only
        assert_eq!(bgr555_to_rgb8(0x7c00), (0, 0, 255)); // blue channel only
    }

    /// End-to-end: the axum + rmcp stack actually binds, completes the MCP
    /// `initialize` handshake, and lists our tools. Uses a raw HTTP client so
    /// no extra deps; drives the real `serve()` path on an ephemeral port.
    #[tokio::test]
    async fn server_initializes_and_lists_tools() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, _rx) = mpsc::unbounded_channel(); // no commands are issued by list/init
        tokio::spawn(async move { serve(listener, tx).await });

        // POST a JSON-RPC `initialize`. Streamable-HTTP replies with an SSE body
        // and a Mcp-Session-Id header we reuse for the follow-up tools/list.
        async fn post(addr: std::net::SocketAddr, body: &str, session: Option<&str>) -> String {
            let mut req = format!(
                "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
                 Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n",
                body.len()
            );
            if let Some(s) = session {
                req.push_str(&format!("Mcp-Session-Id: {s}\r\n"));
            }
            req.push_str("Connection: close\r\n\r\n");
            req.push_str(body);
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream.write_all(req.as_bytes()).await.unwrap();
            let mut resp = String::new();
            stream.read_to_string(&mut resp).await.unwrap();
            resp
        }

        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
        let resp = post(addr, init, None).await;
        assert!(resp.contains("200 OK"), "initialize failed: {resp}");
        assert!(resp.contains("protocolVersion"), "no init result: {resp}");
        let session = resp
            .lines()
            .find_map(|l| {
                l.strip_prefix("mcp-session-id: ")
                    .or_else(|| l.strip_prefix("Mcp-Session-Id: "))
            })
            .map(|s| s.trim().to_string())
            .expect("server returned no session id");

        // The spec requires a `notifications/initialized` before other calls.
        let _ = post(
            addr,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            Some(&session),
        )
        .await;

        let list = post(
            addr,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            Some(&session),
        )
        .await;
        for tool in [
            "screenshot",
            "toggle_wireframe",
            "read_ram",
            "load_game",
            "status",
        ] {
            assert!(list.contains(tool), "tools/list missing {tool}: {list}");
        }
    }
}
