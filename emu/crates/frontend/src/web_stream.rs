//! Same-origin streamed discs for the web build.
//!
//! A full disc image is far too large to bake into the wasm the way the
//! `bundled` payloads are (`include_bytes!` puts it in the module's data
//! section, which wasm-opt must rewrite and every visitor must download before
//! the page runs). Instead the Pages deploy stages the image as a plain static
//! file next to the wasm, and this module fetches it when the user actually
//! asks for it: cue sheet first (a few hundred bytes of text), then the single
//! BIN, with byte progress readable per frame.
//!
//! The fetch runs in JS and hands Rust the finished payload once, through the
//! same poll-per-frame shape `web_files` uses. Single-threaded wasm, so the
//! JS-side state cell is sound.
//!
//! Memory: the browser holds the chunk list plus the assembled Uint8Array,
//! and the wasm copy makes a third transient instance of the image while the
//! per-track slices are cut. Roughly 3x the disc size peaks during boot, which
//! a ~200 MB image fits comfortably inside wasm32's address space.
// ponytail: whole-image fetch before boot; range-fetch the data track first
// and let CD-DA arrive in the background if time-to-boot ever matters more.

use wasm_bindgen::prelude::*;

/// One disc served next to the wasm, fetched on demand.
pub struct StreamedDisc {
    /// Menu launch id; the `stream:` prefix is how launch routing spots it.
    /// Also the IndexedDB save-state key, so renaming it orphans saves.
    pub id: &'static str,
    /// Menu title.
    pub title: &'static str,
    /// Menu subtitle. Says the size out loud: clicking starts a download.
    pub subtitle: &'static str,
    /// CUE sheet URL, relative to the page.
    pub cue_url: &'static str,
    /// BIN image URL, relative to the page.
    pub bin_url: &'static str,
}

/// The streamed catalog. The deploy workflow stages these files from the
/// demo-disc repo's rolling `web-disc` release; a missing file surfaces as an
/// HTTP error in the status bar rather than a build failure.
pub static DISCS: &[StreamedDisc] = &[StreamedDisc {
    id: "stream:demo-disc",
    title: "PSoXide Demo Disc",
    subtitle: "homebrew - 200 MB download",
    cue_url: "demo-disc.cue",
    bin_url: "demo-disc.bin",
}];

/// Look up a streamed disc by its menu launch id.
pub fn find(id: &str) -> Option<&'static StreamedDisc> {
    DISCS.iter().find(|d| d.id == id)
}

// One fetch at a time, state polled per frame. The JS keeps the bytes until
// Rust takes them, so a slow frame drops nothing.
#[wasm_bindgen(inline_js = r#"
let _sd = { state: 'idle' };
export function sdStart(cueUrl, binUrl) {
  if (_sd.state === 'running') return;
  _sd = { state: 'running', received: 0, total: 0 };
  (async () => {
    try {
      const cueResp = await fetch(cueUrl);
      if (!cueResp.ok) throw new Error(cueUrl + ': HTTP ' + cueResp.status);
      const cue = await cueResp.text();
      // XHR rather than fetch+reader: the emulator's rAF loop starves the
      // stream reader's microtask ping-pong (one chunk per frame), while
      // XHR assembles the buffer natively and still reports byte progress.
      const bin = await new Promise((resolve, reject) => {
        const xhr = new XMLHttpRequest();
        xhr.open('GET', binUrl);
        xhr.responseType = 'arraybuffer';
        xhr.onprogress = (e) => { _sd.received = e.loaded; if (e.lengthComputable) _sd.total = e.total; };
        xhr.onload = () => {
          if (xhr.status >= 200 && xhr.status < 300) resolve(new Uint8Array(xhr.response));
          else reject(new Error(binUrl + ': HTTP ' + xhr.status));
        };
        xhr.onerror = () => reject(new Error(binUrl + ': network error'));
        xhr.send();
      });
      _sd = { state: 'done', cue, bin };
    } catch (e) {
      _sd = { state: 'error', msg: String(e) };
    }
  })();
}
export function sdState() { return _sd.state; }
export function sdReceived() { return _sd.received || 0; }
export function sdTotal() { return _sd.total || 0; }
export function sdTakeCue() { return _sd.cue || ''; }
export function sdTakeBin() { const b = _sd.bin; _sd = { state: 'idle' }; return b; }
export function sdTakeError() { const m = _sd.msg || 'fetch failed'; _sd = { state: 'idle' }; return m; }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = sdStart)]
    fn sd_start(cue_url: &str, bin_url: &str);
    #[wasm_bindgen(js_name = sdState)]
    fn sd_state() -> String;
    #[wasm_bindgen(js_name = sdReceived)]
    fn sd_received() -> f64;
    #[wasm_bindgen(js_name = sdTotal)]
    fn sd_total() -> f64;
    #[wasm_bindgen(js_name = sdTakeCue)]
    fn sd_take_cue() -> String;
    #[wasm_bindgen(js_name = sdTakeBin)]
    fn sd_take_bin() -> Vec<u8>;
    #[wasm_bindgen(js_name = sdTakeError)]
    fn sd_take_error() -> String;
}

use std::cell::RefCell;

thread_local! {
    /// Which streamed disc the in-flight fetch belongs to, so the shell can
    /// name it in the status bar and key the save state after boot.
    static ACTIVE: RefCell<Option<&'static StreamedDisc>> = const { RefCell::new(None) };
}

/// Where the current fetch stands. `Done` and `Failed` drain the JS state;
/// everything else is a snapshot.
pub enum Status {
    /// Nothing in flight.
    Idle,
    /// Bytes on the wire.
    Running {
        /// The disc being fetched.
        disc: &'static StreamedDisc,
        /// BIN bytes received so far.
        received: u64,
        /// Content-Length when the server said one, else 0.
        total: u64,
    },
    /// Fetch complete; payload handed over exactly once.
    Done {
        /// The disc that finished.
        disc: &'static StreamedDisc,
        /// The CUE sheet text.
        cue: String,
        /// The raw BIN image.
        bin: Vec<u8>,
    },
    /// Fetch failed; message handed over exactly once.
    Failed {
        /// The disc that failed.
        disc: &'static StreamedDisc,
        /// What the browser said.
        message: String,
    },
}

/// Kick off a fetch. A second start while one runs is ignored, both here and
/// in the JS, so double-clicking the menu entry cannot race the payload.
pub fn start(disc: &'static StreamedDisc) {
    ACTIVE.with(|a| *a.borrow_mut() = Some(disc));
    sd_start(disc.cue_url, disc.bin_url);
}

/// Per-frame poll, `web_files`-style.
pub fn poll() -> Status {
    let Some(disc) = ACTIVE.with(|a| *a.borrow()) else {
        return Status::Idle;
    };
    match sd_state().as_str() {
        "running" => Status::Running {
            disc,
            received: sd_received() as u64,
            total: sd_total() as u64,
        },
        "done" => {
            ACTIVE.with(|a| *a.borrow_mut() = None);
            Status::Done {
                disc,
                cue: sd_take_cue(),
                bin: sd_take_bin(),
            }
        }
        "error" => {
            ACTIVE.with(|a| *a.borrow_mut() = None);
            Status::Failed {
                disc,
                message: sd_take_error(),
            }
        }
        _ => Status::Idle,
    }
}
