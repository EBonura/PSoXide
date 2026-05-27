#![allow(missing_docs)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use psoxide_settings::library::LibraryEntry;

const BURNER_SCAN_INTERVAL: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CdBurner {
    pub(crate) name: String,
    pub(crate) bus: String,
    pub(crate) support: String,
}

impl CdBurner {
    pub(crate) fn can_burn(&self) -> bool {
        !self.support.eq_ignore_ascii_case("unsupported")
    }

    pub(crate) fn label(&self) -> String {
        format!("{} ({}, {})", self.name, self.bus, self.support)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BurnTarget {
    pub(crate) title: String,
    pub(crate) path: PathBuf,
}

pub(crate) struct BurnState {
    pub(crate) open: bool,
    pub(crate) target: Option<BurnTarget>,
    pub(crate) burners: Vec<CdBurner>,
    pub(crate) selected_burner: usize,
    pub(crate) speed: String,
    pub(crate) simulate: bool,
    pub(crate) eject: bool,
    pub(crate) status: String,
    child: Option<Child>,
    initialized: bool,
    last_signature: String,
    next_scan: Instant,
}

impl Default for BurnState {
    fn default() -> Self {
        Self {
            open: false,
            target: None,
            burners: Vec::new(),
            selected_burner: 0,
            speed: "Default".to_string(),
            simulate: true,
            eject: true,
            status: "No burner scan yet".to_string(),
            child: None,
            initialized: false,
            last_signature: String::new(),
            next_scan: Instant::now(),
        }
    }
}

impl BurnState {
    pub(crate) fn open_for(&mut self, entry: &LibraryEntry) {
        self.open = true;
        self.target = Some(BurnTarget {
            title: entry.title.clone(),
            path: entry.path.clone(),
        });
        self.status = "Scanning burners...".to_string();
        self.next_scan = Instant::now();
    }

    pub(crate) fn scan_now(&mut self) -> Result<Option<String>, String> {
        let burners = scan_burners()?;
        let signature = burner_signature(&burners);
        let notice = self.notice_for_signature(&burners, &signature);
        self.burners = burners;
        self.selected_burner = self
            .selected_burner
            .min(self.burners.len().saturating_sub(1));
        if self
            .burners
            .get(self.selected_burner)
            .map_or(true, |burner| !burner.can_burn())
        {
            self.selected_burner = self
                .burners
                .iter()
                .position(CdBurner::can_burn)
                .unwrap_or(0);
        }
        self.last_signature = signature;
        self.initialized = true;
        self.next_scan = Instant::now() + BURNER_SCAN_INTERVAL;
        let usable = self.burners.iter().filter(|burner| burner.can_burn()).count();
        self.status = if self.burners.is_empty() {
            "No optical drive detected".to_string()
        } else if usable == 0 {
            format!("{} drive(s) detected, none burnable", self.burners.len())
        } else {
            format!("{usable} burnable drive(s) detected")
        };
        Ok(notice)
    }

    pub(crate) fn tick(&mut self) -> Result<Option<String>, String> {
        if let Some(notice) = self.poll_child()? {
            return Ok(Some(notice));
        }
        if Instant::now() < self.next_scan {
            return Ok(None);
        }
        self.scan_now()
    }

    pub(crate) fn start_burn(&mut self) -> Result<String, String> {
        if self.child.is_some() {
            return Err("burn already running".to_string());
        }
        let target = self
            .target
            .as_ref()
            .ok_or_else(|| "no burn target selected".to_string())?;
        if self.burners.is_empty() {
            return Err("no CD burner detected".to_string());
        }
        let burner = self
            .burners
            .get(self.selected_burner)
            .ok_or_else(|| "no CD burner selected".to_string())?;
        if !burner.can_burn() {
            return Err(format!("selected drive is not burnable: {}", burner.label()));
        }

        let drive = (self.selected_burner + 1).to_string();
        let mut command = Command::new("drutil");
        command
            .arg("-drive")
            .arg(drive)
            .arg("burn")
            .arg(if self.simulate { "-test" } else { "-notest" });
        if self.eject {
            command.arg("-eject");
        }
        if self.speed != "Default" {
            command.arg("-speed").arg(self.speed.trim_end_matches('x'));
        }
        command
            .arg(&target.path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = command
            .spawn()
            .map_err(|error| format!("spawn drutil burn: {error}"))?;
        let mode = if self.simulate {
            "simulation"
        } else {
            "real burn"
        };
        self.status = format!("Burn {mode} started");
        self.child = Some(child);
        Ok(self.status.clone())
    }

    pub(crate) fn is_burning(&self) -> bool {
        self.child.is_some()
    }

    fn poll_child(&mut self) -> Result<Option<String>, String> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        let Some(status) = child
            .try_wait()
            .map_err(|error| format!("poll drutil burn: {error}"))?
        else {
            return Ok(None);
        };
        self.child = None;
        self.status = if status.success() {
            "Burn completed".to_string()
        } else {
            format!("Burn failed: {status}")
        };
        Ok(Some(self.status.clone()))
    }

    fn notice_for_signature(&self, burners: &[CdBurner], signature: &str) -> Option<String> {
        if !self.initialized || self.last_signature == signature {
            return None;
        }
        if burners.is_empty() {
            return Some("CD drive disconnected".to_string());
        }
        Some(format!(
            "CD drive connected: {}",
            burners
                .iter()
                .map(CdBurner::label)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

pub(crate) fn scan_burners() -> Result<Vec<CdBurner>, String> {
    let output = Command::new("drutil")
        .arg("list")
        .output()
        .map_err(|error| format!("spawn drutil list: {error}"))?;
    if !output.status.success() {
        return Err(format!("drutil list failed: {}", output.status));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_drutil_list(&text))
}

fn parse_drutil_list(text: &str) -> Vec<CdBurner> {
    let mut burners = Vec::new();
    let mut lines = text.lines();
    let Some(header) = lines.next() else {
        return burners;
    };
    let Some(vendor_at) = header.find("Vendor") else {
        return burners;
    };
    let Some(product_at) = header.find("Product") else {
        return burners;
    };
    let Some(rev_at) = header.find("Rev") else {
        return burners;
    };
    let Some(bus_at) = header.find("Bus") else {
        return burners;
    };
    let Some(support_at) = header.find("SupportLevel") else {
        return burners;
    };

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let vendor = field(line, vendor_at, product_at);
        let product = field(line, product_at, rev_at);
        let bus = field(line, bus_at, support_at);
        let support = field(line, support_at, line.len());
        if vendor.is_empty() || product.is_empty() {
            continue;
        }
        burners.push(CdBurner {
            name: format!("{vendor} {product}"),
            bus: bus.to_string(),
            support: support.to_string(),
        });
    }
    burners
}

fn field(line: &str, start: usize, end: usize) -> &str {
    line.get(start..end).unwrap_or("").trim()
}

fn burner_signature(burners: &[CdBurner]) -> String {
    burners
        .iter()
        .map(|burner| format!("{}:{}:{}", burner.name, burner.bus, burner.support))
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_drutil_burner_rows() {
        let burners = parse_drutil_list(concat!(
            "   Vendor   Product           Rev   Bus       SupportLevel\n",
            "   HL-DT-ST DVDRW GX50N       RR06  USB       Apple Shipping\n",
        ));
        assert_eq!(burners.len(), 1);
        assert_eq!(burners[0].name, "HL-DT-ST DVDRW GX50N");
        assert_eq!(burners[0].bus, "USB");
        assert!(burners[0].can_burn());
    }

    #[test]
    fn unsupported_drives_are_not_burnable() {
        let burners = parse_drutil_list(concat!(
            "   Vendor   Product           Rev   Bus       SupportLevel\n",
            "1  TSSTcorp CDDVDW SN-208AB   LA02  USB       Unsupported\n",
        ));
        assert_eq!(burners.len(), 1);
        assert!(!burners[0].can_burn());
    }
}
