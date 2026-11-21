//! Model equivalence: a journey row on another hardware model stands on the `real` row of the
//! same journey when nothing the two models treat differently happened in the `real` run.
//!
//! The emulator changes behaviour by model in exactly three places (emulator 076d97fb, read in
//! full; PSoXide-emulator docs/conformance.md, "Model equivalence"):
//! - the GP0(1Fh) model (`gpu-irq`, also what `mister` changes, with its `seed` and `trace`):
//!   consulted when a GP0(1Fh) executes and when the CPU writes one in command position;
//! - `cd-ack=strict`: consulted on a CD interrupt acknowledge that breaks a CD rule;
//! - `mdec-req=hidden`: the MDEC status request bits, which rise only after an MDEC command.
//!
//! The runs are the same up to the first such event, and that event would come at the same point
//! in both, so a `real` run that met none of the events a model's differences hang on is that
//! model's run too. The gate prints the counts in its `SIGNALS` line; a gate without it never
//! lets a row stand.

use super::journeys::GateLine;

/// What the `real` run met of the events the models treat differently (`SIGNALS`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Signals {
    /// GP0(1Fh) commands executed.
    pub gp0_1f: u64,
    /// GP0(1Fh) words the CPU wrote in command position, executed or not.
    pub gp0_1f_written: u64,
    /// CD acknowledges that broke a CD rule.
    pub cd_ack_broken: u64,
    /// MDEC command words written.
    pub mdec_commands: u64,
    /// The journey limits some golden to some models (`golden_on`).
    pub golden_on: bool,
}

impl Signals {
    /// `SIGNALS <journey> <model> gp0-1f=N gp0-1f-written=N cd-ack-broken=N mdec-commands=N
    /// golden-on=yes|no` (the journey and model are skipped; the model may hold `=`).
    pub fn parse(rest: &str) -> Option<Signals> {
        let mut s = Signals::default();
        let mut seen = 0;
        for kv in rest.split_whitespace().skip(2) {
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            let n = || v.parse::<u64>().ok();
            match k {
                "gp0-1f" => s.gp0_1f = n()?,
                "gp0-1f-written" => s.gp0_1f_written = n()?,
                "cd-ack-broken" => s.cd_ack_broken = n()?,
                "mdec-commands" => s.mdec_commands = n()?,
                "golden-on" => s.golden_on = v == "yes",
                _ => continue,
            }
            seen += 1;
        }
        (seen == 5).then_some(s)
    }
}

/// How a model differs from `real`, in the knobs the emulator reads.
#[derive(Debug, Default, PartialEq, Eq)]
struct Knobs {
    /// The GP0(1Fh) model is not the retail one (`mister` defaults to `exact`).
    gpu_irq: bool,
    cd_ack_strict: bool,
    mdec_hidden: bool,
}

/// The knobs of a `--hardware` spec, or why it is not understood (then nothing stands on it).
fn knobs(model: &str) -> Result<Knobs, String> {
    let mut parts = model.split(',').map(str::trim);
    let mut k = match parts.next().unwrap_or("") {
        "" | "real" | "default" => Knobs::default(),
        "mister" | "mister-fpga" | "fpga" | "superstation" => Knobs {
            gpu_irq: true,
            ..Knobs::default()
        },
        other => return Err(format!("unknown hardware target `{other}`")),
    };
    for option in parts.filter(|p| !p.is_empty()) {
        match option.split_once('=') {
            Some(("gpu-irq", v)) => k.gpu_irq = v != "reliable",
            Some(("cd-ack", v)) => k.cd_ack_strict = v == "strict",
            Some(("mdec-req", v)) => k.mdec_hidden = v == "hidden",
            // Only the GP0(1Fh) model reads them, and only while it is not the retail one.
            Some(("seed" | "trace", _)) => {}
            _ => return Err(format!("unknown hardware option `{option}`")),
        }
    }
    Ok(k)
}

/// Whether `model`'s row may stand on this `real` row: `Ok(the signals that made it so)`, or
/// `Err(why it must run)`.
pub fn stands_on_real(model: &str, real: &GateLine) -> Result<String, String> {
    if !real.passed || real.fail > 0 || !real.aborts.is_empty() {
        return Err("the real row did not pass".into());
    }
    if real.carried_from.is_some() {
        return Err("the real row was carried from an older gate, not run on this one".into());
    }
    let Some(s) = &real.signals else {
        return Err("the real row has no SIGNALS line (older gate, or a gate receipt)".into());
    };
    if s.golden_on || real.golden_skipped > 0 {
        return Err("the journey limits goldens to some models (golden_on)".into());
    }
    let k = knobs(model)?;
    let mut said = Vec::new();
    let mut refused = Vec::new();
    if k.gpu_irq {
        if s.gp0_1f == 0 && s.gp0_1f_written == 0 {
            said.push("no GP0(1Fh)");
        } else {
            refused.push(format!(
                "GP0(1Fh) {} executed, {} written",
                s.gp0_1f, s.gp0_1f_written
            ));
        }
    }
    if k.cd_ack_strict {
        if s.cd_ack_broken == 0 {
            said.push("no CD-ack hit");
        } else {
            refused.push(format!("{} CD-ack hit(s)", s.cd_ack_broken));
        }
    }
    if k.mdec_hidden {
        if s.mdec_commands == 0 {
            said.push("no MDEC command");
        } else {
            refused.push(format!("{} MDEC command(s)", s.mdec_commands));
        }
    }
    if !refused.is_empty() {
        return Err(refused.join(", "));
    }
    if said.is_empty() {
        return Err("the model differs from real in nothing the rule knows".into());
    }
    Ok(said.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real(signals: Option<Signals>) -> GateLine {
        GateLine {
            passed: true,
            checks: 3,
            pass: 3,
            ticks: 100,
            signals,
            ..GateLine::default()
        }
    }

    fn quiet() -> Signals {
        Signals::default()
    }

    #[test]
    fn signals_parse_and_refuse_a_short_line() {
        let s = Signals::parse(
            "oot-smoke mister,gpu-irq=never gp0-1f=0 gp0-1f-written=2 cd-ack-broken=0 \
             mdec-commands=7 golden-on=no",
        )
        .unwrap();
        assert_eq!(
            s,
            Signals {
                gp0_1f_written: 2,
                mdec_commands: 7,
                ..Signals::default()
            }
        );
        assert_eq!(Signals::parse("x real gp0-1f=0 cd-ack-broken=0"), None);
    }

    #[test]
    fn a_quiet_real_run_stands_for_every_full_tier_model() {
        let r = real(Some(quiet()));
        assert_eq!(stands_on_real("mister", &r).unwrap(), "no GP0(1Fh)");
        assert_eq!(
            stands_on_real("mister,gpu-irq=never", &r).unwrap(),
            "no GP0(1Fh)"
        );
        assert_eq!(
            stands_on_real("real,cd-ack=strict,mdec-req=hidden", &r).unwrap(),
            "no CD-ack hit, no MDEC command"
        );
    }

    #[test]
    fn each_signal_keeps_only_the_models_that_read_it_running() {
        let gp0 = real(Some(Signals {
            gp0_1f: 5,
            ..quiet()
        }));
        assert!(stands_on_real("mister", &gp0).is_err());
        assert!(stands_on_real("real,cd-ack=strict,mdec-req=hidden", &gp0).is_ok());
        // A GP0(1Fh) written and never executed still differs between the models.
        let written = real(Some(Signals {
            gp0_1f_written: 1,
            ..quiet()
        }));
        assert!(stands_on_real("mister,gpu-irq=never", &written).is_err());
        let cd = real(Some(Signals {
            cd_ack_broken: 1,
            ..quiet()
        }));
        assert!(stands_on_real("mister", &cd).is_ok());
        assert!(stands_on_real("real,cd-ack=strict,mdec-req=hidden", &cd).is_err());
        let mdec = real(Some(Signals {
            mdec_commands: 9,
            ..quiet()
        }));
        assert!(stands_on_real("real,cd-ack=strict,mdec-req=hidden", &mdec).is_err());
        assert!(stands_on_real("real,cd-ack=strict", &mdec).is_ok());
    }

    #[test]
    fn nothing_stands_on_a_red_row_an_old_gate_or_golden_on() {
        let mut red = real(Some(quiet()));
        red.passed = false;
        assert!(stands_on_real("mister", &red).is_err());
        let mut aborted = real(Some(quiet()));
        aborted.aborts.push("guest fault".into());
        assert!(stands_on_real("mister", &aborted).is_err());
        assert!(stands_on_real("mister", &real(None)).is_err());
        let golden = real(Some(Signals {
            golden_on: true,
            ..quiet()
        }));
        assert!(stands_on_real("mister", &golden).is_err());
        let mut skipped = real(Some(quiet()));
        skipped.golden_skipped = 1;
        assert!(stands_on_real("mister", &skipped).is_err());
    }

    #[test]
    fn unknown_models_and_options_never_stand() {
        let r = real(Some(quiet()));
        assert!(stands_on_real("duckstation", &r).is_err());
        assert!(stands_on_real("mister,dither=off", &r).is_err());
        // real with only a seed differs in nothing: there is nothing to stand on.
        assert!(stands_on_real("real,seed=3", &r).is_err());
        assert!(stands_on_real("real,gpu-irq=reliable", &r).is_err());
    }
}
