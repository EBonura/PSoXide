//! `stack-guard`: prove every scratchpad stack call tree in a linked PS-EXE
//! fits its region, and every interrupt handler fits the dispatcher's stack.

use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::path::Path;

use psx_scene_abi::stack::Slot;
use psx_scene_abi::{slot, CORE_WINDOW};

use regex::Regex;

use crate::detect::{is, Detector, Image, MAGIC, READS_ALL};
use crate::linkmap::{io_message, LinkMap};
use crate::listing::{load_address, word_at_offset, Listing, HEADER};
use crate::module::{modules, Module, Overlay};
use crate::text::{int_auto, sp_adjust, squeeze, strip, trailing_hex};

/// Usage text.
pub const USAGE: &str = "\
Prove every scratchpad stack call tree in a linked PS-EXE fits its region, and
every interrupt handler it registers with psx-rt's dispatcher fits the
dispatcher's stack.

`psx_rt::scratchpad::ScratchpadStack::<START, END>::run(f)` runs `f` with $sp
in scratchpad bytes START..END. Nothing at run time can stop a deep call tree
from running off the bottom of the region, and an inlining change can grow a
tree by hundreds of bytes without any source change. This walks the linked
image's static call graph from every monomorphised entry,

    <psx_rt::scratchpad::ScratchpadStack<START, END>>::stack_entry::<R, F>
    (and stack_entry_fn::<E>, the entry of ScratchpadStack::call)

sums frame sizes (every `addiu sp,sp,-N`; a tail call counts as a call) down
the deepest path, and fails when the total exceeds the region minus psx-rt's
20-byte overhead. It also fails on what it cannot bound: recursion, calls
through a register, register jumps it cannot prove are a switch, and $sp
adjusted any other way.

    stack-guard game.exe game.map
    stack-guard game.exe game.map --root REGEX --budget BYTES
    stack-guard game.exe game.map --forbid __psx_rt_flush_i_cache
    stack-guard game.exe game.map --module 80408000..8040c000@801c0000 --forbid __psx_rt_flush_i_cache

The map is ld.lld's `-Map` output for the same link. The exe may be
hazard-patched: calls rerouted through HAZARD_TRAMPOLINES are followed to
their targets. `--root`/`--budget` check a game's own stack switch instead
(an entry name regex and its byte budget). `--forbid SYMBOL` (repeatable)
fails a tree that reaches the function named SYMBOL: psx-rt's I-cache flush,
`__psx_rt_flush_i_cache` (and, with it, the range flush `__psx_rt_flush_i_cache_range`), runs with the scratchpad unmapped, so a frame on the
scratchpad stack would read the cache tags instead. A forbidden symbol that
is not linked is reported but does not fail. Without a map, `--forbid` fails
because symbol reachability cannot be proved; otherwise the tool only checks
that the image does not contain psx-rt's stack switch.
`--module` (repeatable) maps compact appended module bytes to their link
addresses for the proof, including calls into module code and trampolines.

The dispatcher's stack (`psx_rt::dispatch`, `PSX_DISPATCH_STACK_BYTES` bytes, 512
by default) is checked whenever the program links the dispatcher and no `--root`
is given. Its size is the map's size for `__psx_rt_dispatch_stack`; the saved
context at the top of it (112 bytes) is subtracted; every handler passed to
`psx_rt::dispatch::attach` or `on_vblank` with a constant function address is
walked like a scratchpad tree and must fit the rest, through any wrapper of up to
three levels. A registration it cannot resolve (a handler read from a table, say)
fails the proof when the stack is under 512 bytes. `--handler REGEX` (repeatable)
adds the functions whose symbol matches REGEX as handlers to prove and accepts
the unresolved registrations as covered by that list.

    stack-guard game.exe game.map                       (the whole proof)
    stack-guard game.exe game.map --handler 'game::on_timer'

A call (`jal`, `j`, or `jalr` through a register holding a constant) into the scene
core's window (psx-scene-abi's CORE_WINDOW) must land on one of its jump-table slots
and costs that slot's measured depth (psx_scene_abi::stack::SLOTS); anywhere else in
the window is refused. The switch's load runs on its own stack and is not charged.
A slot that waits on the drive also runs the program's idle hook (the function passed
to psx_core_set_idle_hook) and the read runs its per-sector hook (psx_core_read's
fourth argument), so the deepest such hook is added to the slot. Hooks are found at
every call to those two slots, followed back as handlers are. A hook that is not a
constant function fails the call; `--core-hook REGEX` (repeatable) names the hook
functions instead. A hook may not call a slot that runs hooks.

    stack-guard game.exe game.map --core-hook 'game::cd::on_sector'
";

const STACK_OVERHEAD: i64 = 20;
const SWITCH: &str = "__psx_rt_call_on_stack";
const LEAVES: [&str; 3] = [SWITCH, "rust_begin_unwind", "__rustc::rust_begin_unwind"];
const COND: &[&str] = &[
    "beq", "bne", "beqz", "bnez", "blez", "bgtz", "bltz", "bgez", "b", "bltzal", "bgezal", "bal",
];

/// A tree the guard cannot bound, or an image it cannot read.
#[derive(Debug)]
pub struct GuardError(pub String);

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A linked image and its map.
pub struct GuardImage {
    /// The whole file.
    pub data: Vec<u8>,
    /// The load address.
    pub base: i64,
    /// One past the last loaded byte.
    pub image_end: i64,
    /// Its listing.
    pub listing: Listing,
    /// Its link map.
    pub map: LinkMap,
    readable: Vec<(i64, i64)>,
    trampolines: Vec<(i64, i64)>,
}

impl GuardImage {
    /// Read `exe` and its `map_path`, refusing a map from another link.
    pub fn open(exe: &Path, map_path: &Path) -> Result<Self, GuardError> {
        Self::open_modules(exe, map_path, &[])
    }

    /// Open a compact image with its streamed modules at link addresses.
    pub fn open_modules(
        exe: &Path,
        map_path: &Path,
        modules: &[Module],
    ) -> Result<Self, GuardError> {
        let file = std::fs::read(exe).map_err(|e| GuardError(io_message(&e, exe)))?;
        let base = load_address(&file);
        let ranges: Vec<_> = modules.iter().map(|m| m.link).collect();
        let map =
            LinkMap::open_with_code(map_path, &ranges).map_err(|e| GuardError(e.to_string()))?;
        let mut diagnostics = Vec::new();
        let overlay = Overlay::new(modules.to_vec(), &file, base, &[map.text], &mut diagnostics)
            .map_err(|_| GuardError(String::from_utf8_lossy(&diagnostics).trim().to_string()))?;
        let data = overlay.expand(&file, base);
        map.check(&data).map_err(|e| GuardError(e.to_string()))?;
        let image_end = base + data.len() as i64 - HEADER;
        let listing = Listing::new(&data, base);
        let resident_end = if modules.is_empty() {
            image_end
        } else {
            base + i64::from(u32::from_le_bytes(file[0x1c..0x20].try_into().unwrap()))
        };
        let mut readable = vec![(base, resident_end)];
        readable.extend(ranges);
        let mut trampolines = map.trampolines.into_iter().collect::<Vec<_>>();
        for module in modules {
            let mut found = None;
            let mut addr = module.link.0;
            while addr + 8 <= module.link.1 {
                let offset = addr - base + HEADER;
                if word_at_offset(&data, offset).map(i64::from) == Some(MAGIC) {
                    let capacity = word_at_offset(&data, offset + 4).unwrap_or(0);
                    let end = addr + 8 + i64::from(capacity) * 4;
                    if capacity > 0
                        && capacity <= 4096
                        && end <= module.link.1
                        && found.replace((addr, end)).is_some()
                    {
                        return Err(GuardError(format!(
                            "module {:#x}..{:#x} has multiple trampoline arrays",
                            module.link.0, module.link.1
                        )));
                    }
                }
                addr += 4;
            }
            let Some(span) = found else {
                return Err(GuardError(format!(
                    "module {:#x}..{:#x} has no complete trampoline array",
                    module.link.0, module.link.1
                )));
            };
            trampolines.push(span);
        }
        Ok(Self {
            data,
            base,
            image_end,
            listing,
            map,
            readable,
            trampolines,
        })
    }

    /// The image as the detector sees it.
    pub fn image(&self) -> Image<'_> {
        Image {
            listing: &self.listing,
            data: &self.data,
            base: self.base,
            image_end: self.image_end,
        }
    }

    /// The word loaded at `addr`.
    pub fn word_at(&self, addr: i64) -> i64 {
        self.image().word_at(addr)
    }

    pub(crate) fn in_trampolines(&self, addr: i64) -> bool {
        self.trampolines
            .iter()
            .any(|&(t0, t1)| t0 <= addr && addr < t1)
    }

    fn readable(&self, start: i64, end: i64) -> bool {
        self.readable
            .iter()
            .any(|&(lo, hi)| lo <= start && start < end && end <= hi)
    }
}

/// True when the instruction's destination is $sp (stores, branches and
/// coprocessor moves only read their first operand).
fn writes_sp(op: &str, args: &str) -> bool {
    strip(args.split(',').next().unwrap_or("")) == "sp" && !is(op, READS_ALL)
}

/// A `(bytes, chain)` depth, or why there is none.
type Depth = Result<(i64, Vec<String>), GuardError>;

/// The deepest of the program's hooks of one kind that the scene core runs, `(0, [])` when it
/// registers none, or why it cannot be bounded.
type HookDepth = Result<(i64, Vec<String>), String>;

/// The program's scene core hooks, found once per walk.
#[derive(Clone)]
struct CoreHooks {
    /// The idle hook (`psx_core_set_idle_hook`'s argument), run by every slot that waits.
    idle: HookDepth,
    /// The per-sector hook (`psx_core_read`'s fourth argument), run by the read.
    sector: HookDepth,
}

/// What a call into the scene core's window may run of the program's own code.
enum CoreCalls<'a> {
    /// A program tree: hooks are found on the first call and walked; `named` are functions
    /// accepted as hooks for the registrations the scan cannot resolve (`--core-hook`).
    Program {
        named: &'a [Regex],
        hooks: Option<CoreHooks>,
    },
    /// A hook's own tree, which may not call a slot that runs hooks.
    Hook(String),
}

struct Walker<'a> {
    image: &'a GuardImage,
    detector: Detector<'a>,
    memo: HashMap<i64, (i64, Vec<String>)>,
    /// Functions no tree may reach (`--forbid`).
    forbidden: &'a [String],
    /// Call targets that cost a fixed number of bytes and are not followed (a scratchpad window's guard stubs:
    /// the callee behind a guard runs on its own, checked, budget).
    leaves: &'a HashMap<i64, i64>,
    core: CoreCalls<'a>,
}

impl<'a> Walker<'a> {
    fn new(image: &'a GuardImage, forbidden: &'a [String], leaves: &'a HashMap<i64, i64>) -> Self {
        Self::with_core_hooks(image, forbidden, leaves, &[])
    }

    fn with_core_hooks(
        image: &'a GuardImage,
        forbidden: &'a [String],
        leaves: &'a HashMap<i64, i64>,
        named: &'a [Regex],
    ) -> Self {
        Self {
            image,
            detector: Detector::new(image.image(), Some(&image.map)),
            memo: HashMap::new(),
            forbidden,
            leaves,
            core: CoreCalls::Program { named, hooks: None },
        }
    }

    /// The jump-table slot a call to `addr` inside the core's window lands on; any other address
    /// in the window is the core's interior, whose stack use is not known.
    fn core_slot(&self, addr: i64, path: &[i64]) -> Result<Slot, GuardError> {
        let caller = path
            .last()
            .and_then(|&at| self.image.map.function(at))
            .map_or_else(|| "the entry".to_string(), |(_, _, name)| short(&name));
        let (lo, hi) = (i64::from(CORE_WINDOW.start), i64::from(CORE_WINDOW.end));
        if let Some(found) = u32::try_from(addr)
            .ok()
            .and_then(psx_scene_abi::stack::slot_at)
        {
            return Ok(found);
        }
        if addr >= i64::from(slot::TABLE_END) {
            return Err(GuardError(format!(
                "{caller} calls {addr:08x}, in the scene core's window ({lo:08x}..{hi:08x}) past its jump table \
                 ({} slots, {lo:08x}..{:08x}); only a slot's stack use is known",
                slot::COUNT,
                slot::TABLE_END
            )));
        }
        Err(GuardError(format!(
            "{caller} calls {addr:08x}, inside the scene core's jump table but not at the start of one of its \
             {}-byte slots; only a slot's stack use is known",
            slot::SLOT_BYTES
        )))
    }

    /// What a call to the core `slot` costs the caller's stack: the slot's own depth, plus the
    /// deepest hook of the program that it can run.
    fn core_call(&mut self, found: Slot) -> Depth {
        let own = i64::from(found.depth);
        let label = format!("{}@{:08x}({own})", found.name, found.addr);
        let runs_hooks = found.idle_hook || found.sector_hook;
        let hooks = match &mut self.core {
            CoreCalls::Hook(hook) if runs_hooks => {
                return Err(GuardError(format!(
                    "the scene core hook {hook} calls {}, which runs the program's hooks itself; a hook may not \
                     wait on the core",
                    found.name
                )));
            }
            CoreCalls::Hook(_) => None,
            CoreCalls::Program { .. } if !runs_hooks => None,
            CoreCalls::Program { named, hooks } => {
                let named = *named;
                if hooks.is_none() {
                    *hooks = Some(core_hooks(self.image, self.forbidden, named));
                }
                hooks.clone()
            }
        };
        let mut deepest = (0, Vec::new());
        if let Some(hooks) = hooks {
            // The read calls its sector hook between sectors, not inside a wait, and a hook may
            // not wait on the core itself, so the two never stack: the deeper one counts.
            let kinds = [
                (found.idle_hook, &hooks.idle, "idle"),
                (found.sector_hook, &hooks.sector, "per-sector"),
            ];
            for (runs, depth, kind) in kinds {
                if !runs {
                    continue;
                }
                match depth {
                    Ok(depth) if depth.0 > deepest.0 => deepest = depth.clone(),
                    Ok(_) => {}
                    Err(why) => {
                        return Err(GuardError(format!(
                            "{} runs the program's {kind} hook, which cannot be bounded: {why}",
                            found.name
                        )))
                    }
                }
            }
        }
        let mut chain = vec![label];
        chain.extend(deepest.1);
        Ok((own + deepest.0, chain))
    }

    /// Call targets outside `[start, end)` for one instruction.
    fn transfers(
        &self,
        start: i64,
        end: i64,
        addr: i64,
        op: &str,
        args: &str,
        name: &str,
    ) -> Result<Vec<i64>, GuardError> {
        if op == "jal" || op == "j" || is(op, COND) {
            let Some(dest) = trailing_hex(args) else {
                return Ok(Vec::new());
            };
            if start <= dest && dest < end {
                // A branch or jump inside the function is control flow; a `jal` into its own range is a call to
                // itself (or into its middle), which is recursion the walk would otherwise never see.
                if op == "jal" {
                    return Err(GuardError(format!(
                        "{name} calls itself at {addr:08x}, so its depth has no bound"
                    )));
                }
                return Ok(Vec::new());
            }
            if self.image.in_trampolines(dest) {
                return self.trampoline(start, end, dest, name);
            }
            return Ok(vec![dest]);
        }
        if op == "jalr" {
            // A call through a register into the scene core's window is a slot call when the
            // register holds a constant (the core's slots are outside the program, so a call
            // into them may be emitted either way).
            if let Some(dest) = crate::dispatch_guard::jalr_target(self.image, addr) {
                if i64::from(CORE_WINDOW.start) <= dest && dest < i64::from(CORE_WINDOW.end) {
                    return Ok(vec![dest]);
                }
            }
            return Err(GuardError(format!(
                "{name} calls through a register at {addr:08x}; the callee cannot be bounded"
            )));
        }
        if op == "jr" && strip(args) != "ra" {
            if self.detector.jump_table(addr).is_none() {
                return Err(GuardError(format!(
                    "{name} jumps through a register at {addr:08x} ({op} {args}) and it is not a jump table it can \
                     prove; a BIOS call or a tail call through a pointer cannot be bounded"
                )));
            }
            // A proven table ends at its first entry that leaves the
            // function (a switch never does), so a dispatch calls nothing.
            return Ok(Vec::new());
        }
        Ok(Vec::new())
    }

    /// Where a hazard trampoline goes. `nop ; j T ; nop` stands for a jump,
    /// call or jump-table entry; `bXX +3 ; nop ; j FALL ; nop ; j T ; nop`
    /// for a conditional branch; `LOAD ; jr/jalr rs ; ...` for a register
    /// jump whose slot load it moved.
    fn trampoline(
        &self,
        start: i64,
        end: i64,
        tramp: i64,
        name: &str,
    ) -> Result<Vec<i64>, GuardError> {
        let listing = &self.image.listing;
        let first = listing.op(tramp);
        let jumps = if is(first, COND) {
            vec![tramp + 8, tramp + 16]
        } else if first == "nop" {
            vec![tramp + 4]
        } else {
            let (op, args) = (listing.op(tramp + 4), listing.args(tramp + 4));
            if op == "jr" && strip(args) == "ra" {
                return Ok(Vec::new());
            }
            return Err(GuardError(format!(
                "{name} jumps or calls through a register via trampoline {tramp:08x} ({op} {args})"
            )));
        };
        let mut found = Vec::new();
        for addr in jumps {
            let dest = if listing.op(addr) == "j" {
                trailing_hex(listing.args(addr))
            } else {
                None
            };
            let Some(dest) = dest else {
                return Err(GuardError(format!(
                    "{name} goes through {tramp:08x}, which is not a hazard_patch.py trampoline"
                )));
            };
            if !(start <= dest && dest < end) {
                found.push(dest);
            }
        }
        Ok(found)
    }

    /// `(bytes, chain)` for the deepest path from the function at `addr`.
    fn depth(&mut self, addr: i64, path: &mut Vec<i64>) -> Result<(i64, Vec<String>), GuardError> {
        if let Some(&cost) = self.leaves.get(&addr) {
            return Ok((cost, vec![format!("guard@{addr:08x}({cost})")]));
        }
        if i64::from(CORE_WINDOW.start) <= addr && addr < i64::from(CORE_WINDOW.end) {
            let found = self.core_slot(addr, path)?;
            return self.core_call(found);
        }
        let Some((start, end, name)) = self.image.map.function(addr) else {
            return Err(GuardError(format!("call to {addr:08x}, outside .text")));
        };
        if !self.image.readable(start, end) {
            return Err(GuardError(format!(
                "{name} at {start:08x}..{end:08x} is not wholly readable resident or declared module code"
            )));
        }
        // A function can carry several symbols (an alias); any of them counts.
        let named = |forbidden: &String| {
            self.image
                .map
                .names
                .get(&start)
                .is_some_and(|names| names.iter().any(|(_, n)| n == forbidden))
        };
        if let Some(forbidden) = self.forbidden.iter().find(|f| named(f)) {
            let callers: Vec<String> = path
                .iter()
                .filter_map(|&at| self.image.map.function(at))
                .map(|(_, _, caller)| short(&caller))
                .collect();
            return Err(GuardError(format!(
                "{} reaches {forbidden}, which no scratchpad stack tree may (--forbid)",
                callers.last().map_or("the entry", String::as_str)
            )));
        }
        if let Some(known) = self.memo.get(&start) {
            return Ok(known.clone());
        }
        if path.contains(&start) {
            return Err(GuardError(format!(
                "{name} recurses, so its depth has no bound"
            )));
        }
        let listing = &self.image.listing;
        let mut frame = 0;
        let mut pc = start;
        while pc < end {
            let (op, args) = (listing.op(pc), listing.args(pc));
            let adjust = if op == "addiu" {
                sp_adjust(&squeeze(args))
            } else {
                None
            };
            if let Some(n) = adjust {
                frame += 0.max(-n);
            } else if writes_sp(op, args)
                && !(name == SWITCH || matches!(squeeze(args).as_str(), "sp,s8" | "sp,fp"))
            {
                return Err(GuardError(format!(
                    "{name} sets $sp at {pc:08x} ({op} {args}); only addiu frames can be counted"
                )));
            }
            pc += 4;
        }
        let mut deepest: (i64, Vec<String>) = (0, Vec::new());
        if !LEAVES.iter().any(|leaf| name.ends_with(leaf)) {
            let mut pc = start;
            while pc < end {
                let (op, args) = (listing.op(pc), listing.args(pc).to_string());
                for callee in self.transfers(start, end, pc, op, &args, &name)? {
                    path.push(start);
                    let below = self.depth(callee, path);
                    path.pop();
                    let below = below?;
                    if below.0 > deepest.0 {
                        deepest = below;
                    }
                }
                pc += 4;
            }
        }
        let mut chain = vec![format!("{}({frame})", short(&name))];
        chain.extend(deepest.1);
        let result = (frame + deepest.0, chain);
        self.memo.insert(start, result.clone());
        Ok(result)
    }
}

/// Every function whose symbol matches one of `named`.
fn matching_functions(image: &GuardImage, named: &[Regex]) -> Vec<i64> {
    image
        .map
        .sorted_names()
        .into_iter()
        .filter(|(_, names)| {
            names
                .iter()
                .any(|(_, name)| named.iter().any(|r| r.is_match(name)))
        })
        .map(|(address, _)| address)
        .collect()
}

fn core_hooks(image: &GuardImage, forbidden: &[String], named: &[Regex]) -> CoreHooks {
    CoreHooks {
        idle: hook_depth(image, forbidden, named, slot::SET_IDLE_HOOK, 0),
        sector: hook_depth(image, forbidden, named, slot::READ, 3),
    }
}

/// The deepest function the program passes as argument `arg` of the core slot at `at`, walked
/// as a tree of its own. A registration it cannot resolve needs `--core-hook`.
fn hook_depth(
    image: &GuardImage,
    forbidden: &[String],
    named: &[Regex],
    at: u32,
    arg: usize,
) -> HookDepth {
    let slot_name = psx_scene_abi::stack::slot_at(at).map_or("?", |s| s.name);
    let found = crate::dispatch_guard::registrations_through(image, &[(i64::from(at), arg)], true);
    let mut hooks: Vec<(i64, String)> = found
        .handlers
        .iter()
        .map(|h| (h.address, h.from.join(", ")))
        .collect();
    if !found.unresolved.is_empty() {
        let extra = matching_functions(image, named);
        if extra.is_empty() {
            let named = if named.is_empty() {
                String::new()
            } else {
                " (no function matches the --core-hook patterns given)".to_string()
            };
            return Err(format!(
                "the hook passed to {slot_name} at {} is not resolved{named}; pass a constant function there, or \
                 name the program's hooks with --core-hook REGEX",
                found.unresolved.join("; ")
            ));
        }
        hooks.extend(
            extra
                .into_iter()
                .map(|a| (a, "named by --core-hook".to_string())),
        );
    }
    let no_leaves = HashMap::new();
    let mut deepest = (0, Vec::new());
    for (address, from) in hooks {
        let name = image
            .map
            .function(address)
            .filter(|&(start, _, _)| start == address)
            .map(|(_, _, name)| short(&name))
            .ok_or_else(|| {
                format!("{address:08x} (passed to {slot_name} in {from}) is not the start of a function")
            })?;
        let mut walker = Walker::new(image, forbidden, &no_leaves);
        walker.core = CoreCalls::Hook(name);
        let depth = walker.depth(address, &mut Vec::new()).map_err(|e| e.0)?;
        if depth.0 > deepest.0 {
            deepest = depth;
        }
    }
    Ok(deepest)
}

/// A symbol name shortened for a report line: no `::h<hash>` suffix, at
/// most 90 characters.
pub fn short(name: &str) -> String {
    let trimmed = match name.len().checked_sub(19) {
        Some(at)
            if name.is_char_boundary(at)
                && name[at..].starts_with("::h")
                && name[at + 3..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            &name[..at]
        }
        _ => name,
    };
    if trimmed.chars().count() <= 90 {
        trimmed.to_string()
    } else {
        let head: String = trimmed.chars().take(87).collect();
        format!("{head}...")
    }
}

type Root = (i64, String, i64, Option<(i64, i64)>);

fn roots(image: &GuardImage, pattern: Option<&Regex>, budget: Option<i64>) -> Vec<Root> {
    let entry = Regex::new(
        r"^<psx_rt::scratchpad::ScratchpadStack<(\d+)(?:usize)?, (\d+)(?:usize)?>>::stack_entry(?:_fn)?::<",
    )
    .expect("entry pattern");
    let mut found = Vec::new();
    for (address, entries) in image.map.sorted_names() {
        for (_, name) in entries {
            if let Some(pattern) = pattern {
                if pattern.is_match(name) {
                    found.push((address, name.clone(), budget.unwrap_or(0), None));
                }
                continue;
            }
            if let Some(m) = entry.captures(name) {
                let (Ok(lo), Ok(hi)) = (m[1].parse::<i64>(), m[2].parse::<i64>()) else {
                    continue;
                };
                found.push((
                    address,
                    name.clone(),
                    hi - lo - STACK_OVERHEAD,
                    Some((lo, hi)),
                ));
            }
        }
    }
    found
}

/// psx-rt's switch: a `jalr` with `move sp,a2` in its delay slot (`jalr a1`
/// today, `jalr t9` before the switch kept its record in a static).
fn has_switch(listing: &Listing) -> bool {
    listing
        .iter()
        .any(|(a, e)| e.op == "jalr" && squeeze(listing.args(a + 4)) == "sp,a2")
}

/// Print one line per entry to `out`; return the number of failures.
pub fn check(
    exe: &Path,
    map_path: Option<&Path>,
    pattern: Option<&str>,
    budget: Option<i64>,
    out: &mut dyn Write,
) -> usize {
    check_forbidding(exe, map_path, pattern, budget, &[], out)
}

/// [`check`], also failing a tree that reaches any function in `forbidden`
/// (symbol names, as in the map): psx-rt's `__psx_rt_flush_i_cache` unmaps
/// the scratchpad the tree's frames are on.
pub fn check_forbidding(
    exe: &Path,
    map_path: Option<&Path>,
    pattern: Option<&str>,
    budget: Option<i64>,
    forbidden: &[String],
    out: &mut dyn Write,
) -> usize {
    check_forbidding_modules(exe, map_path, pattern, budget, forbidden, &[], out)
}

/// Check a compact image with streamed modules mapped to their link addresses. The failures
/// are the scratchpad stack entries' and the dispatcher handlers' together.
pub fn check_forbidding_modules(
    exe: &Path,
    map_path: Option<&Path>,
    pattern: Option<&str>,
    budget: Option<i64>,
    forbidden: &[String],
    modules: &[Module],
    out: &mut dyn Write,
) -> usize {
    let mut dispatcher = 0;
    let scratchpad = check_everything(
        exe,
        map_path,
        pattern,
        budget,
        forbidden,
        modules,
        &[],
        &[],
        &mut dispatcher,
        out,
    );
    scratchpad + dispatcher
}

/// [`check_forbidding_modules`] with the handlers named by `--handler` (symbol regexes proved
/// against the dispatcher's stack as well), returning the scratchpad failures and storing the
/// dispatcher's in `dispatcher`.
#[allow(clippy::too_many_arguments)]
pub fn check_everything(
    exe: &Path,
    map_path: Option<&Path>,
    pattern: Option<&str>,
    budget: Option<i64>,
    forbidden: &[String],
    modules: &[Module],
    handlers: &[String],
    core_hooks: &[String],
    dispatcher: &mut usize,
    out: &mut dyn Write,
) -> usize {
    let Some(map_path) = map_path else {
        if !forbidden.is_empty() || !modules.is_empty() {
            let flags = if modules.is_empty() {
                "--forbid"
            } else {
                "--module"
            };
            let _ = writeln!(
                out,
                "stack guard: {flags} needs a link map to prove symbol reachability"
            );
            return 1;
        }
        let data = match std::fs::read(exe) {
            Ok(data) => data,
            Err(error) => {
                let _ = writeln!(out, "stack guard: {}", io_message(&error, exe));
                return 1;
            }
        };
        let listing = Listing::new(&data, load_address(&data));
        if has_switch(&listing) {
            let _ = writeln!(
                out,
                "stack guard: {} switches to a scratchpad stack; pass its link map (ld.lld -Map)",
                exe.display()
            );
            return 1;
        }
        let _ = writeln!(out, "stack guard: no scratchpad stack in {}", exe.display());
        return 0;
    };
    let pattern = match pattern.map(Regex::new).transpose() {
        Ok(pattern) => pattern,
        Err(error) => {
            let _ = writeln!(out, "stack guard: bad --root pattern: {error}");
            return 1;
        }
    };
    let image = match GuardImage::open_modules(exe, map_path, modules) {
        Ok(image) => image,
        Err(error) => {
            let _ = writeln!(out, "stack guard: {error}");
            return 1;
        }
    };
    for symbol in forbidden {
        if !image
            .map
            .names
            .values()
            .any(|aliases| aliases.iter().any(|(_, name)| name == symbol))
        {
            let _ = writeln!(out, "stack guard: forbidden symbol {symbol} is not linked");
        }
    }
    let entries = roots(&image, pattern.as_ref(), budget);
    if entries.is_empty() {
        if let Some(pattern) = &pattern {
            let _ = writeln!(
                out,
                "stack guard: no symbol matches '{}' in {}",
                pattern.as_str(),
                map_path.display()
            );
            return 1;
        }
        if has_switch(&image.listing) {
            let _ = writeln!(
                out,
                "stack guard: {} contains psx-rt's stack switch but no ScratchpadStack entry is in {}",
                exe.display(),
                map_path.display()
            );
            return 1;
        }
        let _ = writeln!(
            out,
            "stack guard: no scratchpad stack entries in {}",
            exe.display()
        );
    }
    let mut named_hooks = Vec::new();
    for hook in core_hooks {
        match Regex::new(hook) {
            Ok(regex) => named_hooks.push(regex),
            Err(error) => {
                let _ = writeln!(out, "stack guard: bad --core-hook pattern: {error}");
                return 1;
            }
        }
    }
    let no_leaves = HashMap::new();
    let mut walker = Walker::with_core_hooks(&image, forbidden, &no_leaves, &named_hooks);
    let mut failures = 0;
    for (address, name, limit, region) in entries {
        let place = region.map_or(String::new(), |(lo, hi)| format!("region {lo}..{hi}, "));
        let (total, chain) = match walker.depth(address, &mut Vec::new()) {
            Ok(found) => found,
            Err(error) => {
                let _ = writeln!(out, "FAIL {}: {error}", short(&name));
                failures += 1;
                continue;
            }
        };
        let verdict = if total <= limit { "ok  " } else { "FAIL" };
        let _ = writeln!(
            out,
            "{verdict} {}: {total} of {limit} bytes ({place}{address:08x}) via {}",
            short(&name),
            chain.join(" > ")
        );
        if total > limit {
            failures += 1;
        }
    }
    // A custom `--root` checks that one tree and nothing else.
    if pattern.is_none() {
        let mut named = Vec::new();
        for handler in handlers {
            match Regex::new(handler) {
                Ok(regex) => named.push(regex),
                Err(error) => {
                    let _ = writeln!(out, "stack guard: bad --handler pattern: {error}");
                    *dispatcher += 1;
                }
            }
        }
        *dispatcher += crate::dispatch_guard::check(&image, map_path, &named, out);
    }
    failures
}

/// The jump table the guard proves for the `jr` at `jr_addr` (for tests).
pub fn jump_table(image: &GuardImage, jr_addr: i64) -> Option<Vec<(i64, i64)>> {
    Detector::new(image.image(), Some(&image.map)).jump_table(jr_addr)
}

/// Run the guard with command-line `args` (no program name); returns the
/// exit status.
pub fn main(args: &[String], out: &mut dyn Write) -> i32 {
    let (mut paths, mut pattern, mut budget) = (Vec::new(), None, None);
    let mut forbidden = Vec::new();
    let mut handlers = Vec::new();
    let mut core_hooks = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--root" => match it.next() {
                Some(value) => pattern = Some(value.clone()),
                None => {
                    let _ = writeln!(out, "stack guard: --root needs a pattern");
                    return 1;
                }
            },
            "--forbid" => match it.next() {
                Some(value) => {
                    forbidden.push(value.clone());
                    // The range flush isolates the cache the same way, so a game that forbids the whole flush forbids it too.
                    if value == "__psx_rt_flush_i_cache" {
                        forbidden.push("__psx_rt_flush_i_cache_range".into());
                    }
                }
                None => {
                    let _ = writeln!(out, "stack guard: --forbid needs a symbol");
                    return 1;
                }
            },
            "--handler" => match it.next() {
                Some(value) => handlers.push(value.clone()),
                None => {
                    let _ = writeln!(out, "stack guard: --handler needs a symbol pattern");
                    return 1;
                }
            },
            "--core-hook" => match it.next() {
                Some(value) => core_hooks.push(value.clone()),
                None => {
                    let _ = writeln!(out, "stack guard: --core-hook needs a symbol pattern");
                    return 1;
                }
            },
            "--budget" => match it.next().map(|v| int_auto(v)) {
                Some(Some(value)) => budget = Some(value),
                _ => {
                    let _ = writeln!(out, "stack guard: --budget needs a number");
                    return 1;
                }
            },
            "--module" => {
                if it.next().is_none() {
                    let _ = writeln!(out, "stack guard: --module needs LINK_LO..LINK_HI@IMAGE_AT");
                    return 2;
                }
            }
            _ => paths.push(arg.clone()),
        }
    }
    let modules = match modules(args) {
        Ok(modules) => modules,
        Err(error) => {
            let _ = writeln!(out, "{error}");
            return 2;
        }
    };
    if !(1..=2).contains(&paths.len()) || pattern.is_none() != budget.is_none() {
        let _ = write!(out, "{USAGE}");
        return 2;
    }
    let exe = Path::new(&paths[0]);
    let map = paths.get(1).map(Path::new);
    let mut dispatcher = 0;
    let failures = check_everything(
        exe,
        map,
        pattern.as_deref(),
        budget,
        &forbidden,
        &modules,
        &handlers,
        &core_hooks,
        &mut dispatcher,
        out,
    );
    if failures != 0 {
        let _ = writeln!(
            out,
            "stack guard: {failures} scratchpad stack entries fail in {}",
            paths[0]
        );
    }
    if dispatcher != 0 {
        let _ = writeln!(
            out,
            "stack guard: {dispatcher} dispatcher stack failures in {}",
            paths[0]
        );
    }
    i32::from(failures + dispatcher != 0)
}

/// Static stack depth, in bytes of frames, of the call tree rooted at `addr`, with every address in `leaves` counted
/// as that many bytes and not followed. Calls through registers, jumps that are not provable tables, `$sp` writes
/// other than `addiu` frames, trampolines that are not hazard patches and recursion are errors, as in `check`. This
/// is what `window_gate` proves a scratchpad window's guards and roots against.
///
/// # Errors
///
/// The walk reaches something it cannot bound (see above) or code outside the image.
pub fn tree_depth(
    image: &GuardImage,
    addr: i64,
    leaves: &HashMap<i64, i64>,
) -> Result<(i64, Vec<String>), GuardError> {
    let mut walker = Walker::new(image, &[], leaves);
    walker.depth(addr, &mut Vec::new())
}

#[cfg(test)]
mod tests {
    use super::short;

    #[test]
    fn short_names() {
        assert_eq!(short("game::f::h0123456789abcdef"), "game::f");
        assert_eq!(short("game::f::h0123"), "game::f::h0123");
        let long = "x".repeat(95);
        assert_eq!(short(&long), format!("{}...", "x".repeat(87)));
    }
}
