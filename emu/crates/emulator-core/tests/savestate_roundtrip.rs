//! Regression test for the save-state feature: run a few real
//! instructions, snapshot the emulator, round-trip it through the
//! on-disk `SaveStateV1` binary format entirely in memory, restore
//! into a fresh `Cpu`/`Bus`, and assert the restored state matches
//! bit-for-bit. No BIOS or disc image required -- the "program" run
//! here is three hand-assembled MIPS instructions baked into a
//! synthetic zeroed BIOS image, just enough to touch a GPR, RAM, and
//! the tick counter so the test isn't just checking an all-zero
//! no-op.

use emulator_core::snapshot::{EmulatorState, EmulatorStateRef};
use emulator_core::{Bus, Cpu};
use psoxide_settings::savestate::SaveStateV1;
use psx_hw::memory;

/// Hand-assemble `lui $t0, 0x1234` / `ori $t0, $t0, 0x5678` /
/// `sw $t0, 0($zero)` at the BIOS reset vector, little-endian (R3000A
/// is LE). Leaves the rest of the BIOS image zeroed, which decodes as
/// `sll $zero, $zero, 0` (a true hardware NOP) -- so stepping past the
/// three real instructions is harmless.
fn synthetic_bios() -> Vec<u8> {
    let mut bios = vec![0u8; memory::bios::SIZE];
    let words: [u32; 3] = [
        0x3C08_1234, // lui  $t0, 0x1234
        0x3508_5678, // ori  $t0, $t0, 0x5678
        0xAC08_0000, // sw   $t0, 0($zero)
    ];
    for (i, word) in words.iter().enumerate() {
        bios[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    bios
}

fn step_n(cpu: &mut Cpu, bus: &mut Bus, n: usize) {
    for _ in 0..n {
        cpu.step(bus).expect("synthetic program must not fault");
    }
}

#[test]
fn save_state_round_trips_cpu_and_bus_state() {
    let mut cpu = Cpu::new();
    let mut bus = Bus::new(synthetic_bios()).expect("bios is exactly BIOS::SIZE bytes");

    // lui + ori + sw + a couple of harmless zero-word NOPs so the
    // tick counter and PC have moved past the "interesting" bytes.
    step_n(&mut cpu, &mut bus, 5);

    // Sanity check the program actually did something before we
    // trust the round-trip assertions below.
    assert_eq!(
        cpu.gpr(8),
        0x1234_5678,
        "t0 should hold the assembled constant"
    );
    assert_eq!(
        bus.read32(0),
        0x1234_5678,
        "sw should have landed in RAM at address 0"
    );
    let tick_before = cpu.tick();
    assert!(tick_before >= 5);

    // Touch a bit of VRAM directly too, so the round-trip covers the
    // big-array (de)serialize path (`Vram::data`), not just plain
    // integer fields.
    bus.gpu.vram.words_mut()[0] = 0xBEEF;
    bus.gpu.vram.words_mut()[1024 * 511 + 1023] = 0xCAFE; // last pixel

    // Pending CD samples are still emulator input: once consumed they
    // update capture buffers in SPU RAM. Losing the queue would make a
    // restored run diverge even though the host-facing output queue is
    // intentionally discarded.
    bus.spu.feed_cd_audio(&[(1_234, -1_234), (5_678, -5_678)]);

    let snapshot = EmulatorStateRef {
        cpu: &cpu,
        bus: &bus,
    };
    let state = SaveStateV1::new(snapshot, "test-game-id", cpu.tick());
    let bytes = state.to_bytes().expect("in-memory encode must succeed");

    let loaded: SaveStateV1<EmulatorState> =
        SaveStateV1::from_bytes(&bytes).expect("in-memory decode must succeed");

    assert_eq!(loaded.header.game_id, "test-game-id");
    assert_eq!(loaded.header.cpu_tick, tick_before);

    let mut restored_cpu = loaded.payload.cpu;
    let mut restored_bus = loaded.payload.bus;

    assert_eq!(restored_cpu.pc(), cpu.pc());
    assert_eq!(restored_cpu.tick(), tick_before);
    for i in 0..32 {
        assert_eq!(
            restored_cpu.gpr(i),
            cpu.gpr(i),
            "gpr {i} mismatch after restore"
        );
    }
    assert_eq!(restored_bus.read32(0), 0x1234_5678);
    assert_eq!(restored_bus.gpu.vram.words()[0], 0xBEEF);
    assert_eq!(restored_bus.gpu.vram.words()[1024 * 511 + 1023], 0xCAFE);
    assert_eq!(restored_bus.spu.cd_audio_queue_len(), 2);

    // The restored state must also keep running: step both the
    // original and the restored copy the same way and confirm they
    // still agree, proving this isn't just a data-only round-trip but
    // a live, resumable `Cpu`/`Bus` pair.
    step_n(&mut cpu, &mut bus, 3);
    step_n(&mut restored_cpu, &mut restored_bus, 3);
    assert_eq!(restored_cpu.pc(), cpu.pc());
    assert_eq!(restored_cpu.tick(), cpu.tick());
    assert_eq!(restored_bus.read32(0), bus.read32(0));
}
