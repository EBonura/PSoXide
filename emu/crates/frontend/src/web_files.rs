//! Browser file/folder access for the web build.
//!
//! Two backends, chosen at runtime by [`supported`]:
//!
//! - **File System Access API** (Chrome/Edge): real-file pickers that return
//!   persistable *handles*. The BIOS + folder handles are saved in IndexedDB
//!   (locations only, never the bytes), so on a later visit they can be
//!   reconnected with one click. The async + IndexedDB logic lives in a small
//!   `inline_js` glue; Rust drives it via `spawn_local` and feeds results back
//!   through the same per-frame queues as the fallback.
//! - **`<input type=file>` fallback** (Firefox/Safari, which don't ship the
//!   real-file API): pick-each-time, no persistence.
//!
//! Either way only the file you launch is read, and nothing is uploaded: bytes
//! are read locally into wasm memory on demand. Single-threaded wasm, so the
//! thread-local queues are sound.
//!
//! Quick-save payloads are separate from file handles and use IndexedDB in
//! every supported browser. They survive tab/browser restarts and are keyed by
//! game id under the page's origin; clearing that site's data removes them.

use std::cell::{Cell, RefCell};

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{spawn_local, JsFuture};

/// Which slot a picked file fills.
#[derive(Clone, Copy)]
pub enum Upload {
    /// A PlayStation BIOS image.
    Bios,
    /// A game image (raw `.bin` disc, or a homebrew `.exe`).
    Game,
    /// A recorded input tape (browser CSV or native `.pxtape`).
    Tape,
}

/// One game in the current list. `file` is `Some` for the `<input>` backend
/// (read the handle directly) and `None` for the File System Access backend
/// (read via JS by relative path).
struct ScannedGame {
    id: String,
    title: String,
    subtitle: String,
    file: Option<web_sys::File>,
}

/// A browser file read that is ready for the frontend to apply.
pub struct LoadedFile {
    pub kind: Upload,
    pub name: String,
    /// Stable launch token for games; BIOS reads have no game id.
    pub game_id: Option<String>,
    pub bytes: Vec<u8>,
}

/// Completion from the browser quick-save store.
pub enum QuickStateEvent {
    Saved {
        game_id: String,
        created_at: u64,
        cpu_tick: u64,
        result: Result<(), String>,
    },
    Loaded {
        game_id: String,
        start_paused: bool,
        result: Result<Option<Vec<u8>>, String>,
    },
    Inspected {
        game_id: String,
        result: Result<Option<(u64, u64)>, String>,
    },
}

thread_local! {
    /// Bytes read and waiting for the shell to apply (BIOS load / game boot).
    static PENDING: RefCell<Vec<LoadedFile>> = const { RefCell::new(Vec::new()) };
    /// IndexedDB quick-save operations completed since the last frame.
    static QUICK_STATES: RefCell<Vec<QuickStateEvent>> = const { RefCell::new(Vec::new()) };
    /// The current game list (from a pick or a reconnect).
    static SCANNED: RefCell<Vec<ScannedGame>> = const { RefCell::new(Vec::new()) };
    /// Set true the frame a scan finishes, so the shell rebuilds the menu.
    static SCAN_READY: Cell<bool> = const { Cell::new(false) };
    /// True when a saved handle exists but the browser needs a user gesture to
    /// re-grant access (set async at startup); cleared once the user reconnects.
    /// When permission is still granted we auto-load instead and never set this.
    static SAVED: Cell<bool> = const { Cell::new(false) };
}

// ---- File System Access API + IndexedDB glue (Chrome/Edge) ----------------
//
// All FS-Access and IndexedDB work happens in JS (cleaner than the verbose
// web-sys unstable bindings). Each async fn returns a Promise that Rust awaits.
#[wasm_bindgen(inline_js = r#"
let _dir = null;
function _idb() {
  return new Promise((res, rej) => {
    const r = indexedDB.open('psoxide-fs', 2);
    r.onupgradeneeded = () => {
      if (!r.result.objectStoreNames.contains('handles')) {
        r.result.createObjectStore('handles');
      }
      if (!r.result.objectStoreNames.contains('savestates')) {
        r.result.createObjectStore('savestates');
      }
    };
    r.onsuccess = () => res(r.result);
    r.onerror = () => rej(r.error);
  });
}
function _put(k, v) {
  return _idb().then(db => new Promise((res, rej) => {
    const t = db.transaction('handles', 'readwrite');
    t.objectStore('handles').put(v, k);
    t.oncomplete = () => res();
    t.onerror = () => rej(t.error);
  }));
}
function _get(k) {
  return _idb().then(db => new Promise((res, rej) => {
    const t = db.transaction('handles', 'readonly');
    const q = t.objectStore('handles').get(k);
    q.onsuccess = () => res(q.result);
    q.onerror = () => rej(q.error);
  }));
}
function _putState(k, v) {
  return _idb().then(db => new Promise((res, rej) => {
    const t = db.transaction('savestates', 'readwrite');
    t.objectStore('savestates').put(v, k);
    t.oncomplete = () => res();
    t.onerror = () => rej(t.error);
  }));
}
function _getState(k) {
  return _idb().then(db => new Promise((res, rej) => {
    const t = db.transaction('savestates', 'readonly');
    const q = t.objectStore('savestates').get(k);
    q.onsuccess = () => res(q.result);
    q.onerror = () => rej(q.error);
  }));
}
async function _list(dh) {
  const out = [];
  async function walk(h, pfx) {
    for await (const [name, ch] of h.entries()) {
      if (ch.kind === 'file') {
        const l = name.toLowerCase();
        if (l.endsWith('.bin') || l.endsWith('.exe')) out.push(pfx + name);
      } else if (ch.kind === 'directory') {
        await walk(ch, pfx + name + '/');
      }
    }
  }
  await walk(dh, '');
  return out.join('\n');
}
export function fsaSupported() {
  return ('showOpenFilePicker' in window) && ('showDirectoryPicker' in window);
}
export async function pickBios() {
  const [h] = await window.showOpenFilePicker();
  await _put('bios', h);
  const f = await h.getFile();
  return new Uint8Array(await f.arrayBuffer());
}
export async function pickFolder() {
  const dh = await window.showDirectoryPicker();
  await _put('folder', dh);
  _dir = dh;
  return await _list(dh);
}
// Gesture-free restore: only succeeds when the browser still reports the saved
// handle's permission as 'granted' (persistent permissions / installed PWA).
// Returns the data when granted, `false` when a handle exists but a user gesture
// is needed, `undefined` when there is no saved handle.
export async function autoBios() {
  const h = await _get('bios');
  if (!h) return undefined;
  try {
    if ((await h.queryPermission({ mode: 'read' })) === 'granted') {
      const f = await h.getFile();
      return new Uint8Array(await f.arrayBuffer());
    }
  } catch (e) {}
  return false;
}
export async function autoFolder() {
  const dh = await _get('folder');
  if (!dh) return undefined;
  try {
    if ((await dh.queryPermission({ mode: 'read' })) === 'granted') {
      _dir = dh;
      return await _list(dh);
    }
  } catch (e) {}
  return false;
}
export async function reconnectBios() {
  const h = await _get('bios');
  if (!h) return null;
  if ((await h.requestPermission({ mode: 'read' })) !== 'granted') return null;
  const f = await h.getFile();
  return new Uint8Array(await f.arrayBuffer());
}
export async function reconnectFolder() {
  const dh = await _get('folder');
  if (!dh) return null;
  if ((await dh.requestPermission({ mode: 'read' })) !== 'granted') return null;
  _dir = dh;
  return await _list(dh);
}
export async function readGame(path) {
  let dh = _dir;
  if (!dh) {
    dh = await _get('folder');
    if (dh) { await dh.requestPermission({ mode: 'read' }); _dir = dh; }
  }
  if (!dh) return null;
  const parts = path.split('/');
  let h = dh;
  for (let i = 0; i < parts.length - 1; i++) h = await h.getDirectoryHandle(parts[i]);
  const fh = await h.getFileHandle(parts[parts.length - 1]);
  const f = await fh.getFile();
  return new Uint8Array(await f.arrayBuffer());
}
export async function saveQuickState(gameId, bytes, createdAt, cpuTick) {
  // Best effort: supporting browsers can exempt this origin from automatic
  // storage-pressure eviction. A denial does not prevent the normal save.
  try { if (navigator.storage?.persist) await navigator.storage.persist(); } catch (e) {}
  await _putState('quick:' + gameId, { bytes, createdAt, cpuTick });
}
export async function loadQuickState(gameId) {
  const state = await _getState('quick:' + gameId);
  return state ? state.bytes : undefined;
}
export async function inspectQuickState(gameId) {
  const state = await _getState('quick:' + gameId);
  return state ? state.createdAt + '\n' + state.cpuTick : undefined;
}
export function downloadCsv(filenameStem, csv) {
  const safeStem = String(filenameStem || 'psoxide-input')
    .replace(/[^a-zA-Z0-9._-]+/g, '-')
    .replace(/^-+|-+$/g, '') || 'psoxide-input';
  const stamp = new Date().toISOString()
    .replace(/\.\d{3}Z$/, 'Z')
    .replace(/[:]/g, '-');
  const blob = new Blob([csv], { type: 'text/csv;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = `${safeStem}-${stamp}.csv`;
  anchor.style.display = 'none';
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = fsaSupported)]
    fn fsa_supported() -> bool;
    #[wasm_bindgen(js_name = pickBios)]
    fn fsa_pick_bios() -> js_sys::Promise;
    #[wasm_bindgen(js_name = pickFolder)]
    fn fsa_pick_folder() -> js_sys::Promise;
    #[wasm_bindgen(js_name = autoBios)]
    fn fsa_auto_bios() -> js_sys::Promise;
    #[wasm_bindgen(js_name = autoFolder)]
    fn fsa_auto_folder() -> js_sys::Promise;
    #[wasm_bindgen(js_name = reconnectBios)]
    fn fsa_reconnect_bios() -> js_sys::Promise;
    #[wasm_bindgen(js_name = reconnectFolder)]
    fn fsa_reconnect_folder() -> js_sys::Promise;
    #[wasm_bindgen(js_name = readGame)]
    fn fsa_read_game(path: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = saveQuickState)]
    fn idb_save_quick_state(
        game_id: &str,
        bytes: &js_sys::Uint8Array,
        created_at: &str,
        cpu_tick: &str,
    ) -> js_sys::Promise;
    #[wasm_bindgen(js_name = loadQuickState)]
    fn idb_load_quick_state(game_id: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name = inspectQuickState)]
    fn idb_inspect_quick_state(game_id: &str) -> js_sys::Promise;
    #[wasm_bindgen(catch, js_name = downloadCsv)]
    fn download_csv(filename_stem: &str, csv: &str) -> Result<(), JsValue>;
}

/// Download a UTF-8 input recording as a timestamped CSV file.
pub fn download_input_csv(filename_stem: &str, csv: &str) -> Result<(), String> {
    download_csv(filename_stem, csv).map_err(|error| {
        error
            .as_string()
            .unwrap_or_else(|| "browser rejected the CSV download".to_string())
    })
}

/// Whether the persistent (File System Access) backend is available.
pub fn supported() -> bool {
    fsa_supported()
}

/// True while a saved BIOS/folder handle is waiting to be reconnected.
pub fn saved_available() -> bool {
    SAVED.with(|s| s.get())
}

/// At startup, try to silently restore the saved BIOS + games folder. The File
/// System Access API only lets us re-read a stored handle without a user gesture
/// when the browser still reports permission as `granted` (persistent
/// permissions / installed PWA) -- that path loads everything automatically.
/// After a normal reload the grant resets to `prompt`, so re-reading needs a
/// click; we flag `SAVED` and the shell offers Reconnect. Call once at startup.
pub fn check_saved() {
    if !supported() {
        return;
    }
    spawn_local(async {
        let mut needs_gesture = false;

        if let Ok(v) = JsFuture::from(fsa_auto_bios()).await {
            if v.as_bool() == Some(false) {
                // Handle exists, but the browser needs a gesture to re-grant.
                needs_gesture = true;
            } else if !v.is_undefined() && !v.is_null() {
                // Permission still granted: the bytes came back, load them now.
                push_bytes(Upload::Bios, "BIOS".to_string(), None, &v);
            }
        }

        if let Ok(v) = JsFuture::from(fsa_auto_folder()).await {
            if v.as_bool() == Some(false) {
                needs_gesture = true;
            } else if let Some(list) = v.as_string() {
                set_scanned_from_paths(&list);
            }
        }

        if needs_gesture {
            SAVED.with(|s| s.set(true));
        }
    });
}

/// Drain everything read since the last call: `(kind, filename, bytes)`.
pub fn drain() -> Vec<LoadedFile> {
    PENDING.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Drain completed IndexedDB quick-save operations.
pub fn drain_quick_states() -> Vec<QuickStateEvent> {
    QUICK_STATES.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// `(id, title, subtitle)` for each game found, if a scan just finished.
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

/// Pick a BIOS: persistent picker if supported, else a one-shot `<input>`.
pub fn pick_bios() {
    if supported() {
        spawn_local(async {
            if let Ok(v) = JsFuture::from(fsa_pick_bios()).await {
                push_bytes(Upload::Bios, "BIOS".to_string(), None, &v);
            }
        });
    } else {
        pick_input(Upload::Bios);
    }
}

/// Pick a games source: a folder via the persistent picker if supported, else a
/// one-shot `<input webkitdirectory>`.
pub fn pick_games() {
    if supported() {
        spawn_local(async {
            if let Ok(v) = JsFuture::from(fsa_pick_folder()).await {
                if let Some(list) = v.as_string() {
                    set_scanned_from_paths(&list);
                }
            }
        });
    } else {
        pick_folder_input();
    }
}

/// Pick a recorded input tape. Always the one-shot `<input>` picker: a tape
/// is read once and never needs a persistent handle.
pub fn pick_tape() {
    pick_input(Upload::Tape);
}

/// Reconnect previously-saved handles (File System Access only). No-op
/// otherwise. Re-grants permission then re-reads the BIOS + relists the folder.
pub fn reconnect() {
    SAVED.with(|s| s.set(false));
    if !supported() {
        return;
    }
    spawn_local(async {
        if let Ok(v) = JsFuture::from(fsa_reconnect_bios()).await {
            if !v.is_null() && !v.is_undefined() {
                push_bytes(Upload::Bios, "BIOS".to_string(), None, &v);
            }
        }
    });
    spawn_local(async {
        if let Ok(v) = JsFuture::from(fsa_reconnect_folder()).await {
            if let Some(list) = v.as_string() {
                set_scanned_from_paths(&list);
            }
        }
    });
}

/// Read one game's bytes by id (dispatches `<input>` vs File System Access).
pub fn read_game(id: &str) {
    // `<input>` backend keeps the File handle in the scan list.
    let file = SCANNED.with(|g| {
        g.borrow()
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.file.clone())
    });
    if let Some(file) = file {
        read_into_pending(file, Upload::Game, Some(id.to_string()));
        return;
    }
    // File System Access backend: id == "web:<relative/path>".
    if let Some(path) = id.strip_prefix("web:") {
        let path = path.to_string();
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        let game_id = id.to_string();
        spawn_local(async move {
            if let Ok(v) = JsFuture::from(fsa_read_game(&path)).await {
                if !v.is_null() && !v.is_undefined() {
                    push_bytes(Upload::Game, name, Some(game_id), &v);
                }
            }
        });
    }
}

/// Persist the current game's quick-save in origin-scoped IndexedDB.
pub fn save_quick_state(game_id: String, bytes: Vec<u8>, created_at: u64, cpu_tick: u64) {
    let array = js_sys::Uint8Array::from(bytes.as_slice());
    let promise = idb_save_quick_state(
        &game_id,
        &array,
        &created_at.to_string(),
        &cpu_tick.to_string(),
    );
    spawn_local(async move {
        let result = JsFuture::from(promise).await.map(|_| ()).map_err(js_error);
        QUICK_STATES.with(|q| {
            q.borrow_mut().push(QuickStateEvent::Saved {
                game_id,
                created_at,
                cpu_tick,
                result,
            });
        });
    });
}

/// Load the current game's quick-save bytes from IndexedDB.
pub fn load_quick_state(game_id: String, start_paused: bool) {
    let promise = idb_load_quick_state(&game_id);
    spawn_local(async move {
        let result = JsFuture::from(promise)
            .await
            .map_err(js_error)
            .map(|value| {
                if value.is_null() || value.is_undefined() {
                    None
                } else {
                    Some(js_sys::Uint8Array::new(&value).to_vec())
                }
            });
        QUICK_STATES.with(|q| {
            q.borrow_mut().push(QuickStateEvent::Loaded {
                game_id,
                start_paused,
                result,
            });
        });
    });
}

/// Read just the quick-save metadata so the save-state panel can show a row
/// after a page reload without decoding the multi-megabyte payload.
pub fn inspect_quick_state(game_id: String) {
    let promise = idb_inspect_quick_state(&game_id);
    spawn_local(async move {
        let result = JsFuture::from(promise)
            .await
            .map_err(js_error)
            .and_then(|value| {
                if value.is_null() || value.is_undefined() {
                    return Ok(None);
                }
                let Some(metadata) = value.as_string() else {
                    return Err("browser save metadata has an invalid type".to_string());
                };
                let Some((created_at, cpu_tick)) = metadata.split_once('\n') else {
                    return Err("browser save metadata is malformed".to_string());
                };
                let created_at = created_at
                    .parse::<u64>()
                    .map_err(|_| "browser save creation time is invalid".to_string())?;
                let cpu_tick = cpu_tick
                    .parse::<u64>()
                    .map_err(|_| "browser save CPU tick is invalid".to_string())?;
                Ok(Some((created_at, cpu_tick)))
            });
        QUICK_STATES.with(|q| {
            q.borrow_mut()
                .push(QuickStateEvent::Inspected { game_id, result });
        });
    });
}

// ---- shared helpers -------------------------------------------------------

fn push_bytes(kind: Upload, name: String, game_id: Option<String>, val: &JsValue) {
    let bytes = js_sys::Uint8Array::new(val).to_vec();
    PENDING.with(|q| {
        q.borrow_mut().push(LoadedFile {
            kind,
            name,
            game_id,
            bytes,
        });
    });
}

fn js_error(value: JsValue) -> String {
    value.as_string().unwrap_or_else(|| format!("{value:?}"))
}

/// Build the scan list from newline-separated relative paths (FS Access path).
fn set_scanned_from_paths(list: &str) {
    let mut scanned: Vec<ScannedGame> = list
        .lines()
        .filter(|l| !l.is_empty())
        .map(|path| ScannedGame {
            id: format!("web:{path}"),
            title: title_of(path),
            subtitle: subfolder_of(path),
            file: None,
        })
        .collect();
    scanned.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    SCANNED.with(|g| *g.borrow_mut() = scanned);
    SCAN_READY.with(|r| r.set(true));
}

/// Filename without extension.
fn title_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(name)
        .to_string()
}

/// Immediate parent folder of a relative path (empty if top-level).
fn subfolder_of(path: &str) -> String {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop(); // drop filename
    parts.pop().map(|s| s.to_string()).unwrap_or_default()
}

// ---- `<input type=file>` fallback (Firefox/Safari) ------------------------

fn pick_input(kind: Upload) {
    let accept = match kind {
        Upload::Bios => ".bin,.rom",
        Upload::Game => ".bin,.exe",
        Upload::Tape => ".csv,.pxtape",
    };
    let Some(input) = make_file_input() else {
        return;
    };
    input.set_accept(accept);
    let input_for_change = input.clone();
    let on_change = Closure::<dyn FnMut()>::new(move || {
        if let Some(file) = input_for_change.files().and_then(|f| f.get(0)) {
            read_into_pending(file, kind, None);
        }
    });
    input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
    on_change.forget();
    input.click();
}

fn pick_folder_input() {
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
            let rel = js_sys::Reflect::get(file.as_ref(), &JsValue::from_str("webkitRelativePath"))
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            let path = if rel.is_empty() { name.clone() } else { rel };
            scanned.push(ScannedGame {
                id: format!("web:{path}"),
                title: title_of(&path),
                subtitle: subfolder_of(&path),
                file: Some(file),
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

fn make_file_input() -> Option<web_sys::HtmlInputElement> {
    let document = web_sys::window()?.document()?;
    let element = document.create_element("input").ok()?;
    let input: web_sys::HtmlInputElement = element.unchecked_into();
    input.set_type("file");
    Some(input)
}

fn read_into_pending(file: web_sys::File, kind: Upload, game_id: Option<String>) {
    let name = file.name();
    let Ok(reader) = web_sys::FileReader::new() else {
        return;
    };
    let reader_for_load = reader.clone();
    let on_load = Closure::<dyn FnMut()>::new(move || {
        if let Ok(buffer) = reader_for_load.result() {
            let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
            PENDING.with(|q| {
                q.borrow_mut().push(LoadedFile {
                    kind,
                    name: name.clone(),
                    game_id: game_id.clone(),
                    bytes,
                });
            });
        }
    });
    reader.set_onload(Some(on_load.as_ref().unchecked_ref()));
    on_load.forget();
    let _ = reader.read_as_array_buffer(&file);
}
