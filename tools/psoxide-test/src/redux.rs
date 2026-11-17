//! The PCSX-Redux adapter: the plan becomes a Lua table, a fixed driver (`redux_driver.lua`)
//! replays it on every vsync, and the official build runs windowless (`-portable -cli -softgpu`)
//! from a private config directory, one process at a time, killed by PID at its timeout.
//!
//! Redux has no stick API, so a DualShock is covered here as Redux's `Analog` pad with button
//! input only; a plan step that moves a stick is refused on Redux by name (the stick runs on
//! PSoXide and ReARMed).

use std::fmt::Write as _;
use std::path::Path;

use crate::journey::{Action, Plan};
use crate::rearmed::Replay;
use crate::util;

/// The driver the plan runs under.
pub const DRIVER: &str = include_str!("redux_driver.lua");

fn lua_string(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// The plan as a Lua chunk that sets `PLAN` and `OUT`.
pub fn plan_lua(plan: &Plan, out: &Path) -> Result<String, String> {
    let device = if plan.pad == "dualshock" {
        "Analog"
    } else {
        "Digital"
    };
    let mut s = String::new();
    let _ = writeln!(s, "OUT = {}", lua_string(&out.display().to_string()));
    let _ = writeln!(
        s,
        "PLAN = {{ device = {}, max_frames = {}, idle_frames = {}, steps = {{",
        lua_string(device),
        plan.max_frames,
        plan.idle_frames
    );
    for step in &plan.steps {
        let (kind, extra) = match &step.action {
            Action::Check { name } => ("check", format!("name = {}, ", lua_string(name))),
            Action::Wait => ("wait", String::new()),
            Action::Hold { buttons, lstick } => {
                if lstick.is_some() {
                    return Err(format!(
                        "step `{}` moves the left stick: Redux has no stick API, so this smoke \
                         cannot run on Redux as written (keep sticks out of the smoke; they run \
                         on PSoXide and ReARMed)",
                        step.label
                    ));
                }
                let list: Vec<String> = buttons.iter().map(|b| lua_string(b)).collect();
                ("hold", format!("buttons = {{{}}}, ", list.join(", ")))
            }
        };
        let probes: Vec<String> = step
            .probes
            .iter()
            .map(|p| {
                format!(
                    "{{ text = {}, addr = {}, size = {}, signed = {}, op = {}, value = {}, value_hi = {} }}",
                    lua_string(&p.text),
                    p.addr,
                    p.size,
                    p.signed,
                    lua_string(&p.op),
                    p.value,
                    p.value_hi
                )
            })
            .collect();
        let _ = writeln!(
            s,
            "  {{ kind = {}, label = {}, timeout = {}, {extra}probes = {{ {} }} }},",
            lua_string(kind),
            lua_string(&step.label),
            step.timeout,
            probes.join(", ")
        );
    }
    s.push_str("} }\n");
    Ok(s)
}

/// Read the driver's PTEST lines.
pub fn parse(output: &str) -> Replay {
    let mut r = Replay::default();
    let mut done = false;
    for line in output.lines() {
        let Some(rest) = line
            .strip_prefix("PTEST\t")
            .or_else(|| line.strip_prefix("PTEST "))
        else {
            continue;
        };
        // Redux prints Lua's print() arguments separated by tabs or spaces, depending on build.
        if let Some(why) = rest.strip_prefix("FAIL").map(str::trim) {
            r.failure = Some(why.to_string());
            continue;
        }
        let fields: Vec<&str> = rest.split_whitespace().collect();
        match fields.as_slice() {
            ["REACHED", name, frame] => r
                .reached
                .push(((*name).to_string(), frame.parse().unwrap_or(0))),
            ["DONE", frames, distinct] => {
                done = true;
                r.frames = frames.trim().parse().unwrap_or(0);
                r.distinct = distinct.trim().parse().unwrap_or(0);
            }
            _ => {}
        }
    }
    if !done && r.failure.is_none() {
        r.failure = Some("the driver never finished (crash or timeout); see run.log".into());
    }
    if output.contains("not available yet") {
        r.failure = Some(format!(
            "Redux paused the emulation: {}",
            output
                .lines()
                .find(|l| l.contains("not available yet"))
                .unwrap_or("")
                .trim()
        ));
    }
    r
}

/// Run one plan on Redux. `contents` is the bundle's `Contents` dir; `cfg` the private config dir.
pub fn replay(
    plan: &Plan,
    contents: &Path,
    cfg: &Path,
    out: &Path,
    timeout_s: u64,
) -> Result<Replay, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    std::fs::create_dir_all(cfg).map_err(|e| format!("create {}: {e}", cfg.display()))?;
    let job = out.join("job.lua");
    let driver = out.join("driver.lua");
    let mut chunk = plan_lua(plan, out)?;
    chunk.push_str(&format!(
        "dofile({})\n",
        lua_string(&driver.display().to_string())
    ));
    std::fs::write(&job, chunk).map_err(|e| e.to_string())?;
    std::fs::write(&driver, DRIVER).map_err(|e| e.to_string())?;
    let app = contents.join("MacOS/PCSX-Redux");
    let bios = contents.join("Resources/share/pcsx-redux/resources/openbios.bin");
    let argv: Vec<String> = vec![
        app.display().to_string(),
        "-portable".into(),
        "-cli".into(),
        "-softgpu".into(),
        "-stdout".into(),
        "-bios".into(),
        bios.display().to_string(),
        "-iso".into(),
        plan.disc.display().to_string(),
        "-dofile".into(),
        job.display().to_string(),
        "-run".into(),
    ];
    let log = out.join("run.log");
    let done = util::run_watched(
        &argv,
        cfg,
        &[],
        &log,
        Some(timeout_s),
        Some("not available yet"),
    )?;
    let mut r = parse(&done.output);
    if done.timed_out && r.failure.is_none() {
        r.failure = Some(format!("killed at the {timeout_s} s timeout"));
    }
    for entry in std::fs::read_dir(out).map_err(|e| e.to_string())?.flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "ppm") {
            if let Some(img) = read_ppm(&p) {
                if img.save(p.with_extension("png")).is_ok() {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
    std::fs::write(
        out.join("result.json"),
        serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(r)
}

/// A binary `P6 W H 255` picture as the driver writes it.
fn read_ppm(path: &Path) -> Option<image::RgbImage> {
    let bytes = std::fs::read(path).ok()?;
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        while bytes.get(at)?.is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while !bytes.get(at)?.is_ascii_whitespace() {
            at += 1;
        }
        fields.push(std::str::from_utf8(&bytes[start..at]).ok()?.to_string());
    }
    if fields[0] != "P6" || fields[3] != "255" {
        return None;
    }
    let (w, h): (u32, u32) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    let pixels = bytes.get(at + 1..)?;
    image::RgbImage::from_raw(w, h, pixels.get(..(w * h * 3) as usize)?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journey::{PlanStep, Probe};

    fn plan(lstick: Option<[u8; 2]>) -> Plan {
        Plan {
            name: "s".into(),
            disc: "/d.cue".into(),
            pad: "dualshock".into(),
            max_frames: 100,
            min_distinct: 60,
            idle_frames: 0,
            steps: vec![PlanStep {
                label: "it's".into(),
                action: Action::Hold {
                    buttons: vec!["cross".into()],
                    lstick,
                },
                probes: vec![Probe {
                    text: "S eq 1".into(),
                    addr: 0x800a139c,
                    size: 4,
                    signed: false,
                    op: "eq".into(),
                    value: 1,
                    value_hi: 0,
                }],
                timeout: 50,
            }],
        }
    }

    #[test]
    fn the_plan_becomes_lua_and_quotes_are_escaped() {
        let lua = plan_lua(&plan(None), Path::new("/o")).unwrap();
        assert!(lua.contains("device = 'Analog'"));
        assert!(lua.contains("label = 'it\\'s'"));
        assert!(lua.contains("addr = 2148144028"));
        assert!(plan_lua(&plan(Some([200, 128])), Path::new("/o"))
            .unwrap_err()
            .contains("no stick API"));
    }

    #[test]
    fn ptest_lines_and_a_pause_are_read() {
        let spaced = parse("PTEST REACHED title 300 \nPTEST DONE 900 120 \n");
        assert_eq!(spaced.reached, [("title".to_string(), 300)]);
        assert!(spaced.failure.is_none());
        let failed =
            parse("PTEST FAIL play: S eq 3 still false after 600 frames\nPTEST DONE 10 2\n");
        assert!(failed.failure.unwrap().starts_with("play: S eq 3"));
        let r = parse("x\nPTEST\tREACHED\ttitle\t300\nPTEST\tDONE\t900\t120\n");
        assert_eq!(r.reached, [("title".to_string(), 300)]);
        assert_eq!((r.frames, r.distinct), (900, 120));
        assert!(r.failure.is_none());
        let r = parse("CD-Rom: w3:1(03) not available yet\nPTEST\tDONE\t900\t5\n");
        assert!(r.failure.unwrap().contains("paused"));
        assert!(parse("").failure.is_some());
    }
}
