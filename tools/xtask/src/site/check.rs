//! `site check`: check built HTML, assets and fragments, including GitHub
//! Pages subpaths, and that every example page shows its complete source.
//!
//! External availability is deliberately checked separately from
//! deterministic CI. The HTML tokenizer, character references and URL
//! resolution follow Python's `html.parser`, `html.unescape` and
//! `urllib.parse`, which the check was written against.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::html_entities::{INVALID_CHARREFS, INVALID_CODEPOINTS, NAMED};
use super::{read_text, site_root, Args};

/// `html.unescape`.
pub fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    static CHARREF: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"&(#[0-9]+;?|#[xX][0-9a-fA-F]+;?|[^\t\n\x0c <&#;]{1,32};?)")
            .expect("valid regex")
    });
    CHARREF
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let s = &caps[1];
            if let Some(number) = s.strip_prefix('#') {
                let (digits, radix) = match number.strip_prefix(['x', 'X']) {
                    Some(hex) => (hex, 16),
                    None => (number, 10),
                };
                let digits = digits.trim_end_matches(';');
                // Python's int() has no overflow; anything past the
                // Unicode range is replaced like an out-of-range number.
                let num = u64::from_str_radix(digits, radix).unwrap_or(u64::MAX);
                if let Ok(i) = INVALID_CHARREFS.binary_search_by_key(&num, |(k, _)| u64::from(*k)) {
                    return INVALID_CHARREFS[i].1.to_string();
                }
                if (0xD800..=0xDFFF).contains(&num) || num > 0x10FFFF {
                    return "\u{FFFD}".to_string();
                }
                if INVALID_CODEPOINTS.contains(&(num as u32)) {
                    return String::new();
                }
                return char::from_u32(num as u32).map_or_else(String::new, String::from);
            }
            let named = |name: &str| {
                NAMED
                    .binary_search_by(|(k, _)| (*k).cmp(name))
                    .ok()
                    .map(|i| NAMED[i].1)
            };
            if let Some(value) = named(s) {
                return value.to_string();
            }
            // The longest legacy name the reference starts with.
            let cuts: Vec<usize> = s
                .char_indices()
                .map(|(i, _)| i)
                .filter(|&i| i >= 2)
                .collect();
            for &cut in cuts.iter().rev() {
                if let Some(value) = named(&s[..cut]) {
                    return format!("{value}{}", &s[cut..]);
                }
            }
            format!("&{s}")
        })
        .into_owned()
}

/// What the checker needs from one page.
#[derive(Default)]
pub struct Page {
    /// Every `id` attribute, in document order.
    pub ids: Vec<String>,
    links: Vec<String>,
    errors: Vec<String>,
    /// Text of each `<pre aria-label="NAME source">`, by NAME.
    pub listings: BTreeMap<String, String>,
    current_listing: Option<String>,
}

type Attrs = Vec<(String, Option<String>)>;

impl Page {
    fn start(&mut self, tag: &str, attrs: &Attrs) {
        // dict(attrs): a repeated attribute keeps its last value.
        let get = |key: &str| {
            attrs
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_deref())
        };
        if tag == "pre" {
            if let Some(label) = get("aria-label")
                .flatten()
                .and_then(|l| l.strip_suffix(" source"))
            {
                self.current_listing = Some(label.to_string());
                self.listings.insert(label.to_string(), String::new());
            }
        }
        if let Some(id) = get("id") {
            self.ids.push(id.unwrap_or("None").to_string());
        }
        for key in ["href", "src"] {
            if let Some(Some(value)) = get(key) {
                self.links.push(value.to_string());
            }
        }
        if tag == "img" && get("alt").flatten().is_none_or(str::is_empty) {
            self.errors.push("image has no descriptive alt text".into());
        }
        let classes = get("class").flatten().unwrap_or("");
        if classes
            .split_whitespace()
            .any(|c| c == "flag" || c == "draft-banner")
        {
            self.errors.push("unresolved review note".into());
        }
    }

    fn end(&mut self, tag: &str) {
        if tag == "pre" {
            self.current_listing = None;
        }
    }

    fn data(&mut self, text: &str) {
        if let Some(listing) = &self.current_listing {
            self.listings
                .get_mut(listing)
                .expect("listing opened")
                .push_str(text);
        }
    }

    /// Tokenize `text` the way `HTMLParser.feed` (without `close`) does.
    pub fn parse(text: &str) -> Self {
        let mut page = Page::default();
        let b = text.as_bytes();
        let n = b.len();
        let mut i = 0;
        let mut cdata: Option<String> = None;
        while i < n {
            if let Some(element) = cdata.take() {
                // Raw text up to `</script` (or `</style`) and a delimiter.
                let lower = text[i..].to_ascii_lowercase();
                let close = format!("</{element}");
                let mut from = 0;
                let end = loop {
                    match lower[from..].find(&close) {
                        Some(k) => {
                            let at = from + k;
                            let next = lower.as_bytes().get(at + close.len());
                            if next.is_none_or(|c| b"\t\n\r\x0c />".contains(c)) {
                                break Some(i + at);
                            }
                            from = at + 1;
                        }
                        None => break None,
                    }
                };
                let Some(end) = end else {
                    // Unterminated: HTMLParser holds it until close().
                    break;
                };
                page.data(&text[i..end]);
                i = end;
                continue;
            }
            let next = text[i..].find('<').map_or(n, |k| i + k);
            if next > i {
                page.data(&unescape(&text[i..next]));
                i = next;
            }
            if i >= n {
                break;
            }
            let rest = &text[i..];
            if rest.len() > 1 && b[i + 1].is_ascii_alphabetic() {
                match parse_start_tag(rest) {
                    Some((tag, attrs, len, self_closing)) => {
                        page.start(&tag, &attrs);
                        if self_closing {
                            page.end(&tag);
                        } else if tag == "script" || tag == "style" {
                            cdata = Some(tag);
                        }
                        i += len;
                    }
                    None => break,
                }
            } else if rest.starts_with("</") {
                let Some(close) = rest.find('>') else { break };
                let name: String = rest[2..close]
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != '/' && *c != '>')
                    .collect::<String>()
                    .to_ascii_lowercase();
                if name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                    page.end(&name);
                }
                i += close + 1;
            } else if let Some(comment) = rest.strip_prefix("<!--") {
                let Some(end) = comment.find("-->") else {
                    break;
                };
                i += 4 + end + 3;
            } else if rest.starts_with("<!") || rest.starts_with("<?") {
                let Some(end) = rest.find('>') else { break };
                i += end + 1;
            } else {
                page.data("<");
                i += 1;
            }
        }
        let mut counts: HashMap<&str, usize> = HashMap::new();
        let mut order = Vec::new();
        for id in &page.ids {
            let count = counts.entry(id).or_insert(0);
            if *count == 0 {
                order.push(id.clone());
            }
            *count += 1;
        }
        for id in order {
            if counts[id.as_str()] > 1 {
                page.errors.push(format!("duplicate id: {id}"));
            }
        }
        page
    }
}

/// Parse a start tag at the beginning of `text`: name, attributes, length
/// and whether it closed itself with `/>`. `None` when it never ends.
fn parse_start_tag(text: &str) -> Option<(String, Attrs, usize, bool)> {
    let b = text.as_bytes();
    let mut i = 1;
    while i < b.len() && !b"\t\n\r\x0c />\0".contains(&b[i]) {
        i += 1;
    }
    let tag = text[1..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while i < b.len()
            && (b[i].is_ascii_whitespace() || (b[i] == b'/' && b.get(i + 1) != Some(&b'>')))
        {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        if b[i] == b'>' {
            return Some((tag, attrs, i + 1, false));
        }
        if b[i] == b'/' {
            // "/>": b[i + 1] is '>' here.
            return Some((tag, attrs, i + 2, true));
        }
        let start = i;
        i += 1;
        while i < b.len() && !(b[i].is_ascii_whitespace() || b"/=>".contains(&b[i])) {
            i += 1;
        }
        let name = text[start..i].to_ascii_lowercase();
        let mut j = i;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < b.len() && b[j] == b'=' {
            while j < b.len() && b[j] == b'=' {
                j += 1;
            }
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let value_start = j;
            let value = match b.get(j) {
                Some(&q) if q == b'"' || q == b'\'' => {
                    let close = text[j + 1..].find(q as char)? + j + 1;
                    j = close + 1;
                    text[value_start + 1..close].to_string()
                }
                _ => {
                    while j < b.len() && !(b[j].is_ascii_whitespace() || b[j] == b'>') {
                        j += 1;
                    }
                    text[value_start..j].to_string()
                }
            };
            attrs.push((
                name,
                Some(if value.is_empty() {
                    value
                } else {
                    unescape(&value)
                }),
            ));
            i = j;
        } else {
            attrs.push((name, None));
        }
    }
}

/// `urllib.parse.urlsplit`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Split {
    /// Scheme, lowercased.
    pub scheme: String,
    /// Network location.
    pub netloc: String,
    /// Path.
    pub path: String,
    /// Query.
    pub query: String,
    /// Fragment.
    pub fragment: String,
}

const USES_RELATIVE: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "imap", "wais", "file", "https", "shttp", "mms",
    "prospero", "rtsp", "rtsps", "rtspu", "sftp", "svn", "svn+ssh", "ws", "wss",
];
const USES_NETLOC: &[&str] = &[
    "",
    "ftp",
    "http",
    "gopher",
    "nntp",
    "telnet",
    "imap",
    "wais",
    "file",
    "mms",
    "https",
    "shttp",
    "snews",
    "prospero",
    "rtsp",
    "rtsps",
    "rtspu",
    "rsync",
    "svn",
    "svn+ssh",
    "sftp",
    "nfs",
    "git",
    "git+ssh",
    "ws",
    "wss",
    "itms-services",
];
const USES_PARAMS: &[&str] = &[
    "", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu",
    "sip", "sips", "mms", "sftp", "tel",
];

/// Split `url`, with `default_scheme` when it names none.
pub fn urlsplit(url: &str, default_scheme: &str) -> Split {
    let url: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .trim_end_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut rest = url.as_str();
    let mut split = Split {
        scheme: default_scheme.to_string(),
        ..Split::default()
    };
    if let Some(colon) = rest.find(':') {
        let candidate = &rest[..colon];
        if colon > 0
            && candidate.as_bytes()[0].is_ascii_alphabetic()
            && candidate
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+-.".contains(&c))
        {
            split.scheme = candidate.to_ascii_lowercase();
            rest = &rest[colon + 1..];
        }
    }
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        split.netloc = after[..end].to_string();
        rest = &after[end..];
    }
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    split.path = path.to_string();
    split.query = query.to_string();
    split.fragment = fragment.to_string();
    split
}

impl Split {
    /// `.hostname`: lowercased, without user info or port.
    pub fn hostname(&self) -> Option<String> {
        let host = self
            .netloc
            .rsplit_once('@')
            .map_or(self.netloc.as_str(), |(_, h)| h);
        let host = if let Some(bracketed) = host.strip_prefix('[') {
            bracketed.split(']').next().unwrap_or("")
        } else {
            host.split(':').next().unwrap_or("")
        };
        (!host.is_empty()).then(|| host.to_lowercase())
    }
}

/// `urlunsplit`.
fn unsplit(scheme: &str, netloc: &str, path: &str, query: &str, fragment: &str) -> String {
    let mut url = path.to_string();
    if !netloc.is_empty()
        || (!scheme.is_empty() && USES_NETLOC.contains(&scheme) && !url.starts_with("//"))
    {
        if !url.is_empty() && !url.starts_with('/') {
            url.insert(0, '/');
        }
        url = format!("//{netloc}{url}");
    }
    if !scheme.is_empty() {
        url = format!("{scheme}:{url}");
    }
    if !query.is_empty() {
        url = format!("{url}?{query}");
    }
    if !fragment.is_empty() {
        url = format!("{url}#{fragment}");
    }
    url
}

/// `urlparse`'s `;params` split of a path.
fn split_params(scheme: &str, path: &str) -> (String, String) {
    if !USES_PARAMS.contains(&scheme) || !path.contains(';') {
        return (path.to_string(), String::new());
    }
    let from = path.rfind('/').unwrap_or(0);
    match path[from..].find(';') {
        Some(k) => (
            path[..from + k].to_string(),
            path[from + k + 1..].to_string(),
        ),
        None => (path.to_string(), String::new()),
    }
}

/// `urllib.parse.urljoin`.
pub fn urljoin(base: &str, url: &str) -> String {
    if base.is_empty() {
        return url.to_string();
    }
    if url.is_empty() {
        return base.to_string();
    }
    let b = urlsplit(base, "");
    let (bpath, bparams) = split_params(&b.scheme, &b.path);
    let u = urlsplit(url, &b.scheme);
    let (path, params) = split_params(&u.scheme, &u.path);
    let with_params = |path: &str, params: &str| {
        if params.is_empty() {
            path.to_string()
        } else {
            format!("{path};{params}")
        }
    };
    if u.scheme != b.scheme || !USES_RELATIVE.contains(&u.scheme.as_str()) {
        return url.to_string();
    }
    let mut netloc = u.netloc.clone();
    if USES_NETLOC.contains(&u.scheme.as_str()) {
        if !netloc.is_empty() {
            return unsplit(
                &u.scheme,
                &netloc,
                &with_params(&path, &params),
                &u.query,
                &u.fragment,
            );
        }
        netloc = b.netloc.clone();
    }
    if path.is_empty() && params.is_empty() {
        let query = if u.query.is_empty() {
            &b.query
        } else {
            &u.query
        };
        return unsplit(
            &u.scheme,
            &netloc,
            &with_params(&bpath, &bparams),
            query,
            &u.fragment,
        );
    }
    let mut base_parts: Vec<&str> = bpath.split('/').collect();
    if base_parts.last() != Some(&"") {
        base_parts.pop();
    }
    let segments: Vec<&str> = if path.starts_with('/') {
        path.split('/').collect()
    } else {
        let mut all: Vec<&str> = base_parts;
        all.extend(path.split('/'));
        // Drop empty middle segments, which would rejoin as "//".
        let last = all.len().saturating_sub(1);
        all.iter()
            .enumerate()
            .filter(|&(k, s)| k == 0 || k == last || !s.is_empty())
            .map(|(_, s)| *s)
            .collect()
    };
    let mut resolved: Vec<&str> = Vec::new();
    for segment in &segments {
        match *segment {
            ".." => {
                resolved.pop();
            }
            "." => {}
            other => resolved.push(other),
        }
    }
    if matches!(segments.last(), Some(&".") | Some(&"..")) {
        resolved.push("");
    }
    let joined = resolved.join("/");
    let joined = if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    };
    unsplit(
        &u.scheme,
        &netloc,
        &with_params(&joined, &params),
        &u.query,
        &u.fragment,
    )
}

/// `urllib.parse.unquote`: percent-decoding as UTF-8, invalid sequences
/// replaced.
pub fn unquote(text: &str) -> String {
    if !text.contains('%') {
        return text.to_string();
    }
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' {
            if let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|&c| hex(c)),
                b.get(i + 2).and_then(|&c| hex(c)),
            ) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn html_pages(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            html_pages(&path, out);
        } else if path.extension().is_some_and(|e| e == "html") {
            out.push(path);
        }
    }
}

/// Check the built site in `--output-dir` (default `public`) as served from
/// `--base-url`. Errors print one per line; any error fails the task.
pub fn main(args: &[String]) -> Result<(), String> {
    let args = Args::parse(args, &[])?;
    let output_dir = PathBuf::from(args.get("output-dir").unwrap_or("public"));
    let root = output_dir.canonicalize().unwrap_or(output_dir);
    let base = format!("{}/", args.need("base-url")?.trim_end_matches('/'));
    let origin = urlsplit(&base, "");
    let mut files = Vec::new();
    html_pages(&root, &mut files);
    files.sort();
    let mut pages: BTreeMap<PathBuf, Page> = BTreeMap::new();
    for path in files {
        pages.insert(path.clone(), Page::parse(&read_text(&path)?));
    }
    let mut errors = Vec::new();
    if pages.is_empty() {
        errors.push("no built HTML found".to_string());
    }
    let local = |host: Option<&str>| matches!(host, Some("localhost" | "127.0.0.1"));
    let span = Regex::new(r"^(\d+)-(\d+)$").expect("valid regex");
    let mut id_sets: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    for (path, page) in &pages {
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        errors.extend(page.errors.iter().map(|e| format!("{name}: {e}")));
        for link in &page.links {
            let url = urlsplit(&urljoin(&format!("{base}{name}"), link), "");
            if local(url.hostname().as_deref()) && !local(origin.hostname().as_deref()) {
                errors.push(format!("{name}: local URL in production: {link}"));
            }
            if url.netloc != origin.netloc || !matches!(url.scheme.as_str(), "http" | "https") {
                continue;
            }
            let mut target;
            if url.path.trim_end_matches('/') == origin.path.trim_end_matches('/') {
                target = root.join("index.html");
            } else if let Some(rest) = url.path.strip_prefix(&origin.path) {
                target = root.join(unquote(rest));
                if target.is_dir() {
                    target.push("index.html");
                }
            } else {
                errors.push(format!("{name}: escapes site base path: {link}"));
                continue;
            }
            if !target.exists() {
                errors.push(format!("{name}: missing target: {link}"));
            } else if let (false, Some(target_page)) = (url.fragment.is_empty(), pages.get(&target))
            {
                let ids = id_sets
                    .entry(target.clone())
                    .or_insert_with(|| target_page.ids.iter().cloned().collect());
                let has = |id: &str| ids.contains(id);
                let mut valid = has(&url.fragment) || has(&unquote(&url.fragment));
                // Rustdoc uses literal percent-encoded ids for generics, and
                // its source viewer resolves #start-end to two numbered lines.
                if !valid
                    && target.starts_with(root.join("api"))
                    && target.to_string_lossy().contains("/src/")
                {
                    valid = span
                        .captures(&url.fragment)
                        .is_some_and(|c| has(&c[1]) && has(&c[2]));
                }
                if !valid {
                    errors.push(format!("{name}: missing fragment: {link}"));
                }
            }
        }
    }
    let reference = site_root().join("data/sdk-reference.json");
    if reference.exists() {
        let data: serde_json::Value =
            serde_json::from_str(&read_text(&reference)?).map_err(|e| e.to_string())?;
        for example in data["examples"].as_array().into_iter().flatten() {
            let path = root
                .join("docs/examples")
                .join(example["name"].as_str().unwrap_or_default())
                .join("index.html");
            for source in example["files"].as_array().into_iter().flatten() {
                let listing = pages
                    .get(&path)
                    .and_then(|p| p.listings.get(source["path"].as_str().unwrap_or_default()));
                if listing.map(String::as_str) != source["code"].as_str() {
                    errors.push(format!(
                        "{}: complete source differs: {}",
                        path.strip_prefix(&root).unwrap_or(&path).display(),
                        source["path"].as_str().unwrap_or_default()
                    ));
                }
            }
        }
    }
    for error in &errors {
        println!("{error}");
    }
    println!(
        "{} HTML pages checked; {} errors",
        pages.len(),
        errors.len()
    );
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("{} site errors", errors.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_matches_python() {
        assert_eq!(
            unescape("a &amp; b &lt;c&gt; &#39;&#x27; &copy &ampx &self; &#128; &#0;"),
            "a & b <c> '' \u{a9} &x &self; \u{20ac} \u{fffd}"
        );
    }

    #[test]
    fn urljoin_matches_python() {
        let base = "https://ebonura.github.io/PSoXide/docs/sdk/index.html";
        for (link, joined) in [
            ("../x/", "https://ebonura.github.io/PSoXide/docs/x/"),
            ("/PSoXide/a.css", "https://ebonura.github.io/PSoXide/a.css"),
            (
                "#frag",
                "https://ebonura.github.io/PSoXide/docs/sdk/index.html#frag",
            ),
            ("mailto:a@b.c", "mailto:a@b.c"),
            ("//cdn.example/x.js", "https://cdn.example/x.js"),
            (
                "?q=1",
                "https://ebonura.github.io/PSoXide/docs/sdk/index.html?q=1",
            ),
            (
                "a//b/./c/..",
                "https://ebonura.github.io/PSoXide/docs/sdk/a/b/",
            ),
            ("../../../../..", "https://ebonura.github.io/"),
        ] {
            assert_eq!(urljoin(base, link), joined, "{link}");
        }
    }

    #[test]
    fn parses_listings_ids_and_links() {
        let page = Page::parse(
            "<!doctype html><p id=a>x</p><pre aria-label=\"main.rs source\"><span>fn &amp;a</span>\n</pre>\
             <script>if (a < b) { document.write('<a href=\"no\">'); }</script>\
             <img src='i.png'><a href=\"/x#y\" class=\"flag\" id=a/>",
        );
        assert_eq!(page.listings["main.rs"], "fn &a\n");
        assert_eq!(page.links, ["i.png", "/x#y"]);
        // An unquoted value runs to the ">", slash included.
        assert_eq!(page.ids, ["a", "a/"]);
        assert!(page
            .errors
            .contains(&"image has no descriptive alt text".to_string()));
        assert!(page.errors.contains(&"unresolved review note".to_string()));
        assert_eq!(page.errors.len(), 2);
        assert_eq!(
            Page::parse("<b id=x><i id=\"x\">").errors,
            ["duplicate id: x"]
        );
    }
}
