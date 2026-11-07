//! The hot call-site ladder: how far a profile-guided game may inline.
//!
//! Profile-guided inlining trades code size for speed through one number, the
//! hot call-site threshold. A game states the RAM it must keep free; this
//! module finds the thresholds that link within that floor and, instead of
//! shipping the first one, benches the best few and ships the fastest.
//!
//! Free RAM is close to monotone in the threshold (more inlining, bigger
//! code), so [`search`] binary-searches a fine grid for the highest threshold
//! that fits, then links the next fitting grid points below it so every
//! candidate is verified, never assumed. Speed is not monotone (hl-psx
//! measured a lower rung ahead of a higher one on one window), which is why
//! the candidates are benched. The pure logic here takes a [`Rig`] so it
//! tests without a compiler or an emulator.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::Result;

/// What a game states, as data.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Highest threshold tried.
    pub from: u32,
    /// Lowest threshold tried.
    pub to: u32,
    /// Grid spacing. Small enough that one step costs well under a percent.
    pub step: u32,
    /// Free RAM every candidate must leave, in bytes.
    pub floor: u32,
    /// How many of the highest fitting thresholds to bench. 1 skips the bench.
    pub candidates: usize,
    /// Candidates within this percentage of the best score tie.
    pub tie_percent: f64,
    /// Link every grid point instead of binary searching.
    pub scan: bool,
    /// Scores of the build being replaced; a candidate more than
    /// `baseline_tolerance` percent under it on any metric is not eligible.
    pub baseline: Vec<(String, f64)>,
    pub baseline_tolerance: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            from: 1000,
            to: 250,
            step: 50,
            floor: 0,
            candidates: 4,
            tie_percent: 0.3,
            scan: false,
            baseline: Vec::new(),
            baseline_tolerance: 1.0,
        }
    }
}

impl Config {
    /// The grid, highest first.
    pub fn grid(&self) -> Result<Vec<u32>> {
        if self.step == 0 || self.from < self.to {
            return Err("the ladder wants --from >= --to and --step > 0".into());
        }
        if self.candidates == 0 {
            return Err("--candidates wants at least 1".into());
        }
        let mut grid = Vec::new();
        let mut at = self.from;
        loop {
            grid.push(at);
            match at.checked_sub(self.step) {
                Some(next) if next >= self.to => at = next,
                _ => break,
            }
        }
        Ok(grid)
    }
}

/// The two things the search needs from a game.
/// A link that held.
#[derive(Clone, Debug, PartialEq)]
pub struct Fit {
    pub free: u32,
    /// A fingerprint of the code. Neighbouring thresholds often link the
    /// very same code (free RAM moves in plateaus), and benching it twice
    /// buys nothing, so equal fingerprints count as one candidate.
    pub code: Option<String>,
}

pub trait Rig {
    /// Link at `threshold`: what it leaves, or `None` when it did not link
    /// or has no safe stack. The last link made is the one on disk.
    fn link(&mut self, threshold: u32) -> Result<Option<Fit>>;
    /// Bench the link for `threshold`: named scores, higher is better.
    fn bench(&mut self, threshold: u32, free: u32) -> Result<Vec<(String, f64)>>;
}

/// One grid point's outcome.
#[derive(Clone, Debug, PartialEq)]
pub struct Rung {
    pub threshold: u32,
    /// `None`: did not link or unsafe stack.
    pub free: Option<u32>,
    /// See [`Fit::code`].
    pub code: Option<String>,
    pub scores: Vec<(String, f64)>,
    /// Geometric mean of the per-metric ratios to the best candidate.
    pub geomean: Option<f64>,
    /// The worst per-metric ratio to the best candidate.
    pub worst: Option<(String, f64)>,
    /// Score over the baseline's, per metric, when a baseline is given.
    pub vs_baseline: Vec<(String, f64)>,
}

impl Rung {
    pub fn fits(&self, floor: u32) -> bool {
        self.free.is_some_and(|free| free >= floor)
    }
}

#[derive(Debug)]
pub struct Outcome {
    /// Every rung that was linked, highest first.
    pub rungs: Vec<Rung>,
    /// The benched candidates' thresholds, highest first.
    pub candidates: Vec<u32>,
    pub chosen: u32,
    pub warnings: Vec<String>,
}

/// Find the candidates and pick one. Links through `rig`, memoised.
pub fn search(rig: &mut dyn Rig, config: &Config) -> Result<Outcome> {
    let grid = config.grid()?;
    let mut linked: BTreeMap<u32, Option<Fit>> = BTreeMap::new();
    let floor = config.floor;
    let fits =
        |rig: &mut dyn Rig, linked: &mut BTreeMap<u32, Option<Fit>>, at: usize| -> Result<bool> {
            let threshold = grid[at];
            if let std::collections::btree_map::Entry::Vacant(slot) = linked.entry(threshold) {
                slot.insert(rig.link(threshold)?);
            }
            Ok(linked[&threshold]
                .as_ref()
                .is_some_and(|fit| fit.free >= floor))
        };
    // The first grid index that fits.
    let first = if config.scan {
        let mut first = None;
        for at in 0..grid.len() {
            if first.is_none() && fits(rig, &mut linked, at)? {
                first = Some(at);
            } else if first.is_some() {
                fits(rig, &mut linked, at)?;
            }
        }
        first
    } else if fits(rig, &mut linked, 0)? {
        Some(0)
    } else {
        // Invariant: `low` does not fit, `high` is untested or fits.
        let (mut low, mut high) = (0, grid.len());
        while high - low > 1 {
            let mid = low + (high - low) / 2;
            if fits(rig, &mut linked, mid)? {
                high = mid;
            } else {
                low = mid;
            }
        }
        (high < grid.len()).then_some(high)
    };
    let Some(first) = first else {
        return Err(format!(
            "no threshold from {} down to {} links with {} B of RAM free ({})",
            config.from,
            config.to,
            config.floor,
            describe(&linked)
        )
        .into());
    };
    // The K highest fitting grid points from there down, each verified, one
    // per distinct code.
    let mut candidates: Vec<u32> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (at, &threshold) in grid.iter().enumerate().skip(first) {
        if candidates.len() == config.candidates {
            break;
        }
        if !fits(rig, &mut linked, at)? {
            continue;
        }
        let code = linked[&threshold].as_ref().and_then(|fit| fit.code.clone());
        if let Some(code) = code {
            if seen.contains(&code) {
                continue;
            }
            seen.push(code);
        }
        candidates.push(threshold);
    }
    let mut warnings = Vec::new();
    let mut scored: BTreeMap<u32, Vec<(String, f64)>> = BTreeMap::new();
    if candidates.len() > 1 {
        for &threshold in &candidates {
            let free = linked[&threshold].as_ref().expect("candidates fit").free;
            let scores = rig.bench(threshold, free)?;
            if scores.is_empty() {
                return Err(
                    format!("the bench printed no `score NAME VALUE` for {threshold}").into(),
                );
            }
            scored.insert(threshold, scores);
        }
    }
    let mut rungs: Vec<Rung> = grid
        .iter()
        .filter_map(|&threshold| {
            linked.get(&threshold).map(|fit| Rung {
                threshold,
                free: fit.as_ref().map(|fit| fit.free),
                code: fit.as_ref().and_then(|fit| fit.code.clone()),
                scores: scored.get(&threshold).cloned().unwrap_or_default(),
                geomean: None,
                worst: None,
                vs_baseline: Vec::new(),
            })
        })
        .collect();
    let chosen = if scored.is_empty() {
        candidates[0]
    } else {
        rate(&mut rungs, &candidates)?;
        let mut eligible = candidates.clone();
        if !config.baseline.is_empty() {
            compare_baseline(&mut rungs, &candidates, config)?;
            let floor = 1.0 - config.baseline_tolerance / 100.0;
            eligible.retain(|threshold| {
                rungs
                    .iter()
                    .find(|rung| rung.threshold == *threshold)
                    .is_some_and(|rung| rung.vs_baseline.iter().all(|(_, ratio)| *ratio >= floor))
            });
            if eligible.is_empty() {
                return Err(format!(
                    "every candidate is more than {}% under the baseline on some metric:\n{}",
                    config.baseline_tolerance,
                    table(
                        &Outcome {
                            rungs: rungs.clone(),
                            candidates: candidates.clone(),
                            chosen: 0,
                            warnings: Vec::new(),
                        },
                        config
                    )
                )
                .into());
            }
        }
        let chosen = pick(&rungs, &eligible, config.tie_percent);
        let rung = rungs
            .iter()
            .find(|rung| rung.threshold == chosen)
            .expect("chosen");
        if let Some((name, ratio)) = &rung.worst {
            if *ratio < 0.99 {
                warnings.push(format!(
                    "the pick {chosen} is {:.1}% slower than the best candidate on {name}",
                    100.0 * (1.0 - ratio)
                ));
            }
        }
        chosen
    };
    Ok(Outcome {
        rungs,
        candidates,
        chosen,
        warnings,
    })
}

/// Fill in geomean and worst ratio for the benched rungs.
fn rate(rungs: &mut [Rung], candidates: &[u32]) -> Result<()> {
    let mut best: BTreeMap<String, f64> = BTreeMap::new();
    for rung in rungs
        .iter()
        .filter(|rung| candidates.contains(&rung.threshold))
    {
        for (name, value) in &rung.scores {
            let entry = best.entry(name.clone()).or_insert(*value);
            *entry = entry.max(*value);
        }
    }
    for rung in rungs
        .iter_mut()
        .filter(|rung| candidates.contains(&rung.threshold))
    {
        let mut log_sum = 0.0;
        let mut worst: Option<(String, f64)> = None;
        for (name, top) in &best {
            let value = rung
                .scores
                .iter()
                .find(|(other, _)| other == name)
                .map(|(_, value)| *value)
                .ok_or_else(|| format!("rung {} has no score {name}", rung.threshold))?;
            if *top <= 0.0 || value <= 0.0 {
                return Err(format!("score {name} must be positive").into());
            }
            let ratio = value / top;
            log_sum += ratio.ln();
            if worst.as_ref().is_none_or(|(_, w)| ratio < *w) {
                worst = Some((name.clone(), ratio));
            }
        }
        rung.geomean = Some((log_sum / best.len() as f64).exp());
        rung.worst = worst;
    }
    Ok(())
}

/// Each benched rung's score over the baseline's, per metric.
fn compare_baseline(rungs: &mut [Rung], candidates: &[u32], config: &Config) -> Result<()> {
    for rung in rungs
        .iter_mut()
        .filter(|rung| candidates.contains(&rung.threshold))
    {
        let mut ratios = Vec::new();
        for (name, base) in &config.baseline {
            let value = rung
                .scores
                .iter()
                .find(|(other, _)| other == name)
                .map(|(_, value)| *value)
                .ok_or_else(|| {
                    format!("the bench printed no score {name} (named by the baseline)")
                })?;
            if *base <= 0.0 {
                return Err(format!("baseline score {name} must be positive").into());
            }
            ratios.push((name.clone(), value / base));
        }
        rung.vs_baseline = ratios;
    }
    Ok(())
}

/// Best geomean; among those within `tie_percent` of it, the most free RAM
/// (then the higher threshold).
fn pick(rungs: &[Rung], candidates: &[u32], tie_percent: f64) -> u32 {
    let rated: Vec<&Rung> = rungs
        .iter()
        .filter(|rung| candidates.contains(&rung.threshold))
        .collect();
    let top = rated
        .iter()
        .filter_map(|rung| rung.geomean)
        .fold(0.0, f64::max);
    let cutoff = top * (1.0 - tie_percent / 100.0);
    rated
        .iter()
        .filter(|rung| rung.geomean.is_some_and(|g| g >= cutoff))
        .max_by_key(|rung| (rung.free, rung.threshold))
        .map(|rung| rung.threshold)
        .expect("a candidate is within the tie of the top")
}

fn describe(linked: &BTreeMap<u32, Option<Fit>>) -> String {
    linked
        .iter()
        .rev()
        .map(|(threshold, fit)| match fit {
            Some(fit) => format!("{threshold}: {} B", fit.free),
            None => format!("{threshold}: did not link"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The per-rung table a reviewer reads (and a commit message quotes).
pub fn table(outcome: &Outcome, config: &Config) -> String {
    let names: Vec<&str> = outcome
        .rungs
        .iter()
        .flat_map(|rung| rung.scores.iter().map(|(name, _)| name.as_str()))
        .fold(Vec::new(), |mut names, name| {
            if !names.contains(&name) {
                names.push(name);
            }
            names
        });
    let mut text = format!(
        "floor {} B, step {}, candidates {}\n",
        config.floor, config.step, config.candidates
    );
    let _ = write!(text, "threshold  free_B  fits");
    for name in &names {
        let _ = write!(text, "  {name}");
    }
    let _ = writeln!(text, "  geomean  worst");
    for rung in &outcome.rungs {
        let free = rung.free.map_or("-".to_string(), |free| free.to_string());
        let fits = if rung.free.is_none() {
            "no link"
        } else if rung.fits(config.floor) {
            "yes"
        } else {
            "under floor"
        };
        let _ = write!(text, "{:>9}  {free:>6}  {fits}", rung.threshold);
        for name in &names {
            match rung.scores.iter().find(|(other, _)| other == name) {
                Some((_, value)) => {
                    let _ = write!(text, "  {value:.3}");
                    if let Some((_, ratio)) =
                        rung.vs_baseline.iter().find(|(other, _)| other == name)
                    {
                        let _ = write!(text, " ({:+.2}%)", 100.0 * (ratio - 1.0));
                    }
                }
                None => text.push_str("  -"),
            }
        }
        match (&rung.geomean, &rung.worst) {
            (Some(g), Some((name, w))) => {
                let _ = write!(text, "  {g:.4}  {w:.4} ({name})");
            }
            _ => text.push_str("  -  -"),
        }
        if rung.threshold == outcome.chosen {
            text.push_str("  <- chosen");
        } else if rung.scores.is_empty() && rung.fits(config.floor) {
            let twin = outcome.rungs.iter().find(|other| {
                other.threshold > rung.threshold
                    && other.code.is_some()
                    && other.code == rung.code
                    && !other.scores.is_empty()
            });
            if let Some(twin) = twin {
                let _ = write!(text, "  same code as {}", twin.threshold);
            }
        }
        text.push('\n');
    }
    for warning in &outcome.warnings {
        let _ = writeln!(text, "warning: {warning}");
    }
    if !config.baseline.is_empty() {
        let _ = writeln!(
            text,
            "(percentages are against the baseline; a candidate more than {}% under it on any metric is not eligible)",
            config.baseline_tolerance
        );
    }
    let _ = writeln!(text, "chosen threshold {}", outcome.chosen);
    text
}

/// `score NAME VALUE` lines of a bench's output.
pub fn parse_scores(output: &str) -> Vec<(String, f64)> {
    output
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next()? == "score").then_some(())?;
            let name = words.next()?.to_string();
            let value = words.next()?.parse().ok()?;
            words.next().is_none().then_some((name, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Free RAM falls 100 B per 50 of threshold from 5000 at 250; speeds are given.
    struct Fake {
        links: Vec<u32>,
        benches: Vec<u32>,
        plateaus: bool,
        speeds: BTreeMap<u32, Vec<(&'static str, f64)>>,
    }

    impl Fake {
        fn new(speeds: &[(u32, &[(&'static str, f64)])]) -> Self {
            Self {
                links: Vec::new(),
                benches: Vec::new(),
                plateaus: false,
                speeds: speeds.iter().map(|(t, s)| (*t, s.to_vec())).collect(),
            }
        }
    }

    fn free_at(threshold: u32) -> u32 {
        5000 - (threshold - 250) / 50 * 100
    }

    impl Rig for Fake {
        fn link(&mut self, threshold: u32) -> Result<Option<Fit>> {
            self.links.push(threshold);
            Ok(Some(Fit {
                free: free_at(threshold),
                // Plateaus of identical code every 100 units, when asked for.
                code: self.plateaus.then(|| (threshold / 100).to_string()),
            }))
        }
        fn bench(&mut self, threshold: u32, _free: u32) -> Result<Vec<(String, f64)>> {
            self.benches.push(threshold);
            Ok(self.speeds[&threshold]
                .iter()
                .map(|(n, v)| (n.to_string(), *v))
                .collect())
        }
    }

    fn config(floor: u32) -> Config {
        Config {
            floor,
            ..Config::default()
        }
    }

    #[test]
    fn grid_steps_down_from_the_top() {
        let grid = config(0).grid().unwrap();
        assert_eq!(grid[..3], [1000, 950, 900]);
        assert_eq!(*grid.last().unwrap(), 250);
        assert!(Config {
            step: 0,
            ..config(0)
        }
        .grid()
        .is_err());
    }

    #[test]
    fn binary_search_links_few_rungs_and_verifies_every_candidate() {
        // floor 3600 B: fits where free >= 3600, i.e. threshold <= 250 + 50*14 = 950.
        let speeds: Vec<(u32, &[(&str, f64)])> = (0..=3)
            .map(|i| (950 - 50 * i, &[("a", 10.0)][..]))
            .collect();
        let mut rig = Fake::new(&speeds);
        let outcome = search(&mut rig, &config(3600)).unwrap();
        assert_eq!(outcome.candidates, [950, 900, 850, 800]);
        assert!(rig.links.len() < 15, "linked {:?}", rig.links);
        assert_eq!(rig.benches, [950, 900, 850, 800]);
    }

    #[test]
    fn the_bench_beats_first_fit() {
        let mut rig = Fake::new(&[
            (950, &[("a", 10.0), ("b", 20.0)]),
            (900, &[("a", 10.5), ("b", 20.4)]),
            (850, &[("a", 10.1), ("b", 19.0)]),
            (800, &[("a", 9.0), ("b", 19.0)]),
        ]);
        let outcome = search(&mut rig, &config(3600)).unwrap();
        assert_eq!(outcome.chosen, 900);
        assert!(outcome.warnings.is_empty());
    }

    #[test]
    fn ties_go_to_more_free_ram() {
        let mut rig = Fake::new(&[
            (950, &[("a", 10.00)]),
            (900, &[("a", 9.98)]),
            (850, &[("a", 9.97)]),
            (800, &[("a", 9.0)]),
        ]);
        // 850 is within 0.3% of 950 and frees the most RAM among the ties.
        assert_eq!(search(&mut rig, &config(3600)).unwrap().chosen, 850);
    }

    #[test]
    fn a_pick_that_loses_a_window_is_flagged() {
        let mut rig = Fake::new(&[
            (950, &[("a", 10.0), ("b", 20.0)]),
            (900, &[("a", 12.0), ("b", 18.0)]),
            (850, &[("a", 9.0), ("b", 15.0)]),
            (800, &[("a", 9.0), ("b", 15.0)]),
        ]);
        let outcome = search(&mut rig, &config(3600)).unwrap();
        assert_eq!(outcome.chosen, 900);
        assert!(outcome.warnings[0].contains("b"), "{:?}", outcome.warnings);
        assert!(table(&outcome, &config(3600)).contains("<- chosen"));
    }

    #[test]
    fn rungs_that_link_the_same_code_are_benched_once() {
        // The code changes every 100 units, so 900 links 950's code, 800 links 850's, and so on.
        let mut rig = Fake::new(&[
            (950, &[("a", 10.0)]),
            (850, &[("a", 10.2)]),
            (750, &[("a", 9.0)]),
            (650, &[("a", 9.5)]),
        ]);
        rig.plateaus = true;
        let outcome = search(&mut rig, &config(3600)).unwrap();
        assert_eq!(outcome.candidates, [950, 850, 750, 650]);
        assert_eq!(rig.benches, [950, 850, 750, 650]);
        assert!(table(&outcome, &config(3600)).contains("same code as 950"));
    }

    #[test]
    fn a_candidate_under_the_baseline_is_not_eligible() {
        let speeds: &[(u32, &[(&str, f64)])] = &[
            (950, &[("a", 10.0), ("b", 20.0)]),
            (900, &[("a", 12.0), ("b", 19.5)]),
            (850, &[("a", 10.1), ("b", 20.1)]),
            (800, &[("a", 9.0), ("b", 15.0)]),
        ];
        let baseline = vec![("a".to_string(), 10.0), ("b".to_string(), 20.0)];
        let with = Config {
            baseline: baseline.clone(),
            ..config(3600)
        };
        let outcome = search(&mut Fake::new(speeds), &with).unwrap();
        // 900 has the best geomean but is 2.5% under the baseline on b.
        assert_eq!(outcome.chosen, 850);
        let text = table(&outcome, &with);
        assert!(text.contains("(-2.50%)"), "{text}");
        // Without a baseline 900 wins.
        assert_eq!(
            search(&mut Fake::new(speeds), &config(3600))
                .unwrap()
                .chosen,
            900
        );
        // A baseline nobody meets is an error that shows the table.
        let high = Config {
            baseline: vec![("a".to_string(), 20.0), ("b".to_string(), 20.0)],
            ..config(3600)
        };
        let error = search(&mut Fake::new(speeds), &high)
            .unwrap_err()
            .to_string();
        assert!(error.contains("under the baseline"), "{error}");
    }

    #[test]
    fn one_candidate_skips_the_bench() {
        let mut rig = Fake::new(&[]);
        let outcome = search(
            &mut rig,
            &Config {
                candidates: 1,
                ..config(3600)
            },
        )
        .unwrap();
        assert_eq!(outcome.chosen, 950);
        assert!(rig.benches.is_empty());
    }

    #[test]
    fn nothing_fitting_is_an_error_naming_every_rung() {
        let mut rig = Fake::new(&[]);
        let error = search(&mut rig, &config(6000)).unwrap_err().to_string();
        assert!(error.contains("no threshold"), "{error}");
    }

    #[test]
    fn scores_come_from_marked_lines_only() {
        let text = "noise 1 2\nscore c0a0 13.9\nscore chain 18.4 extra\nscore c4a3 18.8\n";
        assert_eq!(
            parse_scores(text),
            [("c0a0".to_string(), 13.9), ("c4a3".to_string(), 18.8)]
        );
    }
}
