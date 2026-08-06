//! Same-origin streamed discs for the web build.
//!
//! A full disc image is far too large to bake into the wasm, and even one
//! straight download is a long sit: the pressing is mostly CD-DA. So the
//! deploy stages a split delivery next to the wasm (see the demo-disc repo's
//! `tools/web-delivery.py`): the data track gzipped, each audio track as
//! lossless FLAC, and a small text manifest naming the pieces with sizes and
//! FNV-1a-32 checksums. The boot gate is the data track plus the first song;
//! the rest stream in behind the running emulator and are patched into the
//! mounted disc as they land. A track whose bytes have not arrived plays as
//! digital silence -- the geometry is complete from the first frame, so the
//! drive model never sees anything a real disc could not do.
//!
//! Ordering note: pieces download in disc order and the launcher cycles its
//! menu tracks in the same order, so delivery stays ahead of the playhead on
//! any connection that can stream audio at all.
//!
//! FLAC decoding runs in Rust (claxon), budgeted per frame from the shell's
//! poll so a 30 MB track never stalls a redraw. Fetches run in JS; bytes
//! cross the boundary once per piece.

use std::cell::RefCell;
use std::io::Cursor;

use wasm_bindgen::prelude::*;

/// One disc served next to the wasm, streamed on demand.
pub struct StreamedDisc {
    /// Menu launch id; the `stream:` prefix is how launch routing spots it.
    /// Also the IndexedDB save-state key, so renaming it orphans saves.
    pub id: &'static str,
    /// Menu title.
    pub title: &'static str,
    /// Menu subtitle. Honest about what a click starts.
    pub subtitle: &'static str,
    /// Manifest URL, relative to the page. Everything else is named by it.
    pub manifest_url: &'static str,
    /// CUE sheet URL, relative to the page.
    pub cue_url: &'static str,
}

/// The streamed catalog. The deploy workflow stages the files from the
/// demo-disc repo's rolling `web-disc` release; a missing file surfaces as
/// an HTTP error in the status bar rather than a build failure.
pub static DISCS: &[StreamedDisc] = &[StreamedDisc {
    id: "stream:demo-disc",
    title: "PSoXide Demo Disc",
    subtitle: "homebrew - boots in ~25 MB, music streams in",
    manifest_url: "web-manifest.txt",
    cue_url: "demo-disc.cue",
}];

/// Look up a streamed disc by its menu launch id.
pub fn find(id: &str) -> Option<&'static StreamedDisc> {
    DISCS.iter().find(|d| d.id == id)
}

// ---- JS: one XHR slot per piece, polled from Rust ---------------------------
//
// XHR rather than fetch-with-reader: the emulator's rAF loop starves a stream
// reader's microtask ping-pong to one chunk per frame, while XHR assembles
// natively and still reports byte progress. Gunzip uses the browser's own
// DecompressionStream. Slots hold their payload until Rust takes it.
#[wasm_bindgen(inline_js = r#"
let _slots = {};
export function wsFetch(id, url) {
  if (_slots[id]) return;
  const s = { state: 'running', received: 0, total: 0 };
  _slots[id] = s;
  const xhr = new XMLHttpRequest();
  xhr.open('GET', url);
  xhr.responseType = 'arraybuffer';
  xhr.onprogress = (e) => { s.received = e.loaded; if (e.lengthComputable) s.total = e.total; };
  xhr.onload = () => {
    if (xhr.status >= 200 && xhr.status < 300) { s.buf = new Uint8Array(xhr.response); s.state = 'done'; }
    else { s.msg = url + ': HTTP ' + xhr.status; s.state = 'error'; }
  };
  xhr.onerror = () => { s.msg = url + ': network error'; s.state = 'error'; };
  xhr.send();
}
export function wsGunzip(id) {
  const s = _slots[id];
  if (!s || s.state !== 'done' || s.unzipping) return;
  // Some CDNs (itch.io's, for one) serve a .gz file with Content-Encoding:
  // gzip, so the browser has already inflated it by the time XHR hands it
  // over. The magic bytes say which case this is; the FNV check downstream
  // vouches for the payload either way.
  if (!(s.buf.length > 2 && s.buf[0] === 0x1f && s.buf[1] === 0x8b)) return;
  s.unzipping = true;
  s.state = 'running';
  (async () => {
    try {
      const out = await new Response(
        new Blob([s.buf]).stream().pipeThrough(new DecompressionStream('gzip'))
      ).arrayBuffer();
      s.buf = new Uint8Array(out);
      s.state = 'done';
    } catch (e) { s.msg = 'gunzip: ' + String(e); s.state = 'error'; }
  })();
}
export function wsState(id) { const s = _slots[id]; return s ? s.state : 'idle'; }
export function wsReceived(id) { const s = _slots[id]; return s ? s.received : 0; }
export function wsTotal(id) { const s = _slots[id]; return s ? s.total : 0; }
export function wsTake(id) { const s = _slots[id]; const b = s.buf; delete _slots[id]; return b; }
export function wsError(id) { const s = _slots[id]; const m = s ? s.msg : 'no slot'; delete _slots[id]; return m || 'fetch failed'; }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = wsFetch)]
    fn ws_fetch(id: &str, url: &str);
    #[wasm_bindgen(js_name = wsGunzip)]
    fn ws_gunzip(id: &str);
    #[wasm_bindgen(js_name = wsState)]
    fn ws_state(id: &str) -> String;
    #[wasm_bindgen(js_name = wsReceived)]
    fn ws_received(id: &str) -> f64;
    #[wasm_bindgen(js_name = wsTotal)]
    fn ws_total(id: &str) -> f64;
    #[wasm_bindgen(js_name = wsTake)]
    fn ws_take(id: &str) -> Vec<u8>;
    #[wasm_bindgen(js_name = wsError)]
    fn ws_error(id: &str) -> String;
}

/// FNV-1a-32, the project's checksum of habit. Matches `web-delivery.py`.
fn fnv1a32(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for &b in data {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    h
}

/// One audio piece from the manifest.
struct TrackMeta {
    number: u8,
    title: String,
    file: String,
    raw_bytes: usize,
    fnv: u32,
}

/// Where a background track stands, for the shell's progress line.
#[derive(Clone, PartialEq, Eq)]
pub enum TrackState {
    /// Not started.
    Pending,
    /// Bytes on the wire, percent of the FLAC fetched.
    Fetching(u8),
    /// Fetched, decoding back into sectors.
    Decoding,
    /// Decoded, verified, patched into the disc.
    Ready,
    /// Fetch or checksum failure; the track stays silent.
    Failed,
}

/// A decode in progress, resumed a budget at a time.
struct Decode {
    reader: claxon::FlacReader<Cursor<Vec<u8>>>,
    /// Reused per-block sample buffer.
    buffer: Vec<i32>,
    out: Vec<u8>,
    index: usize,
}

/// FLAC blocks read per poll. Whole blocks only: claxon's readers resume
/// cleanly between fully-consumed blocks, where a sample iterator dropped
/// mid-block silently discards the block's tail. Kept small: the guest is
/// emulating a PlayStation on the same thread, and a long decode slice
/// underruns the CD audio, which the launcher's stall watchdog answers by
/// restarting the song. Three blocks is ~25k samples, a fraction of a
/// millisecond, and still decodes a full track well inside its own playtime.
const DECODE_BUDGET_BLOCKS: usize = 3;

enum Phase {
    /// Fetching manifest, cue, data piece and first track.
    Boot,
    /// The shell took the boot payload; remaining tracks stream in.
    Background,
    /// Every track is home (or given up on).
    Done,
    /// The boot payload could not be assembled.
    Failed,
}

#[derive(Default)]
struct Pieces {
    cue: Option<String>,
    data: Option<Vec<u8>>,
    first_pcm: Option<Vec<u8>>,
}

struct Stream {
    disc: &'static StreamedDisc,
    phase: Phase,
    tracks: Vec<TrackMeta>,
    states: Vec<TrackState>,
    data_raw_bytes: usize,
    data_fnv: u32,
    gunzip_requested: bool,
    pieces: Pieces,
    decode: Option<Decode>,
    /// Decoded-and-verified tracks waiting for the shell to patch in.
    ready: Vec<(u8, Vec<u8>)>,
    error: String,
}

thread_local! {
    static STREAM: RefCell<Option<Stream>> = const { RefCell::new(None) };
}

/// What the shell hears each frame while the boot payload assembles.
pub enum BootStatus {
    /// Nothing in flight.
    Idle,
    /// Human-readable progress for the status line.
    Progress(String),
    /// Everything the boot needs, handed over exactly once.
    Ready {
        /// The disc being streamed.
        disc: &'static StreamedDisc,
        /// CUE sheet text.
        cue: String,
        /// Track 1's file extent, decompressed.
        data: Vec<u8>,
        /// First audio track's raw sectors.
        first_pcm: Vec<u8>,
        /// First audio track's number.
        first_number: u8,
        /// `(number, raw_bytes)` for every audio track, for placeholders.
        layout: Vec<(u8, usize)>,
    },
    /// The stream is dead; message handed over exactly once.
    Failed(String),
}

/// A background delivery event for the shell to apply.
pub enum BgEvent {
    /// A track's exact sectors, verified, ready to patch into the disc.
    TrackReady(u8, Vec<u8>),
}

/// Kick off a stream. Idempotent while one is running: double-clicking the
/// menu entry cannot fork the pipeline.
pub fn start(disc: &'static StreamedDisc) {
    STREAM.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_some() {
            return;
        }
        ws_fetch("manifest", disc.manifest_url);
        ws_fetch("cue", disc.cue_url);
        *slot = Some(Stream {
            disc,
            phase: Phase::Boot,
            tracks: Vec::new(),
            states: Vec::new(),
            data_raw_bytes: 0,
            data_fnv: 0,
            gunzip_requested: false,
            pieces: Pieces::default(),
            decode: None,
            ready: Vec::new(),
            error: String::new(),
        });
    });
}

fn parse_manifest(text: &str, s: &mut Stream) -> Result<(), String> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("data") => {
                let _file = parts.next().ok_or("manifest: data file missing")?;
                let _gz = parts
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or("manifest: gz bytes")?;
                s.data_raw_bytes = parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("manifest: raw bytes")?;
                s.data_fnv = parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("manifest: data fnv")?;
            }
            Some("track") => {
                let number: u8 = parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("manifest: track number")?;
                let file = parts.next().ok_or("manifest: track file")?.to_string();
                let _flac = parts
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or("manifest: flac bytes")?;
                let raw_bytes: usize = parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("manifest: track raw")?;
                let fnv: u32 = parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("manifest: track fnv")?;
                let title = parts.collect::<Vec<_>>().join(" ");
                s.tracks.push(TrackMeta {
                    number,
                    title,
                    file,
                    raw_bytes,
                    fnv,
                });
                s.states.push(TrackState::Pending);
            }
            _ => {}
        }
    }
    if s.data_raw_bytes == 0 || s.tracks.is_empty() {
        return Err("manifest: empty or unreadable".into());
    }
    Ok(())
}

/// Begin fetching track `index` if it exists and has not started.
fn start_fetch(s: &mut Stream, index: usize) {
    if index < s.tracks.len() && s.states[index] == TrackState::Pending {
        ws_fetch(&format!("track{index}"), &s.tracks[index].file.clone());
        s.states[index] = TrackState::Fetching(0);
    }
}

/// Move a fetched track into the budgeted decoder; called when the decoder
/// is free.
fn start_decode(s: &mut Stream, index: usize) {
    let slot = format!("track{index}");
    if ws_state(&slot) != "done" {
        return;
    }
    let flac = ws_take(&slot);
    match claxon::FlacReader::new(Cursor::new(flac)) {
        Ok(reader) => {
            let cap = s.tracks[index].raw_bytes;
            s.decode = Some(Decode {
                reader,
                buffer: Vec::new(),
                out: Vec::with_capacity(cap),
                index,
            });
            s.states[index] = TrackState::Decoding;
        }
        Err(e) => {
            s.states[index] = TrackState::Failed;
            s.error = format!("{}: FLAC header: {e}", s.tracks[index].title);
        }
    }
}

/// Run the decoder for one budget slice. On completion, verify and queue the
/// track for patching.
fn pump_decode(s: &mut Stream) {
    let Some(dec) = s.decode.as_mut() else { return };
    let mut finished = false;
    {
        let mut frames = dec.reader.blocks();
        for _ in 0..DECODE_BUDGET_BLOCKS {
            match frames.read_next_or_eof(core::mem::take(&mut dec.buffer)) {
                Ok(Some(block)) => {
                    for i in 0..block.duration() {
                        dec.out
                            .extend_from_slice(&(block.sample(0, i) as i16).to_le_bytes());
                        dec.out
                            .extend_from_slice(&(block.sample(1, i) as i16).to_le_bytes());
                    }
                    dec.buffer = block.into_buffer();
                }
                Ok(None) | Err(_) => {
                    finished = true;
                    break;
                }
            }
        }
    }
    if !finished {
        return;
    }
    let dec = s.decode.take().expect("decode present");
    let meta = &s.tracks[dec.index];
    if dec.out.len() == meta.raw_bytes && fnv1a32(&dec.out) == meta.fnv {
        s.states[dec.index] = TrackState::Ready;
        s.ready.push((meta.number, dec.out));
    } else {
        s.states[dec.index] = TrackState::Failed;
        s.error = format!("{}: decode mismatch, leaving it silent", meta.title);
    }
}

fn fail(s: &mut Stream, message: String) -> BootStatus {
    s.phase = Phase::Failed;
    BootStatus::Failed(message)
}

/// Per-frame poll while the boot payload assembles. `Ready` and `Failed`
/// hand over exactly once; afterwards use [`poll_background`].
pub fn poll_boot() -> BootStatus {
    STREAM.with(|cell| {
        let mut slot = cell.borrow_mut();
        let Some(s) = slot.as_mut() else {
            return BootStatus::Idle;
        };
        if !matches!(s.phase, Phase::Boot) {
            return BootStatus::Idle;
        }

        // Manifest and cue, both tiny.
        if s.tracks.is_empty() {
            match ws_state("manifest").as_str() {
                "done" => {
                    let text = String::from_utf8_lossy(&ws_take("manifest")).into_owned();
                    if let Err(e) = parse_manifest(&text, s) {
                        return fail(s, e);
                    }
                    ws_fetch("data", "demo-data.bin.gz");
                    start_fetch(s, 0);
                }
                "error" => return fail(s, ws_error("manifest")),
                _ => return BootStatus::Progress(format!("{}: reading manifest...", s.disc.title)),
            }
        }
        if s.pieces.cue.is_none() {
            match ws_state("cue").as_str() {
                "done" => {
                    s.pieces.cue = Some(String::from_utf8_lossy(&ws_take("cue")).into_owned())
                }
                "error" => return fail(s, ws_error("cue")),
                _ => return BootStatus::Progress(format!("{}: reading cue...", s.disc.title)),
            }
        }

        // The data piece: fetch, then browser-native gunzip, then verify.
        if s.pieces.data.is_none() {
            match ws_state("data").as_str() {
                "done" if !s.gunzip_requested => {
                    s.gunzip_requested = true;
                    ws_gunzip("data");
                    return BootStatus::Progress(format!(
                        "{}: unpacking data track...",
                        s.disc.title
                    ));
                }
                "done" => {
                    let raw = ws_take("data");
                    if raw.len() != s.data_raw_bytes || fnv1a32(&raw) != s.data_fnv {
                        return fail(s, "data track: checksum mismatch".into());
                    }
                    s.pieces.data = Some(raw);
                }
                "error" => return fail(s, ws_error("data")),
                _ => {
                    let mb = ws_received("data") / (1024.0 * 1024.0);
                    return BootStatus::Progress(format!(
                        "{}: data track... {mb:.1} MB",
                        s.disc.title
                    ));
                }
            }
        }

        // First song: fetch, budgeted decode, verify.
        if s.pieces.first_pcm.is_none() {
            if let TrackState::Fetching(_) = s.states[0] {
                match ws_state("track0").as_str() {
                    "done" => {
                        start_decode(s, 0);
                        // The wire is free again: fetch the next song while
                        // this one decodes.
                        start_fetch(s, 1);
                    }
                    "error" => return fail(s, ws_error("track0")),
                    _ => {
                        let pct = if ws_total("track0") > 0.0 {
                            (ws_received("track0") / ws_total("track0") * 100.0) as u8
                        } else {
                            0
                        };
                        s.states[0] = TrackState::Fetching(pct);
                        return BootStatus::Progress(format!(
                            "{}: {} {pct}%",
                            s.disc.title, s.tracks[0].title
                        ));
                    }
                }
            }
            if s.decode.as_ref().is_some_and(|d| d.index == 0) {
                pump_decode(s);
            }
            match s.states[0] {
                TrackState::Ready => {
                    let (_, pcm) = s.ready.remove(0);
                    s.pieces.first_pcm = Some(pcm);
                }
                TrackState::Failed => {
                    let message = s.error.clone();
                    return fail(s, message);
                }
                _ => {
                    return BootStatus::Progress(format!(
                        "{}: decoding {}...",
                        s.disc.title, s.tracks[0].title
                    ))
                }
            }
        }

        // Everything the boot gate needs is in hand.
        s.phase = Phase::Background;
        start_fetch(s, 1);
        BootStatus::Ready {
            disc: s.disc,
            cue: s.pieces.cue.take().expect("cue fetched"),
            data: s.pieces.data.take().expect("data fetched"),
            first_pcm: s.pieces.first_pcm.take().expect("first track decoded"),
            first_number: s.tracks[0].number,
            layout: s.tracks.iter().map(|t| (t.number, t.raw_bytes)).collect(),
        }
    })
}

/// Per-frame poll after boot: advances fetches and decodes under budget and
/// returns tracks ready to be patched into the mounted disc.
pub fn poll_background() -> Vec<BgEvent> {
    STREAM.with(|cell| {
        let mut slot = cell.borrow_mut();
        let Some(s) = slot.as_mut() else {
            return Vec::new();
        };
        if !matches!(s.phase, Phase::Background) {
            return Vec::new();
        }

        // Hand finished fetches to the decoder and keep the wire busy: the
        // next fetch starts the moment the current one lands.
        for index in 1..s.tracks.len() {
            if let TrackState::Fetching(_) = s.states[index] {
                let id = format!("track{index}");
                match ws_state(&id).as_str() {
                    "done" => {
                        if s.decode.is_none() {
                            start_decode(s, index);
                        }
                        start_fetch(s, index + 1);
                    }
                    "error" => {
                        let _ = ws_error(&id);
                        s.states[index] = TrackState::Failed;
                    }
                    _ => {
                        let pct = if ws_total(&id) > 0.0 {
                            (ws_received(&id) / ws_total(&id) * 100.0) as u8
                        } else {
                            0
                        };
                        s.states[index] = TrackState::Fetching(pct);
                    }
                }
            }
        }
        pump_decode(s);
        // A finished decode frees the decoder for the next fetched track.
        if s.decode.is_none() {
            for index in 1..s.tracks.len() {
                if matches!(s.states[index], TrackState::Fetching(_))
                    && ws_state(&format!("track{index}")) == "done"
                {
                    start_decode(s, index);
                    break;
                }
            }
        }

        let events: Vec<BgEvent> = s
            .ready
            .drain(..)
            .map(|(number, pcm)| BgEvent::TrackReady(number, pcm))
            .collect();

        if s.decode.is_none()
            && s.ready.is_empty()
            && s.states
                .iter()
                .all(|t| matches!(t, TrackState::Ready | TrackState::Failed))
        {
            s.phase = Phase::Done;
        }
        events
    })
}

/// One line for the status bar while tracks are still arriving, `None` once
/// the whole disc is home (or before a stream starts).
pub fn progress_line() -> Option<String> {
    STREAM.with(|cell| {
        let slot = cell.borrow();
        let s = slot.as_ref()?;
        if !matches!(s.phase, Phase::Background) {
            return None;
        }
        let ready = s
            .states
            .iter()
            .filter(|t| matches!(t, TrackState::Ready))
            .count();
        let active = s.states.iter().enumerate().find_map(|(i, t)| match t {
            TrackState::Fetching(pct) => Some(format!("{} {pct}%", s.tracks[i].title)),
            TrackState::Decoding => Some(format!("{} decoding", s.tracks[i].title)),
            _ => None,
        });
        match active {
            Some(what) => Some(format!(
                "Music streaming in: {what} ({ready}/{} ready)",
                s.tracks.len()
            )),
            None => Some(format!(
                "Music streaming in: {ready}/{} ready",
                s.tracks.len()
            )),
        }
    })
}
