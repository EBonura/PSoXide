#![allow(dead_code)]

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PadPulse {
    pub mask: u16,
    pub start_vblank: u64,
    pub frames: u64,
}

pub fn parse_u16_mask(s: &str) -> Option<u16> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

pub fn parse_pad_pulses(text: &str) -> Result<Vec<PadPulse>, String> {
    let mut out = Vec::new();
    for entry in text.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (mask_text, rest) = entry
            .split_once('@')
            .ok_or_else(|| format!("pad pulse `{entry}` is missing @"))?;
        let mask = parse_u16_mask(mask_text)
            .ok_or_else(|| format!("pad pulse `{entry}` has invalid mask `{mask_text}`"))?;
        let (start_text, frames_text) = match rest.split_once('+') {
            Some((s, f)) => (s.trim(), f.trim()),
            None => (rest.trim(), "1"),
        };
        let start_vblank = start_text
            .parse()
            .map_err(|_| format!("pad pulse `{entry}` has invalid start `{start_text}`"))?;
        let frames = frames_text
            .parse()
            .map_err(|_| format!("pad pulse `{entry}` has invalid frame count `{frames_text}`"))?;
        out.push(PadPulse {
            mask,
            start_vblank,
            frames,
        });
    }
    Ok(out)
}

pub fn effective_mask(base: u16, pulses: &[PadPulse], current_vblank: u64) -> u16 {
    let mut mask = base;
    for p in pulses {
        if current_vblank >= p.start_vblank && current_vblank < p.start_vblank + p.frames {
            mask |= p.mask;
        }
    }
    mask
}

pub fn format_pad_pulses(pulses: &[PadPulse]) -> String {
    if pulses.is_empty() {
        return "(none)".into();
    }
    pulses
        .iter()
        .map(|pulse| {
            format!(
                "0x{:04x}@{}+{}",
                pulse.mask, pulse.start_vblank, pulse.frames
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Drive port-1 buttons from a base mask plus timed pulses, writing to
/// the bus only when the effective mask changes (including the first
/// call, so the initial state always lands).
pub fn sync_pad_mask(
    bus: &mut emulator_core::Bus,
    base_mask: u16,
    pulses: &[PadPulse],
    current_mask: &mut Option<u16>,
) {
    let next_mask = effective_mask(base_mask, pulses, bus.irq().raise_counts()[0]);
    if current_mask.is_some_and(|mask| mask == next_mask) {
        return;
    }
    bus.set_port1_buttons(emulator_core::ButtonState::from_bits(next_mask));
    *current_mask = Some(next_mask);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pad_pulses_accepts_hex_decimal_and_default_duration() {
        let pulses = parse_pad_pulses("0x0008@100+30,16384@950").unwrap();

        assert_eq!(pulses.len(), 2);
        assert_eq!(pulses[0].mask, 0x0008);
        assert_eq!(pulses[0].start_vblank, 100);
        assert_eq!(pulses[0].frames, 30);
        assert_eq!(pulses[1].mask, 0x4000);
        assert_eq!(pulses[1].start_vblank, 950);
        assert_eq!(pulses[1].frames, 1);
    }

    #[test]
    fn effective_mask_applies_pulse_only_inside_window() {
        let pulses = parse_pad_pulses("0x0008@10+2,0x4000@20+1").unwrap();

        assert_eq!(effective_mask(0, &pulses, 9), 0);
        assert_eq!(effective_mask(0, &pulses, 10), 0x0008);
        assert_eq!(effective_mask(0, &pulses, 11), 0x0008);
        assert_eq!(effective_mask(0, &pulses, 12), 0);
        assert_eq!(effective_mask(0x0020, &pulses, 20), 0x4020);
    }
}
