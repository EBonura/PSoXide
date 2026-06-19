//! CPU / COP0 register viewer.
//!
//! Left-docked side panel showing:
//! - GPRs in a 2-column grid (named + hex value)
//! - PC / HI / LO
//! - COP0: SR, Cause, EPC, BadVAddr (the registers we actually touch)
//! - Retired instruction count
//! - Execution history (newest last)
//!
//! Layout + grouping mirrors PSoXide-2's `debug_pane_contents`, using
//! the themed `section` helper so each group reads as a framed block.

use std::collections::{BTreeSet, VecDeque};

use emulator_core::Cpu;
use psx_trace::InstructionRecord;

use crate::theme;

/// Canonical MIPS GPR names, indexed 0..=31.
const GPR_NAMES: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6",
    "t7", "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp",
    "ra",
];

/// R3000 exception codes, by their numeric value in `CAUSE.ExcCode`.
const EXC_CODES: &[(u32, &str)] = &[
    (0, "Int"),
    (4, "AdEL"),
    (5, "AdES"),
    (6, "IBE"),
    (7, "DBE"),
    (8, "Syscall"),
    (9, "Bp"),
    (10, "RI"),
    (11, "CpU"),
    (12, "Ov"),
];

/// Paint the register viewer inside an existing container.
///
/// Flat layout: small accent subheads instead of stacked framed blocks
/// (the section already sits inside the sidebar's frame; more nesting
/// was chrome, not information).
pub fn draw_contents(
    ui: &mut egui::Ui,
    cpu: &Cpu,
    history: &VecDeque<InstructionRecord>,
    breakpoints: &mut BTreeSet<u32>,
    snapshot: &mut Option<[u32; 32]>,
) {
    theme::subhead(ui, "GPR");
    draw_gprs(ui, cpu, snapshot);
    theme::subhead(ui, "CPU State");
    draw_cpu_state(ui, cpu);
    theme::subhead(ui, "Breakpoints");
    draw_breakpoints(ui, breakpoints);
    theme::subhead(ui, "Execution History");
    draw_history(ui, history);
}

fn draw_breakpoints(ui: &mut egui::Ui, breakpoints: &mut BTreeSet<u32>) {
    if breakpoints.is_empty() {
        ui.monospace("(none - set from the memory panel)");
        return;
    }
    // Collect first so we can mutate the set while iterating.
    let addrs: Vec<u32> = breakpoints.iter().copied().collect();
    for addr in addrs {
        ui.horizontal(|ui| {
            ui.monospace(format!("{addr:08X}"));
            if ui.small_button("×").on_hover_text("Remove").clicked() {
                breakpoints.remove(&addr);
            }
        });
    }
}

fn draw_history(ui: &mut egui::Ui, history: &VecDeque<InstructionRecord>) {
    if history.is_empty() {
        ui.monospace("(empty - step or run the CPU)");
        return;
    }
    // Newest at the bottom: log-style reading order, capped to a fixed
    // height with its own scroll so 64 rows don't swallow the sidebar.
    egui::ScrollArea::vertical()
        .id_salt("exec-history")
        .max_height(150.0)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for record in history {
                let mnem = crate::disasm::disasm(record.pc, record.instr);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{:08X}  {mnem}", record.pc)).monospace(),
                    )
                    .truncate(),
                );
            }
        });
}

fn draw_gprs(ui: &mut egui::Ui, cpu: &Cpu, snapshot: &mut Option<[u32; 32]>) {
    let gprs = cpu.gprs();

    ui.horizontal(|ui| {
        if ui
            .button("Snapshot")
            .on_hover_text("Freeze current GPR values")
            .clicked()
        {
            *snapshot = Some(*gprs);
        }
        if snapshot.is_some() && ui.button("Clear").clicked() {
            *snapshot = None;
        }
        if let Some(_snap) = snapshot {
            ui.label("(changed GPRs shown in accent color)");
        }
    });

    // Reflow the 32 registers to as many columns as the current panel
    // width fits (1/2/4), instead of a hardcoded two-column grid that
    // clips when narrow and wastes space when wide. One cell is
    // "name=XXXXXXXX" (13 monospace chars) plus grid spacing.
    let glyph_w =
        ui.fonts(|fonts| fonts.glyph_width(&egui::FontId::monospace(theme::FONT_SIZE_MONO), '0'));
    let col_stride = 13.0 * glyph_w + 12.0;
    let cols = ((ui.available_width() / col_stride).floor() as usize).clamp(1, 4);
    // Fill column-major so register order reads down each column.
    let rows = 32usize.div_ceil(cols);
    egui::Grid::new("gprs")
        .num_columns(cols)
        .spacing(egui::vec2(12.0, 2.0))
        .show(ui, |ui| {
            for row in 0..rows {
                for col in 0..cols {
                    let i = col * rows + row;
                    if i < 32 {
                        reg_cell_diff(ui, GPR_NAMES[i], gprs[i], snapshot.map(|s| s[i]));
                    }
                }
                ui.end_row();
            }
        });
}

fn reg_cell_diff(ui: &mut egui::Ui, name: &str, value: u32, snap: Option<u32>) {
    let changed = snap.is_some_and(|s| s != value);
    let text = format!("{name:>4}={value:08X}");
    if changed {
        ui.monospace(egui::RichText::new(text).color(theme::ACCENT));
    } else {
        ui.monospace(text);
    }
}

/// PC / HI / LO / retired tick + the COP0 registers we model, as one
/// compact group laid out with the same width-reflow as the GPRs.
fn draw_cpu_state(ui: &mut egui::Ui, cpu: &Cpu) {
    let cop0 = cpu.cop0();
    let cells: [(&str, u32); 7] = [
        ("PC", cpu.pc()),
        ("HI", cpu.hi()),
        ("LO", cpu.lo()),
        ("SR", cop0[12]),
        ("Cause", cop0[13]),
        ("EPC", cop0[14]),
        ("BadVA", cop0[8]),
    ];
    let glyph_w =
        ui.fonts(|fonts| fonts.glyph_width(&egui::FontId::monospace(theme::FONT_SIZE_MONO), '0'));
    let col_stride = 14.0 * glyph_w + 12.0;
    let cols = ((ui.available_width() / col_stride).floor() as usize).clamp(1, 4);
    egui::Grid::new("cpu_state")
        .num_columns(cols)
        .spacing(egui::vec2(12.0, 2.0))
        .show(ui, |ui| {
            for (i, (name, value)) in cells.iter().enumerate() {
                reg_cell(ui, name, *value);
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }
            ui.end_row();
        });

    ui.monospace(format!("tick={}", cpu.tick()));

    // Bit-level breakdowns for the two registers whose hex values are
    // opaque at a glance. BadVAddr / EPC are raw addresses; they don't
    // benefit from the same treatment.
    ui.add_space(2.0);
    ui.small(format!("SR: {}", format_sr_bits(cop0[12])));
    ui.small(format!("Cause: {}", format_cause_bits(cop0[13])));
}

fn format_sr_bits(sr: u32) -> String {
    // Bits we actually care about at a glance. Flags show by name only
    // when set; the KU/IE stack shows as three comma-joined pairs.
    let mut flags: Vec<&str> = Vec::new();
    for (bit, name) in [
        (16, "IsC"),
        (17, "SwC"),
        (22, "BEV"),
        (28, "CU0"),
        (30, "CU2"),
    ] {
        if sr & (1 << bit) != 0 {
            flags.push(name);
        }
    }
    let stack = format!(
        "c={ku_c}/{ie_c} p={ku_p}/{ie_p} o={ku_o}/{ie_o}",
        ie_c = sr & 1,
        ku_c = (sr >> 1) & 1,
        ie_p = (sr >> 2) & 1,
        ku_p = (sr >> 3) & 1,
        ie_o = (sr >> 4) & 1,
        ku_o = (sr >> 5) & 1,
    );
    let im = (sr >> 8) & 0xFF;
    let flags_str = if flags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", flags.join(" "))
    };
    format!("{stack}  IM=0x{im:02X}{flags_str}")
}

fn format_cause_bits(cause: u32) -> String {
    let exc_code = (cause >> 2) & 0x1F;
    let ip = (cause >> 8) & 0xFF;
    let bd = if cause & (1 << 31) != 0 { " BD" } else { "" };
    let exc_name = EXC_CODES
        .iter()
        .find_map(|(c, name)| if *c == exc_code { Some(*name) } else { None })
        .unwrap_or("?");
    format!("ExcCode={exc_code} ({exc_name})  IP=0x{ip:02X}{bd}")
}

fn reg_cell(ui: &mut egui::Ui, name: &str, value: u32) {
    ui.monospace(format!("{name:>4}={value:08X}"));
}
