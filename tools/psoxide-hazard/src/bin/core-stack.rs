//! `core-stack IMAGE MAP`: the scene core's stack depths from its built image (`psx-core.bin`)
//! and its link map, checked against what psx-scene-abi publishes (`psx_scene_abi::stack`): every
//! jump-table slot's depth below its caller's `$sp`, whether it reaches the idle hook, and the
//! switch's load on the switch stack. Prints one line per slot; exit 1 on any difference.

use psoxide_hazard::core_stack::{depths, functions_from_map, slot_target};
use psx_scene_abi::{stack, CORE_WINDOW};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [image, map] = args.as_slice() else {
        eprintln!("usage: core-stack IMAGE MAP");
        return ExitCode::from(2);
    };
    let (image, map) = match (std::fs::read(image), std::fs::read_to_string(map)) {
        (Ok(i), Ok(m)) => (i, m),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("core-stack: {e}");
            return ExitCode::FAILURE;
        }
    };
    let base = CORE_WINDOW.start;
    let functions = functions_from_map(&map);
    let found = depths(&image, base, &functions);
    let name_at = |addr: u32| {
        functions
            .iter()
            .find(|f| f.addr == addr)
            .map(|f| f.name.clone())
    };
    let mut bad = 0;
    let mut check = |what: &str, got: Option<(u32, bool)>, want: u32, hook: Option<bool>| {
        let ok = got.is_some_and(|(d, h)| d == want && hook.is_none_or(|w| w == h));
        let shown = got.map_or_else(
            || "not found".to_owned(),
            |(d, h)| format!("{d} B{}", if h { ", idle hook" } else { "" }),
        );
        println!(
            "core-stack: {what}: {shown} (published {want}) {}",
            if ok { "ok" } else { "DIFFERS" }
        );
        if !ok {
            bad += 1;
        }
    };
    for slot in stack::SLOTS {
        let got = slot_target(&image, base, slot.addr)
            .and_then(name_at)
            .and_then(|n| found.get(&n).copied());
        // A slot published as calling the idle hook must reach it. `psx_core_cd` reaches the core's
        // waits too but parks the hook for its whole call, so only its depth is checked.
        let hook = (slot.addr != psx_scene_abi::slot::CD).then_some(slot.idle_hook);
        check(slot.name, got, slot.depth, hook);
    }
    let land = found
        .iter()
        .find(|(n, _)| n.ends_with("switch::land"))
        .map(|(_, d)| *d);
    check("switch load (switch stack)", land, stack::SWITCH_LOAD, None);
    let max = stack::SLOTS.iter().map(|s| s.depth).max().unwrap_or(0);
    if max != stack::MAX {
        println!(
            "core-stack: stack::MAX {} but the deepest slot is {max}",
            stack::MAX
        );
        bad += 1;
    }
    if bad == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
