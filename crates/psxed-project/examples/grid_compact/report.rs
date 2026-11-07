//! Stage 1: bytes per section and per feature of the current representation.

use psx_grid::{section, SECTION_RECORD_BYTES};

use crate::load::Loaded;

const NAMES: [&str; 15] = [
    "SECTORS",
    "EXTS",
    "WALLS",
    "SPLITBS",
    "COLPLANES",
    "LIQUIDS",
    "MOVERS",
    "MOVERVERTS",
    "MATUSE",
    "VERTICES",
    "QUADS",
    "CELLS",
    "LODBLOCKS",
    "LODQUADS",
    "PLANEREFS",
];

pub fn breakdown(levels: &[Loaded]) {
    for level in levels {
        println!("== {} ==", level.name);
        let lv = level.level();
        println!(
            "core {} B ({} rooms, {} portals, {} materials)",
            level.core_bytes(),
            lv.rooms.len(),
            lv.portals.len(),
            lv.materials.len()
        );
        for r in 0..level.rooms() {
            let room = level.room(r);
            let h = room.col.header;
            println!(
                "room {r}: {}x{} sectors, chunk {} B unpadded ({} B padded), flags {:#x}",
                h.width,
                h.depth,
                level.room_bytes(r),
                (level.room_bytes(r)).div_ceil(2048) * 2048,
                h.flags
            );
            for id in 0..psx_grid::SECTION_COUNT {
                let s = h.sections[id];
                println!(
                    "    {:<11} {:>6} x {:>2} B = {:>7} B",
                    NAMES[id],
                    s.count,
                    SECTION_RECORD_BYTES[id],
                    s.count as usize * SECTION_RECORD_BYTES[id]
                );
            }
            let _ = section::SECTORS;
        }
    }
}
