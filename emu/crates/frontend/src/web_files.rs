//! Browser file/folder access for the web build.
//!
//! The browser has no filesystem, so:
//! - the BIOS is picked as a single file ([`pick`] with [`Upload::Bios`]),
//! - games are picked by choosing a *folder* ([`pick_folder`]); the browser
//!   returns every file under it (recursively), which we scan for disc/EXE
//!   images, mirroring the native library scan. We keep the `File` handles and
//!   only read a game's bytes when it is actually launched ([`read_game`]), so a
//!   600 MB disc isn't pulled into memory until needed.
//!
//! Everything is read asynchronously with `FileReader` and handed back through
//! thread-local queues for the shell to drain each frame (the same landing-pad
//! idea as the async GPU init in `main.rs`). wasm is single-threaded, so the
//! thread-locals are sound and avoid threading an `Rc` through `AppState`.

use std::cell::{Cell, RefCell};

use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

/// Which slot a single-file upload fills.
#[derive(Clone, Copy)]
pub enum Upload {
    /// A PlayStation BIOS image.
    Bios,
    /// A game image (raw `.bin` disc, or a homebrew `.exe`).
    Game,
}

/// One game found by a folder scan: a stable id, display title, the subfolder it
/// lives in, and the `File` handle we read on launch.
struct ScannedGame {
    id: String,
    title: String,
    subtitle: String,
    file: web_sys::File,
}

thread_local! {
    /// Bytes read and waiting for the shell to apply (BIOS load / game boot).
    static PENDING: RefCell<Vec<(Upload, String, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
    /// The most recent folder scan's games (kept so launch can re-read a file).
    static SCANNED: RefCell<Vec<ScannedGame>> = const { RefCell::new(Vec::new()) };
    /// Set true the frame a folder scan finishes, so the shell rebuilds the menu.
    static SCAN_READY: Cell<bool> = const { Cell::new(false) };
}

/// Drain everything read since the last call: `(kind, filename, bytes)`.
pub fn drain() -> Vec<(Upload, String, Vec<u8>)> {
    PENDING.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// `(id, title, subtitle)` for each game found, if a folder scan just finished.
pub fn take_scanned() -> Option<Vec<(String, String, String)>> {
    if SCAN_READY.with(|r| r.replace(false)) {
        Some(SCANNED.with(|g| {
            g.borrow()
                .iter()
                .map(|s| (s.id.clone(), s.title.clone(), s.subtitle.clone()))
                .collect()
        }))
    } else {
        None
    }
}

/// Open a single-file picker (used for the BIOS). The bytes land on the pending
/// queue for [`drain`].
pub fn pick(kind: Upload) {
    let accept = match kind {
        Upload::Bios => ".bin,.rom",
        Upload::Game => ".bin,.exe",
    };
    let Some(input) = make_file_input() else {
        return;
    };
    input.set_accept(accept);
    let input_for_change = input.clone();
    let on_change = Closure::<dyn FnMut()>::new(move || {
        if let Some(file) = input_for_change.files().and_then(|f| f.get(0)) {
            read_into_pending(file, kind);
        }
    });
    input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
    on_change.forget();
    input.click();
}

/// Open a *folder* picker and scan it (recursively) for `.bin`/`.exe` games,
/// mirroring the native library scan. Populates the scan registry; the shell
/// reads it via [`take_scanned`].
pub fn pick_folder() {
    let Some(input) = make_file_input() else {
        return;
    };
    input.set_webkitdirectory(true);
    let input_for_change = input.clone();
    let on_change = Closure::<dyn FnMut()>::new(move || {
        let Some(files) = input_for_change.files() else {
            return;
        };
        let mut scanned: Vec<ScannedGame> = Vec::new();
        for i in 0..files.length() {
            let Some(file) = files.get(i) else {
                continue;
            };
            let name = file.name();
            let lower = name.to_ascii_lowercase();
            if !(lower.ends_with(".bin") || lower.ends_with(".exe")) {
                continue;
            }
            // `webkitRelativePath` isn't exposed by this web-sys File, so read
            // the reflected property directly.
            let rel = js_sys::Reflect::get(
                file.as_ref(),
                &wasm_bindgen::JsValue::from_str("webkitRelativePath"),
            )
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default();
            let path = if rel.is_empty() { name.clone() } else { rel };
            let title = name
                .rsplit_once('.')
                .map(|(stem, _)| stem.to_string())
                .unwrap_or_else(|| name.clone());
            // Subtitle = the immediate subfolder, so stacked-per-folder rips are
            // distinguishable in the list.
            let subtitle = {
                let mut parts: Vec<&str> = path.split('/').collect();
                parts.pop(); // drop the filename
                parts.pop().map(|s| s.to_string()).unwrap_or_default()
            };
            scanned.push(ScannedGame {
                id: format!("web:{path}"),
                title,
                subtitle,
                file,
            });
        }
        scanned.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        SCANNED.with(|g| *g.borrow_mut() = scanned);
        SCAN_READY.with(|r| r.set(true));
    });
    input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
    on_change.forget();
    input.click();
}

/// Read a scanned game's bytes (by id) and push them onto the pending queue as a
/// [`Upload::Game`], so the shell boots it on the next frame.
pub fn read_game(id: &str) {
    let file = SCANNED.with(|g| {
        g.borrow()
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.file.clone())
    });
    if let Some(file) = file {
        read_into_pending(file, Upload::Game);
    }
}

/// Create a hidden `<input type=file>`, or `None` if the DOM is unavailable.
fn make_file_input() -> Option<web_sys::HtmlInputElement> {
    let document = web_sys::window()?.document()?;
    let element = document.create_element("input").ok()?;
    let input: web_sys::HtmlInputElement = element.unchecked_into();
    input.set_type("file");
    Some(input)
}

/// Read `file` as an ArrayBuffer and push the bytes to the pending queue.
fn read_into_pending(file: web_sys::File, kind: Upload) {
    let name = file.name();
    let Ok(reader) = web_sys::FileReader::new() else {
        return;
    };
    let reader_for_load = reader.clone();
    let on_load = Closure::<dyn FnMut()>::new(move || {
        if let Ok(buffer) = reader_for_load.result() {
            let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
            PENDING.with(|q| q.borrow_mut().push((kind, name.clone(), bytes)));
        }
    });
    reader.set_onload(Some(on_load.as_ref().unchecked_ref()));
    on_load.forget();
    let _ = reader.read_as_array_buffer(&file);
}
