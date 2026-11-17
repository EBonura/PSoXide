//! `psoxide-duel`: host duel sweeps over a project's fighters.
//!
//! ```text
//! psoxide-duel sweep --project projects/grid-dungeon [--project ...] \
//!     [--seeds 200] [--layout open|pillars] [--out DIR]
//! psoxide-duel sensitivity --project projects/grid-dungeon [--seeds 200] [--out DIR]
//! psoxide-duel log --project projects/grid-dungeon --seed 1 [--matchup aletha-ai:enemy-ai]
//! ```
//!
//! `sweep` runs the standard matchups (Aletha and the training enemy, each
//! side driven by the enemy AI or a scripted pad, and both mirrors) and
//! prints one table row per matchup; `--out` keeps one JSON per matchup.
//! `sensitivity` nudges each placeholder one step down and up on both
//! fighters and reports how the mirrors and Aletha against the enemy move.
//! `log` prints one fight's `duel:` lines in the disc log format, which the
//! editor's `duel-report` reads.

use std::path::{Path, PathBuf};

use psoxide_duel::arena::{self, Controller, Fighter, Layout, PadScript};
use psoxide_duel::fighter::{FighterSpec, Placeholder};
use psoxide_duel::project::{self, ProjectFighters};
use psoxide_duel::sweep::{self, Matchup, Summary};
use serde_json::json;

struct Args {
    command: String,
    projects: Vec<PathBuf>,
    seeds: u32,
    seed: u32,
    layouts: Vec<Layout>,
    out: Option<PathBuf>,
    matchup: String,
}

fn parse() -> Args {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "sweep".into());
    let mut parsed = Args {
        command,
        projects: Vec::new(),
        seeds: 200,
        seed: 1,
        layouts: Vec::new(),
        out: None,
        matchup: "aletha-ai:enemy-ai".into(),
    };
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| panic!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--project" => parsed.projects.push(PathBuf::from(value())),
            "--seeds" => parsed.seeds = value().parse().expect("--seeds N"),
            "--seed" => parsed.seed = value().parse().expect("--seed N"),
            "--layout" => {
                let name = value();
                parsed.layouts.push(
                    Layout::from_name(&name).unwrap_or_else(|| panic!("unknown layout {name}")),
                );
            }
            "--out" => parsed.out = Some(PathBuf::from(value())),
            "--matchup" => parsed.matchup = value(),
            other => panic!("unknown flag {other}"),
        }
    }
    if parsed.projects.is_empty() {
        parsed.projects.push(PathBuf::from("projects/grid-dungeon"));
    }
    if parsed.layouts.is_empty() {
        parsed.layouts = vec![Layout::Room, Layout::RoomPillars];
    }
    parsed
}

/// The fighters of one project, placed.
struct Placed {
    loaded: ProjectFighters,
    aletha: Fighter,
    enemy: Fighter,
}

fn place(loaded: ProjectFighters) -> Placed {
    Placed {
        aletha: Fighter::new(loaded.aletha.clone()),
        enemy: Fighter::new(loaded.enemy.clone()),
        loaded,
    }
}

fn controller(name: &str) -> Controller {
    match name {
        "ai" => Controller::Ai,
        "pad" | "pad-rush" => Controller::Pad(PadScript::Rush),
        "stand" | "pad-stand" => Controller::Pad(PadScript::Stand),
        other => panic!("unknown controller {other}"),
    }
}

/// The standard matchups of one project.
fn standard<'a>(placed: &'a Placed, layout: Layout) -> Vec<Matchup<'a>> {
    let ai = Controller::Ai;
    let pad = Controller::Pad(PadScript::Rush);
    let m = |name: &str, a: &'a Fighter, b: &'a Fighter, ca, cb| Matchup {
        name: name.to_owned(),
        fighters: [a, b],
        controllers: [ca, cb],
        layout,
        swap_cooldown_ticks: placed.loaded.swap_cooldown_ticks,
        distance: placed.loaded.start_distance,
    };
    vec![
        m("aletha-ai:enemy-ai", &placed.aletha, &placed.enemy, ai, ai),
        m("enemy-ai:aletha-ai", &placed.enemy, &placed.aletha, ai, ai),
        m("enemy-ai:enemy-ai", &placed.enemy, &placed.enemy, ai, ai),
        m(
            "aletha-ai:aletha-ai",
            &placed.aletha,
            &placed.aletha,
            ai,
            ai,
        ),
        m(
            "aletha-ai:enemy-pad",
            &placed.aletha,
            &placed.enemy,
            ai,
            pad,
        ),
        m(
            "aletha-pad:enemy-ai",
            &placed.aletha,
            &placed.enemy,
            pad,
            ai,
        ),
        m(
            "enemy-pad:enemy-pad",
            &placed.enemy,
            &placed.enemy,
            pad,
            pad,
        ),
    ]
}

fn row(project: &str, layout: Layout, matchup: &Matchup<'_>, s: &Summary) -> String {
    let p = |v: [u32; 2]| format!("{} / {}", v[0], v[1]);
    format!(
        "| {project} | {} | {} | {} / {} | {:.0}% (fair: 50 +/- {:.0}) | {} | {} | {} | {} | {} | {} | {:.0} |",
        layout.name(),
        matchup.name,
        s.a_won(),
        s.b_won(),
        s.a_share_percent(),
        s.fair_half_width_percent(),
        s.count(arena::Outcome::Stalled) + s.count(arena::Outcome::Timeout),
        s.count(arena::Outcome::DoubleKo),
        p(s.total(|t| t.parries)),
        p(s.total(|t| t.crits)),
        p(s.total(|t| t.knockdowns)),
        p(s.total(|t| t.evades)),
        s.mean_length(),
    )
}

const HEADER: &str = "| project | layout | matchup (A:B) | wins A / B | A share | stalls+timeouts | double KO | parries A / B | crits A / B | knockdowns A / B | evades A / B | mean length (ticks) |\n|---|---|---|---|---|---|---|---|---|---|---|---|";

fn write_json(out: &Option<PathBuf>, name: &str, value: &serde_json::Value) {
    if let Some(dir) = out {
        std::fs::create_dir_all(dir).expect("create --out");
        std::fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_vec_pretty(value).expect("json"),
        )
        .expect("write json");
    }
}

fn load(path: &Path) -> Placed {
    let started = std::time::Instant::now();
    let loaded = project::load(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    eprintln!(
        "{}: cooked in {:.1}s; enemy {} (poise {}, parry {} ticks), aletha (poise {}, parry {} ticks), start {} units",
        loaded.project,
        started.elapsed().as_secs_f64(),
        loaded.enemy.name,
        loaded.enemy.record.combat.poise,
        loaded.enemy.record.combat.parry_ticks,
        loaded.aletha.record.combat.poise,
        loaded.aletha.record.combat.parry_ticks,
        loaded.start_distance,
    );
    for spec in [&loaded.enemy, &loaded.aletha] {
        let r = &spec.record;
        eprintln!(
            "  {}: radius {} height {} walk {} run {} pref {} tol {} range {}..{} windup {} active {}/{}/{} recover {} touch {} flags {:#x} hp {} strikes {:?} emitters {:?}",
            spec.name, r.radius, r.height, r.motion.walk_speed, r.motion.run_speed, r.preferred_distance, r.spacing_tolerance,
            r.attack_min_range, r.attack_max_range, r.windup_ticks, r.attack_active_ticks, r.heavy_attack_active_ticks,
            r.ranged_attack_active_ticks, r.recovery_ticks, r.touch_damage, r.flags, r.combat.max_health, spec.strikes, spec.emitters
        );
        eprintln!(
            "    clip timings (light, heavy, ranged): {:?}",
            spec.timings
        );
    }
    place(loaded)
}

fn main() {
    let args = parse();
    match args.command.as_str() {
        "sweep" => {
            println!("{HEADER}");
            for path in &args.projects {
                let placed = load(path);
                for &layout in &args.layouts {
                    for matchup in standard(&placed, layout) {
                        let started = std::time::Instant::now();
                        let summary = sweep::sweep(&matchup, 1..=args.seeds);
                        eprintln!(
                            "{} {} {}: {} seeds in {:.1}s",
                            placed.loaded.project,
                            layout.name(),
                            matchup.name,
                            args.seeds,
                            started.elapsed().as_secs_f64()
                        );
                        println!(
                            "{}",
                            row(&placed.loaded.project, layout, &matchup, &summary)
                        );
                        let mut value = summary.to_json(&matchup);
                        value["project"] = json!(placed.loaded.project);
                        write_json(
                            &args.out,
                            &format!(
                                "{}-{}-{}",
                                placed.loaded.project,
                                layout.name(),
                                matchup.name.replace(':', "-vs-")
                            ),
                            &value,
                        );
                    }
                }
            }
        }
        "sensitivity" => sensitivity(&args),
        "log" => {
            let placed = load(&args.projects[0]);
            let (a, b) = args.matchup.split_once(':').expect("--matchup A:B");
            let side = |s: &str| -> (&Fighter, Controller) {
                let (who, by) = s.split_once('-').expect("fighter-controller");
                let fighter = match who {
                    "aletha" => &placed.aletha,
                    "enemy" => &placed.enemy,
                    other => panic!("unknown fighter {other}"),
                };
                (fighter, controller(by))
            };
            let (fa, ca) = side(a);
            let (fb, cb) = side(b);
            let matchup = Matchup {
                name: args.matchup.clone(),
                fighters: [fa, fb],
                controllers: [ca, cb],
                layout: args.layouts[0],
                swap_cooldown_ticks: placed.loaded.swap_cooldown_ticks,
                distance: placed.loaded.start_distance,
            };
            let result = sweep::fight(&matchup, args.seed, true);
            print!("{}", result.log);
        }
        other => panic!("unknown command {other}"),
    }
}

/// Every placeholder one step down and up, on both fighters at once, for
/// the mirrors and Aletha against the enemy.
fn sensitivity(args: &Args) {
    println!("| project | layout | placeholder | value (aletha / enemy) | aletha mirror A share | enemy mirror A share | aletha vs enemy: aletha share | parries aletha / enemy | crits aletha / enemy | knockdowns aletha / enemy | mean length |\n|---|---|---|---|---|---|---|---|---|---|---|");
    let mut all = Vec::new();
    for path in &args.projects {
        let base = load(path);
        for &layout in &args.layouts {
            let mut configs: Vec<(String, i32)> = vec![("baseline".into(), 0)];
            for p in Placeholder::ALL {
                configs.push((p.name().into(), -1));
                configs.push((p.name().into(), 1));
            }
            for (name, direction) in configs {
                let placeholder = Placeholder::ALL.into_iter().find(|p| p.name() == name);
                let mut aletha = base.loaded.aletha.clone();
                let mut enemy = base.loaded.enemy.clone();
                let mut dry = psx_game_runtime::ranged_tactics::DRY_CYCLES;
                let values = match placeholder {
                    Some(Placeholder::DryCycles) => {
                        dry = (i32::from(dry) + direction).clamp(0, 255) as u8;
                        format!("{dry} / {dry}")
                    }
                    Some(p) => {
                        let va = p.nudge(&mut aletha.record, direction);
                        let ve = p.nudge(&mut enemy.record, direction);
                        format!("{va} / {ve}")
                    }
                    None => {
                        let show = |s: &FighterSpec| {
                            Placeholder::ALL
                                .iter()
                                .map(|p| format!("{}={}", p.name(), p.value(&s.record)))
                                .collect::<Vec<_>>()
                                .join(" ")
                        };
                        eprintln!(
                            "baseline aletha: {}\nbaseline enemy: {}",
                            show(&aletha),
                            show(&enemy)
                        );
                        "as authored".into()
                    }
                };
                arena::set_dry_cycles(dry);
                let distance = base.loaded.start_distance;
                let fa = Fighter::new(aletha);
                let fe = Fighter::new(enemy);
                let ai = Controller::Ai;
                let m = |a: &Fighter, b: &Fighter, name: &str| {
                    let matchup = Matchup {
                        name: name.into(),
                        fighters: [a, b],
                        controllers: [ai, ai],
                        layout,
                        swap_cooldown_ticks: base.loaded.swap_cooldown_ticks,
                        distance,
                    };
                    let summary = sweep::sweep(&matchup, 1..=args.seeds);
                    (summary.to_json(&matchup), summary)
                };
                let (ja, mirror_a) = m(&fa, &fa, "aletha-ai:aletha-ai");
                let (je, mirror_e) = m(&fe, &fe, "enemy-ai:enemy-ai");
                let (jx, cross) = m(&fa, &fe, "aletha-ai:enemy-ai");
                let label = if direction == 0 {
                    name.clone()
                } else {
                    format!(
                        "{name} {}",
                        if direction < 0 { "-1 step" } else { "+1 step" }
                    )
                };
                let p = |v: [u32; 2]| format!("{} / {}", v[0], v[1]);
                println!(
                    "| {} | {} | {label} | {values} | {:.0}% | {:.0}% | {:.0}% ({} / {}) | {} | {} | {} | {:.0} |",
                    base.loaded.project,
                    layout.name(),
                    mirror_a.a_share_percent(),
                    mirror_e.a_share_percent(),
                    cross.a_share_percent(),
                    cross.a_won(),
                    cross.b_won(),
                    p(cross.total(|t| t.parries)),
                    p(cross.total(|t| t.crits)),
                    p(cross.total(|t| t.knockdowns)),
                    cross.mean_length(),
                );
                all.push(json!({
                    "project": base.loaded.project,
                    "layout": layout.name(),
                    "placeholder": name,
                    "direction": direction,
                    "values_aletha_enemy": values,
                    "aletha_mirror": ja,
                    "enemy_mirror": je,
                    "aletha_vs_enemy": jx,
                }));
            }
            arena::set_dry_cycles(psx_game_runtime::ranged_tactics::DRY_CYCLES);
        }
    }
    write_json(&args.out, "sensitivity", &json!(all));
}
