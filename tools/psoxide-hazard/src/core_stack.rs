//! The scene core's stack depths, from its built image: every function's frame (its first
//! `addiu $sp, $sp, -n`) and its deepest chain of calls (`jal`) and tail calls (`j` to another
//! function), each counted with the caller's frame still on the stack. `jalr` calls (the program's
//! hooks) are not followed: a slot's published depth is its own, a hook's adds to it.
//!
//! `core-stack` checks that every jump-table slot's depth is the one psx-scene-abi publishes
//! (`psx_scene_abi::stack`), so a core change that deepens a slot cannot ship with stale figures.

use std::collections::HashMap;

/// A function of the image: start address, size in bytes, name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    /// First instruction's address.
    pub addr: u32,
    /// Bytes.
    pub size: u32,
    /// Symbol name as the map spells it.
    pub name: String,
}

/// The functions of an lld link map: symbol lines (alignment 1, a name, not an assignment).
#[must_use]
pub fn functions_from_map(map: &str) -> Vec<Function> {
    let mut out = Vec::new();
    for line in map.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [vma, _lma, size, align, name, ..] = fields.as_slice() else {
            continue;
        };
        if *align != "1" || fields.len() != 5 || name.contains("rcgu") || name.ends_with("::ST") {
            continue;
        }
        let (Ok(addr), Ok(size)) = (u32::from_str_radix(vma, 16), u32::from_str_radix(size, 16))
        else {
            continue;
        };
        if size != 0 {
            out.push(Function {
                addr,
                size,
                name: (*name).to_owned(),
            });
        }
    }
    out
}

/// What one function does to the stack.
#[derive(Clone, Debug, Default)]
struct Shape {
    frame: u32,
    calls: Vec<usize>,
    tails: Vec<usize>,
    hooks: bool,
}

/// Every function's depth below its caller's `$sp`, and whether it calls a hook, for an image
/// linked at `base`.
#[must_use]
pub fn depths(image: &[u8], base: u32, functions: &[Function]) -> HashMap<String, (u32, bool)> {
    let of = |addr: u32| {
        functions
            .iter()
            .position(|f| addr >= f.addr && addr < f.addr + f.size)
    };
    let mut shapes = vec![Shape::default(); functions.len()];
    for (i, f) in functions.iter().enumerate() {
        let mut framed = false;
        let mut pc = f.addr;
        while pc < f.addr + f.size {
            let at = (pc - base) as usize;
            let Some(word) = image.get(at..at + 4) else {
                break;
            };
            let w = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            let op = w >> 26;
            let target = (pc & 0xf000_0000) | ((w & 0x03ff_ffff) << 2);
            match op {
                // The first addiu $sp, $sp, -n.
                0x09 if !framed
                    && (w >> 21) & 31 == 29
                    && (w >> 16) & 31 == 29
                    && w & 0x8000 != 0 =>
                {
                    shapes[i].frame = 0x1_0000 - (w & 0xffff);
                    framed = true;
                }
                0x03 => {
                    if let Some(j) = of(target) {
                        shapes[i].calls.push(j);
                    }
                }
                0x02 => {
                    if let Some(j) = of(target).filter(|&j| j != i) {
                        shapes[i].tails.push(j);
                    }
                }
                0x00 if w & 0x3f == 0x09 => shapes[i].hooks = true,
                _ => {}
            }
            pc += 4;
        }
    }
    type Memo = [Option<(u32, bool)>];
    fn depth(i: usize, shapes: &[Shape], memo: &mut Memo, seen: &mut [bool]) -> (u32, bool) {
        if let Some(d) = memo[i] {
            return d;
        }
        if seen[i] {
            return (0, false);
        }
        seen[i] = true;
        let s = &shapes[i];
        let (mut deepest, mut hooks) = (0, s.hooks);
        for &c in s.calls.iter().chain(&s.tails) {
            let (d, h) = depth(c, shapes, memo, seen);
            deepest = deepest.max(d);
            hooks |= h;
        }
        let d = (s.frame + deepest, hooks);
        seen[i] = false;
        memo[i] = Some(d);
        d
    }
    let mut memo = vec![None; functions.len()];
    let mut seen = vec![false; functions.len()];
    let mut out = HashMap::new();
    for (i, f) in functions.iter().enumerate() {
        out.insert(f.name.clone(), depth(i, &shapes, &mut memo, &mut seen));
    }
    out
}

/// The function a jump-table slot jumps to: its `j` target.
#[must_use]
pub fn slot_target(image: &[u8], base: u32, slot: u32) -> Option<u32> {
    let at = slot.checked_sub(base)? as usize;
    let word = image.get(at..at + 4)?;
    let w = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
    (w >> 26 == 0x02).then_some((slot & 0xf000_0000) | ((w & 0x03ff_ffff) << 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(w: u32) -> [u8; 4] {
        w.to_le_bytes()
    }

    #[test]
    fn a_chain_counts_every_frame_and_a_tail_call_counts_with_its_caller() {
        // 0x100: a: addiu sp,-24; jal c; nop; j b; nop
        // 0x114: b: addiu sp,-8; jr ra; nop
        // 0x120: c: addiu sp,-40; jalr t0; nop; jr ra
        let base = 0x100;
        let mut image = Vec::new();
        for w in [
            0x27bd_ffe8,
            0x0c00_0000 | (0x120 >> 2),
            0,
            0x0800_0000 | (0x114 >> 2),
            0,
            0x27bd_fff8,
            0x03e0_0008,
            0,
            0x27bd_ffd8,
            0x0100_f809,
            0,
            0x03e0_0008,
        ] {
            image.extend_from_slice(&word(w));
        }
        let f = |addr, size, name: &str| Function {
            addr,
            size,
            name: name.to_owned(),
        };
        let functions = [f(0x100, 0x14, "a"), f(0x114, 0xc, "b"), f(0x120, 0x10, "c")];
        let d = depths(&image, base, &functions);
        assert_eq!(d["c"], (40, true));
        assert_eq!(d["b"], (8, false));
        assert_eq!(d["a"], (24 + 40, true), "a reaches the hook through c");
        assert_eq!(slot_target(&image, base, 0x10c), Some(0x114));
        assert_eq!(slot_target(&image, base, 0x100), None);
    }

    #[test]
    fn map_symbol_lines_become_functions() {
        let map = "801fdf00 801fdf00       68     1         x.rcgu.o:(.text.a)\n\
                   801fdf68 801fdf68       a0     1                 psx_core_image::write_block\n\
                   801fe008 801fe008        0     1                 marker\n\
                   801fe008 801fe008       2c     4         .text\n";
        let f = functions_from_map(map);
        assert_eq!(
            f,
            [Function {
                addr: 0x801f_df68,
                size: 0xa0,
                name: "psx_core_image::write_block".into()
            }]
        );
    }
}
