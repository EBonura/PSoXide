//! Keep only the hot functions of a sample profile.
//!
//! A game built with `-profile-sample-accurate` treats a function the profile does not name as
//! cold and optimises it for size. A profile that names every function the replays touched makes
//! LLVM inline and unroll hot callees into dozens of callers (Half-Life, 2026-10-15: +47 KB of
//! `.text` and `play()` from 56 KB to 128 KB over a stale profile, for +19 to +34% frame rate).
//! The samples are concentrated (the top 33 of 561 functions held 80%), so a shipped profile
//! keeps the functions that cover most of the samples and leaves the rest unnamed. Functions that
//! only wait (CD, vblank, controller, SPU upload) or only run while loading are hot in the
//! histogram but not in the frame: `--cold NAME` drops them first and leaves them out of the
//! share the coverage is counted against.

use crate::Result;

/// What a coverage prune kept.
#[derive(Debug, PartialEq, Eq)]
pub struct Kept {
    /// Function blocks written.
    pub functions: usize,
    /// Function blocks in the input profile.
    pub total: usize,
    /// Share of the samples of the functions that are not cold which the kept functions hold, in
    /// permille.
    pub covered_permille: u64,
}

/// The `--coverage PERMILLE` and `--cold NAME` flags of `flags`, and the flags left. `--cold`
/// repeats; a name matches every function whose name contains it. `None` when `--coverage` is
/// absent (and then `--cold` alone is an error: it would do nothing).
pub fn take_flags(flags: &[String]) -> Result<(Option<Selection>, Vec<String>)> {
    let (mut permille, mut cold, mut rest) = (None, Vec::new(), Vec::new());
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        match flag.as_str() {
            "--coverage" => {
                let value = flags.next().and_then(|value| value.parse::<u64>().ok());
                match value {
                    Some(value) if value <= 1000 => permille = Some(value),
                    _ => {
                        return Err(
                            "--coverage wants a share of the samples in permille, 0 to 1000".into(),
                        )
                    }
                }
            }
            "--cold" => match flags.next() {
                Some(name) if !name.is_empty() => cold.push(name.clone()),
                _ => return Err("--cold wants a function name fragment".into()),
            },
            _ => rest.push(flag.clone()),
        }
    }
    match (permille, cold.is_empty()) {
        (Some(permille), _) => Ok((Some(Selection { permille, cold }), rest)),
        (None, true) => Ok((None, rest)),
        (None, false) => Err("--cold needs --coverage".into()),
    }
}

/// Which functions a coverage prune keeps.
#[derive(Debug, PartialEq, Eq)]
pub struct Selection {
    /// Stop taking functions once they hold this share of the samples.
    pub permille: u64,
    /// Name fragments of functions that are never kept and do not count towards the share.
    pub cold: Vec<String>,
}

/// One top-level function of a profile: the header line `name:total:head` and every line under it.
struct Block<'a> {
    name: &'a str,
    total: u64,
    lines: Vec<&'a str>,
}

/// Top-level function blocks of an AutoFDO text profile. Comment lines (`#`) and blank lines
/// before the first block belong to no function.
fn blocks(text: &str) -> Vec<Block<'_>> {
    let mut blocks: Vec<Block<'_>> = Vec::new();
    for line in text.lines() {
        if !line.starts_with(' ') && !line.starts_with('#') && !line.is_empty() {
            let mut fields = line.rsplitn(3, ':');
            let _head = fields.next();
            let total = fields
                .next()
                .and_then(|total| total.parse().ok())
                .unwrap_or(0);
            let name = fields.next().unwrap_or("");
            blocks.push(Block {
                name,
                total,
                lines: vec![line],
            });
        } else if let Some(block) = blocks.last_mut() {
            block.lines.push(line);
        }
    }
    blocks
}

/// `text` with only its hottest functions, in their original order.
///
/// Functions are taken by descending samples (ties in file order), skipping any whose name
/// contains a `cold` fragment, until they hold `selection.permille` of the samples of the
/// functions that are not cold. The `#` lines a profile starts with (the source record
/// `psoxide-pgo stamp` writes) are kept, or `rebind` would refuse the result as a profile with no
/// record. Nested lines travel with their function.
pub fn keep_hottest(text: &str, selection: &Selection) -> (String, Kept) {
    let blocks = blocks(text);
    let is_cold = |name: &str| {
        selection
            .cold
            .iter()
            .any(|fragment| name.contains(fragment.as_str()))
    };
    let all: u64 = blocks
        .iter()
        .filter(|block| !is_cold(block.name))
        .map(|block| block.total)
        .sum();
    let mut order: Vec<usize> = (0..blocks.len()).collect();
    order.sort_by(|&a, &b| blocks[b].total.cmp(&blocks[a].total).then(a.cmp(&b)));
    let mut keep = vec![false; blocks.len()];
    let mut covered = 0u64;
    for index in order {
        if covered * 1000 >= all * selection.permille {
            break;
        }
        if is_cold(blocks[index].name) {
            continue;
        }
        keep[index] = true;
        covered += blocks[index].total;
    }
    let mut out = String::new();
    for line in text.lines().take_while(|line| line.starts_with('#')) {
        out.push_str(line);
        out.push('\n');
    }
    let mut functions = 0;
    for (block, keep) in blocks.iter().zip(&keep) {
        if *keep {
            functions += 1;
            for line in &block.lines {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    let covered_permille = (covered * 1000).checked_div(all).unwrap_or(0);
    (
        out,
        Kept {
            functions,
            total: blocks.len(),
            covered_permille,
        },
    )
}

/// The one-line report `hottest` and `rebind --coverage` print.
pub fn report(kept: &Kept) -> String {
    format!(
        "coverage prune kept the hottest {} of {} functions, {}.{}% of the samples",
        kept.functions,
        kept.total,
        kept.covered_permille / 10,
        kept.covered_permille % 10
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = "\
hot:600:0
 1: 600
cd_read:300:0
 1: 300
warm:90:0
 1: other:50
  2: 50
 3: 40
cold:10:0
 1: 10
";

    fn select(permille: u64, cold: &[&str]) -> Selection {
        Selection {
            permille,
            cold: cold.iter().map(ToString::to_string).collect(),
        }
    }

    /// Half-Life's own copy (`host/hl-build/profile_prune.rs`, `prune`), verbatim apart from the
    /// names and the `checked_div` clippy asks for, kept as the oracle for the shared one.
    mod half_life {
        pub fn prune(text: &str, coverage_permille: u64, cold: &[&str]) -> (String, usize, u64) {
            fn blocks(text: &str) -> Vec<(String, u64, Vec<&str>)> {
                let mut blocks: Vec<(String, u64, Vec<&str>)> = Vec::new();
                for line in text.lines() {
                    if !line.starts_with(' ') && !line.starts_with('#') && !line.is_empty() {
                        let mut fields = line.rsplitn(3, ':');
                        let _head = fields.next();
                        let total = fields.next().and_then(|t| t.parse().ok()).unwrap_or(0);
                        let name = fields.next().unwrap_or("").to_string();
                        blocks.push((name, total, vec![line]));
                    } else if let Some(block) = blocks.last_mut() {
                        block.2.push(line);
                    }
                }
                blocks
            }
            let blocks = blocks(text);
            let is_cold = |name: &str| cold.iter().any(|fragment| name.contains(fragment));
            let all: u64 = blocks
                .iter()
                .filter(|block| !is_cold(&block.0))
                .map(|block| block.1)
                .sum();
            let mut order: Vec<usize> = (0..blocks.len()).collect();
            order.sort_by(|&a, &b| blocks[b].1.cmp(&blocks[a].1).then(a.cmp(&b)));
            let mut keep = vec![false; blocks.len()];
            let mut covered = 0u64;
            for index in order {
                if covered * 1000 >= all * coverage_permille {
                    break;
                }
                if is_cold(&blocks[index].0) {
                    continue;
                }
                keep[index] = true;
                covered += blocks[index].1;
            }
            let mut out = String::new();
            for line in text.lines().take_while(|line| line.starts_with('#')) {
                out.push_str(line);
                out.push('\n');
            }
            let mut kept = 0;
            for (index, block) in blocks.iter().enumerate() {
                if keep[index] {
                    kept += 1;
                    for line in &block.2 {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
            }
            let covered_permille = (covered * 1000).checked_div(all).unwrap_or(0);
            (out, kept, covered_permille)
        }
    }

    #[test]
    fn keeps_the_hottest_until_covered_and_skips_cold_names() {
        // Without cd_read the samples are 700: hot (600) is 857 permille of them, so 700 is
        // reached by hot alone; at 990 warm and cold join.
        let (_, kept) = keep_hottest(PROFILE, &select(700, &["cd_read"]));
        assert_eq!(kept.functions, 1);
        let (text, kept) = keep_hottest(PROFILE, &select(990, &["cd_read"]));
        assert_eq!(kept.functions, 3);
        assert!(
            text.contains("hot:600:0") && text.contains("warm:90:0") && text.contains("cold:10:0")
        );
        assert!(!text.contains("cd_read"));
        assert!(text.find("hot").unwrap() < text.find("warm").unwrap());
        assert!(text.contains("  2: 50"));
    }

    #[test]
    fn stops_once_the_coverage_is_reached() {
        let (_, kept) = keep_hottest(PROFILE, &select(600, &[]));
        assert_eq!(kept.functions, 1);
        assert_eq!(kept.covered_permille, 600);
        assert_eq!(kept.total, 4);
    }

    #[test]
    fn the_source_record_a_profile_starts_with_survives() {
        let stamped =
            format!("# psoxide-pgo source: abc\n# psoxide-pgo file: 1 game/src/main.rs\n{PROFILE}");
        let (text, _) = keep_hottest(&stamped, &select(800, &[]));
        assert!(
            text.starts_with("# psoxide-pgo source: abc\n# psoxide-pgo file: 1 game/src/main.rs\n")
        );
        assert!(text.contains("hot:600:0"));
    }

    #[test]
    fn an_empty_profile_keeps_nothing() {
        let (text, kept) = keep_hottest("", &select(800, &[]));
        assert!(text.is_empty());
        assert_eq!(kept.functions, 0);
    }

    #[test]
    fn full_coverage_keeps_every_function_that_is_not_cold() {
        let (text, kept) = keep_hottest(PROFILE, &select(1000, &["cd_read"]));
        assert_eq!(kept.functions, 3);
        assert_eq!(kept.covered_permille, 1000);
        assert!(!text.contains("cd_read"));
        let (all, _) = keep_hottest(PROFILE, &select(1000, &[]));
        assert_eq!(all, PROFILE);
    }

    #[test]
    fn equal_samples_are_taken_in_file_order() {
        let tied = "a:10:0\n 1: 10\nb:10:0\n 1: 10\nc:10:0\n 1: 10\n";
        let (text, kept) = keep_hottest(tied, &select(400, &[]));
        assert_eq!(kept.functions, 2);
        assert_eq!(text, "a:10:0\n 1: 10\nb:10:0\n 1: 10\n");
    }

    /// A profile shaped like Half-Life's committed one: a stamp record, mangled and portable
    /// names, inlined callees, names with colons and an `@crate` suffix, CD and stream functions
    /// at the top of the histogram, a long cold tail.
    fn half_life_shaped_profile() -> String {
        let mut text = String::from(
            "# psoxide-pgo source: 1db5ed192d6b771cd05c733f3ebc5e81c9ec24c3\n\
             # psoxide-pgo file: d55c9c640bc5621018aca028ab0ee77b1b51f545 game/src/cdstream.rs\n",
        );
        let heads = [
            ("hl_psx::main", 9000),
            ("_ZN6psx_io2cd10read_sector17h0123456789abcdefE", 8000),
            ("psx_goldsrc::render::draw_world @psx_goldsrc", 6500),
            ("hl_psx::chunk_stream::pump", 5000),
            ("psx_gpu::prim::RectFlat::new @psx_gpu", 4200),
            ("hl_psx::cdstream::poll", 3900),
            ("psx_pack::Fnv32::update @psx_pack", 3000),
            ("hl_psx::SectorReader::next", 2500),
            ("psx_gte::scene::project @psx_gte", 2200),
            ("hl_psx::logic_state::tick", 1800),
        ];
        for (name, total) in heads {
            text.push_str(&format!("{name}:{total}:3\n 1: {total}\n"));
            text.push_str(&format!(
                " 2: psx_math::int32::mul_q12_i32 @psx_math:{}\n  1: {}\n",
                total / 4,
                total / 4
            ));
        }
        for tail in 0..40u64 {
            text.push_str(&format!(
                "hl_psx::tail{tail}:{}:0\n 1: {}\n",
                100 - tail,
                100 - tail
            ));
        }
        text
    }

    #[test]
    fn matches_half_lifes_own_prune_on_a_profile_of_its_shape() {
        let text = half_life_shaped_profile();
        let cold = [
            "6psx_io2cd",
            "chunk_stream",
            "cdstream",
            "SectorReader",
            "psx_pack",
        ];
        for permille in [0, 500, 750, 800, 990, 1000] {
            let (ours, kept) = keep_hottest(&text, &select(permille, &cold));
            let (theirs, functions, covered_permille) = half_life::prune(&text, permille, &cold);
            assert_eq!(ours, theirs, "output at {permille} permille");
            assert_eq!(kept.functions, functions);
            assert_eq!(kept.covered_permille, covered_permille);
        }
        let (ours, kept) = keep_hottest(&text, &select(750, &cold));
        assert!(ours.contains("hl_psx::main:9000:3"));
        assert!(!ours.contains("read_sector") && !ours.contains("cdstream::poll"));
        assert!(kept.covered_permille >= 750);
    }

    #[test]
    fn flags_parse_and_leave_the_others() {
        let flags: Vec<String> = [
            "--min-bound",
            "90",
            "--coverage",
            "750",
            "--cold",
            "a",
            "--cold",
            "b",
            "--prune",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        let (selection, rest) = take_flags(&flags).expect("flags");
        assert_eq!(selection, Some(select(750, &["a", "b"])));
        assert_eq!(rest, ["--min-bound", "90", "--prune"]);
        let (none, rest) = take_flags(&["--prune".to_string()]).expect("no coverage");
        assert_eq!((none, rest), (None, vec!["--prune".to_string()]));
    }

    #[test]
    fn bad_flags_are_refused() {
        let flags = |list: &[&str]| list.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert!(take_flags(&flags(&["--coverage"])).is_err());
        assert!(take_flags(&flags(&["--coverage", "many"])).is_err());
        assert!(take_flags(&flags(&["--coverage", "1001"])).is_err());
        assert!(take_flags(&flags(&["--cold"])).is_err());
        assert!(take_flags(&flags(&["--cold", "x"])).is_err());
    }
}
