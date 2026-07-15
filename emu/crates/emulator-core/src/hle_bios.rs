//! High-level emulation of the PS1 BIOS syscall tables.
//!
//! The BIOS publishes three entry points -- at physical addresses
//! `0xA0`, `0xB0`, and `0xC0` -- that dispatch to a table of service
//! functions. Each caller does:
//!
//! ```text
//!     la $t0, 0xA0       # or 0xB0 / 0xC0
//!     jr $t0
//!     li $t1, <func>     # in the branch delay slot
//! ```
//!
//! and the BIOS dispatcher reads `$t1`, calls the right handler, and
//! returns to `$ra`.
//!
//! When we side-load a PSX-EXE we bypass the BIOS boot sequence, so
//! the dispatcher stubs at those RAM addresses aren't populated. This
//! module fills the gap by intercepting the instruction fetch when
//! `PC` hits one of the three entry addresses, running the requested
//! service in host Rust, and "returning" by setting `PC = $ra`.
//!
//! Scope for the first pass: TTY output, `FlushCache`, and the event
//! system in "always-ready" mode so homebrew that polls `TestEvent`
//! doesn't spin forever. Games that use richer BIOS facilities
//! (file I/O, memory cards, controllers) can land their handlers
//! here incrementally as we exercise them.

use crate::Bus;
use psx_hw::memory::to_physical;

/// Minimal low-RAM kernel objects used by side-loaded EXEs. Retail BIOS owns
/// this area; keeping the synthetic objects here lets homebrew that hooks the
/// unresolved-exception callback use the documented process/thread pointers.
pub(crate) const PROCESS_LIST_PTR: u32 = 0x0000_0108;
pub(crate) const UNRESOLVED_HANDLER_PTR: u32 = 0x0000_0300;
pub(crate) const SYNTHETIC_PROCESS: u32 = 0x8000_0400;
pub(crate) const SYNTHETIC_THREAD: u32 = 0x8000_0500;
pub(crate) const EXCEPTION_RETURN_STUB: u32 = 0x8000_00D0;

pub(crate) const THREAD_REGISTERS: u32 = SYNTHETIC_THREAD + 8;
pub(crate) const THREAD_RETURN_PC: u32 = THREAD_REGISTERS + 32 * 4;
pub(crate) const THREAD_HI: u32 = THREAD_RETURN_PC + 4;
pub(crate) const THREAD_LO: u32 = THREAD_HI + 4;
pub(crate) const THREAD_SR: u32 = THREAD_LO + 4;
pub(crate) const THREAD_CAUSE: u32 = THREAD_SR + 4;

/// One of the three BIOS dispatcher tables.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Table {
    /// Entry point at physical `0xA0`.
    A,
    /// Entry point at physical `0xB0`.
    B,
    /// Entry point at physical `0xC0`.
    C,
}

impl Table {
    fn from_phys(phys: u32) -> Option<Self> {
        match phys {
            0xA0 => Some(Table::A),
            0xB0 => Some(Table::B),
            0xC0 => Some(Table::C),
            _ => None,
        }
    }
}

/// Result of one HLE dispatch: `$v0` return value and the updated PC.
#[derive(Copy, Clone, Debug)]
pub struct Hle {
    /// Value to write into `$r2 ($v0)`. `0` if the syscall doesn't
    /// return a meaningful value.
    pub v0: u32,
    /// Value to set PC to after the call. Normally `$ra`, so the CPU
    /// resumes right after the caller's `jalr` (or, in the BIOS-stub
    /// pattern, right after the `jr $t0 ; li $t1, N` pair).
    pub next_pc: u32,
}

/// Look at `cpu_pc`; if it matches a BIOS table entry, run the
/// service that `$t1 ($r9)` selects and return the post-call state.
/// Otherwise return `None` and let the CPU fetch normally.
///
/// `args` is the four argument registers `$a0..$a3` (`$r4..$r7`),
/// `t1_func_num` is `$r9` (the function selector set by the caller's
/// delay-slot load), and `ra` is `$r31`.
pub fn dispatch(
    cpu_pc: u32,
    bus: &mut Bus,
    args: [u32; 4],
    sp: u32,
    t1_func_num: u32,
    ra: u32,
) -> Option<Hle> {
    let phys = to_physical(cpu_pc);
    let table = Table::from_phys(phys)?;
    let func = (t1_func_num & 0xFF) as u8;
    let v0 = run(table, func, bus, args, sp);
    Some(Hle { v0, next_pc: ra })
}

fn run(table: Table, func: u8, bus: &mut Bus, args: [u32; 4], sp: u32) -> u32 {
    bus.hle_bios_log_call(table, func);
    match (table, func) {
        // --- A-table ---

        // A(0x2A) malloc / A(0x33) memset / similar memory helpers.
        // Most commercial games roll their own allocator; these
        // fallthroughs prevent a jump-to-zero on a stray call.
        (Table::A, 0x2A) => 0,
        (Table::A, 0x33) => {
            // memset(dest, val, n) -- write `n` bytes of `val` to `dest`.
            let (dest, val, n) = (args[0], args[1] as u8, args[2]);
            for i in 0..n.min(0x20_0000) {
                let _ = bus.write8_safe(dest.wrapping_add(i), val);
            }
            dest
        }

        // A(0x3C) putchar / A(0x3D) getchar.
        (Table::A, 0x3C) => {
            write_byte_to_stdout(args[0] as u8);
            0
        }
        // A(0x3D) getchar -- no stdin source yet; return -1 (EOF).
        (Table::A, 0x3D) => u32::MAX,

        // A(0x3E) puts(*s) / A(0x3F) printf.
        (Table::A, 0x3E) => {
            write_cstring_to_stdout(bus, args[0]);
            0
        }
        (Table::A, 0x3F) => {
            // printf varargs follow the MIPS o32 ABI: a1-a3 first, then the
            // caller's reserved argument area beginning at sp+16. Reading
            // both sources lets public hardware suites print complete rows
            // instead of losing their fourth and later values.
            hle_printf(bus, args[0], &[args[1], args[2], args[3]], sp);
            0
        }

        // A(0x44) FlushCache -- the CPU intercept invalidates its
        // instruction cache before this HLE handler returns.
        (Table::A, 0x44) => 0,

        // A(0x70) _bu_init (memcard filesystem init) -- accept.
        (Table::A, 0x70) => 0,

        // A(0x96) AddCDROMDevice / A(0x97) AddMemCardDevice -- games
        // call these during init to register filesystem drivers.
        // We don't model the device table; accept so the game moves on.
        (Table::A, 0x96) | (Table::A, 0x97) => 0,

        // A(0x9F) EnterCriticalSection / A(0xA0) ExitCriticalSection.
        // On hardware these manipulate SR.IE. HLE BIOS can't safely
        // forge IE-manipulation, but games use them as bracket
        // scopes -- as long as pairs balance and both return plausibly,
        // the game proceeds. EnterCriticalSection returns 1.
        (Table::A, 0x9F) => 1,
        (Table::A, 0xA0) => 0,

        // --- B-table ---

        // B(0x00) SysMalloc -- not a real malloc; many games replace
        // the kernel heap with their own and never call this.
        (Table::B, 0x00) => 0,

        // B(0x07) DeliverEvent -- accept; our event system is always-
        // ready so there's nothing to deliver.
        (Table::B, 0x07) => 0,

        // B(0x08) OpenEvent: return a synthetic handle. We accept
        // everything; the handle encodes table + slot for debug.
        (Table::B, 0x08) => 0xF400_0000 | (args[0] & 0xFFFF),

        // B(0x09) CloseEvent, B(0x0A) WaitEvent, B(0x0B) TestEvent,
        // B(0x0C) EnableEvent, B(0x0D) DisableEvent -- always-ready.
        (Table::B, 0x09)
        | (Table::B, 0x0A)
        | (Table::B, 0x0B)
        | (Table::B, 0x0C)
        | (Table::B, 0x0D) => 1,

        // B(0x12) InitPad(buf1, siz1, buf2, siz2): tell the kernel
        // where to stash pad state. Since we poll the hardware
        // directly via psx-pad there's nothing for us to do.
        (Table::B, 0x12) => 1,

        // B(0x13) StartPad, B(0x14) StopPad -- accept.
        (Table::B, 0x13) | (Table::B, 0x14) => 1,

        // B(0x17) ReturnFromException is completed by Cpu::execute_one:
        // when a side-loaded guest IRQ hook is active it restores the
        // interrupted CPU frame instead of returning to this call's `$ra`.
        (Table::B, 0x17) => 0,

        // B(0x18) ResetEntryInt / B(0x19) HookEntryInt. The latter receives
        // a BIOS-compatible JumpBuffer pointer (ra, sp, fp, s0..s7, gp).
        // Retaining it lets side-loaded EXEs use their real guest ISR rather
        // than relying on a synthetic VBlank callback.
        (Table::B, 0x18) => {
            bus.set_hle_irq_jump_buffer(None);
            0
        }
        (Table::B, 0x19) => {
            bus.set_hle_irq_jump_buffer(Some(args[0]));
            0
        }

        // B(0x3D) std_out_putchar -- same as A(0x3C).
        (Table::B, 0x3D) => {
            write_byte_to_stdout(args[0] as u8);
            0
        }

        // B(0x4A) InitCard, B(0x4B) StartCard, B(0x4C) StopCard.
        (Table::B, 0x4A) | (Table::B, 0x4B) | (Table::B, 0x4C) => 1,

        // --- C-table (kernel interrupt handlers) ---

        // C(0x00) EnqueueTimerAndVblankIrqs / C(0x01) EnqueueSyscallHandler.
        // Install canned handlers. We never actually invoke them --
        // but accepting the registration lets games proceed.
        (Table::C, 0x00) | (Table::C, 0x01) | (Table::C, 0x02) | (Table::C, 0x03) => 0,

        // C(0x0A) ChangeClearRCnt -- affects how the kernel's
        // root-counter handler clears flags. No-op.
        (Table::C, 0x0A) => args[1],

        // Everything else: zero. Games that trip a real missing
        // syscall will show up in the HLE call histogram and we
        // can fill them in one at a time.
        _ => 0,
    }
}

fn write_byte_to_stdout(byte: u8) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&[byte]);
    let _ = out.flush();
}

fn write_cstring_to_stdout(bus: &mut Bus, addr: u32) {
    let mut p = addr;
    // Bound at 4 KiB per call so a bogus pointer can't hang us.
    for _ in 0..4096 {
        let b = bus.try_read8(p).unwrap_or(0);
        if b == 0 {
            break;
        }
        write_byte_to_stdout(b);
        p = p.wrapping_add(1);
    }
}

/// Minimal printf for A(0x3F): %% %c %s %d %i %u %x %X with field width,
/// left alignment, zero padding, and alternate hexadecimal form (for example
/// `%-10s`, `%08x`, and `%#10x`). Register and stack varargs follow o32.
fn hle_printf(bus: &mut Bus, fmt_addr: u32, varargs: &[u32; 3], sp: u32) {
    let mut out: Vec<u8> = Vec::with_capacity(128);
    let mut next_arg = 0usize;
    let mut p = fmt_addr;
    let mut budget = 4096;
    while budget > 0 {
        budget -= 1;
        let b = bus.try_read8(p).unwrap_or(0);
        p = p.wrapping_add(1);
        if b == 0 {
            break;
        }
        if b != b'%' {
            out.push(b);
            continue;
        }
        // Accept flags on either side of the width. The latter is unusual,
        // but JaCzekanski's public access-time test uses `%2-d` and the real
        // BIOS accepts it.
        let mut zero_pad = false;
        let mut left_align = false;
        let mut alternate = false;
        let mut width = 0usize;
        let conv;
        loop {
            let c = bus.try_read8(p).unwrap_or(0);
            p = p.wrapping_add(1);
            match c {
                b'-' => left_align = true,
                b'#' => alternate = true,
                b'0' if width == 0 && !zero_pad => zero_pad = true,
                b'0'..=b'9' => width = width * 10 + (c - b'0') as usize,
                b'l' => {} // longs are 32-bit here; ignore the modifier
                _ => {
                    conv = c;
                    break;
                }
            }
        }
        match conv {
            b'%' => out.push(b'%'),
            b'c' => {
                if let Some(v) = next_printf_arg(bus, varargs, sp, &mut next_arg) {
                    append_padded(&mut out, &[v as u8], width, b' ', left_align);
                }
            }
            b's' => {
                if let Some(v) = next_printf_arg(bus, varargs, sp, &mut next_arg) {
                    let start = out.len();
                    let mut sp = v;
                    for _ in 0..4096 {
                        let sb = bus.try_read8(sp).unwrap_or(0);
                        if sb == 0 {
                            break;
                        }
                        out.push(sb);
                        sp = sp.wrapping_add(1);
                    }
                    pad_existing_field(&mut out, start, width, b' ', left_align);
                }
            }
            b'd' | b'i' => {
                if let Some(v) = next_printf_arg(bus, varargs, sp, &mut next_arg) {
                    let field = format!("{}", v as i32);
                    append_padded(
                        &mut out,
                        field.as_bytes(),
                        width,
                        if zero_pad { b'0' } else { b' ' },
                        left_align,
                    );
                }
            }
            b'u' => {
                if let Some(v) = next_printf_arg(bus, varargs, sp, &mut next_arg) {
                    let field = format!("{v}");
                    append_padded(
                        &mut out,
                        field.as_bytes(),
                        width,
                        if zero_pad { b'0' } else { b' ' },
                        left_align,
                    );
                }
            }
            b'x' | b'X' => {
                if let Some(v) = next_printf_arg(bus, varargs, sp, &mut next_arg) {
                    let digits = if conv == b'x' {
                        format!("{v:x}")
                    } else {
                        format!("{v:X}")
                    };
                    let s = if alternate && v != 0 {
                        format!("{}{}", if conv == b'x' { "0x" } else { "0X" }, digits)
                    } else {
                        digits
                    };
                    append_padded(
                        &mut out,
                        s.as_bytes(),
                        width,
                        if zero_pad { b'0' } else { b' ' },
                        left_align,
                    );
                }
            }
            // Unknown / out-of-register conversion: emit verbatim so the
            // reader at least sees what the guest meant.
            other => {
                out.push(b'%');
                if zero_pad {
                    out.push(b'0');
                }
                if left_align {
                    out.push(b'-');
                }
                if alternate {
                    out.push(b'#');
                }
                if width > 0 {
                    out.extend_from_slice(format!("{width}").as_bytes());
                }
                out.push(other);
            }
        }
    }
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
}

fn next_printf_arg(bus: &Bus, registers: &[u32; 3], sp: u32, next: &mut usize) -> Option<u32> {
    let index = *next;
    *next += 1;
    if let Some(value) = registers.get(index) {
        return Some(*value);
    }
    // o32 reserves four argument words at the caller's stack pointer. The
    // fixed format pointer occupies slot 0; a1-a3 occupy slots 1-3, and the
    // fourth vararg begins at slot 4 (sp+16).
    let addr = sp.wrapping_add(16 + ((index - registers.len()) as u32) * 4);
    let bytes = [
        bus.try_read8(addr)?,
        bus.try_read8(addr.wrapping_add(1))?,
        bus.try_read8(addr.wrapping_add(2))?,
        bus.try_read8(addr.wrapping_add(3))?,
    ];
    Some(u32::from_le_bytes(bytes))
}

fn append_padded(out: &mut Vec<u8>, field: &[u8], width: usize, pad: u8, left_align: bool) {
    let padding = width.saturating_sub(field.len());
    if !left_align {
        out.extend(std::iter::repeat_n(pad, padding));
    }
    out.extend_from_slice(field);
    if left_align {
        out.extend(std::iter::repeat_n(pad, padding));
    }
}

fn pad_existing_field(out: &mut Vec<u8>, start: usize, width: usize, pad: u8, left_align: bool) {
    let field_len = out.len().saturating_sub(start);
    let padding = width.saturating_sub(field_len);
    if left_align {
        out.extend(std::iter::repeat_n(pad, padding));
    } else if padding != 0 {
        out.splice(start..start, std::iter::repeat_n(pad, padding));
    }
}

#[cfg(test)]
mod tests {
    use super::{append_padded, dispatch, pad_existing_field};
    use crate::Bus;

    #[test]
    fn printf_field_padding_handles_both_alignments() {
        let mut out = Vec::new();
        append_padded(&mut out, b"RAM", 5, b' ', true);
        append_padded(&mut out, b"7", 3, b'0', false);
        assert_eq!(out, b"RAM  007");

        let start = out.len();
        out.extend_from_slice(b"BIOS");
        pad_existing_field(&mut out, start, 6, b' ', false);
        assert_eq!(&out[start..], b"  BIOS");
    }

    #[test]
    fn hook_entry_int_tracks_and_resets_guest_jump_buffer() {
        let mut bus = Bus::new_without_bios();
        let hook = 0x8001_4000;

        let installed =
            dispatch(0xB0, &mut bus, [hook, 0, 0, 0], 0, 0x19, 0x8001_0100).expect("B0 dispatch");
        assert_eq!(installed.next_pc, 0x8001_0100);
        assert_eq!(bus.hle_irq_jump_buffer(), Some(hook));

        dispatch(0xB0, &mut bus, [0; 4], 0, 0x18, 0x8001_0200).expect("B0 dispatch");
        assert_eq!(bus.hle_irq_jump_buffer(), None);
    }
}
