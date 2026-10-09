//! `psoxide-dev verify-web-dist`: verify that a web build contains every
//! streamed demo-disc asset, and (with `--public-disc`) that the rolling web
//! payload is the reviewed public disc.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The reviewed public audio layout: track number and title.
const PUBLIC_TRACK_TITLES: [(u32, &str); 7] = [
    (2, "KNUCKLE DUST"),
    (3, "RUSTED HAMMER"),
    (4, "CHAINSAW HEART"),
    (5, "NIGHT CRAWLER"),
    (6, "CORTEX IGNITION"),
    (7, "GH-PSX"),
    (8, "HARDWARE TESTS"),
];

type Error = String;

fn read(path: &Path) -> Result<String, Error> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Each manifest filename and its delivery size in bytes.
fn manifest_assets(manifest: &Path) -> Result<Vec<(String, i64)>, Error> {
    let manifest_name = name(manifest);
    let mut assets = Vec::new();
    for (index, line) in read(manifest)?.lines().enumerate() {
        let line_number = index + 1;
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.is_empty() {
            continue;
        }
        let (filename, delivery_bytes) = if fields[0] == "data" && fields.len() == 5 {
            (fields[1], fields[2])
        } else if fields[0] == "track" && fields.len() >= 7 {
            (fields[2], fields[3])
        } else {
            return Err(format!(
                "{manifest_name}:{line_number}: malformed manifest row"
            ));
        };
        if filename.contains('/') || filename == "." {
            return Err(format!(
                "{manifest_name}:{line_number}: unsafe asset name {filename:?}"
            ));
        }
        let bytes = delivery_bytes
            .parse::<i64>()
            .map_err(|_| format!("{manifest_name}:{line_number}: invalid delivery size"))?;
        assets.push((filename.to_string(), bytes));
    }
    if assets.is_empty() {
        return Err(format!("{manifest_name}: no delivery assets listed"));
    }
    Ok(assets)
}

/// Validate the CUE, manifest, and every file named by the manifest. Returns
/// the asset count and their total size.
fn verify(dist: &Path) -> Result<(usize, u64), Error> {
    let manifest = dist.join("web-manifest.txt");
    let cue = dist.join("demo-disc.cue");
    for required in [&manifest, &cue] {
        if !required.is_file() {
            return Err(format!("missing {}", required.display()));
        }
    }
    if !read(&cue)?.contains("FILE \"demo-disc.bin\" BINARY") {
        return Err("demo-disc.cue does not reference demo-disc.bin".into());
    }
    let assets = manifest_assets(&manifest)?;
    let mut total_bytes = 0;
    let mut seen = HashSet::new();
    for (filename, expected) in &assets {
        if !seen.insert(filename.clone()) {
            return Err(format!("duplicate manifest asset {filename}"));
        }
        let path = dist.join(filename);
        if !path.is_file() {
            return Err(format!("missing manifest asset {}", path.display()));
        }
        let actual = fs::metadata(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .len();
        if i64::try_from(actual).ok() != Some(*expected) {
            return Err(format!(
                "{filename}: expected {expected} bytes, found {actual}"
            ));
        }
        total_bytes += actual;
    }
    Ok((assets.len(), total_bytes))
}

/// Case-insensitive `half[ -]?life`.
fn names_half_life(text: &str) -> bool {
    let lower = text.to_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find("half") {
        let rest = &lower[from + found + 4..];
        let rest = rest.strip_prefix([' ', '-']).unwrap_or(rest);
        if rest.starts_with("life") {
            return true;
        }
        from += found + 1;
    }
    false
}

/// The `TRACK n` numbers of a cue sheet, in order.
fn cue_tracks(cue: &str) -> Vec<u64> {
    cue.split('\n')
        .filter_map(|line| {
            let rest = line.trim_start_matches([' ', '\t']).strip_prefix("TRACK")?;
            let spaced = rest.trim_start_matches([' ', '\t']);
            if spaced.len() == rest.len() {
                return None;
            }
            let digits: String = spaced.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect()
}

/// Fail closed if the rolling web payload is not the reviewed public disc.
fn verify_public_disc(dist: &Path) -> Result<(), Error> {
    let manifest_text = read(&dist.join("web-manifest.txt"))?;
    let cue_text = read(&dist.join("demo-disc.cue"))?;
    if names_half_life(&manifest_text) {
        return Err("public web manifest names Half-Life content".into());
    }
    let mut actual_titles: BTreeMap<u32, String> = BTreeMap::new();
    for (index, line) in manifest_text.lines().enumerate() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.first() == Some(&"track") {
            let line_number = index + 1;
            if fields.len() < 7 {
                return Err(format!(
                    "web-manifest.txt:{line_number}: track title is missing"
                ));
            }
            let number = fields[1]
                .parse()
                .map_err(|_| format!("web-manifest.txt:{line_number}: invalid track number"))?;
            actual_titles.insert(number, fields[6..].join(" "));
        }
    }
    let reviewed: BTreeMap<u32, String> = PUBLIC_TRACK_TITLES
        .iter()
        .map(|(n, title)| (*n, title.to_string()))
        .collect();
    if actual_titles != reviewed {
        return Err("public web-disc audio layout changed; review and update the allowlist".into());
    }
    if cue_tracks(&cue_text) != (1..=8).collect::<Vec<u64>>() {
        return Err("public demo-disc.cue must contain exactly tracks 1 through 8".into());
    }
    Ok(())
}

/// `psoxide-dev verify-web-dist <dist> [--public-disc]`
pub fn run(args: &[String]) -> Result<(), String> {
    let mut dist: Option<PathBuf> = None;
    let mut public = false;
    for arg in args {
        match arg.as_str() {
            "--public-disc" => public = true,
            flag if flag.starts_with("--") => return Err(format!("unrecognized argument: {flag}")),
            path if dist.is_none() => dist = Some(PathBuf::from(path)),
            extra => return Err(format!("unrecognized argument: {extra}")),
        }
    }
    let dist = dist.ok_or("usage: psoxide-dev verify-web-dist <dist> [--public-disc]")?;
    let (count, total_bytes) = verify(&dist)?;
    if public {
        verify_public_disc(&dist)?;
    }
    println!(
        "web delivery verified: {count} manifest assets, {:.2} MiB",
        total_bytes as f64 / (1024.0 * 1024.0)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Dist(PathBuf);

    impl Dist {
        /// The reviewed public layout: one data file and tracks 2 through 8.
        fn public() -> Dist {
            // A per-process counter keeps parallel tests (and several
            // fixtures in one test) in separate directories; a clock stamp
            // alone collides on coarse timers.
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("web-dist-{}-{serial}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let mut manifest = vec!["data demo-data.bin.gz 1 1 0".to_string()];
            for (number, title) in PUBLIC_TRACK_TITLES {
                let filename = format!("track-{number:02}.flac");
                manifest.push(format!("track {number} {filename} 1 1 0 {title}"));
                fs::write(dir.join(filename), b"x").unwrap();
            }
            fs::write(dir.join("demo-data.bin.gz"), b"x").unwrap();
            fs::write(dir.join("web-manifest.txt"), manifest.join("\n") + "\n").unwrap();
            let mut cue = vec!["FILE \"demo-disc.bin\" BINARY".to_string()];
            for number in 1..=8 {
                let mode = if number == 1 { "MODE2/2352" } else { "AUDIO" };
                cue.push(format!("  TRACK {number:02} {mode}"));
                cue.push("    INDEX 01 00:00:00".into());
            }
            fs::write(dir.join("demo-disc.cue"), cue.join("\n") + "\n").unwrap();
            Dist(dir)
        }

        fn append(&self, file: &str, text: &str) {
            let path = self.0.join(file);
            fs::write(&path, fs::read_to_string(&path).unwrap() + text).unwrap();
        }
    }

    impl Drop for Dist {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reviewed_public_layout_passes() {
        let dist = Dist::public();
        assert_eq!(verify(&dist.0).unwrap(), (8, 8));
        verify_public_disc(&dist.0).unwrap();
    }

    #[test]
    fn half_life_title_is_rejected() {
        let dist = Dist::public();
        dist.append(
            "web-manifest.txt",
            "track 9 track-09.flac 1 1 0 HALF-LIFE\n",
        );
        assert!(verify_public_disc(&dist.0)
            .unwrap_err()
            .contains("Half-Life"));
        assert!(names_half_life("Half Life"));
        assert!(names_half_life("xhalflifex"));
        assert!(!names_half_life("half  life"));
    }

    #[test]
    fn unreviewed_track_layout_is_rejected() {
        let dist = Dist::public();
        dist.append("demo-disc.cue", "  TRACK 09 AUDIO\n    INDEX 01 00:00:00\n");
        assert!(verify_public_disc(&dist.0)
            .unwrap_err()
            .contains("tracks 1 through 8"));
    }

    #[test]
    fn delivery_checks_fail_closed() {
        let dist = Dist::public();
        fs::write(dist.0.join("track-03.flac"), b"xx").unwrap();
        assert!(verify(&dist.0)
            .unwrap_err()
            .contains("expected 1 bytes, found 2"));
        fs::write(dist.0.join("track-03.flac"), b"x").unwrap();
        fs::remove_file(dist.0.join("track-04.flac")).unwrap();
        assert!(verify(&dist.0)
            .unwrap_err()
            .contains("missing manifest asset"));
        let dist = Dist::public();
        dist.append("web-manifest.txt", "data demo-data.bin.gz 1 1 0\n");
        assert!(verify(&dist.0)
            .unwrap_err()
            .contains("duplicate manifest asset"));
        let dist = Dist::public();
        dist.append("web-manifest.txt", "nonsense\n");
        assert!(verify(&dist.0)
            .unwrap_err()
            .contains("malformed manifest row"));
        let dist = Dist::public();
        dist.append("web-manifest.txt", "data ../x 1 1 0\n");
        assert!(verify(&dist.0).unwrap_err().contains("unsafe asset name"));
        let dist = Dist::public();
        fs::write(dist.0.join("demo-disc.cue"), "FILE \"other.bin\" BINARY\n").unwrap();
        assert!(verify(&dist.0).unwrap_err().contains("does not reference"));
        assert!(verify(&dist.0.join("missing")).is_err());
    }
}
