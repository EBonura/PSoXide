//! The baseline, `psoxide-test.baseline.toml`: the numbers a run is judged against. Written only
//! by `psoxide-test bless`, committed in the commit that lands a build.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::game::BASELINE_FILE;

/// The whole baseline.
#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub schema: u32,
    /// The commit the numbers were measured on.
    pub blessed_commit: String,
    /// The SDK revision that commit pins.
    pub sdk_pin: String,
    #[serde(default)]
    pub size: Option<SizeBase>,
    /// One table per speed window.
    #[serde(default)]
    pub speed: BTreeMap<String, WindowBase>,
    /// Per journey: ticks on `real` (the schedule) and the open-loop ratchet.
    #[serde(default)]
    pub journeys: BTreeMap<String, JourneyBase>,
    /// Host-check metrics, by check name (`name` or `name@model`).
    #[serde(default)]
    pub host: BTreeMap<String, u64>,
}

/// One link's size, from `psoxide_size::ram`.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SizeBase {
    pub text: u32,
    pub data: u32,
    pub bss: u32,
    pub free: u32,
}

/// One window's metrics, one entry per offset.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WindowBase {
    pub offsets: Vec<i32>,
    pub flips: Vec<u64>,
    pub work_per_frame: Vec<u64>,
    pub frame_work_p99: Vec<u64>,
    pub vblanks: Vec<String>,
}

/// One journey's schedule hint and open-loop count.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct JourneyBase {
    pub ticks: u64,
    pub open_loop: u32,
}

impl Baseline {
    /// Read the baseline beside the game file; `None` when there is none yet.
    pub fn load(repo: &Path) -> Result<Option<Baseline>, String> {
        let path = repo.join(BASELINE_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let base: Baseline =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if base.schema != 1 {
            return Err(format!(
                "{}: schema {} is not 1",
                path.display(),
                base.schema
            ));
        }
        Ok(Some(base))
    }

    /// Write the baseline beside the game file.
    pub fn save(&self, repo: &Path) -> Result<(), String> {
        let path = repo.join(BASELINE_FILE);
        let body = toml::to_string(self).map_err(|e| format!("encode baseline: {e}"))?;
        let text = format!(
            "# Written by `psoxide-test bless`; edit only by blessing again (decision 4: a bar \
             changes by editing\n# psoxide-test.toml with a reason, never by hand-editing these \
             numbers).\n{body}"
        );
        std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

impl WindowBase {
    /// Tolerance of one metric: the range over the blessed offsets divided by their number,
    /// the mean step between adjacent window phases.
    pub fn tolerance(values: &[u64]) -> f64 {
        match (values.iter().min(), values.iter().max()) {
            (Some(lo), Some(hi)) if !values.is_empty() => (hi - lo) as f64 / values.len() as f64,
            _ => 0.0,
        }
    }

    /// The blessed value at `offset`.
    pub fn at(&self, values: &[u64], offset: i32) -> Option<u64> {
        let index = self.offsets.iter().position(|o| *o == offset)?;
        values.get(index).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerance_is_the_mean_phase_step() {
        let flips = [791, 791, 787, 785, 779, 779, 779, 779];
        assert!((WindowBase::tolerance(&flips) - 1.5).abs() < 1e-9);
        assert_eq!(WindowBase::tolerance(&[5]), 0.0);
    }

    #[test]
    fn a_baseline_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut base = Baseline {
            schema: 1,
            blessed_commit: "abc".into(),
            sdk_pin: "def".into(),
            ..Baseline::default()
        };
        base.size = Some(SizeBase {
            text: 1,
            data: 2,
            bss: 3,
            free: 4,
        });
        base.journeys.insert(
            "journey".into(),
            JourneyBase {
                ticks: 1822,
                open_loop: 3,
            },
        );
        base.save(dir.path()).unwrap();
        let back = Baseline::load(dir.path()).unwrap().unwrap();
        assert_eq!(back.size, base.size);
        assert_eq!(back.journeys["journey"].ticks, 1822);
    }
}
