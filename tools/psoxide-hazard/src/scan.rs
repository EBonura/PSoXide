//! `hazard-scan`: prove a PS-EXE free of R3000 load-delay hazards created by
//! branch delay slots, whatever built it.

use std::io::Write;
use std::path::Path;

use crate::detect::{
    every_word, is_branch, looks_like_code, straight_line_pairs, Detector, Image, IsCode, Unlisted,
    TABLE_CEILING,
};
use crate::linkmap::{io_message, LinkMap};
use crate::listing::{load_address, Listing, HEADER};
use crate::module::{modules, Module, Overlay};
use crate::text::strip;
use crate::{cli_args, open_map, report_unlisted, text_bounds};

/// Usage text.
pub const USAGE: &str = "\
Scan a PS-EXE for R3000 load-delay hazards created by branch delay slots.

The R3000 has no load interlock: the instruction after a load still sees the
register's old value. LLVM inserts the required nop after a load, but its
MipsDelaySlotFiller can then hoist that load into a branch delay slot, and
the first instruction of the branch target (or of the fall-through) reads
the register one instruction too early. A guest either passes
`-Cllvm-args=-disable-mips-df-backward-search`, or keeps every filler search
on and runs `hazard-patch` after the link (`tools/sdk-examples.mk` does the
latter). This scan proves an image is clean, whatever built it.

    hazard-scan path/to/game.exe [more.exe ...]
    hazard-scan path/to/game.exe --map path/to/game.map
    hazard-scan path/to/game.exe              # no map: heuristic, see below

Prints every hazard as `branch | delay-slot load | consumer` and exits 1 if
any image has one. Loads into $zero (cache probes) are ignored, and so is
anything within 16 words of a word that does not decode as an instruction.
`--map` (one image only) resolves jump tables as `hazard-patch --map` does,
and scans only the map's `.text` (plus `--code LO..HI` ranges and
`--module LINK_LO..LINK_HI@IMAGE_AT` code modules, which are linked at one
address and sit in the file at another, as `hazard-patch` describes), as
`hazard-patch --map` patches only it (`--text-only` is accepted and does
nothing). Without a map, or with
`--whole-image`, every word of the load that looks like code is scanned, and
data that decodes as a branch can be reported.
It also warns, without failing, about a GTE command (COP2 `cofun`) in a
branch delay slot: an interrupt taken on it runs the branch and the command
twice. A slot load whose consumer cannot be seen from the image (`jr ra`,
`jalr`, a `jr` whose jump table cannot be resolved) counts as a hazard.
";

/// psx-spx's test for a GTE command: opcode 0x12 (COP2) with bit 25 set.
pub fn is_gte_command(word: i64) -> bool {
    word & 0xFE00_0000 == 0x4A00_0000
}

/// Addresses of branches whose delay slot holds a GTE command.
pub fn gte_in_delay_slots(image: &Image<'_>, is_code: IsCode<'_>) -> Vec<i64> {
    image
        .listing
        .iter()
        .filter(|(addr, e)| {
            is_branch(e.op)
                && image.listing.contains(addr + 4)
                && is_gte_command(image.word_at(addr + 4))
                && is_code(image.listing, *addr)
        })
        .map(|(addr, _)| addr)
        .collect()
}

/// Why a scan stopped before reporting.
pub enum ScanError {
    /// The image or map could not be read, or the map is from another link;
    /// the message is already printed.
    Failed(i32),
    /// A jump table entry lands on an unlisted word.
    Unlisted(Unlisted),
}

/// Scan one image: print its warnings to `out` and return one line per
/// hazard. `is_code` is the data guard (see [`looks_like_code`]).
pub fn scan(
    path: &Path,
    map_path: Option<&str>,
    is_code: IsCode<'_>,
    out: &mut dyn Write,
) -> Result<Vec<String>, ScanError> {
    scan_with_ceiling(path, map_path, is_code, TABLE_CEILING, out)
}

/// [`scan`] with another cap on the entries one table may have without a
/// map (see [`TABLE_CEILING`]).
pub fn scan_with_ceiling(
    path: &Path,
    map_path: Option<&str>,
    is_code: IsCode<'_>,
    table_ceiling: usize,
    out: &mut dyn Write,
) -> Result<Vec<String>, ScanError> {
    scan_in(path, map_path, is_code, table_ceiling, None, &[], out)
}

fn scan_in(
    path: &Path,
    map_path: Option<&str>,
    is_code: IsCode<'_>,
    table_ceiling: usize,
    text: Option<&[(i64, i64)]>,
    modules: &[Module],
    out: &mut dyn Write,
) -> Result<Vec<String>, ScanError> {
    let file = std::fs::read(path).map_err(|error| {
        let _ = writeln!(out, "{}", io_message(&error, path));
        ScanError::Failed(1)
    })?;
    let link_map: Option<LinkMap> = open_map(map_path, &file, out).map_err(ScanError::Failed)?;
    let base = load_address(&file);
    // A code module sits in the file at one address and runs at another:
    // scan the image with it at its link address.
    let overlay = Overlay::new(modules.to_vec(), &file, base, text.unwrap_or(&[]), out)
        .map_err(ScanError::Failed)?;
    let data = overlay.expand(&file, base);
    let mut listing = Listing::new(&data, base);
    if let Some(ranges) = text {
        listing.retain_text(ranges);
    }
    let image_end = base + data.len() as i64 - HEADER;
    let image = Image {
        listing: &listing,
        data: &data,
        base,
        image_end,
    };
    let mut detector = Detector::new(image, link_map.as_ref());
    detector.table_ceiling = table_ceiling;
    if link_map.is_none() {
        if let Some(note) = detector.unmapped_warning(is_code) {
            let _ = writeln!(out, "{note}");
        }
    }
    let gte_slots = gte_in_delay_slots(&image, is_code);
    if !gte_slots.is_empty() {
        let list: Vec<String> = gte_slots.iter().map(|a| format!("{a:08x}")).collect();
        let _ = writeln!(
            out,
            "warning: {} GTE commands in branch delay slots, run twice by an interrupt taken on them: {}",
            gte_slots.len(),
            list.join(" ")
        );
    }
    let straight = straight_line_pairs(&listing, is_code).len();
    if straight != 0 {
        let _ = writeln!(
            out,
            "warning: {straight} straight-line load-use pairs (next instruction reads the loaded register)"
        );
    }
    let found = detector.find_hazards(is_code);
    for warning in detector.take_warnings() {
        let _ = writeln!(out, "{warning}");
    }
    let mut hazards = Vec::new();
    for h in found.map_err(ScanError::Unlisted)? {
        let site = format!(
            "{:08x}: {} {} | slot {} {}",
            h.addr, h.op, h.args, h.slot_op, h.slot_args
        );
        hazards.push(match h.consumer {
            Some(c) => {
                let e = listing.get(c).expect("consumer is listed");
                format!("{site} | {c:08x}: {} {}", e.op, e.args)
            }
            None if h.op == "jalr" => format!("{site} | callee unknown"),
            None if strip(&h.args) == "ra" => format!("{site} | the caller's first instruction"),
            None => format!("{site} | jump table not resolved, target unknown"),
        });
    }
    Ok(hazards)
}

/// Run the scanner with command-line `args` (no program name); returns the
/// exit status.
pub fn main(args: &[String], out: &mut dyn Write) -> i32 {
    match text_bounds(args, out) {
        Ok(Some(text)) => main_in(args, &text, out),
        Ok(None) => main_with(args, &looks_like_code, out),
        Err(Some(status)) => status,
        Err(None) => {
            let _ = write!(out, "{USAGE}");
            2
        }
    }
}

/// [`main`] over `text`, the `[lo, hi)` bounds of the image's `.text` from
/// its link map: only those words are listed, and all of them are code.
pub fn main_in(args: &[String], text: &[(i64, i64)], out: &mut dyn Write) -> i32 {
    run(args, &every_word, Some(text), out)
}

/// [`main`] with another data guard.
pub fn main_with(args: &[String], is_code: IsCode<'_>, out: &mut dyn Write) -> i32 {
    run(args, is_code, None, out)
}

fn run(
    args: &[String],
    is_code: IsCode<'_>,
    text: Option<&[(i64, i64)]>,
    out: &mut dyn Write,
) -> i32 {
    let usage = |out: &mut dyn Write| {
        let _ = write!(out, "{USAGE}");
        2
    };
    let Some((paths, _, map_path)) = cli_args(args) else {
        return usage(out);
    };
    if paths.is_empty() || (map_path.is_some() && paths.len() != 1) {
        return usage(out);
    }
    let modules = match modules(args) {
        Ok(modules) => modules,
        Err(error) => {
            let _ = writeln!(out, "{error}");
            return 2;
        }
    };
    let mut total = 0;
    for path in &paths {
        let hazards = match scan_in(
            Path::new(path),
            map_path.as_deref(),
            is_code,
            TABLE_CEILING,
            text,
            &modules,
            out,
        ) {
            Ok(hazards) => hazards,
            Err(ScanError::Failed(status)) => return status,
            Err(ScanError::Unlisted(missing)) => return report_unlisted(&missing, out),
        };
        for hazard in &hazards {
            let _ = writeln!(out, "{hazard}");
        }
        let _ = writeln!(out, "{} hazards in {path}", hazards.len());
        total += hazards.len();
    }
    i32::from(total != 0)
}
