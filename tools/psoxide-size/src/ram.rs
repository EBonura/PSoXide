//! RAM accounting of a linked guest, read from its lld link map: the free RAM
//! a game keeps, per output section (`.text`, `.data` with the read-only data,
//! `.bss`), per crate, per module and the largest symbols. The overlay images
//! (`.overlay.*`, linked above main RAM and stored on the disc) are listed
//! apart because they do not occupy the image.
//!
//! Free RAM is `STACK_INIT - STACK_RESERVE - __heap_start`: the bytes the
//! linker leaves for the heap, which is what the link asserts against (the
//! reserve is the map's own `STACK_RESERVE`, so a game that links a smaller one
//! is measured against that).

use crate::map::{LinkMap, Section};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The SDK's own stack reserve, used when a map names none.
pub const SDK_STACK_RESERVE: u32 = 0x8000;

/// How many of the largest symbols each section lists.
const TOP: usize = 30;

/// Which output section an input section was placed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Text,
    Data,
    Bss,
    Overlay,
}

impl Kind {
    const ALL: [Kind; 4] = [Kind::Text, Kind::Data, Kind::Bss, Kind::Overlay];

    fn of(out: &str) -> Option<Kind> {
        match out {
            ".text" => Some(Kind::Text),
            ".data" => Some(Kind::Data),
            ".bss" => Some(Kind::Bss),
            name if name.starts_with(".overlay.") => Some(Kind::Overlay),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Text => ".text (code)",
            Kind::Data => ".data (data + rodata)",
            Kind::Bss => ".bss (zeroed statics)",
            Kind::Overlay => ".overlay (disc-resident code, not in the image)",
        }
    }

    fn short(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Data => "data",
            Kind::Bss => "bss",
            Kind::Overlay => "overlay",
        }
    }
}

/// One input section placed in the image.
#[derive(Debug, Clone)]
pub struct Item {
    pub kind: Kind,
    pub size: u32,
    /// Crate the object file belongs to (`game_name`, `psx_gpu`, ...).
    pub krate: String,
    /// Demangled symbol when the map names one, else the input section name.
    pub name: String,
}

/// The sections of a link map as [`Item`]s, plus the figures the headline needs.
#[derive(Debug, Default)]
pub struct Ram {
    pub items: Vec<Item>,
    /// Size of each output section, padding included.
    pub text_total: u32,
    pub data_total: u32,
    pub bss_total: u32,
    pub free: Option<u32>,
    pub stack_reserve: Option<u32>,
}

/// The crate an object file belongs to: `deps/game-<hash>.rcgu.o` is `game`,
/// `deps/libpsx_gpu-<hash>.rlib(member)` is `psx_gpu`.
fn crate_of(object: &str) -> String {
    let path = object.split(":(").next().unwrap_or(object);
    let file = path.rsplit('/').next().unwrap_or(path);
    let file = file.split('(').next().unwrap_or(file);
    let file = file.strip_prefix("lib").unwrap_or(file);
    file.split(['-', '.']).next().unwrap_or(file).to_string()
}

/// The name the map gives an input section: the first symbol under it that is
/// as large as the section and is not a script assignment, else the section name.
fn item_name(sec: &Section) -> String {
    sec.syms
        .iter()
        .find(|sym| {
            sym.size == sec.size
                && !sym.name.starts_with('.')
                && !sym.name.contains(" = ")
                && !sym.name.starts_with("PROVIDE")
        })
        .map_or_else(|| sec.name.clone(), |sym| sym.name.clone())
}

/// Read a map: placed input sections by output section, the section totals,
/// the stack reserve and the free RAM.
pub fn read(map: &LinkMap) -> Ram {
    let mut ram = Ram::default();
    for out in &map.outputs {
        match out.name.as_str() {
            ".text" => ram.text_total = out.size,
            ".data" => ram.data_total = out.size,
            ".bss" => ram.bss_total = out.size,
            _ => {}
        }
    }
    for sec in &map.sections {
        let Some(kind) = Kind::of(&sec.out) else {
            continue;
        };
        if sec.size == 0 {
            continue;
        }
        ram.items.push(Item {
            kind,
            size: sec.size,
            krate: crate_of(&sec.object),
            name: item_name(sec),
        });
    }
    ram.stack_reserve = map.stack_reserve();
    ram.free = map.heap_free();
    ram
}

/// A symbol name without the leading `<`, `& ` and `&mut ` of an impl path.
fn bare(name: &str) -> &str {
    let name = name.trim_start_matches('<');
    let name = name.strip_prefix("& ").unwrap_or(name);
    name.strip_prefix("&mut ").unwrap_or(name)
}

/// The module a symbol sits in: `game::entity::Scene::update` is
/// `game::entity`, a function directly under its crate is `game (root)`. The
/// link is fat-LTO, so the symbol path (not the object file) tells which
/// crate the code came from.
fn module_of(item: &Item) -> String {
    if item.name.starts_with('.') {
        return format!("{} (unnamed)", item.krate);
    }
    let parts: Vec<&str> = bare(&item.name).split("::").collect();
    match parts.len() {
        0 | 1 => item.krate.clone(),
        2 => format!("{} (root)", parts[0]),
        _ => format!("{}::{}", parts[0], parts[1]),
    }
}

/// The crate a symbol belongs to for the "by crate" table: the first path
/// segment when the map names the symbol, else the object file's crate.
fn origin_of(item: &Item) -> String {
    if item.name.starts_with('.') {
        return format!("{} (unnamed)", item.krate);
    }
    let name = bare(&item.name);
    match name.split("::").next() {
        Some(first) if name.contains("::") => first.to_string(),
        _ => item.krate.clone(),
    }
}

fn kb(bytes: u32) -> String {
    format!("{}.{} KB", bytes / 1024, (bytes % 1024) * 10 / 1024)
}

fn table(out: &mut String, title: &str, rows: BTreeMap<String, u32>, limit: usize) {
    let mut rows: Vec<(String, u32)> = rows.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let _ = writeln!(out, "  {title}");
    for (name, bytes) in rows.iter().take(limit) {
        let _ = writeln!(out, "    {bytes:>8}  {name}");
    }
    if rows.len() > limit {
        let rest: u32 = rows.iter().skip(limit).map(|row| row.1).sum();
        let _ = writeln!(out, "    {rest:>8}  ({} more)", rows.len() - limit);
    }
}

/// The full report. The first line is the number a budget gate reads.
pub fn report(ram: &Ram) -> String {
    let mut out = String::new();
    let free = ram.free.unwrap_or(0);
    let _ = writeln!(
        out,
        "heap region {free} B (link time: STACK_INIT - STACK_RESERVE - __heap_start)"
    );
    let _ = writeln!(
        out,
        "stack reserve {} B (the link script's STACK_RESERVE)",
        ram.stack_reserve.unwrap_or(SDK_STACK_RESERVE)
    );
    let image = ram.text_total + ram.data_total + ram.bss_total;
    let _ = writeln!(
        out,
        "image {} B = .text {} + .data {} + .bss {}; RAM region {} B for the image, {} B free ({})",
        image,
        ram.text_total,
        ram.data_total,
        ram.bss_total,
        image + free,
        free,
        kb(free)
    );
    for kind in Kind::ALL {
        let items: Vec<&Item> = ram.items.iter().filter(|i| i.kind == kind).collect();
        let total: u32 = items.iter().map(|i| i.size).sum();
        let _ = writeln!(
            out,
            "\n{} {} B ({}), {} items",
            kind.label(),
            total,
            kb(total),
            items.len()
        );
        let mut by_crate = BTreeMap::new();
        let mut by_module = BTreeMap::new();
        let mut by_symbol = BTreeMap::new();
        for item in &items {
            *by_crate.entry(origin_of(item)).or_insert(0) += item.size;
            *by_module.entry(module_of(item)).or_insert(0) += item.size;
            *by_symbol
                .entry(format!("{} [{}]", item.name, item.krate))
                .or_insert(0) += item.size;
        }
        table(&mut out, "by crate", by_crate, 12);
        table(&mut out, "by module", by_module, 24);
        table(&mut out, "largest symbols", by_symbol, TOP);
    }
    out
}

/// Every placed input section, one per line: `kind size crate name`, largest
/// first, tab separated. For diffing two links or finding a symbol the report
/// folded away.
pub fn items(ram: &Ram) -> String {
    let mut rows: Vec<&Item> = ram.items.iter().collect();
    rows.sort_by(|a, b| b.size.cmp(&a.size).then(a.name.cmp(&b.name)));
    let mut out = String::new();
    for item in rows {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}",
            item.kind.short(),
            item.size,
            item.krate,
            item.name
        );
    }
    out
}

/// A budget file: the first number on the first non-comment line is the floor.
pub fn parse_floor(text: &str) -> Option<u32> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::parse;

    const MAP: &str = "\
     VMA      LMA     Size Align Out     In      Symbol
       0        0        0     1 STACK_INIT = RAM_BASE + 0x001FFF00
       0        0        0     1 RAM_BASE = 0x80000000
       0        0        0     1 STACK_RESERVE = 0xD000
80408000 80408000       40     4 .overlay.game_x.text
80408000 80408000       40     4         /r/game/target/deps/game-1.rcgu.o:(.overlay.game_x.text)
80408000 80408000       40     1                 game::monster_code::thing
80010000 80010000      100     4 .text
80010000 80010000       78     4         /r/game/target/deps/game-1.rcgu.o:(.text._start)
80010000 80010000       78     1                 _start
80010078 80010078       88     4         /r/game/target/deps/game-1.rcgu.o:(.text.pose)
80010078 80010078       88     1                 <game_core::entity::Scene>::pose
80010100 80010100       20     4 .data
80010100 80010100       10     4         /r/game/target/deps/game-1.rcgu.o:(.rodata..L__unnamed_3)
80010200 80010200      400     8 .bss
80010200 80010200      300     8         /r/game/target/deps/game-1.rcgu.o:(.bss._RNvCs_4game3BIG)
80010200 80010200      300     1                 game::asset::BIG (.0)
80010600 80010600        0     1         __bss_end = .
80010600 80010600        0     1 __heap_start = .
80010600 80010600        0     1 __heap_end = STACK_INIT - STACK_RESERVE
";

    #[test]
    fn attributes_sections_origins_and_modules() {
        let ram = read(&parse(MAP));
        assert_eq!(ram.text_total, 0x100);
        let sum = |kind| -> u32 {
            ram.items
                .iter()
                .filter(|i| i.kind == kind)
                .map(|i| i.size)
                .sum()
        };
        assert_eq!(sum(Kind::Overlay), 0x40);
        assert_eq!(sum(Kind::Text), 0x78 + 0x88);
        let find = |needle: &str| ram.items.iter().find(|i| i.name.contains(needle)).unwrap();
        let pose = find("pose");
        assert_eq!(origin_of(pose), "game_core");
        assert_eq!(module_of(pose), "game_core::entity");
        assert_eq!(module_of(find("BIG")), "game::asset");
        assert_eq!(module_of(find("unnamed")), "game (unnamed)");
    }

    #[test]
    fn free_follows_the_maps_stack_reserve() {
        let ram = read(&parse(MAP));
        assert_eq!(ram.stack_reserve, Some(0xd000));
        assert_eq!(ram.free, Some(0x801f_ff00 - 0xd000 - 0x8001_0600));
    }

    #[test]
    fn floor_file_skips_comments() {
        assert_eq!(parse_floor("# floor\n\n12288  # bytes\n"), Some(12288));
        assert_eq!(parse_floor("# only a comment\n"), None);
    }
}
