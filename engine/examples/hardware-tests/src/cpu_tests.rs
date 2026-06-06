use super::*;

pub(super) fn run_cpu_scan() -> ScanReport {
    let mut hash = 0x811C_9DC5;
    let mut items = 0u16;

    macro_rules! sample_r {
        ($instr:expr, $rs:expr, $rt:expr) => {{
            let instr = $instr;
            let result = cpu_r::<{ $instr }>($rs, $rt);
            hash = mix32(hash, instr);
            hash = mix32(hash, $rs);
            hash = mix32(hash, $rt);
            hash = mix32(hash, result);
            items = items.wrapping_add(1);
        }};
    }
    macro_rules! sample_i {
        ($instr:expr, $rs:expr) => {{
            let instr = $instr;
            let result = cpu_i::<{ $instr }>($rs);
            hash = mix32(hash, instr);
            hash = mix32(hash, $rs);
            hash = mix32(hash, result);
            items = items.wrapping_add(1);
        }};
    }
    macro_rules! sample_hilo {
        ($instr:expr, $rs:expr, $rt:expr) => {{
            let instr = $instr;
            let (lo, hi) = cpu_hilo::<{ $instr }>($rs, $rt);
            hash = mix32(hash, instr);
            hash = mix32(hash, $rs);
            hash = mix32(hash, $rt);
            hash = mix32(hash, lo);
            hash = mix32(hash, hi);
            items = items.wrapping_add(1);
        }};
    }

    sample_r!(mips_r(8, 9, 10, 0, 0x21), 0x7FFF_FFFE, 3);
    sample_r!(mips_r(8, 9, 10, 0, 0x23), 0x8000_0002, 7);
    sample_r!(mips_r(8, 9, 10, 0, 0x24), 0xF0F0_A55A, 0x0FF0_5AA5);
    sample_r!(mips_r(8, 9, 10, 0, 0x25), 0xF0F0_A55A, 0x0FF0_5AA5);
    sample_r!(mips_r(8, 9, 10, 0, 0x26), 0xF0F0_A55A, 0x0FF0_5AA5);
    sample_r!(mips_r(0, 9, 10, 7, 0x00), 0, 0x0000_0123);
    sample_r!(mips_r(0, 9, 10, 5, 0x02), 0, 0x8000_0000);
    sample_r!(mips_r(0, 9, 10, 5, 0x03), 0, 0x8000_0000);
    sample_r!(mips_r(8, 9, 10, 0, 0x04), 9, 0x0000_0101);
    sample_r!(mips_r(8, 9, 10, 0, 0x06), 11, 0xF000_0000);
    sample_r!(mips_r(8, 9, 10, 0, 0x07), 11, 0xF000_0000);
    sample_r!(mips_r(8, 9, 10, 0, 0x2A), 0xFFFF_FFFF, 1);
    sample_r!(mips_r(8, 9, 10, 0, 0x2B), 1, 0xFFFF_FFFF);

    sample_i!(mips_i(0x09, 8, 10, 0x8001), 0x0000_0002);
    sample_i!(mips_i(0x0A, 8, 10, 0x0001), 0xFFFF_FFFF);
    sample_i!(mips_i(0x0B, 8, 10, 0xFFFF), 1);
    sample_i!(mips_i(0x0C, 8, 10, 0x5AA5), 0xF0F0_A55A);
    sample_i!(mips_i(0x0D, 8, 10, 0x5AA5), 0xF0F0_0000);
    sample_i!(mips_i(0x0E, 8, 10, 0x5AA5), 0xF0F0_F0F0);
    sample_i!(mips_i(0x0F, 0, 10, 0xBEEF), 0);

    sample_hilo!(mips_r(8, 9, 0, 0, 0x18), 0xFFFF_FFFD, 7);
    sample_hilo!(mips_r(8, 9, 0, 0, 0x19), 0xFFFF_FFFD, 7);
    sample_hilo!(mips_r(8, 9, 0, 0, 0x1A), 0xFFFF_FFFD, 2);
    sample_hilo!(mips_r(8, 9, 0, 0, 0x1B), 0xFFFF_FFFD, 2);

    let (lo, hi) = cpu_mthi_mtlo(0x1357_2468, 0x89AB_CDEF);
    hash = mix32(hash, lo);
    hash = mix32(hash, hi);
    items = items.wrapping_add(1);

    hash = mix32(hash, cpu_branch_delay_battery());
    hash = mix32(hash, cpu_load_store_battery());
    hash = mix32(hash, cpu_unaligned_load_store_battery());
    items = items.wrapping_add(3);

    ScanReport::info(items, hash, 0, "safe mips-i forms")
}

pub(super) fn test_cpu_endian() -> TestResult {
    static mut WORD: u32 = 0;
    unsafe {
        let word = &raw mut WORD;
        ptr::write_volatile(word, 0x4433_2211);
        let bytes = word as *const u8;
        let observed = (ptr::read_volatile(bytes.add(0)) as u32)
            | ((ptr::read_volatile(bytes.add(1)) as u32) << 8)
            | ((ptr::read_volatile(bytes.add(2)) as u32) << 16)
            | ((ptr::read_volatile(bytes.add(3)) as u32) << 24);
        expect_eq(0x4433_2211, observed, "byte order")
    }
}

pub(super) fn test_cpu_arithmetic() -> TestResult {
    let mut observed = 0u32;
    if 0x7FFF_FFFFu32.wrapping_add(1) == 0x8000_0000 {
        observed |= 1;
    }
    if (((0x8000_0000u32 as i32) >> 31) as u32) == 0xFFFF_FFFF {
        observed |= 2;
    }
    if 0x1234_5678u32.wrapping_mul(9) == 0xA3D7_0A38 {
        observed |= 4;
    }
    expect_eq(0x7, observed, "alu bits")
}

pub(super) fn test_cpu_rtype_opcodes() -> TestResult {
    let mut observed = 0u32;
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x21) }>(0x1000_0000, 0x0000_0007) == 0x1000_0007 {
        observed |= 1 << 0; // ADDU
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x23) }>(0x1000_0000, 0x0000_0007) == 0x0FFF_FFF9 {
        observed |= 1 << 1; // SUBU
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x24) }>(0xF0F0_A55A, 0x0FF0_5AA5) == 0x00F0_0000 {
        observed |= 1 << 2; // AND
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x25) }>(0xF0F0_A55A, 0x0FF0_5AA5) == 0xFFF0_FFFF {
        observed |= 1 << 3; // OR
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x26) }>(0xF0F0_A55A, 0x0FF0_5AA5) == 0xFF00_FFFF {
        observed |= 1 << 4; // XOR
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x27) }>(0xF0F0_A55A, 0x0FF0_5AA5) == 0x000F_0000 {
        observed |= 1 << 5; // NOR
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x2A) }>(0xFFFF_FFFF, 0x0000_0001) == 1 {
        observed |= 1 << 6; // SLT
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x2B) }>(0x0000_0001, 0xFFFF_FFFF) == 1 {
        observed |= 1 << 7; // SLTU
    }
    if cpu_r::<{ mips_r(0, 9, 10, 3, 0x00) }>(0, 0x0000_0011) == 0x0000_0088 {
        observed |= 1 << 8; // SLL
    }
    if cpu_r::<{ mips_r(0, 9, 10, 4, 0x02) }>(0, 0x8000_0000) == 0x0800_0000 {
        observed |= 1 << 9; // SRL
    }
    if cpu_r::<{ mips_r(0, 9, 10, 4, 0x03) }>(0, 0x8000_0000) == 0xF800_0000 {
        observed |= 1 << 10; // SRA
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x04) }>(3, 0x0000_0011) == 0x0000_0088 {
        observed |= 1 << 11; // SLLV
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x06) }>(4, 0x8000_0000) == 0x0800_0000 {
        observed |= 1 << 12; // SRLV
    }
    if cpu_r::<{ mips_r(8, 9, 10, 0, 0x07) }>(4, 0x8000_0000) == 0xF800_0000 {
        observed |= 1 << 13; // SRAV
    }
    expect_eq(0x3FFF, observed, "rtype")
}

pub(super) fn test_cpu_immediate_opcodes() -> TestResult {
    let mut observed = 0u32;
    if cpu_i::<{ mips_i(0x09, 8, 10, 0x7FFF) }>(0x0000_0001) == 0x0000_8000 {
        observed |= 1 << 0; // ADDIU
    }
    if cpu_i::<{ mips_i(0x0C, 8, 10, 0x0FF0) }>(0xF0F0_A55A) == 0x0000_0550 {
        observed |= 1 << 1; // ANDI
    }
    if cpu_i::<{ mips_i(0x0D, 8, 10, 0x00FF) }>(0x1234_0000) == 0x1234_00FF {
        observed |= 1 << 2; // ORI
    }
    if cpu_i::<{ mips_i(0x0E, 8, 10, 0x00FF) }>(0x1234_00F0) == 0x1234_000F {
        observed |= 1 << 3; // XORI
    }
    if cpu_i::<{ mips_i(0x0A, 8, 10, 0x0001) }>(0xFFFF_FFFF) == 1 {
        observed |= 1 << 4; // SLTI
    }
    if cpu_i::<{ mips_i(0x0B, 8, 10, 0xFFFF) }>(0x0000_0001) == 1 {
        observed |= 1 << 5; // SLTIU
    }
    if cpu_i::<{ mips_i(0x0F, 0, 10, 0x1234) }>(0) == 0x1234_0000 {
        observed |= 1 << 6; // LUI
    }
    expect_eq(0x7F, observed, "itype")
}

pub(super) fn test_cpu_hilo_opcodes() -> TestResult {
    let (mult_lo, mult_hi) = cpu_hilo::<{ mips_r(8, 9, 0, 0, 0x18) }>(0xFFFF_FFFE, 3);
    let (multu_lo, multu_hi) = cpu_hilo::<{ mips_r(8, 9, 0, 0, 0x19) }>(0xFFFF_FFFE, 3);
    let (div_lo, div_hi) = cpu_hilo::<{ mips_r(8, 9, 0, 0, 0x1A) }>(0xFFFF_FFFD, 2);
    let (divu_lo, divu_hi) = cpu_hilo::<{ mips_r(8, 9, 0, 0, 0x1B) }>(7, 3);
    let (mt_lo, mt_hi) = cpu_mthi_mtlo(0x1357_2468, 0x89AB_CDEF);

    let mut observed = 0u32;
    if (mult_lo, mult_hi) == (0xFFFF_FFFA, 0xFFFF_FFFF) {
        observed |= 1 << 0; // MULT
    }
    if (multu_lo, multu_hi) == (0xFFFF_FFFA, 0x0000_0002) {
        observed |= 1 << 1; // MULTU
    }
    if (div_lo, div_hi) == (0xFFFF_FFFF, 0xFFFF_FFFF) {
        observed |= 1 << 2; // DIV
    }
    if (divu_lo, divu_hi) == (2, 1) {
        observed |= 1 << 3; // DIVU
    }
    if (mt_lo, mt_hi) == (0x89AB_CDEF, 0x1357_2468) {
        observed |= 1 << 4; // MTHI/MTLO + MFHI/MFLO
    }
    expect_eq(0x1F, observed, "hilo")
}

pub(super) fn test_cpu_branch_delay_opcodes() -> TestResult {
    let observed = cpu_branch_delay_battery();
    expect_eq(0x1FF, observed, "branch")
}

pub(super) fn test_cpu_load_store_opcodes() -> TestResult {
    let observed = cpu_load_store_battery();
    expect_eq(0x1FF, observed, "load/store")
}

pub(super) fn test_cpu_unaligned_load_store_pairs() -> TestResult {
    let observed = cpu_unaligned_load_store_battery();
    expect_eq(0xFF, observed, "lwl/lwr")
}

#[inline(never)]
pub(super) fn cpu_r<const INSTR: u32>(rs_value: u32, rt_value: u32) -> u32 {
    #[cfg(target_arch = "mips")]
    {
        let out: u32;
        unsafe {
            core::arch::asm!(
                ".word {instr}",
                instr = const INSTR,
                in("$8") rs_value,
                in("$9") rt_value,
                lateout("$10") out,
                options(nostack, nomem, preserves_flags),
            );
        }
        out
    }
    #[cfg(not(target_arch = "mips"))]
    {
        emulate_cpu_r(INSTR, rs_value, rt_value)
    }
}

#[inline(never)]
pub(super) fn cpu_i<const INSTR: u32>(rs_value: u32) -> u32 {
    #[cfg(target_arch = "mips")]
    {
        let out: u32;
        unsafe {
            core::arch::asm!(
                ".word {instr}",
                instr = const INSTR,
                in("$8") rs_value,
                lateout("$10") out,
                options(nostack, nomem, preserves_flags),
            );
        }
        out
    }
    #[cfg(not(target_arch = "mips"))]
    {
        emulate_cpu_i(INSTR, rs_value)
    }
}

#[inline(never)]
pub(super) fn cpu_hilo<const INSTR: u32>(rs_value: u32, rt_value: u32) -> (u32, u32) {
    #[cfg(target_arch = "mips")]
    {
        let lo: u32;
        let hi: u32;
        unsafe {
            core::arch::asm!(
                ".word {instr}",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word {mflo}",
                ".word {mfhi}",
                ".word 0",
                instr = const INSTR,
                mflo = const mips_r(0, 0, 10, 0, 0x12),
                mfhi = const mips_r(0, 0, 11, 0, 0x10),
                in("$8") rs_value,
                in("$9") rt_value,
                lateout("$10") lo,
                lateout("$11") hi,
                options(nostack, nomem, preserves_flags),
            );
        }
        (lo, hi)
    }
    #[cfg(not(target_arch = "mips"))]
    {
        emulate_cpu_hilo(INSTR, rs_value, rt_value)
    }
}

#[inline(never)]
pub(super) fn cpu_mthi_mtlo(hi_value: u32, lo_value: u32) -> (u32, u32) {
    #[cfg(target_arch = "mips")]
    {
        let lo: u32;
        let hi: u32;
        unsafe {
            core::arch::asm!(
                ".word {mthi}",
                ".word {mtlo}",
                ".word {mflo}",
                ".word {mfhi}",
                ".word 0",
                mthi = const mips_r(8, 0, 0, 0, 0x11),
                mtlo = const mips_r(9, 0, 0, 0, 0x13),
                mflo = const mips_r(0, 0, 10, 0, 0x12),
                mfhi = const mips_r(0, 0, 11, 0, 0x10),
                in("$8") hi_value,
                in("$9") lo_value,
                lateout("$10") lo,
                lateout("$11") hi,
                options(nostack, nomem, preserves_flags),
            );
        }
        (lo, hi)
    }
    #[cfg(not(target_arch = "mips"))]
    {
        (lo_value, hi_value)
    }
}

#[inline(never)]
pub(super) fn cpu_branch_delay_battery() -> u32 {
    #[cfg(target_arch = "mips")]
    {
        let out: u32;
        unsafe {
            core::arch::asm!(
                ".word {clear}",
                ".word {beq_taken}",
                ".word {delay_1}",
                ".word {skipped_100}",
                ".word {bne_not_taken}",
                ".word {delay_2}",
                ".word {fallthrough_4}",
                ".word {beq_always}",
                ".word {delay_8}",
                ".word {skipped_200}",
                ".word {set_neg}",
                ".word {blez_taken}",
                ".word {delay_16}",
                ".word {skipped_400}",
                ".word {bgtz_not_taken}",
                ".word {delay_32}",
                ".word {fallthrough_64}",
                ".word {set_neg}",
                ".word {bltz_taken}",
                ".word {delay_128}",
                ".word {skipped_800}",
                ".word {bgez_taken}",
                ".word {delay_256}",
                ".word {skipped_1000}",
                clear = const mips_i(0x09, 0, 10, 0),
                beq_taken = const mips_i(0x04, 8, 8, 2),
                delay_1 = const mips_i(0x0D, 10, 10, 1),
                skipped_100 = const mips_i(0x0D, 10, 10, 0x0100),
                bne_not_taken = const mips_i(0x05, 8, 8, 1),
                delay_2 = const mips_i(0x0D, 10, 10, 2),
                fallthrough_4 = const mips_i(0x0D, 10, 10, 4),
                beq_always = const mips_i(0x04, 0, 0, 2),
                delay_8 = const mips_i(0x0D, 10, 10, 8),
                skipped_200 = const mips_i(0x0D, 10, 10, 0x0200),
                set_neg = const mips_i(0x09, 0, 11, 0xFFFF),
                blez_taken = const mips_i(0x06, 11, 0, 2),
                delay_16 = const mips_i(0x0D, 10, 10, 16),
                skipped_400 = const mips_i(0x0D, 10, 10, 0x0400),
                bgtz_not_taken = const mips_i(0x07, 0, 0, 1),
                delay_32 = const mips_i(0x0D, 10, 10, 32),
                fallthrough_64 = const mips_i(0x0D, 10, 10, 64),
                bltz_taken = const mips_i(0x01, 11, 0, 2),
                delay_128 = const mips_i(0x0D, 10, 10, 128),
                skipped_800 = const mips_i(0x0D, 10, 10, 0x0800),
                bgez_taken = const mips_i(0x01, 0, 1, 2),
                delay_256 = const mips_i(0x0D, 10, 10, 256),
                skipped_1000 = const mips_i(0x0D, 10, 10, 0x1000),
                in("$8") 1u32,
                lateout("$10") out,
                lateout("$11") _,
                options(nostack, nomem, preserves_flags),
            );
        }
        out
    }
    #[cfg(not(target_arch = "mips"))]
    {
        0x1FF
    }
}

#[repr(align(4))]
pub(super) struct AlignedBytes([u8; 16]);

#[inline(never)]
pub(super) fn cpu_load_store_battery() -> u32 {
    static mut BUF: AlignedBytes = AlignedBytes([0; 16]);
    #[cfg(target_arch = "mips")]
    unsafe {
        let base = (&raw mut BUF.0) as *mut u8;
        for i in 0..16 {
            ptr::write_volatile(base.add(i), 0);
        }
        ptr::write_volatile(base.add(8) as *mut u32, 0xA5A5_1357);
        let lw: u32;
        let lh: u32;
        let lhu: u32;
        let lb: u32;
        let lbu: u32;
        let delayed: u32;
        let loaded_after_delay: u32;
        core::arch::asm!(
            ".word {sw}",
            ".word {lw}",
            ".word {sh}",
            ".word {lh}",
            ".word {lhu}",
            ".word {sb}",
            ".word {lb}",
            ".word {lbu}",
            ".word 0",
            sw = const mips_i(0x2B, 8, 9, 0),
            lw = const mips_i(0x23, 8, 10, 0),
            sh = const mips_i(0x29, 8, 11, 4),
            lh = const mips_i(0x21, 8, 12, 4),
            lhu = const mips_i(0x25, 8, 13, 4),
            sb = const mips_i(0x28, 8, 14, 6),
            lb = const mips_i(0x20, 8, 15, 6),
            lbu = const mips_i(0x24, 8, 24, 6),
            in("$8") base as u32,
            in("$9") 0x1234_5678u32,
            in("$11") 0xFFFF_80FEu32,
            in("$14") 0x0000_00F2u32,
            lateout("$10") lw,
            lateout("$12") lh,
            lateout("$13") lhu,
            lateout("$15") lb,
            lateout("$24") lbu,
            options(nostack, preserves_flags),
        );
        core::arch::asm!(
            ".word {set_old}",
            ".word {lw_delay}",
            ".word {capture_delay_slot}",
            ".word 0",
            set_old = const mips_i(0x09, 0, 10, 5),
            lw_delay = const mips_i(0x23, 8, 10, 8),
            capture_delay_slot = const mips_r(10, 0, 25, 0, 0x21),
            in("$8") base as u32,
            lateout("$10") loaded_after_delay,
            lateout("$25") delayed,
            options(nostack, preserves_flags),
        );
        let mut observed = cpu_load_store_observed(base, lw, lh, lhu, lb, lbu);
        if delayed == 5 && loaded_after_delay == 0xA5A5_1357 {
            observed |= 1 << 8;
        }
        observed
    }
    #[cfg(not(target_arch = "mips"))]
    unsafe {
        let base = (&raw mut BUF.0) as *mut u8;
        ptr::write_volatile(base as *mut u32, 0x1234_5678);
        ptr::write_volatile(base.add(4) as *mut u16, 0x80FE);
        ptr::write_volatile(base.add(6), 0xF2);
        cpu_load_store_observed(
            base,
            0x1234_5678,
            0xFFFF_80FE,
            0x0000_80FE,
            0xFFFF_FFF2,
            0xF2,
        ) | (1 << 8)
    }
}

unsafe fn cpu_load_store_observed(
    base: *mut u8,
    lw: u32,
    lh: u32,
    lhu: u32,
    lb: u32,
    lbu: u32,
) -> u32 {
    let mut observed = 0u32;
    if lw == 0x1234_5678 {
        observed |= 1 << 0;
    }
    if ptr::read_volatile(base.add(0)) == 0x78 && ptr::read_volatile(base.add(3)) == 0x12 {
        observed |= 1 << 1;
    }
    if lh == 0xFFFF_80FE {
        observed |= 1 << 2;
    }
    if lhu == 0x0000_80FE {
        observed |= 1 << 3;
    }
    if ptr::read_volatile(base.add(4)) == 0xFE && ptr::read_volatile(base.add(5)) == 0x80 {
        observed |= 1 << 4;
    }
    if lb == 0xFFFF_FFF2 {
        observed |= 1 << 5;
    }
    if lbu == 0x0000_00F2 {
        observed |= 1 << 6;
    }
    if ptr::read_volatile(base.add(6)) == 0xF2 {
        observed |= 1 << 7;
    }
    observed
}

#[inline(never)]
pub(super) fn cpu_unaligned_load_store_battery() -> u32 {
    static mut BUF: AlignedBytes = AlignedBytes([0; 16]);

    #[cfg(target_arch = "mips")]
    unsafe {
        let base = (&raw mut BUF.0) as *mut u8;
        const SEED: [u8; 16] = [
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0xEE, 0xEE, 0xEE, 0xEE, 0xEE, 0xEE,
            0xEE, 0xEE,
        ];
        for (index, byte) in SEED.iter().enumerate() {
            ptr::write_volatile(base.add(index), *byte);
        }

        let loaded: u32;
        core::arch::asm!(
            ".word {set_old}",
            ".word {lwl}",
            ".word 0",
            ".word {lwr}",
            ".word 0",
            set_old = const mips_i(0x0F, 0, 10, 0xDEAD),
            lwl = const mips_i(0x22, 8, 10, 4),
            lwr = const mips_i(0x26, 8, 10, 1),
            in("$8") base as u32,
            lateout("$10") loaded,
            options(nostack, preserves_flags),
        );

        core::arch::asm!(
            ".word {swl}",
            ".word {swr}",
            swl = const mips_i(0x2A, 8, 9, 12),
            swr = const mips_i(0x2E, 8, 9, 9),
            in("$8") base as u32,
            in("$9") 0xAABB_CCDDu32,
            options(nostack, preserves_flags),
        );

        cpu_unaligned_observed(base, loaded)
    }

    #[cfg(not(target_arch = "mips"))]
    unsafe {
        let base = (&raw mut BUF.0) as *mut u8;
        const SEED: [u8; 16] = [
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0xEE, 0xEE, 0xEE, 0xEE, 0xEE, 0xEE,
            0xEE, 0xEE,
        ];
        for (index, byte) in SEED.iter().enumerate() {
            ptr::write_volatile(base.add(index), *byte);
        }
        ptr::write_volatile(base.add(9), 0xDD);
        ptr::write_volatile(base.add(10), 0xCC);
        ptr::write_volatile(base.add(11), 0xBB);
        ptr::write_volatile(base.add(12), 0xAA);
        cpu_unaligned_observed(base, 0x5544_3322)
    }
}

unsafe fn cpu_unaligned_observed(base: *mut u8, loaded: u32) -> u32 {
    let mut observed = 0u32;
    if loaded == 0x5544_3322 {
        observed |= 1 << 0;
    }
    if ptr::read_volatile(base.add(0)) == 0x11 {
        observed |= 1 << 1;
    }
    if ptr::read_volatile(base.add(8)) == 0xEE {
        observed |= 1 << 2;
    }
    if ptr::read_volatile(base.add(9)) == 0xDD {
        observed |= 1 << 3;
    }
    if ptr::read_volatile(base.add(10)) == 0xCC {
        observed |= 1 << 4;
    }
    if ptr::read_volatile(base.add(11)) == 0xBB {
        observed |= 1 << 5;
    }
    if ptr::read_volatile(base.add(12)) == 0xAA {
        observed |= 1 << 6;
    }
    if ptr::read_volatile(base.add(13)) == 0xEE {
        observed |= 1 << 7;
    }
    observed
}

#[cfg(not(target_arch = "mips"))]
pub(super) fn emulate_cpu_r(instr: u32, rs_value: u32, rt_value: u32) -> u32 {
    let shamt = (instr >> 6) & 0x1F;
    match instr & 0x3F {
        0x00 => rt_value << shamt,
        0x02 => rt_value >> shamt,
        0x03 => ((rt_value as i32) >> shamt) as u32,
        0x04 => rt_value << (rs_value & 0x1F),
        0x06 => rt_value >> (rs_value & 0x1F),
        0x07 => ((rt_value as i32) >> (rs_value & 0x1F)) as u32,
        0x21 => rs_value.wrapping_add(rt_value),
        0x23 => rs_value.wrapping_sub(rt_value),
        0x24 => rs_value & rt_value,
        0x25 => rs_value | rt_value,
        0x26 => rs_value ^ rt_value,
        0x27 => !(rs_value | rt_value),
        0x2A => ((rs_value as i32) < (rt_value as i32)) as u32,
        0x2B => (rs_value < rt_value) as u32,
        _ => 0,
    }
}

#[cfg(not(target_arch = "mips"))]
pub(super) fn emulate_cpu_i(instr: u32, rs_value: u32) -> u32 {
    let imm = instr as u16;
    match (instr >> 26) & 0x3F {
        0x09 => rs_value.wrapping_add((imm as i16 as i32) as u32),
        0x0A => ((rs_value as i32) < (imm as i16 as i32)) as u32,
        0x0B => (rs_value < (imm as i16 as i32 as u32)) as u32,
        0x0C => rs_value & imm as u32,
        0x0D => rs_value | imm as u32,
        0x0E => rs_value ^ imm as u32,
        0x0F => (imm as u32) << 16,
        _ => 0,
    }
}

#[cfg(not(target_arch = "mips"))]
pub(super) fn emulate_cpu_hilo(instr: u32, rs_value: u32, rt_value: u32) -> (u32, u32) {
    match instr & 0x3F {
        0x18 => {
            let value = (rs_value as i32 as i64).wrapping_mul(rt_value as i32 as i64);
            (value as u32, (value >> 32) as u32)
        }
        0x19 => {
            let value = (rs_value as u64).wrapping_mul(rt_value as u64);
            (value as u32, (value >> 32) as u32)
        }
        0x1A => {
            let a = rs_value as i32;
            let b = rt_value as i32;
            ((a / b) as u32, (a % b) as u32)
        }
        0x1B => (rs_value / rt_value, rs_value % rt_value),
        _ => (0, 0),
    }
}
