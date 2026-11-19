//! `stack-guard`'s proof for psx-rt's interrupt dispatcher: every handler a program registers
//! fits the dispatcher's stack.
//!
//! The dispatcher (`psx_rt::dispatch`) saves the interrupted context at the top of its own
//! stack, `__psx_rt_dispatch_stack` (`PSX_DISPATCH_STACK_BYTES` bytes, 512 unless the program's
//! build says otherwise), and runs every handler on what is left below it. Nothing at run time
//! stops a handler from running off the bottom, and an inlining change can grow one by
//! hundreds of bytes with no source change, so a program that shrinks the stack needs the
//! proof, from the linked image, that it still fits.
//!
//! The walk, from the image and its link map:
//!
//! 1. The stack's size is the map's size for `__psx_rt_dispatch_stack`; the saved context is
//!    the highest `sw ..,N(sp)` of `__psx_rt_dispatch_tramp` plus a word (112 bytes). A handler
//!    has the difference.
//! 2. Every `jal`/`j` to `psx_rt::dispatch::attach` (the handler is its second argument) and
//!    `psx_rt::dispatch::on_vblank` (its first) is a registration. The argument register is
//!    followed back through the function's control flow (`lui`/`addiu`/`ori`/`move`, the call's
//!    delay slot included, every path having to agree) to a constant function address. A
//!    register still holding the caller's own argument makes the caller a registrar too, and
//!    its callers are followed in turn (three levels). When every caller passes the same
//!    function the compiler removes the argument and the registrar stores the address itself;
//!    the constants stored in the registrars are handlers too.
//! 3. Each handler found is walked with [`tree_depth`], the scratchpad guard's static call
//!    graph (frame sizes summed down the deepest path, calls through a register, recursion and
//!    unprovable jumps refused), and must fit.
//!
//! Only a program that shrank the stack (under the default 512 bytes) needs the proof, so a
//! handler the walk cannot bound (the full engine's `fail` recurses, for one) and a registration
//! whose handler it cannot resolve (it comes from a table or a field, or paths disagree) fail
//! the check only then. At the default size they are reported as notes, and a handler that
//! does not fit still fails. `--handler REGEX` names the handlers of a program that has such
//! registrations (each is walked too, and the unresolved sites are then reported as covered by
//! the list: complete only if the list is).

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use regex::Regex;

use crate::detect::{branch_target, is, is_branch, reads, writes, COND};
use crate::stack_guard::{short, tree_depth, GuardImage};
use crate::text::{fields, int_auto, trailing_hex};

/// The stack's symbol, in `.bss`.
const STACK_SYMBOL: &str = "__psx_rt_dispatch_stack";
/// The routine that saves the context, switches to the stack and calls the handlers.
const TRAMPOLINE: &str = "__psx_rt_dispatch_tramp";
/// The functions that register a handler, and which argument register holds it (0 is `a0`).
const REGISTRARS: [(&str, usize); 2] = [
    ("psx_rt::dispatch::attach", 1),
    ("psx_rt::dispatch::on_vblank", 0),
];
/// The stack size of a build that sets no `PSX_DISPATCH_STACK_BYTES` (`psx_rt::dispatch`).
pub const DEFAULT_STACK_BYTES: i64 = 512;
const ARGUMENT_REGISTERS: [&str; 4] = ["a0", "a1", "a2", "a3"];
/// How many levels of wrapper around a registrar are followed.
const MAX_WRAPPERS: usize = 3;

/// The dispatcher stack of a linked image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Geometry {
    /// The stack's size.
    pub stack_bytes: i64,
    /// The saved context at the top of it.
    pub context_bytes: i64,
}

impl Geometry {
    /// Bytes a handler's frames may take.
    #[must_use]
    pub fn budget(&self) -> i64 {
        self.stack_bytes - self.context_bytes
    }
}

/// Where the dispatcher stack stands in `image`, or `None` if the program does not link the
/// dispatcher.
///
/// # Errors
///
/// The map or the code has no recognisable stack or context (a psx-rt this tool does not know).
pub fn geometry(image: &GuardImage, map_path: &Path) -> Result<Option<Geometry>, String> {
    let Some(tramp) = image
        .map
        .names
        .iter()
        .find(|(_, names)| names.iter().any(|(_, name)| name == TRAMPOLINE))
        .map(|(&address, _)| address)
    else {
        return Ok(None);
    };
    let text =
        std::fs::read_to_string(map_path).map_err(|e| format!("{}: {e}", map_path.display()))?;
    let stack_bytes = text
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            (parts.len() == 5 && parts[4] == STACK_SYMBOL)
                .then(|| i64::from_str_radix(parts[2], 16).ok())
                .flatten()
        })
        .next()
        .ok_or_else(|| format!("{STACK_SYMBOL} is not in {}", map_path.display()))?;
    let (start, end, _) = image
        .map
        .function(tramp)
        .ok_or_else(|| format!("{TRAMPOLINE} is not in a function"))?;
    let context_bytes = (start..end)
        .step_by(4)
        .filter(|&at| image.listing.op(at) == "sw")
        .filter_map(|at| {
            let args = image.listing.args(at);
            let (_, place) = args.split_once(',')?;
            let offset = place.strip_suffix("(sp)")?;
            int_auto(offset)
        })
        .max()
        .map(|highest| highest + 4)
        .ok_or_else(|| format!("{TRAMPOLINE} saves no context on the stack"))?;
    if stack_bytes <= context_bytes {
        return Err(format!(
            "the dispatcher stack ({stack_bytes} bytes) is not larger than its context ({context_bytes})"
        ));
    }
    Ok(Some(Geometry {
        stack_bytes,
        context_bytes,
    }))
}

/// What an argument register holds at a registration.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    /// A known number: the handler's address.
    Constant(i64),
    /// The caller's own argument `n`, untouched.
    Incoming(usize),
    /// A path that came round to where it started, which sets nothing new.
    Cycle,
    /// Anything else, and why.
    Unknown(String),
}

/// The branches that reach each address: `target -> branch instruction addresses`, for the
/// branches, jumps and jump tables of one function; `None` when a jump through a register is
/// not a table the guard can prove.
fn branch_sources(image: &GuardImage, start: i64, end: i64) -> Option<HashMap<i64, Vec<i64>>> {
    let listing = &image.listing;
    let mut sources: HashMap<i64, Vec<i64>> = HashMap::new();
    for at in (start..end).step_by(4) {
        let (op, args) = (listing.op(at), listing.args(at));
        if op == "jr" && args.trim() != "ra" {
            for (_, target) in crate::stack_guard::jump_table(image, at)? {
                sources.entry(target).or_default().push(at);
            }
        } else if is(op, COND) || op == "j" {
            if let Some(target) = branch_target(op, args) {
                sources.entry(target).or_default().push(at);
            }
        }
    }
    Some(sources)
}

/// The most instructions one argument's search looks at.
const SEARCH_LIMIT: usize = 4000;

/// Where a search has been: the instruction, the register wanted and the steps still to apply.
type Visit = (i64, String, Vec<(bool, i64)>);

/// A search back through one function's control flow for the value of an argument register.
struct Search<'a> {
    image: &'a GuardImage,
    start: i64,
    sources: &'a HashMap<i64, Vec<i64>>,
    looked_at: usize,
    visited: HashSet<Visit>,
}

/// The registers a call may change (the C ABI's caller-saved set).
fn caller_saved(register: &str) -> bool {
    !matches!(
        register,
        "s0" | "s1" | "s2" | "s3" | "s4" | "s5" | "s6" | "s7" | "s8" | "fp" | "sp" | "gp" | "zero"
    )
}

impl Search<'_> {
    /// What the instructions that can run just before `at` leave in each register, together:
    /// the one value they all agree on, or why not.
    fn before(&mut self, at: i64, register: &str, steps: &[(bool, i64)]) -> Value {
        let listing = &self.image.listing;
        if at == self.start {
            return match ARGUMENT_REGISTERS.iter().position(|&r| r == register) {
                Some(n) if steps.is_empty() => Value::Incoming(n),
                _ => Value::Unknown(format!("{register} is not set in the function")),
            };
        }
        // Who runs just before `at`: the instruction ahead of it, unless that is an unconditional
        // jump's delay slot; a call returns here, so it clobbers the caller-saved registers; and
        // the delay slot of every branch that lands on `at`.
        let mut before: Vec<(i64, bool)> = Vec::new();
        if is_branch(listing.op(at - 4)) {
            // `at` is a delay slot: the branch ran just before it.
            before.push((at - 4, false));
        } else {
            let branch = at - 8;
            if branch >= self.start && is_branch(listing.op(branch)) {
                match listing.op(branch) {
                    "j" | "jr" | "b" => {}
                    "jal" | "jalr" | "bal" | "bltzal" | "bgezal" => before.push((at - 4, true)),
                    _ => before.push((at - 4, false)),
                }
            } else {
                before.push((at - 4, false));
            }
            for &branch in self.sources.get(&at).map_or(&[][..], Vec::as_slice) {
                before.push((branch + 4, false));
            }
        }
        let mut agreed: Option<Value> = None;
        for (previous, call) in before {
            if call && caller_saved(register) {
                return Value::Unknown("a call comes before the value is set".into());
            }
            let value = self.at(previous, register, steps);
            match (&agreed, value) {
                (_, Value::Cycle) => {}
                (None, value) => agreed = Some(value),
                (Some(seen), value) if *seen == value => {}
                (Some(_), _) => return Value::Unknown("paths set different values".into()),
            }
        }
        agreed.unwrap_or(Value::Cycle)
    }

    /// The value `register` holds after the instruction at `at`.
    fn at(&mut self, at: i64, register: &str, steps: &[(bool, i64)]) -> Value {
        self.looked_at += 1;
        if self.looked_at > SEARCH_LIMIT {
            return Value::Unknown("the function is too large to follow".into());
        }
        if !self
            .visited
            .insert((at, register.to_string(), steps.to_vec()))
        {
            return Value::Cycle;
        }
        let (op, args) = (self.image.listing.op(at), self.image.listing.args(at));
        if op.is_empty() || op == ".word" {
            return Value::Unknown(format!("no instruction at {at:08x}"));
        }
        if !writes(op, args, register) {
            return self.before(at, register, steps);
        }
        let parts = fields(args);
        let source = |n: usize| parts.get(n).copied().unwrap_or("");
        let mut steps = steps.to_vec();
        let mut register = register;
        let base = match op {
            "lui" => int_auto(source(1)).map(|high| (high << 16) & 0xffff_ffff),
            "li" => int_auto(source(1)).map(|value| value & 0xffff_ffff),
            "addiu" | "ori" => {
                let Some(immediate) = int_auto(source(2)) else {
                    return Value::Unknown(format!("`{op} {args}` is not a constant"));
                };
                steps.push((op == "ori", immediate));
                register = source(1);
                None
            }
            "move" => {
                register = source(1);
                None
            }
            "addu" if source(2) == "zero" => {
                register = source(1);
                None
            }
            "addu" if source(1) == "zero" => {
                register = source(2);
                None
            }
            _ => return Value::Unknown(format!("it is loaded or computed by `{op} {args}`")),
        };
        let base = base.or_else(|| (register == "zero").then_some(0));
        match base {
            Some(base) => {
                Value::Constant(steps.iter().rev().fold(base, |value, &(or, immediate)| {
                    (if or {
                        value | immediate
                    } else {
                        value.wrapping_add(immediate)
                    }) & 0xffff_ffff
                }))
            }
            None => self.before(at, register, &steps),
        }
    }
}

/// The value of argument register `arg` at the call or tail call at `site`, which is in the
/// function starting at `start`.
fn argument_value(
    image: &GuardImage,
    sources: &HashMap<i64, Vec<i64>>,
    start: i64,
    site: i64,
    arg: usize,
) -> Value {
    let mut search = Search {
        image,
        start,
        sources,
        looked_at: 0,
        visited: HashSet::new(),
    };
    // The call's delay slot runs before the callee, so the search starts there.
    match search.at(site + 4, ARGUMENT_REGISTERS[arg], &[]) {
        Value::Cycle => Value::Unknown("the value is only set in a loop".into()),
        value => value,
    }
}

/// Where `dest` really goes: a hazard trampoline (`nop ; j target`) stands for its target.
fn through_trampoline(image: &GuardImage, dest: i64) -> i64 {
    if image.in_trampolines(dest)
        && image.listing.op(dest) == "nop"
        && image.listing.op(dest + 4) == "j"
    {
        return trailing_hex(image.listing.args(dest + 4)).unwrap_or(dest);
    }
    dest
}

/// Every `jal`, and every `j` out of its own function, by callee: the sites that call it.
fn call_sites(image: &GuardImage) -> HashMap<i64, Vec<i64>> {
    let mut sites: HashMap<i64, Vec<i64>> = HashMap::new();
    for (at, entry) in image.listing.iter() {
        if entry.op != "jal" && entry.op != "j" {
            continue;
        }
        let Some((start, end, _)) = image.map.function(at) else {
            continue;
        };
        let Some(dest) = trailing_hex(&entry.args) else {
            continue;
        };
        let dest = through_trampoline(image, dest);
        if entry.op == "j" && (start..end).contains(&dest) {
            continue;
        }
        sites.entry(dest).or_default().push(at);
    }
    sites
}

/// A handler the program registers.
#[derive(Debug)]
struct Registered {
    address: i64,
    /// Functions that register it.
    from: Vec<String>,
}

/// What the scan found.
#[derive(Debug, Default)]
struct Registrations {
    handlers: Vec<Registered>,
    /// Registrations whose handler it could not resolve: where, and why.
    unresolved: Vec<String>,
}

/// Whether the function at `start` reads argument register `arg` before it writes it, in its
/// first run of instructions (up to the first branch, and that branch's delay slot, which runs
/// before it). The compiler removes an argument every caller passes the same value for and puts
/// the value in the function, which then writes the register before it reads it, or never
/// reads it: a specialised `attach` sets up its own call to `link` with `a1` written in the
/// call's delay slot.
fn reads_argument(image: &GuardImage, start: i64, end: i64, arg: usize) -> bool {
    let listing = &image.listing;
    let register = ARGUMENT_REGISTERS[arg];
    for at in (start..end).step_by(4) {
        let (op, args) = (listing.op(at), listing.args(at));
        if reads(op, args, register) {
            return true;
        }
        if writes(op, args, register) {
            return false;
        }
        if is_branch(op) {
            let slot = at + 4;
            if slot < end {
                let (op, args) = (listing.op(slot), listing.args(slot));
                if !reads(op, args, register) && writes(op, args, register) {
                    return false;
                }
            }
            return true;
        }
    }
    true
}

/// Whether `name` is `symbol`, or a copy of it the compiler made (`symbol.constprop.0`).
fn is_function(name: &str, symbol: &str) -> bool {
    let name = short(name);
    name == symbol
        || name
            .strip_prefix(symbol)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Handlers the compiler put into a registrar's own code: with every caller passing the same
/// function, it removes the argument and the registrar stores the address itself. Each
/// `sw` of a register that holds the address of a function of the program (the dispatcher's
/// own routines apart) is one.
fn specialised_handlers(
    image: &GuardImage,
    registrar: (i64, i64),
    sources: &HashMap<i64, Vec<i64>>,
) -> Vec<i64> {
    let (start, end) = registrar;
    let listing = &image.listing;
    let mut found = Vec::new();
    for at in (start..end).step_by(4) {
        if listing.op(at) != "sw" {
            continue;
        }
        let args = listing.args(at);
        let Some((register, place)) = args.split_once(',') else {
            continue;
        };
        if matches!(register.trim(), "zero" | "sp" | "gp") || place.ends_with("(sp)") {
            continue;
        }
        let mut search = Search {
            image,
            start,
            sources,
            looked_at: 0,
            visited: HashSet::new(),
        };
        let Value::Constant(address) = search.before(at, register.trim(), &[]) else {
            continue;
        };
        let Some((function, _, name)) = image.map.function(address) else {
            continue;
        };
        if function == address
            && !name.starts_with("__psx_rt_")
            && !name.starts_with("psx_rt::dispatch")
        {
            found.push(address);
        }
    }
    found
}

fn registrations(image: &GuardImage) -> Registrations {
    let mut registrars: Vec<(i64, usize, usize)> = Vec::new();
    for (address, names) in image.map.sorted_names() {
        for (_, name) in names {
            if let Some(&(_, arg)) = REGISTRARS
                .iter()
                .find(|(symbol, _)| is_function(name, symbol))
            {
                registrars.push((address, arg, 0));
            }
        }
    }
    let callers = call_sites(image);
    let mut source_cache: HashMap<i64, Option<HashMap<i64, Vec<i64>>>> = HashMap::new();
    let mut found = Registrations::default();
    let add = |found: &mut Registrations, address: i64, place: String| match found
        .handlers
        .iter_mut()
        .find(|h| h.address == address)
    {
        Some(handler) => handler.from.push(place),
        None => found.handlers.push(Registered {
            address,
            from: vec![place],
        }),
    };
    let mut seen: HashSet<(i64, usize)> = HashSet::new();
    while let Some((registrar, arg, depth)) = registrars.pop() {
        if !seen.insert((registrar, arg)) {
            continue;
        }
        let Some((_, end, registrar_name)) = image.map.function(registrar) else {
            continue;
        };
        let own = source_cache
            .entry(registrar)
            .or_insert_with(|| branch_sources(image, registrar, end));
        if depth == 0 {
            if let Some(sources) = own {
                for address in specialised_handlers(image, (registrar, end), sources) {
                    add(
                        &mut found,
                        address,
                        format!("{} (the handler is in its code)", short(&registrar_name)),
                    );
                }
            }
        }
        if !reads_argument(image, registrar, end, arg) {
            continue;
        }
        for &site in callers.get(&registrar).map_or(&[][..], Vec::as_slice) {
            let Some((start, end, caller)) = image.map.function(site) else {
                continue;
            };
            let place = format!("{} at {site:08x}", short(&caller));
            let sources = source_cache
                .entry(start)
                .or_insert_with(|| branch_sources(image, start, end));
            let value = match sources {
                Some(sources) => argument_value(image, sources, start, site, arg),
                None => Value::Unknown(
                    "the function has a jump through a register it cannot prove".into(),
                ),
            };
            match value {
                Value::Constant(address) => add(&mut found, address, place),
                Value::Incoming(n) if depth < MAX_WRAPPERS => {
                    registrars.push((start, n, depth + 1));
                }
                Value::Incoming(_) => found.unresolved.push(format!(
                    "{place}: wrapped in more than {MAX_WRAPPERS} levels of function"
                )),
                Value::Unknown(why) => found.unresolved.push(format!("{place}: {why}")),
                Value::Cycle => found
                    .unresolved
                    .push(format!("{place}: the value is only set in a loop")),
            }
        }
    }
    found.handlers.sort_by_key(|h| h.address);
    found.unresolved.sort();
    found
}

/// Prove every handler the program registers fits the dispatcher stack, printing one line each
/// to `out`; `named` are handlers the caller names by symbol (`--handler`), proved as well.
/// Returns the number of failures; a program that does not link the dispatcher has none.
pub fn check(image: &GuardImage, map_path: &Path, named: &[Regex], out: &mut dyn Write) -> usize {
    let geometry = match geometry(image, map_path) {
        Ok(Some(geometry)) => geometry,
        Ok(None) => return 0,
        Err(error) => {
            let _ = writeln!(out, "FAIL dispatcher stack: {error}");
            return 1;
        }
    };
    let budget = geometry.budget();
    let shrunk = geometry.stack_bytes < DEFAULT_STACK_BYTES;
    let mut failures = 0;
    let mut found = registrations(image);
    for pattern in named {
        let mut matched = false;
        for (address, names) in image.map.sorted_names() {
            if names.iter().any(|(_, name)| pattern.is_match(name)) {
                matched = true;
                if !found.handlers.iter().any(|h| h.address == address) {
                    found.handlers.push(Registered {
                        address,
                        from: vec![format!("named by --handler {}", pattern.as_str())],
                    });
                }
            }
        }
        if !matched {
            let _ = writeln!(
                out,
                "FAIL no symbol matches --handler '{}'",
                pattern.as_str()
            );
            failures += 1;
        }
    }
    found.handlers.sort_by_key(|h| h.address);
    let _ = writeln!(
        out,
        "dispatcher stack: {} bytes, {} for the saved context, {budget} for a handler",
        geometry.stack_bytes, geometry.context_bytes
    );
    for handler in &found.handlers {
        let name = image
            .map
            .function(handler.address)
            .filter(|&(start, _, _)| start == handler.address)
            .map(|(_, _, name)| short(&name));
        let Some(name) = name else {
            let _ = writeln!(
                out,
                "FAIL dispatcher handler {:08x} (registered in {}) is not the start of a function",
                handler.address,
                handler.from.join(", ")
            );
            failures += 1;
            continue;
        };
        match tree_depth(image, handler.address, &HashMap::new()) {
            Ok((total, chain)) => {
                let verdict = if total <= budget { "ok  " } else { "FAIL" };
                let _ = writeln!(
                    out,
                    "{verdict} dispatcher handler {name}: {total} of {budget} bytes ({:08x}, registered in {}) via {}",
                    handler.address,
                    handler.from.join(", "),
                    chain.join(" > ")
                );
                if total > budget {
                    failures += 1;
                }
            }
            Err(error) if shrunk => {
                let _ = writeln!(out, "FAIL dispatcher handler {name}: {error}");
                failures += 1;
            }
            Err(error) => {
                let _ = writeln!(
                    out,
                    "note: dispatcher handler {name} cannot be bounded: {error}; not proved (the stack is \
                     the default {DEFAULT_STACK_BYTES} bytes)"
                );
            }
        }
    }
    if found.handlers.is_empty() && found.unresolved.is_empty() {
        let _ = writeln!(out, "dispatcher: no handler is registered");
    }
    for site in &found.unresolved {
        if shrunk && named.is_empty() {
            let _ = writeln!(
                out,
                "FAIL dispatcher registration {site}; a stack under {DEFAULT_STACK_BYTES} bytes needs every \
                 handler proved: register with a constant function, or name the handlers with --handler"
            );
            failures += 1;
        } else if shrunk {
            let _ = writeln!(
                out,
                "note: dispatcher registration {site}; covered by --handler"
            );
        } else {
            let _ = writeln!(
                out,
                "note: dispatcher registration {site}; not proved (the stack is the default {DEFAULT_STACK_BYTES} bytes)"
            );
        }
    }
    failures
}
