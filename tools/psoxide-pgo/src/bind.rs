//! Whether a sample profile's function names find a symbol in the build it is applied to.
//!
//! LLVM matches a sample profile to code by exact symbol name and says nothing when it
//! finds none: a profile whose names no longer exist is applied as an empty one, and the
//! build comes out larger and slower with no message. A Rust v0 symbol carries the
//! disambiguator of every crate in its path, and cargo derives each from a hash of the
//! crate's dependency edges, features and profile, so one SDK change can re-hash a game
//! crate and with it every name in a profile that spells the hash out.
//!
//! The key that survives that is [`crate::portable_name`] (the demangled path without
//! disambiguators, plus the instantiating crate for a generic instance). This module
//! looks every top-level name of a profile up under that key, whichever form the profile
//! was written in (raw symbols from the plain converter, or portable names from
//! `collect`), counts what finds no symbol, and fails below a threshold.
//!
//! The unit is a function record (a header line), weighted by the samples under it
//! (inlined callees included, since LLVM reaches them through the caller). Line-level
//! drift inside a function that does exist is [`crate::stale`]'s question, not this one's.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::{portable_name, Result};

/// Below this share of the profile's samples bound to a symbol, fail.
///
/// Three profiles of one game, committed at three SDK revisions, each bound to the build
/// at the newest: 98.6%, 98.6% and 99.1% of samples bind once names are normalised, and
/// 25.7%, 28.1% and 99.1% under exact names (the first two share almost no function name
/// with the build, since the game crate and everything above it re-hashed; the third was
/// collected where the hashes still match). An SDK example built twice with a feature and a
/// dependency edge changed binds 100% normalised and 50.5% exact. Drift inside functions that
/// still exist does not reach this figure ([`crate::stale`] reports it). The gap between 98%
/// and 30% is wide, and a profile with a tenth of its samples in functions that do not exist
/// is not applying, so the line sits at the one `stale` already refuses at.
pub const MIN_BOUND: f64 = 0.90;

/// Set to anything but `0` to continue past a failing bind, as `--allow-unbound` does.
pub const ALLOW_VAR: &str = "PSOXIDE_PGO_ALLOW_UNBOUND";

/// The threshold a bind must reach and whether a miss is overridden.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gate {
    pub min: f64,
    pub allow: bool,
}

impl Default for Gate {
    /// [`MIN_BOUND`], overridden when [`ALLOW_VAR`] is set and not `0`.
    fn default() -> Self {
        Gate {
            min: MIN_BOUND,
            allow: std::env::var_os(ALLOW_VAR)
                .is_some_and(|value| !value.is_empty() && value != "0"),
        }
    }
}

impl Gate {
    /// Read `--min-bound PERCENT` and `--allow-unbound` out of `args`, on top of the
    /// default. Anything else is returned for the caller to read.
    pub fn from_args(args: &[String]) -> Result<(Gate, Vec<&str>)> {
        let mut gate = Gate::default();
        let mut rest = Vec::new();
        let mut args = args.iter().map(String::as_str);
        while let Some(arg) = args.next() {
            match arg {
                "--allow-unbound" => gate.allow = true,
                "--min-bound" => {
                    let value = args.next().ok_or("--min-bound wants a percentage")?;
                    let percent: f64 = value
                        .parse()
                        .map_err(|_| format!("--min-bound wants a percentage, not {value:?}"))?;
                    if !(0.0..=100.0).contains(&percent) {
                        return Err(format!("--min-bound {percent} is not a percentage").into());
                    }
                    gate.min = percent / 100.0;
                }
                other => rest.push(other),
            }
        }
        Ok((gate, rest))
    }

    /// Fail, or say so and continue under the override, when `report` binds too little.
    pub fn check(&self, report: &Report) -> Result<()> {
        report.gate(self.min, self.allow)
    }
}

/// How a profile name is compared with the build's symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matching {
    /// Under [`portable_name`] on both sides: what `rebind` does.
    Normalised,
    /// Spelled exactly as the build's symbol: what LLVM does with a profile given to it
    /// as it is.
    Exact,
}

/// The symbols of one build, under both spellings.
#[derive(Default)]
pub struct Available {
    /// Portable key to its symbol, or `None` when two different symbols share the key
    /// (the profile cannot say which it meant, and neither is merged into the other).
    keys: HashMap<String, Option<String>>,
    exact: HashSet<String>,
}

impl Available {
    /// Add one linker or DWARF linkage name.
    pub fn add(&mut self, symbol: &str) {
        if !self.exact.insert(symbol.to_string()) {
            return;
        }
        let key = portable_name(symbol).into_owned();
        if key == symbol {
            // A name with no Rust form to normalise (`memcpy`).
            return;
        }
        match self.keys.get(&key) {
            None => {
                self.keys.insert(key, Some(symbol.to_string()));
            }
            Some(Some(existing)) if existing != symbol => {
                self.keys.insert(key, None);
            }
            Some(_) => {}
        }
    }

    /// The symbol a profile name binds to, if exactly one does.
    pub fn resolve(&self, name: &str) -> Lookup {
        let key = portable_name(name);
        // A Rust name either demangles or is already portable, and every Rust item path
        // has a `::`; C names such as `main` have neither.
        if key == name && !name.contains("::") {
            return if self.exact.contains(name) {
                Lookup::Kept
            } else {
                Lookup::Missing
            };
        }
        match self.keys.get(key.as_ref()) {
            Some(Some(symbol)) => Lookup::Bound(symbol.clone()),
            Some(None) => Lookup::Ambiguous,
            None => Lookup::Missing,
        }
    }

    /// Every portable key two or more symbols share, sorted.
    pub fn collisions(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self
            .keys
            .iter()
            .filter(|(_, symbol)| symbol.is_none())
            .map(|(key, _)| key.as_str())
            .collect();
        keys.sort_unstable();
        keys
    }
}

/// What one profile name found.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// A Rust name: the symbol of the build it binds to.
    Bound(String),
    /// Found under its own spelling (a name with no Rust form, such as `memcpy`, or any
    /// name in exact matching): nothing to rewrite.
    Kept,
    /// No symbol of the build has this name.
    Missing,
    /// Two or more symbols of the build share its portable key.
    Ambiguous,
}

/// The binding of one profile onto one build.
#[derive(Debug, PartialEq)]
pub struct Report {
    pub matching: Matching,
    pub samples: u64,
    pub unbound_samples: u64,
    pub functions: usize,
    pub unbound_functions: usize,
    /// Of the unbound functions, those whose key two symbols share.
    pub ambiguous_functions: usize,
    /// Unbound functions by samples, largest first, ties by name.
    pub largest: Vec<(String, u64)>,
}

impl Report {
    /// The share of samples whose function found a symbol (1.0 for an empty profile is
    /// wrong: an empty profile binds nothing).
    pub fn share(&self) -> f64 {
        if self.samples == 0 {
            return 0.0;
        }
        (self.samples - self.unbound_samples) as f64 / self.samples as f64
    }

    /// Err below `min` unless `allow`.
    pub fn gate(&self, min: f64, allow: bool) -> Result<()> {
        if self.share() >= min {
            return Ok(());
        }
        if allow {
            println!(
                "psoxide-pgo: the profile binds only {:.1}% of its samples (under {:.0}%); \
                 continuing ({ALLOW_VAR} or --allow-unbound)",
                100.0 * self.share(),
                100.0 * min
            );
            return Ok(());
        }
        Err(format!(
            "UNBOUND PROFILE: only {:.1}% of the profile's samples sit in functions this build \
             has a symbol for (under {:.0}%), so LLVM would apply almost none of it and say \
             nothing. Collect it again on this build, or pass --allow-unbound ({ALLOW_VAR}=1) to \
             continue",
            100.0 * self.share(),
            100.0 * min
        )
        .into())
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let how = match self.matching {
            Matching::Normalised => "names normalised",
            Matching::Exact => "exact names",
        };
        let pct = |part: f64, whole: f64| {
            if whole == 0.0 {
                0.0
            } else {
                100.0 * part / whole
            }
        };
        write!(
            f,
            "profile bind ({how}): {:.1}% of samples bound; no symbol for {:.1}% of samples \
             ({} of {}) and {} of {} functions ({} ambiguous)",
            100.0 * self.share(),
            pct(self.unbound_samples as f64, self.samples as f64),
            self.unbound_samples,
            self.samples,
            self.unbound_functions,
            self.functions,
            self.ambiguous_functions
        )?;
        for (name, samples) in self.largest.iter().take(LISTED) {
            write!(
                f,
                "\n  unbound {:5.2}%  {name}",
                pct(*samples as f64, self.samples as f64)
            )?;
        }
        if self.largest.len() > LISTED {
            write!(f, "\n  ... and {} more", self.largest.len() - LISTED)?;
        }
        Ok(())
    }
}

/// How many of the largest unbound functions are listed.
const LISTED: usize = 10;

/// Every function record of a text profile as (name, samples under it).
fn records(profile: &str) -> impl Iterator<Item = (&str, u64)> {
    profile
        .lines()
        .filter(|line| !line.starts_with('#') && !line.starts_with(' ') && !line.is_empty())
        .filter_map(|line| {
            let mut fields = line.rsplitn(3, ':');
            let (_head, total, name) = (fields.next()?, fields.next()?, fields.next()?);
            Some((name, total.parse().ok()?))
        })
}

/// Bind `profile` (any spelling of its names) onto `available`.
pub fn measure(profile: &str, available: &Available, matching: Matching) -> Report {
    let mut report = Report {
        matching,
        samples: 0,
        unbound_samples: 0,
        functions: 0,
        unbound_functions: 0,
        ambiguous_functions: 0,
        largest: Vec::new(),
    };
    for (name, samples) in records(profile) {
        report.samples += samples;
        report.functions += 1;
        let lookup = match matching {
            Matching::Normalised => available.resolve(name),
            Matching::Exact if available.exact.contains(name) => Lookup::Kept,
            Matching::Exact => Lookup::Missing,
        };
        if lookup == Lookup::Missing || lookup == Lookup::Ambiguous {
            report.unbound_samples += samples;
            report.unbound_functions += 1;
            if lookup == Lookup::Ambiguous {
                report.ambiguous_functions += 1;
            }
            // Report keys in the form that does not change between builds.
            report
                .largest
                .push((portable_name(name).into_owned(), samples));
        }
    }
    report
        .largest
        .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    // One function in two builds of the same crate: only the disambiguator differs.
    const OLD: &str = "_RNvCsfiefkAYodV3_6hl_psx12loading_idle";
    const NEW: &str = "_RNvCsXZk3xBXeOP_6hl_psx12loading_idle";
    const GONE: &str = "_RNvCsfiefkAYodV3_6hl_psx7removed";

    fn build(symbols: &[&str]) -> Available {
        let mut available = Available::default();
        for symbol in symbols {
            available.add(symbol);
        }
        available
    }

    fn profile(old: &str) -> String {
        format!("{old}:300:5\n 1: 300\n{GONE}:100:1\n 1: 100\nmemcpy:50:1\n 0: 50\n")
    }

    #[test]
    fn a_profile_from_another_hash_binds_under_normalised_names_only() {
        let available = build(&[NEW, "memcpy"]);
        let normalised = measure(&profile(OLD), &available, Matching::Normalised);
        assert_eq!((normalised.samples, normalised.unbound_samples), (450, 100));
        assert_eq!((normalised.functions, normalised.unbound_functions), (3, 1));
        assert_eq!(normalised.largest, [("hl_psx::removed".to_string(), 100)]);
        let exact = measure(&profile(OLD), &available, Matching::Exact);
        assert_eq!((exact.samples, exact.unbound_samples), (450, 400));
        assert_eq!(exact.unbound_functions, 2);
        assert!(exact.share() < normalised.share());
    }

    #[test]
    fn portable_names_bind_like_raw_ones() {
        let available = build(&[NEW]);
        let report = measure(
            "hl_psx::loading_idle:7:1\n 1: 7\n",
            &available,
            Matching::Normalised,
        );
        assert_eq!(report.share(), 1.0);
    }

    #[test]
    fn two_symbols_with_one_key_bind_neither() {
        // Same path in two crates that both end up named `a`: one key, two symbols.
        let first = "_RNvCs1_1a1f";
        let second = "_RNvCs2_1a1f";
        let available = build(&[first, second]);
        assert_eq!(available.collisions(), ["a::f"]);
        let report = measure(
            &format!("{first}:9:1\n 1: 9\n"),
            &available,
            Matching::Normalised,
        );
        assert_eq!((report.unbound_samples, report.ambiguous_functions), (9, 1));
        assert_eq!(available.resolve(first), Lookup::Ambiguous);
    }

    #[test]
    fn the_gate_fails_below_the_threshold_and_an_override_continues() {
        let available = build(&[]);
        let report = measure(&profile(OLD), &available, Matching::Normalised);
        let error = report.gate(MIN_BOUND, false).unwrap_err().to_string();
        assert!(error.contains("UNBOUND PROFILE"), "{error}");
        assert!(report.gate(MIN_BOUND, true).is_ok());
        assert!(report.gate(0.0, false).is_ok());
    }

    #[test]
    fn an_empty_profile_binds_nothing() {
        let report = measure(
            "# only comments\n",
            &Available::default(),
            Matching::Normalised,
        );
        assert_eq!(report.share(), 0.0);
        assert!(report.gate(MIN_BOUND, false).is_err());
    }

    #[test]
    fn the_report_lists_the_largest_unbound_functions_first() {
        let text =
            format!("{GONE}:100:1\n 1: 100\n_RNvCsfiefkAYodV3_6hl_psx5other:300:1\n 1: 300\n");
        let report = measure(&text, &Available::default(), Matching::Normalised);
        let printed = report.to_string();
        assert!(printed.contains("0.0% of samples bound"), "{printed}");
        let other = printed.find("hl_psx::other").unwrap();
        let removed = printed.find("hl_psx::removed").unwrap();
        assert!(other < removed, "{printed}");
    }
}
