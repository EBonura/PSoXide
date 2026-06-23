//! Browser file upload for the web build.
//!
//! The browser has no filesystem, so the BIOS image and a game image are picked
//! through a transient `<input type=file>`, read asynchronously with
//! `FileReader`, and queued for the shell to apply on the next frame. `AppState`
//! has no handle to the event loop, so bytes come back through a thread-local
//! queue -- the same "landing pad" idea as the async GPU init in `main.rs`
//! (`graphics_init`). wasm is single-threaded, so a thread-local is sound and
//! avoids threading an `Rc` through `AppState`.

use std::cell::RefCell;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

/// Which slot an uploaded file fills.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Upload {
    /// A PlayStation BIOS image.
    Bios,
    /// A game image (raw `.bin` disc, or a homebrew `.exe`).
    Game,
}

thread_local! {
    static PENDING: RefCell<Vec<(Upload, String, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
}

/// Drain everything uploaded since the last call: `(kind, filename, bytes)`.
pub fn drain() -> Vec<(Upload, String, Vec<u8>)> {
    PENDING.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Open a browser file picker for `kind`. The chosen file is read asynchronously
/// and pushed onto the pending queue for the shell to drain. No-op if the DOM is
/// unavailable.
pub fn pick(kind: Upload) {
    let accept = match kind {
        Upload::Bios => ".bin,.rom",
        Upload::Game => ".bin,.exe",
    };
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Ok(element) = document.create_element("input") else {
        return;
    };
    let input: web_sys::HtmlInputElement = element.unchecked_into();
    input.set_type("file");
    input.set_accept(accept);

    // onchange -> read files[0] as an ArrayBuffer; onload -> push the bytes.
    let input_for_change = input.clone();
    let on_change = Closure::<dyn FnMut()>::new(move || {
        let Some(file) = input_for_change.files().and_then(|f| f.get(0)) else {
            return;
        };
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
        on_load.forget(); // one-shot; lives until the read completes
        let _ = reader.read_as_array_buffer(&file);
    });
    input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
    on_change.forget(); // keeps the input element + callback alive until it fires
    input.click();
}
