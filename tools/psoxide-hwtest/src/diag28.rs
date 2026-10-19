//! The v2.8 sweeps: the CD radius table (`src/radius.rs` in the guest, records
//! `0xB00`-`0xBFF`) and the GPU list node size table (`src/ll_sweep.rs`,
//! `0xC00`-`0xC47`), decoded from a capture into rows.
//!
//! The radius rows carry the group checksum the guest computed and the one
//! the host recomputes from the filler formula, so a sector that arrived with
//! wrong data is named even if the guest's own compare was not what it seemed.

use std::collections::HashMap;
use std::io::Write;

use crate::radius_fill::group_fold;
use crate::report::{parse_capture, payloads_from_paths, Capture};
use crate::util::{Error, Result};

const RADIUS_META: u32 = 0xB00;
const RADIUS_POSITION: u32 = 0xB10;
const RADIUS_POSITIONS_MAX: u32 = 28;
const LL_FIRST: u32 = 0xC00;
const LL_SIZES: [u32; 9] = [16, 20, 24, 28, 32, 36, 41, 48, 64];
const METHODS: [&str; 3] = ["sdk_2x", "raw_2x", "raw_1x"];
const CODES: [&str; 4] = ["ok", "retried", "wrong_data", "short"];

fn triples(capture: &Capture) -> HashMap<u32, (i64, i64, i64)> {
    capture
        .records
        .iter()
        .map(|r| (r.record_id, (r.minimum, r.median, r.maximum)))
        .collect()
}

fn clock_text(lba: i64) -> String {
    let frames = lba + 150;
    format!("{:02}:{:02}", frames / 4500, frames % 4500 / 75)
}

/// Print both tables. Returns the number of faults the sweeps found: groups
/// that were not clean, plus list sizes that lost the interrupt or a rectangle.
pub fn report(capture: &Capture, out: &mut dyn Write) -> Result<usize> {
    let rec = triples(capture);
    let mut faults = 0;
    match rec.get(&RADIUS_META) {
        None => writeln!(out, "radius,not_in_capture")?,
        Some(meta) if meta.0 == 0 => writeln!(out, "radius,no_filler_on_disc")?,
        Some(meta) => {
            let planned = meta.0;
            let first = rec.get(&(RADIUS_META + 1)).map_or(0, |t| t.1 * 65536 + t.0);
            let sectors = rec.get(&(RADIUS_META + 2)).map_or(0, |t| t.1 * 65536 + t.0);
            let done = rec.get(&RADIUS_META).map_or(0, |t| t.1).max(0);
            writeln!(
                out,
                "radius,file_first_lba,{first},sectors,{sectors},positions_planned,{planned},positions_done,{done}"
            )?;
            writeln!(out, "radius,pos,disc_time,method,state,delivered,good,seconds,int5,timeouts,retries,first_error,fold,expected_fold,fold_ok")?;
            for p in 0..RADIUS_POSITIONS_MAX {
                let base = RADIUS_POSITION + 8 * p;
                let Some(&(lo, hi, codes)) = rec.get(&(base + 6)) else {
                    continue;
                };
                let lba = hi * 65536 + lo;
                for (m, method) in METHODS.iter().enumerate() {
                    let id = base + 2 * m as u32;
                    let (Some(&(delivered, good, centis)), Some(&(counts, retries_err, fold))) =
                        (rec.get(&id), rec.get(&(id + 1)))
                    else {
                        continue;
                    };
                    let code = CODES[((codes >> (2 * m)) & 3) as usize];
                    let expected = i64::from(group_fold((lba - first) as u32, 64));
                    let fold_ok = delivered >= 64 && fold == expected;
                    if code != "ok" || (delivered >= 64 && !fold_ok) {
                        faults += 1;
                    }
                    writeln!(
                        out,
                        "radius,{p},{},{method},{code},{delivered},{good},{}.{:02},{},{},{},0x{:02X},0x{fold:04X},0x{expected:04X},{}",
                        clock_text(lba),
                        centis / 100,
                        centis % 100,
                        counts >> 8,
                        counts & 0xFF,
                        retries_err >> 8,
                        retries_err & 0xFF,
                        if delivered >= 64 { if fold_ok { "yes" } else { "NO" } } else { "n/a" },
                    )?;
                }
            }
        }
    }
    writeln!(out, "ll,size,gpu,irq,rects_drawn,rects,dma_finished,clocks_to_irq,words_advanced,gpu_answered")?;
    let mut any = false;
    for (index, size) in LL_SIZES.iter().enumerate() {
        for idle in [false, true] {
            let id = LL_FIRST + 4 * (index as u32 * 2 + u32::from(idle));
            let (Some(&(flags, mask_lo, mask_hi)), Some(&(clk_lo, clk_hi, advanced))) =
                (rec.get(&id), rec.get(&(id + 1)))
            else {
                continue;
            };
            any = true;
            let mask = (mask_hi as u64) << 16 | mask_lo as u64;
            let rects = size / 3;
            let irq = flags & 1 != 0;
            let all = flags & 4 != 0;
            if !irq || !all {
                faults += 1;
            }
            let clocks = if clk_lo == 0xFFFF && clk_hi == 0xFFFF {
                "never".to_string()
            } else {
                (clk_hi * 65536 + clk_lo).to_string()
            };
            writeln!(
                out,
                "ll,{size},{},{},{}/{rects},{rects},{},{clocks},{advanced},{}",
                if idle { "idle" } else { "busy" },
                if irq { "yes" } else { "NO" },
                mask.count_ones(),
                if flags & 2 != 0 { "yes" } else { "NO" },
                if flags & 8 != 0 { "yes" } else { "NO" },
            )?;
        }
    }
    if !any {
        writeln!(out, "ll,not_in_capture")?;
    }
    writeln!(out, "diag28,faults,{faults}")?;
    Ok(faults)
}

/// `diag-report PAYLOAD_OR_FILE ...`: exit 1 when a sweep found a fault.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    if let Some(flag) = args.iter().find(|arg| arg.starts_with("--")) {
        return Err(Error(format!("unrecognized arguments: {flag}")));
    }
    let capture = parse_capture(&payloads_from_paths(args)?)?;
    Ok(i32::from(report(&capture, out)? != 0))
}
