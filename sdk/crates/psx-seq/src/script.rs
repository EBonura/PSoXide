//! Reading operands and the control-flow opcodes shared by all three levels.

use crate::types::Script;

/// First control-flow opcode.
pub(crate) const FLOW_FIRST: u8 = 0xf2;
/// End of script.
pub(crate) const OP_END: u8 = 0xff;
/// Script result: finished.
pub(crate) const SCRIPT_END: i32 = -1;

/// Operand layout of opcodes `0xB0..=0xFF`: the low two bits are the operand
/// count, bit 7, 6 and 5 say whether operand 0, 1 and 2 are 16 bits wide.
pub(crate) const ARGS: [u8; 80] = [
    0x81, 0x00, 0x81, 0x01, 0x00, 0x00, 0x00, 0x81, // B0..B7
    0x01, 0x01, 0x01, 0x42, 0x81, 0xc2, 0x00, 0x00, // B8..BF
    0x00, 0x01, 0x81, 0x00, 0x00, 0x00, 0x01, 0x42, // C0..C7
    0x01, 0x01, 0x01, 0x81, 0x01, 0x01, 0x81, 0x81, // C8..CF
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, // D0..D7
    0x01, 0x01, 0x81, 0x01, 0x01, 0x01, 0x81, 0x01, // D8..DF
    0x01, 0x03, 0x03, 0x01, 0x00, 0x01, 0x01, 0x81, // E0..E7
    0x03, 0x01, 0x00, 0x02, 0x00, 0x01, 0x01, 0x82, // E8..EF
    0x00, 0x01, 0x01, 0x01, 0x01, 0x81, 0x00, 0x00, // F0..F7
    0x01, 0x81, 0x81, 0x81, 0x81, 0x00, 0x00, 0x00, // F8..FF
];

/// Read one byte (reads past the end give `0xFF`, which ends a script).
pub(crate) fn rd8(code: &[u8], s: &mut Script) -> u8 {
    let v = code.get(s.pc as usize).copied().unwrap_or(OP_END);
    s.pc = s.pc.wrapping_add(1);
    v
}

/// Read a big-endian 16-bit operand.
pub(crate) fn rd16(code: &[u8], s: &mut Script) -> u16 {
    let hi = rd8(code, s) as u16;
    (hi << 8) | rd8(code, s) as u16
}

/// Read a delay: one byte, or two when the top bit of the first is set.
pub(crate) fn rd_delay(code: &[u8], s: &mut Script) -> u16 {
    let a = rd8(code, s) as u16;
    if a & 0x80 != 0 {
        ((a << 8) & 0x7f00) | rd8(code, s) as u16
    } else {
        a
    }
}

/// The single operand of a control-flow opcode (or `0`).
pub(crate) fn flow_arg(code: &[u8], s: &mut Script, op: u8) -> u16 {
    let t = ARGS[(op - 0xb0) as usize];
    if t & 3 == 1 {
        if t & 0x80 == 0 {
            rd8(code, s) as u16
        } else {
            rd16(code, s)
        }
    } else {
        0
    }
}

/// Run a control-flow opcode. Returns `0` to carry on, [`SCRIPT_END`], or a
/// positive delay the caller should wait.
pub(crate) fn flow(code: &[u8], s: &mut Script, op: u8, arg: u16) -> i32 {
    match op {
        0xff => {
            if s.depth == 0 {
                return SCRIPT_END;
            }
            s.depth -= 1;
            s.pc = s.stack[s.depth as usize & 3];
        }
        0xfd => return rd_delay(code, s) as i32,
        0xfe => return 1,
        0xfc => {
            if s.depth < 4 {
                s.stack[s.depth as usize] = s.pc;
                s.depth += 1;
            }
            s.pc = arg;
        }
        0xf8 if s.depth < 4 => {
            s.loops[s.depth as usize] = arg as u8;
            s.stack[s.depth as usize] = s.pc;
            s.depth += 1;
        }
        0xf7 if s.depth > 0 => {
            let d = s.depth as usize - 1;
            s.loops[d] = s.loops[d].wrapping_sub(1);
            if s.loops[d] != 0 {
                s.pc = s.stack[d];
            } else {
                s.depth -= 1;
            }
        }
        0xf6 => s.depth = s.depth.saturating_sub(1),
        0xf5 | 0xf9 | 0xfa | 0xfb => {
            let skip = (op == 0xfa && s.value != 0)
                || (op == 0xf9 && s.value >= 0)
                || (op == 0xf5 && s.value < 0);
            if !skip {
                s.pc = arg;
            }
        }
        0xf2..=0xf4 => {
            let skip = (op == 0xf3 && s.value != 0) || (op == 0xf2 && s.value >= 0);
            if !skip {
                s.pc = s.pc.wrapping_add(arg as u8 as i8 as u16);
            }
        }
        _ => {}
    }
    0
}
