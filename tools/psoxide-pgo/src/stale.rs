//! How much of a sample profile still describes the code it is applied to.
//!
//! A sample profile is keyed by function name, then line offset from the
//! function's declaration and discriminator. LLVM cannot tell when those keys
//! stopped meaning the same code (no CFG checksum without pseudo-probes, which
//! this rustc cannot emit), so it quietly inlines by stale numbers: hl-psx
//! repinned to a new SDK with its old profile ran 7.5% to 11.6% slower and
//! gained 20 KB of free RAM. This compares the profile against the key set of
//! the build it will be applied to and says so.
//!
//! The key set is the profile `convert` gives the build's own executable when
//! every instruction word ran once. A profile line is "bound" when its
//! (function, offset, discriminator) is in that set, wherever it sits in the
//! inline nesting: the inliner looks a callee up by name, not by caller.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::Result;

/// Below this share of the profile's samples bound, say so.
pub const WARN_BELOW: f64 = 0.98;
/// Below this share, refuse (`PSOXIDE_PGO_ALLOW_STALE=1` overrides).
pub const REFUSE_BELOW: f64 = 0.90;

type Key<'a> = (&'a str, u32, u32);

/// Every body line of a profile as (function, offset, discriminator, count),
/// attributing nested lines to the callee they sit under.
fn lines(text: &str) -> Vec<(&str, u32, u32, u64)> {
    let mut out = Vec::new();
    // Function name at each indent level.
    let mut stack: Vec<&str> = Vec::new();
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent == 0 {
            let name = line.rsplitn(3, ':').nth(2).unwrap_or(line);
            stack.clear();
            stack.push(name);
            continue;
        }
        stack.truncate(indent);
        let Some((location, rest)) = line.trim_start().split_once(": ") else {
            continue;
        };
        let (offset, discriminator) = match location.split_once('.') {
            Some((offset, disc)) => (offset, disc),
            None => (location, "0"),
        };
        let (Ok(offset), Ok(discriminator)) = (offset.parse(), discriminator.parse()) else {
            continue;
        };
        match rest.parse::<u64>() {
            Ok(count) => {
                if let Some(function) = stack.get(indent - 1) {
                    out.push((*function, offset, discriminator, count));
                }
            }
            Err(_) => {
                if let Some((callee, _)) = rest.rsplit_once(':') {
                    stack.push(callee);
                }
            }
        }
    }
    out
}

#[derive(Debug, PartialEq)]
pub struct Report {
    pub samples: u64,
    pub bound_samples: u64,
    pub functions: usize,
    /// Functions with no entry in the build at all (renamed, moved, deleted).
    pub missing: usize,
    /// Functions in the build whose profile lines partly miss.
    pub changed: usize,
    /// Hottest functions that lost samples: (name, samples lost).
    pub worst: Vec<(String, u64)>,
}

impl Report {
    pub fn share(&self) -> f64 {
        if self.samples == 0 {
            return 0.0;
        }
        self.bound_samples as f64 / self.samples as f64
    }

    /// Err below [`REFUSE_BELOW`] unless `PSOXIDE_PGO_ALLOW_STALE` is set.
    pub fn gate(&self) -> Result<()> {
        if self.share() < REFUSE_BELOW && std::env::var_os("PSOXIDE_PGO_ALLOW_STALE").is_none() {
            return Err(format!(
                "the profile is stale: only {:.1}% of its samples bind onto this build (under \
                 {:.0}%). Re-collect it on this code (PSOXIDE_PGO_ALLOW_STALE=1 to build anyway)",
                100.0 * self.share(),
                100.0 * REFUSE_BELOW
            )
            .into());
        }
        Ok(())
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let share = self.share();
        let level = if share < REFUSE_BELOW {
            "STALE PROFILE: "
        } else if share < WARN_BELOW {
            "warning, aging profile: "
        } else {
            ""
        };
        write!(
            f,
            "{level}profile binds {:.1}% of its samples onto this build; {} of {} functions \
             missing, {} changed",
            100.0 * share,
            self.missing,
            self.functions,
            self.changed
        )?;
        if share < WARN_BELOW && !self.worst.is_empty() {
            let worst: Vec<String> = self
                .worst
                .iter()
                .take(5)
                .map(|(name, lost)| {
                    format!(
                        "{name} ({:.2}%)",
                        100.0 * *lost as f64 / self.samples as f64
                    )
                })
                .collect();
            write!(f, "; most lost: {}", worst.join(", "))?;
        }
        Ok(())
    }
}

/// Compare a profile against the key profile of the build it will be applied to.
pub fn compare(profile: &str, build_keys: &str) -> Report {
    let present: HashSet<Key> = lines(build_keys)
        .into_iter()
        .map(|(function, offset, disc, _)| (function, offset, disc))
        .collect();
    let named: HashSet<&str> = present.iter().map(|(function, _, _)| *function).collect();
    let mut report = Report {
        samples: 0,
        bound_samples: 0,
        functions: 0,
        missing: 0,
        changed: 0,
        worst: Vec::new(),
    };
    let mut per: HashMap<&str, (u64, u64)> = HashMap::new();
    for (function, offset, disc, count) in lines(profile) {
        let entry = per.entry(function).or_default();
        entry.0 += count;
        if present.contains(&(function, offset, disc)) {
            entry.1 += count;
        }
    }
    let mut lost: Vec<(String, u64)> = Vec::new();
    for (function, (total, bound)) in per {
        report.samples += total;
        report.bound_samples += bound;
        report.functions += 1;
        if !named.contains(function) {
            report.missing += 1;
        } else if bound < total {
            report.changed += 1;
        }
        if bound < total {
            lost.push((function.to_string(), total - bound));
        }
    }
    lost.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    report.worst = lost;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILD: &str = "\
a::f:9:1
 1: 1
 2: 1
 3: 1
 4: 1
a::g:3:1
 1: 1
 3.2: 1
";

    #[test]
    fn nested_lines_belong_to_the_callee() {
        let text = "a::f:10:1\n 1: 5\n 2: a::g:5\n  1: 5\n";
        assert_eq!(lines(text), [("a::f", 1, 0, 5), ("a::g", 1, 0, 5)]);
    }

    #[test]
    fn an_unchanged_profile_binds_everything() {
        let report = compare("a::f:10:1\n 1: 7\n 3: 3\n a::g:1\n", BUILD);
        assert_eq!(report.share(), 1.0);
        assert!(report.gate().is_ok());
    }

    #[test]
    fn shifted_lines_and_vanished_functions_count_against_it() {
        // f's hot line moved from offset 3 to 5; h is gone; g still binds.
        let profile = "a::f:10:1\n 1: 1\n 5: 80\na::g:5:1\n 1: 10\na::h:9:1\n 2: 9\n";
        let report = compare(profile, BUILD);
        assert_eq!((report.samples, report.bound_samples), (100, 11));
        assert_eq!(
            (report.functions, report.missing, report.changed),
            (3, 1, 1)
        );
        assert_eq!(report.worst[0], ("a::f".to_string(), 80));
        assert!(report.to_string().contains("STALE PROFILE"));
        assert!(report.gate().is_err());
    }

    #[test]
    fn discriminators_are_part_of_the_key() {
        let report = compare("a::g:5:1\n 3.2: 5\n 3.3: 5\n", BUILD);
        assert_eq!(report.share(), 0.5);
    }
}
