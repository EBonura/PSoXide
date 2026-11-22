//! Results a verdict stands on, and the programs a disc image carries.
//!
//! A build may declare a members manifest (`[[build]] members`): the programs its image holds,
//! with each one's executable digest and link map. The harness writes `members.sym` beside it
//! (every member symbol as `<slug>:<name>`, which the gate and the adapters read as a symbol list),
//! the `requires` stage checks every member against the result the game file pins for it, and
//! journey templates are played once per member.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::game::Requires;
use crate::report::{Check, Outcome, Source, StageResult};
use crate::stage::Ctx;
use crate::util;

/// The member symbol list the harness writes beside the manifest.
pub const MEMBER_SYMBOLS: &str = "members.sym";

/// One program of a disc image, as the image's build describes it.
#[derive(Debug, Deserialize, Clone)]
pub struct Member {
    /// Position on the launcher's shelf (TOC order, 0-based).
    pub index: u32,
    pub id: String,
    /// SHA-256 of the executable the image carries for this member.
    pub exe_sha256: String,
    /// SHA-256 of the member's own disc image the lineup took (embedded whole, or the source of
    /// the executable for a member that is embedded as an executable only).
    #[serde(default)]
    pub bin_sha256: Option<String>,
    /// The member's link map, relative to the manifest's directory (or absolute).
    #[serde(default)]
    pub map: Option<String>,
    /// Words of the member's text no other program holds at that address: `[address, value]`,
    /// both `0x` hex. Named `<slug>:word<N>` in `members.sym`, values `{word:N}` in templates.
    #[serde(default)]
    pub words: Vec<[String; 2]>,
}

impl Member {
    /// The symbol prefix: the id lowercased, every run of other characters one `-`.
    pub fn slug(&self) -> String {
        slug(&self.id)
    }
}

/// `CELESTE COLLECTION` -> `celeste-collection`.
pub fn slug(id: &str) -> String {
    let mut out = String::new();
    for c in id.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn hex(text: &str) -> Result<u32, String> {
    let digits = text.trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(digits, 16).map_err(|_| format!("`{text}` is not a 0x hex word"))
}

/// Read a manifest.
pub fn read_manifest(path: &Path) -> Result<Vec<Member>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let members: Vec<Member> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut slugs = std::collections::BTreeSet::new();
    for m in &members {
        if m.slug().is_empty() || !slugs.insert(m.slug()) {
            return Err(format!(
                "{}: member id `{}` is empty or not unique as a symbol prefix",
                path.display(),
                m.id
            ));
        }
        for [a, v] in &m.words {
            hex(a)?;
            hex(v)?;
        }
    }
    Ok(members)
}

/// Write `members.sym` beside the manifest: each member's map symbols as `<slug>:<name>`, `(.N)`
/// stripped, plus its signature words as `<slug>:word<N>`.
pub fn write_member_symbols(manifest: &Path, members: &[Member]) -> Result<PathBuf, String> {
    let dir = manifest.parent().unwrap_or(Path::new("."));
    let mut lines = std::collections::BTreeSet::new();
    for m in members {
        let slug = m.slug();
        if let Some(map) = &m.map {
            let path = dir.join(map);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("member `{}`: read its map {}: {e}", m.id, path.display()))?;
            for section in &psoxide_size::map::parse(&text).sections {
                for sym in &section.syms {
                    let name = super::build::strip_section_suffix(&sym.name);
                    if !name.contains(char::is_whitespace) {
                        lines.insert(format!("{:08x} {slug}:{name}", sym.vma));
                    }
                }
            }
        }
        for (n, [addr, _]) in m.words.iter().enumerate() {
            lines.insert(format!("{:08x} {slug}:word{n}", hex(addr)?));
        }
    }
    let mut body = String::new();
    for line in lines {
        body.push_str(&line);
        body.push('\n');
    }
    let out = dir.join(MEMBER_SYMBOLS);
    std::fs::write(&out, body).map_err(|e| format!("write {}: {e}", out.display()))?;
    Ok(out)
}

/// The manifest of the build that declares one, when it exists.
pub fn manifest(ctx: &Ctx<'_>) -> Option<Result<Vec<Member>, String>> {
    let build = ctx.game.builds.iter().find(|b| b.members.is_some())?;
    let file = build.members.as_deref()?;
    Some(read_manifest(&build.out_dir(&ctx.repo).join(file)))
}

/// Where a result file of this content hash is: this config's work tree, then `[results]` dirs.
pub fn find_result(ctx: &Ctx<'_>, hash: &str) -> Result<PathBuf, String> {
    let name = format!("{hash}.json");
    let mut roots = vec![ctx.config.work.clone()];
    roots.extend(ctx.config.results.iter().cloned());
    for root in &roots {
        let direct = root.join(&name);
        if direct.is_file() {
            return Ok(direct);
        }
        let Ok(games) = std::fs::read_dir(root) else {
            continue;
        };
        for game in games.flatten() {
            let candidate = game.path().join("results").join(&name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(format!(
        "no result file {} under {}: add the work tree that holds it to `results` in the \
         psoxide-test config",
        util::short(hash),
        roots
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The parts of a member's result the requires stage judges.
#[derive(Debug, Deserialize)]
struct MemberResult {
    game: String,
    commit: String,
    tier: String,
    verdict: String,
    #[serde(default)]
    landing_missing: Vec<String>,
    /// Absent = install (results written before profiles, and every run without `--profile`).
    #[serde(default)]
    profile: crate::stage::Profile,
    stages: Vec<MemberStage>,
}

#[derive(Debug, Deserialize)]
struct MemberStage {
    name: String,
    outcome: MemberOutcome,
}

#[derive(Debug, Deserialize, Default)]
struct MemberOutcome {
    #[serde(default)]
    facts: BTreeMap<String, String>,
}

/// Judge one `[[requires]]` entry against the image's member. `Ok` is the passing value.
///
/// `profile` is the image's own run's: an install-profile image refuses a landing-profile member
/// (that member never proved itself on every model or on the strict speed bar).
pub fn judge(
    req: &Requires,
    member: &Member,
    result_path: &Path,
    profile: crate::stage::Profile,
) -> Result<String, String> {
    let bytes =
        std::fs::read(result_path).map_err(|e| format!("read {}: {e}", result_path.display()))?;
    let digest = util::sha256_bytes(&bytes);
    if !digest.eq_ignore_ascii_case(&req.result) {
        return Err(format!(
            "{} hashes to {}, not the {} its name and [[requires]] give: the file was changed",
            result_path.display(),
            util::short(&digest),
            util::short(&req.result)
        ));
    }
    let r: MemberResult = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{}: not a result file: {e}", result_path.display()))?;
    if r.tier != "full" || r.verdict != "PASS" || !r.landing_missing.is_empty() {
        return Err(format!(
            "{} at {} is tier {}, verdict {}, landing {}: a member needs a complete full PASS",
            r.game,
            util::short(&r.commit),
            r.tier,
            r.verdict,
            if r.landing_missing.is_empty() {
                "complete".to_string()
            } else {
                format!("missing {}", r.landing_missing.join(", "))
            }
        ));
    }
    if profile.is_install() && !r.profile.is_install() {
        return Err(format!(
            "{} at {} is a {}-profile result: an install-profile image needs each member's \
             install-profile full PASS",
            r.game,
            util::short(&r.commit),
            r.profile.name()
        ));
    }
    let judged = r
        .stages
        .iter()
        .find(|s| s.name == "build")
        .and_then(|s| s.outcome.facts.get(&req.file))
        .ok_or_else(|| {
            format!(
                "{}'s result names no `{}` among its build facts (file is `<build>/<file>`)",
                r.game, req.file
            )
        })?;
    // A judged disc image compares with the member's image, anything else with its executable.
    let (carried, what) = if req.file.ends_with(".bin") {
        let bin = member.bin_sha256.as_deref().ok_or_else(|| {
            format!(
                "[[requires]] `{}` names a disc image ({}) but the manifest gives no bin_sha256 \
                 for this member",
                req.id, req.file
            )
        })?;
        (bin, "disc image")
    } else {
        (member.exe_sha256.as_str(), "executable")
    };
    if !judged.eq_ignore_ascii_case(carried) {
        return Err(format!(
            "the image carries {what} {} for `{}` but {} at {} judged {} as {}: rebuild the image \
             from the judged bytes, or pin the result that judged these",
            util::short(carried),
            member.id,
            r.game,
            util::short(&r.commit),
            req.file,
            util::short(judged)
        ));
    }
    Ok(format!(
        "{} at {}: full PASS, {} {} is member {} on the image",
        r.game,
        util::short(&r.commit),
        req.file,
        util::short(judged),
        member.index
    ))
}

/// The requires stage: every member of the image is the bytes a complete full PASS judged, and
/// every member has an entry. Not receipted: it only reads files, in well under a second.
pub fn run(ctx: &Ctx<'_>) -> StageResult {
    let mut checks = Vec::new();
    let mut facts = BTreeMap::new();
    match manifest(ctx) {
        None => checks.push(Check::fail(
            "members",
            "[[requires]] needs a [[build]] that declares `members`",
        )),
        Some(Err(e)) => checks.push(Check::fail("members", e)),
        Some(Ok(members)) => {
            for req in &ctx.game.requires {
                let name = format!("requires {}", req.id);
                let Some(member) = members.iter().find(|m| m.id == req.id) else {
                    checks.push(Check::fail(
                        name,
                        format!(
                            "the manifest has no member `{}` (it lists {})",
                            req.id,
                            members
                                .iter()
                                .map(|m| m.id.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    ));
                    continue;
                };
                let verdict =
                    find_result(ctx, &req.result).and_then(|p| judge(req, member, &p, ctx.profile));
                match verdict {
                    Ok(value) => {
                        facts.insert(format!("requires/{}", req.id), req.result.clone());
                        checks.push(Check::pass(name, value));
                    }
                    Err(e) => checks.push(Check::fail(name, e)),
                }
            }
            for m in &members {
                if !ctx.game.requires.iter().any(|r| r.id == m.id) {
                    checks.push(Check::fail(
                        format!("requires {}", m.id),
                        format!(
                            "member {} `{}` is on the image but no [[requires]] pins its result",
                            m.index, m.id
                        ),
                    ));
                }
            }
        }
    }
    StageResult {
        name: "requires".into(),
        source: Source::Run,
        key: String::new(),
        outcome: Outcome { checks, facts },
    }
}

/// Substitute a template for one member. `{repo}` is the absolute repo root, so the generated
/// file (which lives outside tests/) names its disc and symbols absolutely.
/// Lines between `# {repeat}` and `# {end}` are written once for each k in 1..=index, with `{k}`
/// substituted: what moving the shelf focus from the first program to this one takes.
pub fn substitute(template: &str, member: &Member, repo: &Path) -> Result<String, String> {
    let mut expanded = String::new();
    let mut block: Option<Vec<&str>> = None;
    for line in template.lines() {
        match (line.trim(), block.as_mut()) {
            ("# {repeat}", None) => block = Some(Vec::new()),
            ("# {repeat}", Some(_)) => return Err("`# {repeat}` blocks do not nest".into()),
            ("# {end}", Some(lines)) => {
                for k in 1..=member.index {
                    for l in lines.iter() {
                        expanded.push_str(&l.replace("{k}", &k.to_string()));
                        expanded.push('\n');
                    }
                }
                block = None;
            }
            ("# {end}", None) => return Err("`# {end}` without `# {repeat}`".into()),
            (_, Some(lines)) => lines.push(line),
            (_, None) => {
                expanded.push_str(line);
                expanded.push('\n');
            }
        }
    }
    if block.is_some() {
        return Err("`# {repeat}` without `# {end}`".into());
    }
    let mut out = expanded
        .replace("{member}", &member.slug())
        .replace("{id}", &member.id)
        .replace("{index}", &member.index.to_string())
        .replace("{repo}", &repo.display().to_string());
    for (n, [_, value]) in member.words.iter().enumerate() {
        out = out.replace(&format!("{{word:{n}}}"), &format!("{:#x}", hex(value)?));
    }
    if let Some(at) = out.find("{word:") {
        return Err(format!(
            "member `{}` has {} signature words; the template asks for `{}`",
            member.id,
            member.words.len(),
            out[at..].split('}').next().unwrap_or("")
        ));
    }
    Ok(out)
}

/// A template filled with a stand-in member (index 0, eight signature words), written to a
/// private temporary directory, so `lint` can read it before any image is built.
pub fn lint_copy(template: &Path) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(template)
        .map_err(|e| format!("read {}: {e}", template.display()))?;
    let stand_in = Member {
        index: 0,
        id: "member".into(),
        exe_sha256: String::new(),
        bin_sha256: None,
        map: None,
        words: vec![["0x80010000".into(), "0x0".into()]; 8],
    };
    let dir = std::env::temp_dir().join(format!("psoxide-test-lint-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let file = dir.join(template.file_name().unwrap_or_default());
    let filled = substitute(&text, &stand_in, Path::new("/repo"))?;
    std::fs::write(&file, filled).map_err(|e| format!("write {}: {e}", file.display()))?;
    Ok(file)
}

/// Expand `paths`: a file whose stem is a template becomes one generated file per member, in
/// `[[requires]]` order, written under `<work>/instances/`; other files stay as they are.
pub fn expand(
    ctx: &Ctx<'_>,
    paths: &[PathBuf],
    templates: &[String],
) -> Result<Vec<(PathBuf, Option<String>)>, String> {
    let stem = |p: &Path| {
        p.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    if !paths.iter().any(|p| templates.contains(&stem(p))) {
        return Ok(paths.iter().map(|p| (p.clone(), None)).collect());
    }
    let members = match manifest(ctx) {
        Some(m) => m?,
        None => return Err("journey instances need a [[build]] that declares `members`".into()),
    };
    let dir = ctx.dir(&["instances"])?;
    let mut out = Vec::new();
    for path in paths {
        let name = stem(path);
        if !templates.contains(&name) {
            out.push((path.clone(), None));
            continue;
        }
        let template =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let template_digest = util::sha256_bytes(template.as_bytes());
        for req in &ctx.game.requires {
            let member = members.iter().find(|m| m.id == req.id).ok_or_else(|| {
                format!("[[requires]] `{}` is not in the image's manifest", req.id)
            })?;
            let text = substitute(&template, member, &ctx.repo)?;
            let file = dir.join(format!("{name}-{}.toml", member.slug()));
            std::fs::write(&file, &text).map_err(|e| format!("write {}: {e}", file.display()))?;
            let keyed = substitute(&template, member, Path::new("<repo>"))?;
            out.push((
                file,
                Some(format!(
                    "template {template_digest} {}",
                    util::sha256_bytes(keyed.as_bytes())
                )),
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member() -> Member {
        Member {
            index: 3,
            id: "WIPEOUT PSoXide".into(),
            exe_sha256: "ab".repeat(32),
            bin_sha256: Some("ef".repeat(32)),
            map: Some("wo.map".into()),
            words: vec![["0x80011420".into(), "0xafa10010".into()]],
        }
    }

    #[test]
    fn slugs_are_lowercase_with_single_dashes() {
        assert_eq!(slug("CELESTE COLLECTION"), "celeste-collection");
        assert_eq!(slug("Quake (shareware)"), "quake-shareware");
        assert_eq!(slug("360X"), "360x");
    }

    #[test]
    fn templates_take_the_member_and_refuse_a_missing_word() {
        let t = "name = \"chain-{member}\"\ndisc = \"{repo}/b/d.cue\"\nv = {index}\nw = {word:0}\n";
        let text = substitute(t, &member(), Path::new("/r")).unwrap();
        assert!(text.contains("chain-wipeout-psoxide"));
        assert!(text.contains("\"/r/b/d.cue\""));
        assert!(text.contains("v = 3") && text.contains("w = 0xafa10010"));
        let r = "a\n# {repeat}\nk = {k}\n# {end}\nb\n";
        assert_eq!(
            substitute(r, &member(), Path::new("/r")).unwrap(),
            "a\nk = 1\nk = 2\nk = 3\nb\n"
        );
        assert!(substitute("# {repeat}\n", &member(), Path::new("/r")).is_err());
        assert!(substitute("{word:1}", &member(), Path::new("/r"))
            .unwrap_err()
            .contains("word:1"));
    }

    #[test]
    fn member_symbols_carry_the_prefix_and_the_words() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("wo.map"),
            "             VMA      LMA     Size Align Out     In      Symbol\n\
             80010000 80010000      100     4 .text\n\
             80010000 80010000      100     4         a.o:(.text)\n\
             80010010 80010010        0     1                 __psx_rt_vblank_count\n",
        )
        .unwrap();
        let manifest = dir.path().join("members.json");
        let sym = write_member_symbols(&manifest, &[member()]).unwrap();
        let text = std::fs::read_to_string(sym).unwrap();
        assert!(text.contains("80011420 wipeout-psoxide:word0"));
    }

    #[test]
    fn a_member_result_must_be_a_complete_full_pass_of_the_same_bytes() {
        use crate::stage::Profile;
        let dir = tempfile::tempdir().unwrap();
        let write = |body: &str| {
            let digest = util::sha256_bytes(body.as_bytes());
            let path = dir.path().join(format!("{digest}.json"));
            std::fs::write(&path, body).unwrap();
            (digest, path)
        };
        let exe = "ab".repeat(32);
        let good = format!(
            r#"{{"game":"W","commit":"c0ffee","tier":"full","verdict":"PASS","landing_missing":[],
            "stages":[{{"name":"build","outcome":{{"facts":{{"ship/wo.exe":"{exe}"}}}}}}]}}"#
        );
        let (digest, path) = write(&good);
        let req = Requires {
            id: "WIPEOUT PSoXide".into(),
            result: digest.clone(),
            file: "ship/wo.exe".into(),
        };
        assert!(judge(&req, &member(), &path, Profile::Install)
            .unwrap()
            .contains("member 3"));
        let mut other = member();
        other.exe_sha256 = "cd".repeat(32);
        assert!(judge(&req, &other, &path, Profile::Install)
            .unwrap_err()
            .contains("the image carries"));
        let bin_good = good
            .replace("ship/wo.exe", "ship/wo.bin")
            .replace(&exe, &"ef".repeat(32));
        let (bd, bp) = write(&bin_good);
        let bin_req = Requires {
            result: bd,
            file: "ship/wo.bin".into(),
            ..req.clone()
        };
        assert!(judge(&bin_req, &member(), &bp, Profile::Install).is_ok());
        let quick = good.replace("\"full\"", "\"quick\"");
        let (qd, qp) = write(&quick);
        let req_quick = Requires {
            result: qd,
            ..req.clone()
        };
        assert!(judge(&req_quick, &member(), &qp, Profile::Install)
            .unwrap_err()
            .contains("complete full PASS"));
        let landing = good.replace("\"tier\"", "\"profile\":\"landing\",\"tier\"");
        let (ld, lp) = write(&landing);
        let req_landing = Requires {
            result: ld,
            ..req.clone()
        };
        assert!(judge(&req_landing, &member(), &lp, Profile::Install)
            .unwrap_err()
            .contains("landing-profile result"));
        assert!(judge(&req_landing, &member(), &lp, Profile::Landing).is_ok());
        std::fs::write(&path, good.replace("c0ffee", "c0ffef")).unwrap();
        assert!(judge(&req, &member(), &path, Profile::Install)
            .unwrap_err()
            .contains("was changed"));
    }
}
