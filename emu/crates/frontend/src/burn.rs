#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use psoxide_settings::library::{cue_referenced_files, LibraryEntry};

const BURNER_SCAN_INTERVAL: Duration = Duration::from_secs(4);
pub(crate) const RECOMMENDED_BURN_SPEED: &str = "4x";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CdBurner {
    pub(crate) drive: usize,
    pub(crate) name: String,
    pub(crate) bus: String,
    pub(crate) support: String,
    pub(crate) writes_cd: bool,
    pub(crate) write_speeds: Vec<String>,
}

impl CdBurner {
    pub(crate) fn can_burn(&self) -> bool {
        self.writes_cd || self.generic_writer()
    }

    pub(crate) fn label(&self) -> String {
        let support = if self.generic_writer() {
            "Generic CD writer"
        } else {
            &self.support
        };
        format!("{} ({}, {})", self.name, self.bus, support)
    }

    fn generic_writer(&self) -> bool {
        self.support.eq_ignore_ascii_case("unsupported")
    }

    fn lowest_write_speed(&self) -> Option<&str> {
        self.write_speeds.first().map(String::as_str)
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
    pub(crate) confirm_real_burn: bool,
    pub(crate) eject: bool,
    pub(crate) status: String,
    child: Option<Child>,
    running_real_burn: bool,
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
            speed: RECOMMENDED_BURN_SPEED.to_string(),
            simulate: true,
            confirm_real_burn: false,
            eject: true,
            status: "No burner scan yet".to_string(),
            child: None,
            running_real_burn: false,
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
        self.confirm_real_burn = false;
        self.status = "Scanning burners...".to_string();
        self.next_scan = Instant::now();
    }

    pub(crate) fn scan_now(&mut self) -> Result<Option<String>, String> {
        self.next_scan = Instant::now() + BURNER_SCAN_INTERVAL;
        if !burn_backend_supported() {
            self.initialized = true;
            self.status = "Disc burning is available on macOS only".to_string();
            return Ok(None);
        }
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
        self.align_speed_to_selected_burner();
        self.last_signature = signature;
        self.initialized = true;
        let usable = self
            .burners
            .iter()
            .filter(|burner| burner.can_burn())
            .count();
        let verified = self
            .burners
            .iter()
            .filter(|burner| burner.writes_cd)
            .count();
        let generic = usable.saturating_sub(verified);
        self.status = if self.burners.is_empty() {
            "No optical drive detected".to_string()
        } else if usable == 0 {
            format!("{} optical drive(s) detected", self.burners.len())
        } else if verified == 0 && generic > 0 {
            format!("{generic} generic CD writer(s) detected")
        } else if generic > 0 {
            format!("{verified} verified + {generic} generic CD writer(s) detected")
        } else {
            format!("{usable} burnable drive(s) detected")
        };
        Ok(notice)
    }

    pub(crate) fn tick(&mut self) -> Result<Option<String>, String> {
        if let Some(notice) = self.poll_child()? {
            return Ok(Some(notice));
        }
        if self.child.is_some() {
            return Ok(None);
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
        if !burn_backend_supported() {
            return Err("disc burning is available on macOS only".to_string());
        }
        let target = self
            .target
            .as_ref()
            .ok_or_else(|| "no burn target selected".to_string())?;
        validate_burn_target_path(&target.path)?;
        if !self.simulate && !self.confirm_real_burn {
            return Err("confirm real burn before writing a CD-R".to_string());
        }
        if self.burners.is_empty() {
            return Err("no CD burner detected".to_string());
        }
        let burner = self
            .burners
            .get(self.selected_burner)
            .ok_or_else(|| "no CD burner selected".to_string())?;
        if !burner.can_burn() {
            return Err(format!(
                "selected drive is not burnable: {}",
                burner.label()
            ));
        }

        let drive = burner.drive.to_string();
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
        self.running_real_burn = !self.simulate;
        self.child = Some(child);
        Ok(self.status.clone())
    }

    pub(crate) fn is_burning(&self) -> bool {
        self.child.is_some()
    }

    pub(crate) fn running_label(&self) -> Option<&'static str> {
        self.child.as_ref().map(|_| {
            if self.running_real_burn {
                "Burning CD-R..."
            } else {
                "Simulating burn..."
            }
        })
    }

    pub(crate) fn speed_choices(&self) -> Vec<String> {
        let mut speeds = self
            .burners
            .get(self.selected_burner)
            .map(|burner| burner.write_speeds.clone())
            .unwrap_or_default();
        if speeds.is_empty() {
            speeds.extend(["4x", "8x", "16x", "24x"].map(str::to_string));
        }
        push_unique(&mut speeds, "Default".to_string());
        if !self.speed.is_empty() {
            push_unique(&mut speeds, self.speed.clone());
        }
        speeds
    }

    pub(crate) fn align_speed_to_selected_burner(&mut self) {
        let Some(burner) = self.burners.get(self.selected_burner) else {
            self.speed = RECOMMENDED_BURN_SPEED.to_string();
            return;
        };
        if burner.write_speeds.is_empty() {
            if self.speed.is_empty() {
                self.speed = RECOMMENDED_BURN_SPEED.to_string();
            }
            return;
        }
        if self.speed == "Default" || !burner.write_speeds.iter().any(|speed| speed == &self.speed)
        {
            self.speed = burner
                .lowest_write_speed()
                .unwrap_or(RECOMMENDED_BURN_SPEED)
                .to_string();
        }
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
        let mode = if self.running_real_burn {
            "Real burn"
        } else {
            "Burn simulation"
        };
        self.running_real_burn = false;
        self.status = if status.success() {
            format!("{mode} completed")
        } else {
            format!("{mode} failed: {status}")
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

pub(crate) fn validate_burn_target_path(path: &Path) -> Result<(), String> {
    let is_cue = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"));
    if !is_cue {
        return Err("burn target must be a CUE sheet".to_string());
    }
    if !path.is_file() {
        return Err(format!("CUE sheet not found: {}", path.display()));
    }
    let files = cue_referenced_files(path)?;
    if files.is_empty() {
        return Err(format!("{} references no disc images", path.display()));
    }
    for file in files {
        if !file.is_file() {
            return Err(format!("CUE sidecar not found: {}", file.display()));
        }
    }
    Ok(())
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn burn_backend_supported() -> bool {
    cfg!(target_os = "macos")
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
    let mut burners = parse_drutil_list(&text);
    for burner in &mut burners {
        let info_writes_cd = drive_reports_cd_write_info(burner.drive);
        let status = drive_status(burner.drive);
        if info_writes_cd || status.writable {
            burner.writes_cd = true;
        }
        burner.write_speeds = status.write_speeds;
    }
    Ok(burners)
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
            drive: parse_drive_number(line).unwrap_or_else(|| burners.len() + 1),
            name: format!("{vendor} {product}"),
            bus: bus.to_string(),
            support: support.to_string(),
            writes_cd: !support.eq_ignore_ascii_case("unsupported"),
            write_speeds: Vec::new(),
        });
    }
    burners
}

fn drive_reports_cd_write_info(drive: usize) -> bool {
    let Ok(output) = Command::new("drutil")
        .arg("-drive")
        .arg(drive.to_string())
        .arg("info")
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    parse_drutil_info_cd_write(&String::from_utf8_lossy(&output.stdout))
}

#[derive(Default)]
struct DriveStatus {
    writable: bool,
    write_speeds: Vec<String>,
}

fn drive_status(drive: usize) -> DriveStatus {
    let Ok(output) = Command::new("drutil")
        .arg("-drive")
        .arg(drive.to_string())
        .arg("status")
        .output()
    else {
        return DriveStatus::default();
    };
    if !output.status.success() {
        return DriveStatus::default();
    }
    parse_drutil_status(&String::from_utf8_lossy(&output.stdout))
}

fn parse_drutil_info_cd_write(text: &str) -> bool {
    text.lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case("CD-Write")
                .then(|| value.trim())
        })
        .is_some_and(|value| !value.is_empty() && !value.eq_ignore_ascii_case("none"))
}

fn parse_drutil_status(text: &str) -> DriveStatus {
    let mut status = DriveStatus::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("Writability") {
            let value = value.trim().to_ascii_lowercase();
            status.writable = value.contains("blank")
                || value.contains("appendable")
                || value.contains("overwritable");
        } else if key.trim().eq_ignore_ascii_case("Write Speeds") {
            status.write_speeds = parse_write_speeds(value);
        }
    }
    status
}

fn parse_write_speeds(value: &str) -> Vec<String> {
    let mut speeds = value
        .split(',')
        .filter_map(normalize_write_speed)
        .collect::<Vec<_>>();
    speeds.sort_by_key(|speed| speed_number(speed).unwrap_or(u32::MAX));
    speeds.dedup();
    speeds
}

fn normalize_write_speed(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches('x').trim();
    let speed = value.parse::<u32>().ok()?;
    (speed > 0).then(|| format!("{speed}x"))
}

fn speed_number(speed: &str) -> Option<u32> {
    speed.trim().trim_end_matches('x').parse().ok()
}

fn field(line: &str, start: usize, end: usize) -> &str {
    line.get(start..end).unwrap_or("").trim()
}

fn parse_drive_number(line: &str) -> Option<usize> {
    line.trim_start().split_whitespace().next()?.parse().ok()
}

fn burner_signature(burners: &[CdBurner]) -> String {
    burners
        .iter()
        .map(|burner| {
            format!(
                "{}:{}:{}:{}:{}",
                burner.drive, burner.name, burner.bus, burner.support, burner.writes_cd
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_drutil_burner_rows() {
        let burners = parse_drutil_list(concat!(
            "   Vendor   Product           Rev   Bus       SupportLevel\n",
            "   HL-DT-ST DVDRW GX50N       RR06  USB       Apple Shipping\n",
        ));
        assert_eq!(burners.len(), 1);
        assert_eq!(burners[0].drive, 1);
        assert_eq!(burners[0].name, "HL-DT-ST DVDRW GX50N");
        assert_eq!(burners[0].bus, "USB");
        assert!(burners[0].can_burn());
    }

    #[test]
    fn unsupported_drives_are_generic_burn_candidates() {
        let burners = parse_drutil_list(concat!(
            "   Vendor   Product           Rev   Bus       SupportLevel\n",
            "1  TSSTcorp CDDVDW SN-208AB   LA02  USB       Unsupported\n",
        ));
        assert_eq!(burners.len(), 1);
        assert_eq!(burners[0].drive, 1);
        assert!(!burners[0].writes_cd);
        assert!(burners[0].can_burn());
        assert_eq!(
            burners[0].label(),
            "TSSTcorp CDDVDW SN-208AB (USB, Generic CD writer)"
        );
    }

    #[test]
    fn parses_drutil_info_cd_write_capability() {
        assert!(parse_drutil_info_cd_write(concat!(
            " Vendor   Product           Rev \n",
            " TSSTcorp CDDVDW SN-208AB   LA02\n",
            "\n",
            "   Interconnect: USB\n",
            "   SupportLevel: Unsupported\n",
            "      CD-Write: -R, -RW, BUFE, CDText, Test, IndexPts, ISRC\n",
        )));
        assert!(!parse_drutil_info_cd_write("      CD-Write: None\n"));
    }

    #[test]
    fn parses_drutil_status_writable_media() {
        let status = parse_drutil_status(concat!(
            "           Type: CD-R                 Name: /dev/disk4\n",
            "   Write Speeds: 10x, 16x, 20x, 24x\n",
            "    Writability: appendable, blank, overwritable\n",
        ));
        assert!(status.writable);
        assert_eq!(
            status.write_speeds,
            ["10x", "16x", "20x", "24x"].map(str::to_string)
        );

        let status = parse_drutil_status("    Writability: not writable\n");
        assert!(!status.writable);
        assert!(status.write_speeds.is_empty());
    }

    #[test]
    fn sorts_write_speeds_for_lowest_speed_default() {
        assert_eq!(
            parse_write_speeds("24x, 8x, 16x, 8x"),
            ["8x", "16x", "24x"].map(str::to_string)
        );
    }

    #[test]
    fn labels_unsupported_cd_writers_as_generic() {
        let burner = CdBurner {
            drive: 1,
            name: "TSSTcorp CDDVDW SN-208AB".to_string(),
            bus: "USB".to_string(),
            support: "Unsupported".to_string(),
            writes_cd: true,
            write_speeds: vec!["10x".to_string(), "16x".to_string()],
        };
        assert_eq!(
            burner.label(),
            "TSSTcorp CDDVDW SN-208AB (USB, Generic CD writer)"
        );
        assert!(burner.can_burn());
    }

    #[test]
    fn validate_burn_target_accepts_cue_with_sidecars() {
        let root = burn_test_temp_dir("valid-cue");
        let cue = root.join("demo.cue");
        fs::write(root.join("demo.bin"), b"disc").unwrap();
        fs::write(
            &cue,
            "FILE \"demo.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();

        assert!(validate_burn_target_path(&cue).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn validate_burn_target_rejects_missing_sidecars() {
        let root = burn_test_temp_dir("missing-sidecar");
        let cue = root.join("demo.cue");
        fs::write(
            &cue,
            "FILE \"missing.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();

        let error = validate_burn_target_path(&cue).unwrap_err();
        assert!(error.contains("CUE sidecar not found"));
        let _ = fs::remove_dir_all(root);
    }

    fn burn_test_temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "psoxide-burn-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
