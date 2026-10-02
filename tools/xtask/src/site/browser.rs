//! The local review tools: headless Chrome against a locally built site.
//!
//! * `browser-check`: responsive layout, images, FAQ and theme persistence.
//! * `review-shots`: full-page screenshots plus a contact sheet in `review/`.
//! * `usability`: clicks and scrolls from the home page to each answer in
//!   `scripts/usability_routes.toml`, as a markdown table.
//!
//! Chrome runs headless with a throwaway profile over
//! `--remote-debugging-pipe`, so no window opens. That gives real device
//! emulation (a 390 px phone viewport, which a plain `--window-size` can't
//! do because headless Chrome keeps a minimum window width), colour-scheme
//! emulation, full-page screenshots and in-page measurement. The site is
//! built for, and served from, 127.0.0.1 only. `CHROME` overrides the
//! browser path.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::check::unquote;
use super::{read_text, run, site_root, write, Args};
use crate::obj;
use crate::pyjson::{dumps, Json};

const CHROME: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

/// A headless Chrome driven over the DevTools pipe.
struct Chrome {
    child: Child,
    input: ChildStdin,
    replies: Receiver<Value>,
    next_id: u64,
    events: Vec<Value>,
    session: String,
}

impl Chrome {
    fn launch(profile: &Path) -> Result<Self, String> {
        let chrome = std::env::var("CHROME").unwrap_or_else(|_| CHROME.to_string());
        // Chrome reads commands on fd 3 and writes replies on fd 4. The
        // shell moves this end's pipes there before it becomes Chrome.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"exec "$0" "$@" 3<&0 4>&1 0</dev/null 1>/dev/null 2>/dev/null"#)
            .arg(chrome)
            .args([
                "--headless=new",
                "--remote-debugging-pipe",
                "--disable-gpu",
                "--hide-scrollbars",
                "--no-first-run",
                "--no-default-browser-check",
                "--use-mock-keychain",
                "--password-store=basic",
                "--mute-audio",
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("about:blank")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("cannot start Chrome: {e}"))?;
        let input = child.stdin.take().expect("piped stdin");
        let mut output = child.stdout.take().expect("piped stdout");
        let (send, replies) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let mut chunk = vec![0u8; 1 << 20];
            loop {
                let Ok(n) = output.read(&mut chunk) else {
                    return;
                };
                if n == 0 {
                    return;
                }
                buffer.extend_from_slice(&chunk[..n]);
                while let Some(end) = buffer.iter().position(|&b| b == 0) {
                    let message: Vec<u8> = buffer.drain(..=end).collect();
                    if let Ok(value) = serde_json::from_slice(&message[..end]) {
                        if send.send(value).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let mut chrome = Self {
            child,
            input,
            replies,
            next_id: 0,
            events: Vec::new(),
            session: String::new(),
        };
        let target = chrome.call("Target.createTarget", json!({"url": "about:blank"}), false)?;
        let target = target["targetId"].as_str().unwrap_or_default().to_string();
        let attached = chrome.call(
            "Target.attachToTarget",
            json!({"targetId": target, "flatten": true}),
            false,
        )?;
        chrome.session = attached["sessionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        chrome.call("Page.enable", json!({}), true)?;
        chrome.call("Runtime.enable", json!({}), true)?;
        Ok(chrome)
    }

    fn receive(&mut self, timeout: Duration) -> Result<Value, String> {
        self.replies
            .recv_timeout(timeout)
            .map_err(|_| "no reply from Chrome".to_string())
    }

    fn call(&mut self, method: &str, params: Value, session: bool) -> Result<Value, String> {
        self.next_id += 1;
        let mut message = json!({"id": self.next_id, "method": method, "params": params});
        if session {
            message["sessionId"] = json!(self.session);
        }
        let mut bytes = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
        bytes.push(0);
        self.input
            .write_all(&bytes)
            .map_err(|e| format!("Chrome closed the pipe: {e}"))?;
        loop {
            let reply = self.receive(Duration::from_secs(60))?;
            if reply["id"].as_u64() == Some(self.next_id) {
                if let Some(error) = reply.get("error") {
                    return Err(format!("{method}: {error}"));
                }
                return Ok(reply.get("result").cloned().unwrap_or(json!({})));
            }
            self.events.push(reply);
        }
    }

    fn wait_event(&mut self, name: &str, timeout: Duration) -> Result<Value, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(i) = self.events.iter().position(|e| e["method"] == name) {
                return Ok(self.events.remove(i));
            }
            let left = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| format!("no {name}"))?;
            let event = self.receive(left)?;
            self.events.push(event);
        }
    }

    fn open(
        &mut self,
        url: &str,
        width: u32,
        height: u32,
        scale: u32,
        mobile: bool,
        scheme: &str,
    ) -> Result<(), String> {
        self.call(
            "Emulation.setDeviceMetricsOverride",
            json!({"width": width, "height": height, "deviceScaleFactor": scale, "mobile": mobile}),
            true,
        )?;
        self.call(
            "Emulation.setEmulatedMedia",
            json!({"features": [
                {"name": "prefers-color-scheme", "value": scheme},
                {"name": "prefers-reduced-motion", "value": "reduce"}]}),
            true,
        )?;
        self.events.clear();
        self.call("Page.navigate", json!({"url": url}), true)?;
        self.wait_event("Page.loadEventFired", Duration::from_secs(60))?;
        // Let lazy images and web fonts settle.
        self.evaluate("document.fonts.ready.then(() => true)")?;
        std::thread::sleep(Duration::from_millis(300));
        Ok(())
    }

    fn evaluate(&mut self, expression: &str) -> Result<Value, String> {
        let result = self.call(
            "Runtime.evaluate",
            json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
            true,
        )?;
        if let Some(details) = result.get("exceptionDetails") {
            return Err(format!("page script failed: {details}"));
        }
        Ok(result["result"]
            .get("value")
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn screenshot_full(&mut self, path: &Path) -> Result<(), String> {
        // Load every lazy image first, then capture the whole page.
        self.evaluate(
            "document.querySelectorAll('img[loading=lazy]').forEach(i => i.loading = 'eager');\
             Promise.all([...document.images].map(i => i.complete ? 0 : new Promise(r => { i.onload = i.onerror = r; })))",
        )?;
        let metrics = self.call("Page.getLayoutMetrics", json!({}), true)?;
        let size = &metrics["cssContentSize"];
        let shot = self.call(
            "Page.captureScreenshot",
            json!({"format": "png", "captureBeyondViewport": true,
                   "clip": {"x": 0, "y": 0, "width": size["width"], "height": size["height"], "scale": 1}}),
            true,
        )?;
        write(
            path,
            base64_decode(shot["data"].as_str().unwrap_or_default())?,
        )
    }
}

impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", "--", &format!("-{}", self.child.id())])
            .status();
        let _ = self.child.wait();
    }
}

fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0);
    for c in text
        .bytes()
        .filter(|&c| c != b'=' && !c.is_ascii_whitespace())
    {
        bits = (bits << 6) | u32::from(value(c).ok_or("bad base64 from Chrome")?);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Ok(out)
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/vnd.microsoft.icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain",
        "xml" => "application/xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

/// An HTTP date (RFC 9110 IMF-fixdate) for a time since the epoch.
fn http_date(time: std::time::SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {day:02} {} {year} {:02}:{:02}:{:02} GMT",
        WEEKDAYS[(days % 7) as usize],
        MONTHS[(month - 1) as usize],
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

fn respond(mut stream: TcpStream, root: &Path) {
    let mut line = String::new();
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    if reader.read_line(&mut line).is_err() {
        return;
    }
    // Drain the headers.
    let mut header = String::new();
    while reader.read_line(&mut header).is_ok_and(|n| n > 2) {
        header.clear();
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let path = unquote(target.split(['?', '#']).next().unwrap_or("/"));
    let relative = path.trim_start_matches('/');
    let mut file = root.join(relative);
    if relative.split('/').any(|p| p == "..") {
        let _ = stream.write_all(b"HTTP/1.0 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
        return;
    }
    if file.is_dir() {
        if !path.ends_with('/') {
            let reply = format!(
                "HTTP/1.0 301 Moved Permanently\r\nLocation: {path}/\r\nContent-Length: 0\r\n\r\n"
            );
            let _ = stream.write_all(reply.as_bytes());
            return;
        }
        file = file.join("index.html");
    }
    match fs::read(&file) {
        Ok(body) => {
            // Last-Modified, as Python's http.server sends it, lets Chrome
            // reuse images across page loads the way the review expects.
            let modified = fs::metadata(&file)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let head = format!(
                "HTTP/1.0 200 OK\r\nDate: {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nLast-Modified: {}\r\n\r\n",
                http_date(std::time::SystemTime::now()),
                content_type(&file),
                body.len(),
                http_date(modified)
            );
            let _ = stream.write_all(head.as_bytes());
            if method != "HEAD" {
                let _ = stream.write_all(&body);
            }
        }
        Err(_) => {
            let _ = stream.write_all(b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        }
    }
}

/// Serve `root` on 127.0.0.1 from a background thread; returns the port.
fn serve(root: PathBuf) -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let root = root.clone();
            std::thread::spawn(move || respond(stream, &root));
        }
    });
    Ok(port)
}

/// A scratch directory removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("psoxide-site-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("public")).map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Build the site from `source` for `http://127.0.0.1:PORT` and serve it.
fn build_and_serve(
    scratch: &Scratch,
    source: &Path,
    zola: &str,
    quiet: bool,
) -> Result<String, String> {
    let public = scratch.0.join("public");
    let port = serve(public.clone())?;
    let base = format!("http://127.0.0.1:{port}");
    let mut build = Command::new(zola);
    build
        .current_dir(source)
        .args(["build", "--force", "--base-url", &base, "--output-dir"])
        .arg(&public);
    if quiet {
        build.stdout(Stdio::null()).stderr(Stdio::null());
    }
    run(&mut build)?;
    Ok(base)
}

const PAGES: [(&str, &str); 7] = [
    ("home", "/"),
    ("compare", "/emulator/compare/"),
    ("projects", "/projects/"),
    ("walkthrough", "/docs/first-ps1-program/"),
    ("ethos", "/ethos/"),
    ("faq", "/faq/"),
    ("docs", "/docs/"),
];

fn require(condition: bool, what: impl std::fmt::Display) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(format!("check failed: {what}"))
    }
}

/// `site browser-check [--zola PATH]`.
pub fn browser_check(args: &[String]) -> Result<(), String> {
    let args = Args::parse(args, &[])?;
    let zola = args.get("zola").unwrap_or("zola");
    let site = site_root();
    let scratch = Scratch::new("browser-check")?;
    let base = build_and_serve(&scratch, &site, zola, false)?;
    let mut browser = Chrome::launch(&scratch.0.join("profile"))?;
    browser.call("Network.enable", json!({}), true)?;
    let mut results = Vec::new();
    for width in [360u32, 390, 768, 1440] {
        for scheme in ["light", "dark"] {
            for (name, route) in PAGES {
                browser.open(
                    &format!("{base}{route}"),
                    width,
                    900,
                    1,
                    width < 720,
                    scheme,
                )?;
                browser.evaluate(
                    "document.querySelectorAll('img').forEach(i => i.loading='eager'); \
                     Promise.all([...document.images].map(i => i.decode().catch(() => null)))",
                )?;
                let state = browser.evaluate(
                    "({overflow: document.documentElement.scrollWidth > innerWidth + 1,\
                       images: [...document.images].filter(i => !i.complete || !i.naturalWidth).map(i => i.src),\
                       h1: document.querySelectorAll('h1').length,\
                       faqOpen: document.querySelectorAll('#faq details[open]').length})",
                )?;
                let at = format!("{name} at {width} px, {scheme}");
                require(
                    state["overflow"] != true,
                    format!("{at}: horizontal overflow"),
                )?;
                require(
                    state["images"].as_array().is_none_or(Vec::is_empty),
                    format!("{at}: images did not load: {}", state["images"]),
                )?;
                require(state["h1"] == 1, format!("{at}: needs exactly one h1"))?;
                if name == "home" && width < 720 {
                    require(
                        state["faqOpen"] == 0,
                        "phone quick answers should start collapsed",
                    )?;
                }
                let failures: Vec<&Value> = browser
                    .events
                    .iter()
                    .filter(|e| {
                        e["method"] == "Runtime.exceptionThrown"
                            || (e["method"] == "Network.responseReceived"
                                && e["params"]["response"]["status"].as_f64().unwrap_or(0.0)
                                    >= 400.0)
                    })
                    .collect();
                require(failures.is_empty(), format!("{at}: {failures:?}"))?;
                results.push(obj! {"page" => name, "width" => i64::from(width), "theme" => scheme, "passed" => true});
            }
        }
    }
    browser.open(&format!("{base}/faq/#videos"), 390, 844, 1, true, "dark")?;
    require(
        browser.evaluate("document.getElementById('videos').open")? == true,
        "FAQ deep link did not open",
    )?;
    browser.evaluate("document.querySelector('#videos summary').click()")?;
    require(
        browser.evaluate("document.getElementById('videos').open")? != true,
        "FAQ did not close",
    )?;
    browser.evaluate("document.querySelector('.theme-toggle').click()")?;
    let theme = browser.evaluate("document.documentElement.dataset.theme")?;
    browser.open(&format!("{base}/projects/"), 390, 844, 1, true, "dark")?;
    require(
        browser.evaluate("document.documentElement.dataset.theme")? == theme,
        "theme did not persist",
    )?;
    browser.open(&format!("{base}/emulator/"), 1440, 900, 1, false, "dark")?;
    require(
        browser.evaluate(
            "location.pathname === '/emulator/' && document.querySelectorAll('h1').length === 1",
        )? == true,
        "emulator page",
    )?;
    drop(browser);
    let count = results.len();
    fs::create_dir_all(site.join("review")).map_err(|e| e.to_string())?;
    write(
        &site.join("review/browser-check.json"),
        dumps(&Json::Arr(results), Some(2), true) + "\n",
    )?;
    println!("{count} page/viewport/theme checks passed; FAQ, theme persistence and emulator page passed");
    Ok(())
}

/// Width and height of a PNG, from its IHDR chunk.
fn png_size(path: &Path) -> Result<(u32, u32), String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return Err(format!("{} is not a PNG", path.display()));
    }
    let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
    Ok((word(16), word(20)))
}

/// `site review-shots [--zola PATH]`: desktop (1440 px) and phone (390 px,
/// emulated as a mobile device) full-page screenshots, each in dark and
/// light, plus `contact-sheet.png`: a row per page, the top of each view.
pub fn review_shots(args: &[String]) -> Result<(), String> {
    const VIEWS: [(&str, u32, u32, &str); 4] = [
        ("desktop-dark", 1440, 1, "dark"),
        ("desktop-light", 1440, 1, "light"),
        ("phone-dark", 390, 2, "dark"),
        ("phone-light", 390, 2, "light"),
    ];
    let args = Args::parse(args, &[])?;
    let zola = args.get("zola").unwrap_or("zola");
    let site = site_root();
    let out = site.join("review");
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let scratch = Scratch::new("review-shots")?;
    let base = build_and_serve(&scratch, &site, zola, true)?;
    let mut chrome = Chrome::launch(&scratch.0.join("profile"))?;
    for (view, width, scale, scheme) in VIEWS {
        let phone = view.starts_with("phone");
        for (name, path) in PAGES {
            let target = out.join(format!("{name}-{view}.png"));
            chrome.open(
                &format!("{base}{path}"),
                width,
                if phone { 844 } else { 900 },
                scale,
                phone,
                scheme,
            )?;
            chrome.screenshot_full(&target)?;
            println!("wrote review/{name}-{view}.png");
        }
    }

    // The contact sheet is laid out as a page and captured by the same
    // browser: tiles keep each page's top (1500 desktop / 1600 phone CSS px)
    // at 360 / 150 px wide, with a label above each.
    let (pad, label_h) = (14u32, 22u32);
    let mut rows = String::new();
    for (name, _) in PAGES {
        rows.push_str("<div class=row>");
        for (view, _, scale, _) in VIEWS {
            let phone = view.starts_with("phone");
            let file = format!("{name}-{view}.png");
            let (w, h) = png_size(&out.join(&file))?;
            let column = if phone { 150 } else { 360 };
            let crop = h.min(if phone { 1600 } else { 1500 } * scale);
            let tile_h = (f64::from(crop) * f64::from(column) / f64::from(w)).round() as u32;
            rows.push_str(&format!(
                "<div class=tile style=\"width:{column}px\"><div class=label>{name}&nbsp;&nbsp;{view}</div>\
                 <div style=\"width:{column}px;height:{tile_h}px;overflow:hidden\">\
                 <img src=\"{file}\" style=\"width:{column}px;display:block\"></div></div>"
            ));
        }
        rows.push_str("</div>");
    }
    let sheet = out.join("contact-sheet.html");
    write(
        &sheet,
        format!(
            "<!doctype html><meta charset=utf-8><style>\
             body{{margin:0;padding:{pad}px 0 0 {pad}px;background:rgb(24,28,33);width:max-content}}\
             .row{{display:flex;gap:{pad}px;align-items:flex-start;margin-bottom:{pad}px;padding-right:{pad}px}}\
             .label{{height:{label_h}px;white-space:nowrap;font:11px/18px monospace;color:rgb(200,210,220);padding-top:4px;box-sizing:border-box}}\
             </style>{rows}"
        ),
    )?;
    let url = format!("file://{}", sheet.display()).replace(' ', "%20");
    // One column per view plus the gaps: the sheet's own width.
    let sheet_w = VIEWS
        .iter()
        .map(|(view, ..)| if view.starts_with("phone") { 150 } else { 360 })
        .sum::<u32>()
        + pad * (VIEWS.len() as u32 + 1);
    chrome.open(&url, sheet_w, 900, 1, false, "dark")?;
    chrome.screenshot_full(&out.join("contact-sheet.png"))?;
    drop(chrome);
    let _ = fs::remove_file(&sheet);
    println!("wrote review/contact-sheet.png");
    Ok(())
}

const VISIBLE_PX: f64 = 80.0;

fn scrolls(top: f64, view_top: f64, viewport: f64) -> i64 {
    let need = top - view_top + VISIBLE_PX - viewport;
    if need <= 0.0 {
        0
    } else {
        (need / (0.9 * viewport)).ceil() as i64
    }
}

type Layouts = BTreeMap<(String, &'static str), Value>;

/// Clicks and scrolls along one route, or `None` when a step is missing.
fn route_cost(
    route: &[(String, String)],
    layouts: &Layouts,
    view: &'static str,
) -> Option<(i64, i64)> {
    let (mut clicks, mut scr) = (0, 0);
    let mut view_top = 0.0;
    for (i, (page, sel)) in route.iter().enumerate() {
        let page = if page == "=" { &route[i - 1].0 } else { page };
        let info = &layouts[&(page.split('#').next().unwrap_or("").to_string(), view)];
        let mut el = info["els"].get(sel).filter(|v| !v.is_null());
        if el.is_none() && route[i].0 == "=" && i > 0 {
            // Revealed by the click (a collapsed answer): it opens right below.
            el = info["els"].get(&route[i - 1].1).filter(|v| !v.is_null());
        }
        let el = el?;
        if el["sticky"] != true {
            let viewport = info["vh"].as_f64().unwrap_or(0.0);
            let top = el["top"].as_f64().unwrap_or(0.0);
            let n = scrolls(top, view_top, viewport);
            scr += n;
            if n != 0 {
                view_top = top + VISIBLE_PX - viewport;
            }
        }
        if i == route.len() - 1 {
            break;
        }
        clicks += 1;
        let next_page = &route[i + 1].0;
        if next_page == "=" {
            continue;
        }
        view_top = 0.0;
        if let Some((next_base, fragment)) = next_page.split_once('#') {
            let next = &layouts[&(next_base.to_string(), view)];
            if let Some(target) = next["els"]
                .get(format!("#{fragment}"))
                .filter(|v| !v.is_null())
            {
                let header = next["header"].as_f64().unwrap_or(0.0);
                view_top = (target["top"].as_f64().unwrap_or(0.0) - header).max(0.0);
            }
        }
    }
    Some((clicks, scr))
}

/// `site usability --set SET [--site DIR] [--zola PATH] [--json FILE]`.
pub fn usability(args: &[String]) -> Result<(), String> {
    const VIEWPORTS: [(&str, u32, u32); 2] = [("desktop", 1440, 900), ("phone", 390, 844)];
    let args = Args::parse(args, &[])?;
    let set = args.need("set")?;
    let source = args.get("site").map_or_else(site_root, PathBuf::from);
    let zola = args.get("zola").unwrap_or("zola");
    let spec: toml::Table = read_text(&site_root().join("scripts/usability_routes.toml"))?
        .parse()
        .map_err(|e| format!("usability_routes.toml: {e}"))?;
    let questions = spec
        .get("q")
        .and_then(toml::Value::as_array)
        .ok_or("no [[q]] in routes")?;
    let routes_of = |q: &toml::Value| -> Vec<Vec<(String, String)>> {
        q.get(set)
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_array)
            .map(|route| {
                route
                    .iter()
                    .filter_map(toml::Value::as_array)
                    .map(|step| {
                        let at = |k: usize| {
                            step.get(k)
                                .and_then(toml::Value::as_str)
                                .unwrap_or("")
                                .to_string()
                        };
                        (at(0), at(1))
                    })
                    .collect()
            })
            .collect()
    };
    // Page -> selectors used on it, including #fragment targets, in the
    // order pages first appear.
    let mut needed: Vec<(String, Vec<String>)> = Vec::new();
    let mut add = |page: &str, sel: String| match needed.iter_mut().find(|(p, _)| p == page) {
        Some((_, sels)) => {
            if !sels.contains(&sel) {
                sels.push(sel);
            }
        }
        None => needed.push((page.to_string(), vec![sel])),
    };
    for q in questions {
        for route in routes_of(q) {
            let mut previous = String::new();
            for (page, sel) in route {
                if page == "=" {
                    add(&previous, sel);
                    continue;
                }
                let (base, fragment) = page.split_once('#').unwrap_or((&page, ""));
                previous = base.to_string();
                add(base, sel);
                if !fragment.is_empty() {
                    add(base, format!("#{fragment}"));
                }
            }
        }
    }

    let scratch = Scratch::new("usability")?;
    let base_url = build_and_serve(&scratch, &source, zola, true)?;
    let mut layouts: Layouts = BTreeMap::new();
    let mut layout_order = Vec::new();
    {
        let mut chrome = Chrome::launch(&scratch.0.join("profile"))?;
        for (page, sels) in &needed {
            let mut sorted = sels.clone();
            sorted.sort();
            let list = dumps(&Json::from(sorted), None, true);
            for (view, w, h) in VIEWPORTS {
                let phone = view == "phone";
                chrome.open(
                    &format!("{base_url}{page}"),
                    w,
                    h,
                    if phone { 2 } else { 1 },
                    phone,
                    "dark",
                )?;
                let layout = chrome.evaluate(&format!(
                    "{}({list})",
                    include_str!("measure.js").trim_end()
                ))?;
                layouts.insert((page.clone(), view), layout);
                layout_order.push((page.clone(), view));
            }
        }
    }

    let mut rows = Vec::new();
    let mut raw = Vec::new();
    for q in questions {
        let field = |k: &str| {
            q.get(k)
                .and_then(toml::Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let mut cells: Vec<(&str, Option<(i64, i64)>)> = Vec::new();
        for (view, _, _) in VIEWPORTS {
            let mut best: Option<(i64, i64)> = None;
            for route in routes_of(q) {
                if let Some(cost) = route_cost(&route, &layouts, view) {
                    if best.is_none_or(|b| (cost.0 + cost.1, cost.0) < (b.0 + b.1, b.0)) {
                        best = Some(cost);
                    }
                }
            }
            cells.push((view, best));
        }
        let shown = |c: Option<(i64, i64)>| {
            c.map_or_else(|| "no".to_string(), |(a, b)| format!("{a} / {b}"))
        };
        let mut entry = vec![
            ("id".to_string(), Json::from(field("id"))),
            ("question".to_string(), Json::from(field("question"))),
        ];
        entry.extend(cells.iter().map(|(v, c)| {
            (
                v.to_string(),
                c.map_or(Json::Null, |(a, b)| Json::from(vec![a, b])),
            )
        }));
        raw.push(Json::Obj(entry));
        rows.push(format!(
            "| {} | {} | {} | {} |",
            field("question"),
            field("audience"),
            shown(cells[0].1),
            shown(cells[1].1)
        ));
    }
    println!("| Question | Who asks | Desktop clicks / scrolls | Phone clicks / scrolls |");
    println!("|---|---|---:|---:|");
    println!("{}", rows.join("\n"));
    if let Some(path) = args.get("json") {
        let layouts_json = Json::Obj(
            layout_order
                .iter()
                .map(|(page, view)| {
                    (
                        format!("{page} {view}"),
                        Json::from_serde(&layouts[&(page.clone(), *view)]),
                    )
                })
                .collect(),
        );
        let dump = obj! {"results" => Json::Arr(raw), "layouts" => layouts_json};
        write(Path::new(path), dumps(&dump, Some(1), true))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_dates_are_imf_fixdates() {
        let at = |secs| http_date(std::time::UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(at(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(at(1_790_962_291), "Fri, 02 Oct 2026 17:31:31 GMT");
        assert_eq!(at(951_782_400), "Tue, 29 Feb 2000 00:00:00 GMT");
    }

    #[test]
    fn route_costs_follow_clicks_and_scrolls() {
        let layout = |els: Value| json!({"vh": 900.0, "vw": 1440.0, "header": 60.0, "els": els});
        let mut layouts = Layouts::new();
        layouts.insert(
            ("/".into(), "desktop"),
            layout(json!({"a.go": {"top": 100.0, "sticky": true}})),
        );
        layouts.insert(
            ("/faq/".into(), "desktop"),
            layout(json!({"#q": {"top": 2000.0, "sticky": false}, "text=Answer": {"top": 2400.0, "sticky": false}})),
        );
        let route = vec![
            ("/".to_string(), "a.go".to_string()),
            ("/faq/#q".to_string(), "text=Answer".to_string()),
        ];
        // Opens at #q (1940 after the header); 2400 + 80 - 900 - 1940 < 0.
        assert_eq!(route_cost(&route, &layouts, "desktop"), Some((1, 0)));
        let missing = vec![("/".to_string(), "nope".to_string())];
        assert_eq!(route_cost(&missing, &layouts, "desktop"), None);
    }
}
