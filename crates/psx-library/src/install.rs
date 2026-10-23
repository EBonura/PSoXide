//! The install itself: the order files are written in and the stale-copy rules.

use crate::provenance::{self, sha256_bytes, sha256_file, Source};
use crate::publish::publish;
use crate::{cue, Error, PROVENANCE_SUFFIX};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{fs, io};

/// A file installed beside the disc as `<name><suffix>`, for example a licence text.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Sidecar<'a> {
    /// The file name suffix, dot included: `.SLICNSE.TXT`.
    pub suffix: &'a str,
    /// The file's contents.
    pub contents: &'a [u8],
}

impl<'a> Sidecar<'a> {
    /// A sidecar installed as `<name><suffix>` with `contents`.
    #[must_use]
    pub fn new(suffix: &'a str, contents: &'a [u8]) -> Self {
        Self { suffix, contents }
    }
}

/// One disc to install. Build one with [`Request::new`] and the `with_*` methods.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Request<'a> {
    /// The built disc's cue sheet; the `.bin` is the file beside it with the same stem.
    pub cue: &'a Path,
    /// The library directory. Created when missing. There is no default.
    pub games_dir: &'a Path,
    /// The library name, `Game Name (repo)`; the files are `<name>.bin`, `<name>.cue`, ...
    pub name: &'a str,
    /// Where the disc's contents came from, recorded in the provenance.
    pub source: Source,
    /// The flat stem the game was installed under before library naming (`quake-psx`), whose
    /// `.bin`, `.cue` and sidecars are removed when that is safe.
    pub legacy_stem: Option<&'a str>,
    /// Sidecars to install besides the provenance record, which is always written.
    pub sidecars: &'a [Sidecar<'a>],
    /// Fields for the provenance record that only the caller knows (build command, gates,
    /// notes, the packaged artifacts). The install's own facts are laid over them. Must be a
    /// JSON object.
    pub provenance_base: Option<&'a Value>,
}

impl<'a> Request<'a> {
    /// A request to install the disc built at `cue` into `games_dir` as `name`.
    #[must_use]
    pub fn new(cue: &'a Path, games_dir: &'a Path, name: &'a str, source: Source) -> Self {
        Self {
            cue,
            games_dir,
            name,
            source,
            legacy_stem: None,
            sidecars: &[],
            provenance_base: None,
        }
    }

    /// Also clear a stale set installed under the flat `stem` (`quake-psx`), when safe.
    #[must_use]
    pub fn with_legacy_stem(mut self, stem: &'a str) -> Self {
        self.legacy_stem = Some(stem);
        self
    }

    /// Install `sidecars` beside the disc.
    #[must_use]
    pub fn with_sidecars(mut self, sidecars: &'a [Sidecar<'a>]) -> Self {
        self.sidecars = sidecars;
        self
    }

    /// Start the provenance record from `base`, a JSON object of the caller's own fields.
    #[must_use]
    pub fn with_provenance_base(mut self, base: &'a Value) -> Self {
        self.provenance_base = Some(base);
        self
    }
}

/// What an install did.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Installed {
    /// The installed cue, the file to open.
    pub cue: PathBuf,
    /// The installed bin.
    pub bin: PathBuf,
    /// The installed provenance record.
    pub provenance: PathBuf,
    /// The digest of the installed bin.
    pub bin_sha256: String,
    /// The digest of the bin that was overwritten, when there was one.
    pub replaced_bin_sha256: Option<String>,
    /// Old-name files that were removed.
    pub removed: Vec<PathBuf>,
}

/// Install the disc described by `request` into its library directory.
///
/// Nothing is written unless every check passes first. Files then go in this order, each by
/// temporary name, digest check and rename: the bin, the sidecars and provenance, and last the
/// cue. Old-name files are removed only after all of that succeeded.
///
/// # Errors
///
/// [`Error::InvalidName`] for a name that cannot be a file name stem,
/// [`Error::CueWithoutFile`] for a cue that does not name a bin,
/// [`Error::LegacyDiscNotOlder`] when removing an old-name bin could lose a newer disc (nothing
/// has been written then), and [`Error::Io`] or [`Error::DigestMismatch`] when a write fails.
pub fn install(request: &Request<'_>) -> Result<Installed, Error> {
    install_at(request, SystemTime::now())
}

pub(crate) fn install_at(request: &Request<'_>, now: SystemTime) -> Result<Installed, Error> {
    let name = request.name;
    if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\']) {
        return Err(Error::InvalidName(name.to_string()));
    }
    let image = request.cue.with_extension("bin");
    let cue_text = cue::retarget(
        &fs::read_to_string(request.cue)
            .map_err(|error| Error::io(format!("read {}", request.cue.display()), error))?,
        name,
        request.cue,
    )?;
    let image_bytes = fs::metadata(&image)
        .map_err(|error| Error::io(format!("read {}", image.display()), error))?
        .len();
    let image_digest = sha256_file(&image)
        .map_err(|error| Error::io(format!("read {}", image.display()), error))?;

    fs::create_dir_all(request.games_dir)
        .map_err(|error| Error::io(format!("create {}", request.games_dir.display()), error))?;
    let live_bin = request.games_dir.join(format!("{name}.bin"));
    let live_cue = request.games_dir.join(format!("{name}.cue"));
    let live_provenance = request.games_dir.join(format!("{name}{PROVENANCE_SUFFIX}"));

    let stale = request
        .legacy_stem
        .map(|stem| stale_legacy_files(request, stem, &image, &image_digest))
        .transpose()?
        .unwrap_or_default();

    let replaced = live_bin
        .is_file()
        .then(|| sha256_file(&live_bin))
        .transpose()
        .map_err(|error| Error::io(format!("read {}", live_bin.display()), error))?;
    let installed_at = provenance::utc_timestamp(now);
    let record = provenance::record(
        request.provenance_base,
        &provenance::Installed {
            bin_name: &format!("{name}.bin"),
            bin_bytes: image_bytes,
            bin_sha256: &image_digest,
            cue_name: &format!("{name}.cue"),
            cue_text: &cue_text,
            source: &request.source,
            installed_at: &installed_at,
            replaced_bin_sha256: replaced.as_deref(),
        },
    )?;

    publish(&live_bin, &image_digest, |temporary| {
        fs::copy(&image, temporary).map(drop)
    })?;
    for sidecar in request.sidecars {
        let live = request.games_dir.join(format!("{name}{}", sidecar.suffix));
        publish(&live, &sha256_bytes(sidecar.contents), |temporary| {
            fs::write(temporary, sidecar.contents)
        })?;
    }
    publish(&live_provenance, &sha256_bytes(&record), |temporary| {
        fs::write(temporary, &record)
    })?;
    // The cue goes last: it is what an emulator opens, so it appears only once its bin and
    // sidecars are in place.
    publish(&live_cue, &sha256_bytes(cue_text.as_bytes()), |temporary| {
        fs::write(temporary, &cue_text)
    })?;

    for path in &stale {
        fs::remove_file(path)
            .map_err(|error| Error::io(format!("remove stale {}", path.display()), error))?;
    }
    Ok(Installed {
        cue: live_cue,
        bin: live_bin,
        provenance: live_provenance,
        bin_sha256: image_digest,
        replaced_bin_sha256: replaced,
        removed: stale,
    })
}

/// The old-name files this install replaces, or an error if the old bin might hold something
/// the install would lose.
fn stale_legacy_files(
    request: &Request<'_>,
    stem: &str,
    image: &Path,
    image_digest: &str,
) -> Result<Vec<PathBuf>, Error> {
    let directory = request.games_dir;
    let legacy_bin = directory.join(format!("{stem}.bin"));
    let legacy_cue = directory.join(format!("{stem}.cue"));
    let mut stale = Vec::new();
    if legacy_bin.is_file() {
        let io_error = |error| Error::io(format!("read {}", legacy_bin.display()), error);
        let same = sha256_file(&legacy_bin).map_err(io_error)? == image_digest;
        if !same && !is_older(&legacy_bin, image).map_err(io_error)? {
            return Err(Error::LegacyDiscNotOlder(legacy_bin));
        }
        stale.push(legacy_bin);
    }
    if legacy_cue.is_file() {
        stale.push(legacy_cue);
    }
    // The record is always rewritten, so its old-name copy is replaced; any other old-name
    // sidecar is removed only when this install carries a sidecar with the same suffix.
    let mut suffixes = vec![PROVENANCE_SUFFIX];
    suffixes.extend(request.sidecars.iter().map(|sidecar| sidecar.suffix));
    for suffix in suffixes {
        let legacy = directory.join(format!("{stem}{suffix}"));
        if legacy.is_file() && !stale.contains(&legacy) {
            stale.push(legacy);
        }
    }
    Ok(stale)
}

fn is_older(path: &Path, than: &Path) -> io::Result<bool> {
    let modified = |path: &Path| fs::metadata(path)?.modified();
    Ok(modified(path)? < modified(than)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs::File;
    use std::time::Duration;

    const NAME: &str = "Game Name (repo)";
    const LICENCE: &str = ".SLICNSE.TXT";

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "psx-library-install-{label}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        fn cue(&self) -> PathBuf {
            self.0.join("dist/repo.cue")
        }

        fn library(&self) -> PathBuf {
            self.0.join("library")
        }

        /// A built disc in `dist/`.
        fn build(&self, bin: &[u8]) {
            let dist = self.0.join("dist");
            fs::create_dir_all(&dist).unwrap();
            fs::write(dist.join("repo.bin"), bin).unwrap();
            fs::write(
                dist.join("repo.cue"),
                "FILE repo.bin BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
            )
            .unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn source() -> Source {
        Source {
            repository: "EBonura/repo".into(),
            branch: Some("main".into()),
            commit: "abc".into(),
            dirty: false,
        }
    }

    fn licence() -> Sidecar<'static> {
        Sidecar {
            suffix: LICENCE,
            contents: b"licence",
        }
    }

    /// Install the disc built in `scratch` with the given sidecars and legacy stem `repo`.
    fn run(scratch: &Scratch, sidecars: &[Sidecar<'_>]) -> Result<Installed, Error> {
        install(
            &Request::new(&scratch.cue(), &scratch.library(), NAME, source())
                .with_legacy_stem("repo")
                .with_sidecars(sidecars),
        )
    }

    fn age(path: &Path, seconds: u64) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(seconds))
            .unwrap();
    }

    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn installs_under_the_library_name_with_a_quoted_cue_and_a_record() {
        let scratch = Scratch::new("fresh");
        scratch.build(b"disc");
        let installed = run(&scratch, &[licence()]).unwrap();
        let library = scratch.library();
        assert_eq!(
            names(&library),
            [
                "Game Name (repo).SLICNSE.TXT",
                "Game Name (repo).bin",
                "Game Name (repo).cue",
                "Game Name (repo).provenance.json",
            ]
        );
        assert_eq!(
            fs::read_to_string(library.join("Game Name (repo).cue")).unwrap(),
            "FILE \"Game Name (repo).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n"
        );
        assert_eq!(installed.cue, library.join("Game Name (repo).cue"));
        assert_eq!(installed.bin_sha256, sha256_bytes(b"disc"));
        assert_eq!(installed.replaced_bin_sha256, None);
        assert!(installed.removed.is_empty());
    }

    #[test]
    fn the_record_matches_the_files_that_were_installed() {
        let scratch = Scratch::new("record");
        scratch.build(b"disc");
        let installed = run(&scratch, &[]).unwrap();
        let record: Value =
            serde_json::from_slice(&fs::read(&installed.provenance).unwrap()).unwrap();
        let library = scratch.library();
        for (artifact, file) in [
            ("bin", "Game Name (repo).bin"),
            ("cue", "Game Name (repo).cue"),
        ] {
            assert_eq!(record["artifacts"][artifact]["file"], file);
            assert_eq!(
                record["artifacts"][artifact]["sha256"],
                sha256_file(&library.join(file)).unwrap()
            );
            assert_eq!(
                record["artifacts"][artifact]["bytes"],
                fs::metadata(library.join(file)).unwrap().len()
            );
        }
        assert_eq!(record["source"]["commit"], "abc");
        assert!(record["installed_at"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn replaces_the_live_disc_in_place_and_records_the_replaced_digest() {
        let scratch = Scratch::new("replace");
        scratch.build(b"first");
        run(&scratch, &[licence()]).unwrap();
        scratch.build(b"second");
        let second = run(&scratch, &[licence()]).unwrap();
        let library = scratch.library();
        assert_eq!(
            fs::read(library.join("Game Name (repo).bin")).unwrap(),
            b"second"
        );
        assert_eq!(names(&library).len(), 4, "{:?}", names(&library));
        assert_eq!(second.replaced_bin_sha256, Some(sha256_bytes(b"first")));
        let record: Value = serde_json::from_slice(&fs::read(&second.provenance).unwrap()).unwrap();
        assert_eq!(record["replaced"]["bin_sha256"], sha256_bytes(b"first"));
    }

    #[test]
    fn reinstalling_the_same_disc_records_no_replacement() {
        let scratch = Scratch::new("same");
        scratch.build(b"disc");
        run(&scratch, &[]).unwrap();
        let again = run(&scratch, &[]).unwrap();
        let record: Value = serde_json::from_slice(&fs::read(&again.provenance).unwrap()).unwrap();
        assert!(record.get("replaced").is_none(), "{record}");
    }

    #[test]
    fn removes_an_identical_or_older_legacy_set() {
        for (label, legacy_bin, older) in
            [("identical", &b"disc"[..], false), ("older", b"old", true)]
        {
            let scratch = Scratch::new(label);
            scratch.build(b"disc");
            let library = scratch.library();
            fs::create_dir_all(&library).unwrap();
            for (file, contents) in [
                ("repo.bin", legacy_bin),
                ("repo.cue", b"FILE repo.bin BINARY\n"),
                ("repo.provenance.json", b"{}"),
                ("repo.SLICNSE.TXT", b"licence"),
            ] {
                fs::write(library.join(file), contents).unwrap();
            }
            if older {
                age(&library.join("repo.bin"), 3600);
            }
            let installed = run(&scratch, &[licence()]).unwrap();
            assert_eq!(names(&library).len(), 4, "{label}: {:?}", names(&library));
            assert!(!library.join("repo.bin").exists(), "{label}");
            assert_eq!(installed.removed.len(), 4, "{label}");
        }
    }

    #[test]
    fn leaves_a_legacy_sidecar_this_install_does_not_replace() {
        let scratch = Scratch::new("orphan");
        scratch.build(b"disc");
        let library = scratch.library();
        fs::create_dir_all(&library).unwrap();
        fs::write(library.join("repo.SLICNSE.TXT"), b"licence").unwrap();
        run(&scratch, &[]).unwrap();
        assert!(library.join("repo.SLICNSE.TXT").exists());
    }

    #[test]
    fn refuses_a_newer_different_legacy_disc_and_writes_nothing() {
        let scratch = Scratch::new("newer");
        scratch.build(b"built disc");
        age(&scratch.0.join("dist/repo.bin"), 3600);
        let library = scratch.library();
        fs::create_dir_all(&library).unwrap();
        fs::write(library.join("repo.bin"), b"someone else's newer disc").unwrap();
        let error = run(&scratch, &[licence()]).unwrap_err();
        assert!(matches!(error, Error::LegacyDiscNotOlder(_)), "{error}");
        assert!(error.to_string().contains("not older"), "{error}");
        assert_eq!(names(&library), ["repo.bin"]);
    }

    #[test]
    fn a_name_that_is_not_a_file_stem_is_rejected_before_anything_is_written() {
        let scratch = Scratch::new("names");
        scratch.build(b"disc");
        for name in ["", ".hidden", "a/b", "a\\b", "../escape"] {
            let error = install(&Request::new(
                &scratch.cue(),
                &scratch.library(),
                name,
                source(),
            ))
            .unwrap_err();
            assert!(matches!(error, Error::InvalidName(_)), "{name:?}: {error}");
        }
        assert!(!scratch.library().exists());
    }

    #[test]
    fn a_cue_without_a_file_line_writes_nothing() {
        let scratch = Scratch::new("badcue");
        scratch.build(b"disc");
        fs::write(scratch.cue(), "TRACK 01 MODE2/2352\n").unwrap();
        let error = run(&scratch, &[]).unwrap_err();
        assert!(matches!(error, Error::CueWithoutFile(_)), "{error}");
        assert!(!scratch.library().exists());
    }

    #[test]
    fn a_missing_bin_is_an_error_naming_it() {
        let scratch = Scratch::new("nobin");
        scratch.build(b"disc");
        fs::remove_file(scratch.0.join("dist/repo.bin")).unwrap();
        let error = run(&scratch, &[]).unwrap_err();
        assert!(error.to_string().contains("repo.bin"), "{error}");
    }

    #[test]
    fn caller_fields_reach_the_record_without_overriding_the_install_facts() {
        let scratch = Scratch::new("base");
        scratch.build(b"disc");
        let base = json!({
            "gates": "journey 11/11",
            "artifacts": { "exe": { "file": "repo.exe" } },
        });
        let installed = install(
            &Request::new(&scratch.cue(), &scratch.library(), NAME, source())
                .with_provenance_base(&base),
        )
        .unwrap();
        let record: Value =
            serde_json::from_slice(&fs::read(&installed.provenance).unwrap()).unwrap();
        assert_eq!(record["gates"], "journey 11/11");
        assert_eq!(record["artifacts"]["exe"]["file"], "repo.exe");
        assert_eq!(record["artifacts"]["bin"]["file"], "Game Name (repo).bin");
    }

    #[test]
    fn the_cue_is_written_after_the_bin_and_every_sidecar() {
        // Make the sidecar write fail: the bin is already published, the cue must not be.
        let scratch = Scratch::new("order");
        scratch.build(b"disc");
        let library = scratch.library();
        fs::create_dir_all(library.join(".Game Name (repo).SLICNSE.TXT.installing")).unwrap();
        let error = run(&scratch, &[licence()]).unwrap_err();
        assert!(matches!(error, Error::Io { .. }), "{error}");
        assert!(library.join("Game Name (repo).bin").exists());
        assert!(!library.join("Game Name (repo).cue").exists());
    }

    #[test]
    fn timestamps_use_the_injected_clock() {
        let scratch = Scratch::new("clock");
        scratch.build(b"disc");
        let installed = install_at(
            &Request::new(&scratch.cue(), &scratch.library(), NAME, source()),
            SystemTime::UNIX_EPOCH + Duration::from_secs(86_400),
        )
        .unwrap();
        let record: Value =
            serde_json::from_slice(&fs::read(&installed.provenance).unwrap()).unwrap();
        assert_eq!(record["installed_at"], "1970-01-02T00:00:00Z");
    }
}
