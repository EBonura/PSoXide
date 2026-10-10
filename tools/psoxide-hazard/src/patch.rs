//! `hazard-patch`: fix R3000 load-delay hazards in a linked PS-EXE without
//! moving any code.

use std::io::Write;
use std::path::Path;

use crate::detect::{
    branch_sources, encode_j, every_word, is, load_destination, looks_like_code, reads, Detector,
    Hazard, Image, IsCode, COND, JUMPS, MAGIC,
};
use crate::linkmap::io_message;
use crate::listing::{load_address, pad_to_payload, word_at_offset, Listing, Padding, HEADER};
use crate::text::{fields, int_hex, strip, trailing_hex};
use crate::{cli_args, open_map, report_unlisted, text_bounds, whole_image};

/// Usage text.
pub const USAGE: &str = "\
Fix R3000 load-delay hazards in a linked PS-EXE without moving any code.

LLVM's MIPS delay-slot filler can leave a load in a branch delay slot whose
destination the next executed instruction reads; the R3000 has no load
interlock, so that instruction sees the stale register. Rebuilding with the
filler disabled costs tens of kilobytes of nops, which some guests cannot
afford. This tool instead reroutes every hazardous branch through a small
trampoline, so the consumer runs at least three instructions after the load:

    j    T            ->  j    TRAMP        TRAMP: nop ; j T ; nop
    jal  F            ->  jal  TRAMP        TRAMP: nop ; j F ; nop
    bXX  rs[,rt], T   ->  j    TRAMP        TRAMP: bXX rs[,rt], +3 ; nop
                                                   j FALL ; nop
                                                   j T    ; nop

A register jump's consumer is not in the image (`jr ra`, `jalr`, a `jr`
whose jump table cannot be resolved), so those move the load out of the
slot instead:

    jr   rs ; LOAD    ->  j TRAMP ; nop    TRAMP: LOAD ; jr rs ; nop
    jalr rs ; LOAD    ->  j TRAMP ; nop    TRAMP: LOAD ; jalr rs ; nop
                                                  j NEXT ; nop

The trampolines live in psx-rt's `HAZARD_TRAMPOLINES` .data array (magic
0x48415a54, capacity, words).

    hazard-patch game.exe --map game.map   # patch in place
    hazard-patch game.exe --map game.map --code 80100000..80120000
    hazard-patch game.exe --map game.map --check
    hazard-patch game.exe --check          # report only, exit 1 on hazards
    hazard-patch game.exe --whole-image    # patch without a map (see below)

`--map` (ld.lld's `-Map` output for the same link) is what makes patching
safe. It gives the bounds of `.text`, the only executable section, and
patching reads and rewrites only words there (plus the trampoline array, and
the jump table entries of a switch, which are data words the map proves lie
in `.rodata`). Everything else in the load is `.data`, `.rodata` and assets,
and its words decode as plausible instructions: a static slice of length 8
is `jr zero`, 0x11111111 is `beq t0,s1`. Treated as code, a table like that
is rewritten into jumps. `--code LO..HI` (hex, repeatable) adds code the
map's `.text` does not span, such as a module linked at another address into
a composite image. A map from another link is refused. Scan an image
patched with `--map` with `hazard-scan --map` too. `--text-only` is accepted
and does nothing: `--map` always bounds to `.text`.

Without a map the executable bounds are unknown, so patching refuses (exit
2) unless `--whole-image` asks for the heuristic over every word of the load:
a word is code unless an undecodable word sits within 16 words of it. That
mode can corrupt data and exists for images with no link map. `--check` only
reports, and needs neither.

Every run that is not `--check` also zero-pads the file to the payload size
its header claims. psoxide.ld starts .bss where .data ends and rounds the
header's size up to whole sectors, so the flat file the linker writes can end
up to 2047 bytes short of it; the kernel reads whole sectors, and the zeros
land over the start of .bss, which `_start` clears. This is the one step
every build runs after a link. A file short by a sector or more is refused as
truncated, and an image without a PS-X EXE header is left as it is.

Patching can change what the rescan proves (a switch whose table entries
all moved to trampolines no longer resolves, so its `jr` needs the second
kind of patch above), so a run repeats detect, patch and rescan until the
image is clean. One invocation is enough; callers must not loop.

Exit status is non-zero when a hazard cannot be patched, the array is
missing or full, or the image still has one when a pass patches nothing.
HAZARD_PATCH_SKIP=\"80012298,8008c30c\" leaves those sites alone and
HAZARD_PATCH_ONLY=\"...\" patches nothing else (diagnostics; hex addresses).
";

fn env_addresses(name: &str) -> Vec<i64> {
    std::env::var(name)
        .unwrap_or_default()
        .split(',')
        .filter(|a| !a.is_empty())
        .filter_map(int_hex)
        .collect()
}

/// Run the patcher with command-line `args` (no program name); returns the
/// exit status.
pub fn main(args: &[String], out: &mut dyn Write) -> i32 {
    match text_bounds(args, out) {
        Ok(text) => main_in(args, text.as_deref(), out),
        Err(Some(status)) => status,
        Err(None) => {
            let _ = write!(out, "{USAGE}");
            2
        }
    }
}

/// The most patch passes one run makes. Each pass that leaves hazards
/// behind has patched at least one site, and a site is only ever patched
/// once, so a run ends well before this; the cap is a backstop that turns
/// a detector that never settles into a failure instead of a hang.
const MAX_PASSES: usize = 8;

/// [`main`] over `text`, the `[lo, hi)` code ranges of the image (its link
/// map's `.text` and any `--code` ranges), when given: only those words are
/// listed, and all of them are code (see [`Listing::retain_text`]).
///
/// Runs [`pass`] until the image is clean. Patching changes what the
/// detector can prove: a switch whose table entries all moved to
/// trampolines (which live in `.data`, outside the listed `.text`) no longer
/// resolves, so its `jr` is reported again with an unknown consumer and
/// needs the second kind of patch, which moves the slot load out. One run
/// makes that second pass itself, so a build needs one invocation, not a
/// loop around it.
pub fn main_in(args: &[String], text: Option<&[(i64, i64)]>, out: &mut dyn Write) -> i32 {
    let mut passes = 1;
    loop {
        let mut again = false;
        let status = pass(args, text, out, &mut again);
        if !again || passes == MAX_PASSES {
            return status;
        }
        passes += 1;
        let _ = writeln!(out, "hazards remain after patching; pass {passes}");
    }
}

/// One detect, patch and rescan of the file in `args`. `again` is set when
/// the rescan still found hazards after this pass patched something, so a
/// further pass can make progress.
fn pass(
    args: &[String],
    text: Option<&[(i64, i64)]>,
    out: &mut dyn Write,
    again: &mut bool,
) -> i32 {
    let is_code: IsCode<'_> = if text.is_some() {
        &every_word
    } else {
        &looks_like_code
    };
    let disassemble = |data: &[u8], base: i64| {
        let mut listing = Listing::new(data, base);
        if let Some(ranges) = text {
            listing.retain_text(ranges);
        }
        listing
    };
    let Some((paths, check_only, map_path)) = cli_args(args) else {
        let _ = write!(out, "{USAGE}");
        return 2;
    };
    if paths.len() != 1 {
        let _ = write!(out, "{USAGE}");
        return 2;
    }
    if text.is_none() && !check_only && !whole_image(args) {
        let _ = writeln!(
            out,
            "refusing to patch {} without .text bounds: pass the link map (--map game.map), \
             or --whole-image to patch every word of the load that looks like code, which \
             can rewrite data",
            paths[0]
        );
        return 2;
    }
    let path = Path::new(&paths[0]);
    let mut data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) => {
            let _ = writeln!(out, "{}", io_message(&error, path));
            return 1;
        }
    };
    let link_map = match open_map(map_path.as_deref(), &data, out) {
        Ok(map) => map,
        Err(status) => return status,
    };
    if !check_only {
        match pad_to_payload(&mut data) {
            Ok(0) | Err(Padding::NotAnExe) => {}
            Ok(added) => {
                if let Err(error) = std::fs::write(path, &data) {
                    let _ = writeln!(out, "{}", io_message(&error, path));
                    return 1;
                }
                let _ = writeln!(
                    out,
                    "padded {} with {added} zero bytes to the {} bytes its header claims",
                    path.display(),
                    data.len() - HEADER as usize
                );
            }
            Err(Padding::Truncated { expected, actual }) => {
                let _ = writeln!(
                    out,
                    "{} holds {actual} payload bytes and its header claims {expected}: truncated, \
                     not just a short last sector; nothing was written",
                    path.display()
                );
                return 1;
            }
        }
    }
    let base = load_address(&data);
    let image_end = base + data.len() as i64 - HEADER;
    let listing = disassemble(&data, base);
    let hazards = {
        let detector = Detector::new(
            Image {
                listing: &listing,
                data: &data,
                base,
                image_end,
            },
            link_map.as_ref(),
        );
        let found = detector.find_hazards(is_code);
        for warning in detector.take_warnings() {
            let _ = writeln!(out, "{warning}");
        }
        match found {
            Ok(found) => found,
            Err(missing) => return report_unlisted(&missing, out),
        }
    };
    for h in &hazards {
        let via = h
            .entry
            .map_or(String::new(), |e| format!(" via table entry {e:08x}"));
        let consumer = match h.consumer {
            Some(c) => format!("{c:08x}"),
            None if h.op == "jalr" => "the callee".into(),
            None if strip(&h.args) == "ra" => "the caller".into(),
            None => "the jump target (table not resolved)".into(),
        };
        let _ = writeln!(
            out,
            "hazard {:08x}: {} {} | slot {} {} | consumer {consumer}{via}",
            h.addr, h.op, h.args, h.slot_op, h.slot_args
        );
    }
    let path_text = path.display();
    if hazards.is_empty() {
        let _ = writeln!(out, "0 hazards in {path_text}");
        return 0;
    }
    if check_only {
        let _ = writeln!(out, "{} hazards in {path_text}", hazards.len());
        return 1;
    }

    let word_at = |data: &[u8], addr: i64| -> u32 {
        word_at_offset(data, addr - base + HEADER).expect("word inside the image")
    };
    // With `.text` bounds the only words this tool may write are in `.text`
    // (the rerouted branches) or in the trampoline array; a write anywhere
    // else is recorded and fails the run before the file is touched. Jump
    // table entries are data words by design and go through `put_entry`.
    let tramp_area = std::cell::Cell::new((0i64, 0i64));
    let outside_text = std::cell::RefCell::new(Vec::new());
    let write = |data: &mut [u8], addr: i64, value: u32| {
        let off = (addr - base + HEADER) as usize;
        data[off..off + 4].copy_from_slice(&value.to_le_bytes());
    };
    let put_word = |data: &mut [u8], addr: i64, value: u32| {
        if let Some(ranges) = text {
            let (area_lo, area_hi) = tramp_area.get();
            let inside = |&(lo, hi): &(i64, i64)| lo <= addr && addr + 4 <= hi;
            if !ranges.iter().any(inside) && !inside(&(area_lo, area_hi)) {
                outside_text.borrow_mut().push(addr);
                return;
            }
        }
        write(data, addr, value);
    };

    // The trampoline array: magic, capacity, then free words.
    let mut area = None;
    let mut off = HEADER;
    while off < data.len() as i64 - 8 {
        if word_at_offset(&data, off).map(i64::from) == Some(MAGIC) {
            let capacity = word_at_offset(&data, off + 4).unwrap_or(0);
            if 0 < capacity && capacity <= 4096 {
                area = Some((base + off - HEADER + 8, i64::from(capacity)));
                break;
            }
        }
        off += 4;
    }
    let Some((area_start, capacity)) = area else {
        let _ = writeln!(
            out,
            "no HAZARD_TRAMPOLINES array (magic {MAGIC:#x}) in {path_text}"
        );
        return 1;
    };
    tramp_area.set((area_start, area_start + capacity * 4));
    // An earlier pass may have used the area. Its trampolines contain nops,
    // so the first zero word is not free space: resume after the last
    // non-zero word plus the nop in its delay slot (every trampoline ends
    // with a jump and one nop).
    let used = (0..capacity)
        .rev()
        .find(|i| word_at(&data, area_start + i * 4) != 0);
    let mut cursor = used.map_or(0, |last| last + 2);

    let nop = 0u32;
    let mut patched = 0;
    let skip = env_addresses("HAZARD_PATCH_SKIP");
    let only = env_addresses("HAZARD_PATCH_ONLY");
    let left_alone =
        |addr: i64| skip.contains(&addr) || (!only.is_empty() && !only.contains(&addr));
    // A conditional branch whose slot load is consumed on both paths is one
    // site, not two: its trampoline already covers the target and the
    // fall-through, and patching it twice would rewrite the first `j TRAMP`.
    let mut seen = Vec::new();
    // Table entries pointing at the same consumer share one trampoline.
    let mut table_trampolines: Vec<(i64, i64)> = Vec::new();
    for h in &hazards {
        let Hazard {
            addr,
            op,
            args,
            slot_op,
            slot_args,
            consumer,
            entry,
        } = h;
        let addr = *addr;
        let op = *op;
        if let (Some(entry), Some(consumer)) = (entry, consumer) {
            if left_alone(addr) {
                let _ = writeln!(out, "left alone {addr:08x} (diagnostic request)");
                continue;
            }
            let tramp = match table_trampolines.iter().find(|t| t.0 == *consumer) {
                Some(&(_, tramp)) => tramp,
                None => {
                    let tramp = area_start + cursor * 4;
                    let words = [nop, encode_j(*consumer, false), nop];
                    if cursor + words.len() as i64 > capacity {
                        let _ = writeln!(
                            out,
                            "trampoline array full at {addr:08x} ({capacity} words)"
                        );
                        return 1;
                    }
                    for (i, w) in words.iter().enumerate() {
                        put_word(&mut data, tramp + i as i64 * 4, *w);
                    }
                    cursor += words.len() as i64;
                    table_trampolines.push((*consumer, tramp));
                    tramp
                }
            };
            write(&mut data, *entry, tramp as u32);
            patched += 1;
            let _ = writeln!(
                out,
                "patched table entry {entry:08x} (jr at {addr:08x}) -> trampoline {tramp:08x} -> {consumer:08x}"
            );
            continue;
        }
        if seen.contains(&addr) {
            continue;
        }
        seen.push(addr);
        if left_alone(addr) {
            let _ = writeln!(out, "left alone {addr:08x} (diagnostic request)");
            continue;
        }
        let rd = load_destination(slot_op, slot_args).unwrap_or("");
        if op == "jr" || op == "jalr" {
            // Move the load out of the slot: the trampoline runs it, then
            // makes the original jump with a nop in its slot.
            let parts = fields(args);
            let jump_reg = parts[parts.len() - 1];
            let link_reg = if op == "jalr" && parts.len() == 2 {
                parts[0]
            } else {
                "ra"
            };
            if rd == jump_reg || (op == "jalr" && rd == link_reg) {
                let _ = writeln!(
                    out,
                    "cannot patch {addr:08x}: the slot load writes the jump or link register ({rd})"
                );
                return 1;
            }
            if op == "jalr" && reads(slot_op, slot_args, link_reg) {
                let _ = writeln!(
                    out,
                    "cannot patch {addr:08x}: the slot load reads the link register ({link_reg}), which jalr writes \
                     before its slot runs; hoisted ahead of the jalr it would read the old value"
                );
                return 1;
            }
            let mut words = vec![word_at(&data, addr + 4), word_at(&data, addr), nop];
            if op == "jalr" {
                // The callee returns into the trampoline, which goes back
                // to the original return address.
                words.extend([encode_j(addr + 8, false), nop]);
            }
            let tramp = area_start + cursor * 4;
            if cursor + words.len() as i64 > capacity {
                let _ = writeln!(
                    out,
                    "trampoline array full at {addr:08x} ({capacity} words)"
                );
                return 1;
            }
            for (i, w) in words.iter().enumerate() {
                put_word(&mut data, tramp + i as i64 * 4, *w);
            }
            cursor += words.len() as i64;
            put_word(&mut data, addr, encode_j(tramp, false));
            put_word(&mut data, addr + 4, nop);
            patched += 1;
            let _ = writeln!(
                out,
                "patched {op} {args} at {addr:08x} -> trampoline {tramp:08x} (load {slot_op} {slot_args})"
            );
            continue;
        }
        if matches!(op, "bltzal" | "bgezal" | "bal") {
            let _ = writeln!(
                out,
                "cannot patch {addr:08x}: {op} links inside the trampoline"
            );
            return 1;
        }
        if is(op, COND) && branch_sources(args).contains(&rd) {
            let _ = writeln!(
                out,
                "cannot patch {addr:08x}: the slot load writes a branch source ({rd})"
            );
            return 1;
        }
        let target = trailing_hex(args).expect("branch target");
        let original = word_at(&data, addr);
        let tramp = area_start + cursor * 4;
        let words: Vec<u32> = if is(op, JUMPS) {
            put_word(&mut data, addr, encode_j(tramp, op == "jal"));
            vec![nop, encode_j(target, false), nop]
        } else {
            let fall = addr + 8;
            put_word(&mut data, addr, encode_j(tramp, false));
            // Same opcode and registers, offset +3 words: skips nop, j FALL, nop.
            vec![
                (original & 0xFFFF_0000) | 3,
                nop,
                encode_j(fall, false),
                nop,
                encode_j(target, false),
                nop,
            ]
        };
        if cursor + words.len() as i64 > capacity {
            let _ = writeln!(
                out,
                "trampoline array full at {addr:08x} ({capacity} words)"
            );
            return 1;
        }
        for (i, w) in words.iter().enumerate() {
            put_word(&mut data, tramp + i as i64 * 4, *w);
        }
        cursor += words.len() as i64;
        patched += 1;
        let _ = writeln!(
            out,
            "patched {addr:08x} -> trampoline {tramp:08x} ({} words)",
            words.len()
        );
    }

    let outside = outside_text.into_inner();
    if !outside.is_empty() {
        let list: Vec<String> = outside.iter().map(|a| format!("{a:08x}")).collect();
        let _ = writeln!(
            out,
            "refusing to patch {path_text}: {} writes outside .text and the trampoline array \
             ({}); nothing was written",
            outside.len(),
            list.join(" ")
        );
        return 1;
    }
    if let Err(error) = std::fs::write(path, &data) {
        let _ = writeln!(out, "{}", io_message(&error, path));
        return 1;
    }
    let listing = disassemble(&data, base);
    let detector = Detector::new(
        Image {
            listing: &listing,
            data: &data,
            base,
            image_end,
        },
        link_map.as_ref(),
    );
    let remaining = detector.find_hazards(is_code);
    for warning in detector.take_warnings() {
        let _ = writeln!(out, "{warning}");
    }
    let remaining = match remaining {
        Ok(remaining) => remaining,
        Err(missing) => return report_unlisted(&missing, out),
    };
    for h in &remaining {
        let _ = writeln!(out, "still hazardous {:08x}: {} {}", h.addr, h.op, h.args);
    }
    let _ = writeln!(
        out,
        "{patched} patched, {} remaining, {cursor}/{capacity} trampoline words used in {path_text}",
        remaining.len()
    );
    if !skip.is_empty() || !only.is_empty() {
        return 0;
    }
    *again = patched > 0 && !remaining.is_empty();
    i32::from(!remaining.is_empty())
}

#[cfg(test)]
mod tests {
    #[test]
    fn usage_without_a_path() {
        let mut out = Vec::new();
        assert_eq!(super::main(&[], &mut out), 2);
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("hazard-patch game.exe"));
    }
}
