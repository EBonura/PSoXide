//! Memory viewer panel -- hex+ASCII dump of a 1 KiB window anchored
//! at a user-selectable address.
//!
//! Quick-jump buttons land the window at the canonical entry points
//! for each region (RAM, scratchpad, MMIO, BIOS) and at the current
//! PC. Unmapped rows render as `--` so the viewer doesn't panic when
//! the user scrolls past the end of a region.

use std::collections::BTreeSet;

use emulator_core::{Bus, Cpu};

use crate::disasm;
use crate::theme;

const BYTES_PER_ROW: usize = 16;
const ROWS: usize = 64;
const WINDOW_SIZE: u32 = (BYTES_PER_ROW * ROWS) as u32;
/// In disasm mode: one row per instruction (4 bytes).
const DISASM_ROWS: usize = 64;

/// Which format the memory panel is displaying.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ViewMode {
    /// Hex+ASCII dump at 16 bytes/row.
    Hex,
    /// Mnemonic-per-row disassembly at 4 bytes/row. Each row also
    /// echoes the raw instruction word on the right so you can still
    /// cross-check opcodes by eye.
    Disasm,
}

/// Mutable view-state the panel owns.
pub struct MemoryView {
    pub addr: u32,
    pub mode: ViewMode,
    /// Address input as a string -- kept separately so partial typing
    /// ("0x8001_") doesn't immediately clobber the numeric anchor.
    addr_input: String,
}

impl Default for MemoryView {
    fn default() -> Self {
        Self {
            addr: 0x8000_0000,
            mode: ViewMode::Hex,
            addr_input: "80000000".into(),
        }
    }
}

impl MemoryView {
    /// Move the viewer to `addr` and sync the text field. Alignment
    /// depends on the current mode -- 16-byte rows in hex, 4-byte rows
    /// in disasm.
    pub fn jump_to(&mut self, addr: u32) {
        let mask = match self.mode {
            ViewMode::Hex => !0x0F,
            ViewMode::Disasm => !0x03,
        };
        self.addr = addr & mask;
        self.addr_input = format!("{:08X}", self.addr);
    }
}

/// Paint the memory viewer inside an existing container.
pub fn draw_contents(
    ui: &mut egui::Ui,
    view: &mut MemoryView,
    bus: Option<&Bus>,
    cpu: &Cpu,
    breakpoints: &mut BTreeSet<u32>,
) {
    draw_header(ui, view, cpu, breakpoints);
    ui.separator();
    match view.mode {
        ViewMode::Hex => draw_hex_dump(ui, view, bus, breakpoints, cpu.pc()),
        ViewMode::Disasm => draw_disasm(ui, view, bus, breakpoints, cpu.pc()),
    }
}

fn draw_header(
    ui: &mut egui::Ui,
    view: &mut MemoryView,
    cpu: &Cpu,
    breakpoints: &mut BTreeSet<u32>,
) {
    ui.horizontal(|ui| {
        ui.label("addr");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut view.addr_input)
                .desired_width(80.0)
                .font(egui::TextStyle::Monospace),
        );
        if resp.lost_focus() && resp.ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            apply_addr_input(view);
        }
    });

    ui.horizontal_wrapped(|ui| {
        if ui.button("RAM").clicked() {
            view.jump_to(0x8000_0000);
        }
        if ui.button("Scratchpad").clicked() {
            view.jump_to(0x1F80_0000);
        }
        if ui.button("MMIO").clicked() {
            view.jump_to(0x1F80_1000);
        }
        if ui.button("BIOS").clicked() {
            view.jump_to(0xBFC0_0000);
        }
        if ui.button("PC").clicked() {
            view.jump_to(cpu.pc());
        }
    });

    ui.horizontal(|ui| {
        let step = match view.mode {
            ViewMode::Hex => 256,
            ViewMode::Disasm => 64, // 16 instructions
        };
        if ui.button(format!("◀ -{step}")).clicked() {
            view.addr = view.addr.wrapping_sub(step);
            view.addr_input = format!("{:08X}", view.addr);
        }
        if ui.button(format!("+{step} ▶")).clicked() {
            view.addr = view.addr.wrapping_add(step);
            view.addr_input = format!("{:08X}", view.addr);
        }
        let bp_label = if breakpoints.contains(&view.addr) {
            "Clear BP"
        } else {
            "Set BP"
        };
        if ui.button(bp_label).clicked() && !breakpoints.remove(&view.addr) {
            breakpoints.insert(view.addr);
        }
    });

    // Mode toggle -- separate row so it doesn't fight for space with
    // the nav buttons.
    ui.horizontal(|ui| {
        ui.radio_value(&mut view.mode, ViewMode::Hex, "Hex");
        ui.radio_value(&mut view.mode, ViewMode::Disasm, "Disasm");
    });
}

fn apply_addr_input(view: &mut MemoryView) {
    let s = view.addr_input.trim_start_matches("0x").trim();
    if let Ok(a) = u32::from_str_radix(s, 16) {
        view.addr = a & !0x0F;
        view.addr_input = format!("{:08X}", view.addr);
    }
}

fn draw_hex_dump(
    ui: &mut egui::Ui,
    view: &MemoryView,
    bus: Option<&Bus>,
    breakpoints: &BTreeSet<u32>,
    pc: u32,
) {
    let Some(bus) = bus else {
        ui.monospace("(no BIOS loaded — Bus unavailable)");
        return;
    };

    // Adapt bytes-per-row to the panel width (16 / 8 / 4) instead of
    // clipping a fixed 78-char line: a hex row costs
    // marker(2) + addr(8) + gap(2) + 3*b hex + mid-gap(1) + gap(1) + b ascii.
    let glyph_w = ui.fonts(|fonts| {
        fonts.glyph_width(&egui::FontId::monospace(theme::FONT_SIZE_MONO), '0')
    });
    let max_chars = (ui.available_width() / glyph_w.max(1.0)) as usize;
    let bytes_per_row = if max_chars >= 14 + 4 * 16 {
        16
    } else if max_chars >= 14 + 4 * 8 {
        8
    } else {
        4
    };
    // The same total window, repartitioned by the adaptive row width.
    let rows = (WINDOW_SIZE as usize) / bytes_per_row;

    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for row in 0..rows {
                let row_addr = view.addr.wrapping_add((row * bytes_per_row) as u32);
                let has_bp = row_has_breakpoint(row_addr, breakpoints, bytes_per_row);
                let has_pc = row_contains(row_addr, pc, bytes_per_row);
                let text = format_row(bus, row_addr, has_bp, has_pc, bytes_per_row);

                let color = match (has_pc, has_bp) {
                    // PC wins over BP -- the arrow marker is the one we
                    // most want to eyeball.
                    (true, _) => Some(egui::Color32::from_rgb(80, 200, 120)),
                    (false, true) => Some(theme::ACCENT),
                    (false, false) => None,
                };
                match color {
                    Some(c) => ui.monospace(egui::RichText::new(text).color(c)),
                    None => ui.monospace(text),
                };
                if row_addr.wrapping_add(bytes_per_row as u32) < row_addr {
                    break;
                }
            }
        });
}

fn row_has_breakpoint(base: u32, breakpoints: &BTreeSet<u32>, bytes_per_row: usize) -> bool {
    for i in 0..bytes_per_row as u32 {
        if breakpoints.contains(&base.wrapping_add(i)) {
            return true;
        }
    }
    false
}

fn row_contains(base: u32, addr: u32, bytes_per_row: usize) -> bool {
    addr.wrapping_sub(base) < bytes_per_row as u32
}

fn draw_disasm(
    ui: &mut egui::Ui,
    view: &MemoryView,
    bus: Option<&Bus>,
    breakpoints: &BTreeSet<u32>,
    pc: u32,
) {
    let Some(bus) = bus else {
        ui.monospace("(no BIOS loaded — Bus unavailable)");
        return;
    };

    egui::ScrollArea::vertical()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for row in 0..DISASM_ROWS {
                let addr = view.addr.wrapping_add(row as u32 * 4);
                let instr = read_instr(bus, addr);
                let has_bp = breakpoints.contains(&addr);
                let has_pc = addr == pc;

                let marker = match (has_pc, has_bp) {
                    (true, _) => '▸',
                    (false, true) => '●',
                    (false, false) => ' ',
                };
                let text = match instr {
                    Some(w) => format!("{marker} {addr:08X}  {w:08X}  {}", disasm::disasm(addr, w)),
                    None => format!("{marker} {addr:08X}  --------  (unmapped)"),
                };

                let color = match (has_pc, has_bp) {
                    (true, _) => egui::Color32::from_rgb(80, 200, 120),
                    (false, true) => theme::ACCENT,
                    (false, false) => theme::TEXT,
                };
                // Truncate gracefully instead of clipping mid-glyph when
                // the sidebar is narrower than the longest mnemonic line.
                ui.add(
                    egui::Label::new(egui::RichText::new(text).monospace().color(color))
                        .truncate(),
                );
            }
        });
}

fn read_instr(bus: &Bus, addr: u32) -> Option<u32> {
    let b0 = bus.try_read8(addr)?;
    let b1 = bus.try_read8(addr.wrapping_add(1))?;
    let b2 = bus.try_read8(addr.wrapping_add(2))?;
    let b3 = bus.try_read8(addr.wrapping_add(3))?;
    Some(u32::from_le_bytes([b0, b1, b2, b3]))
}

fn format_row(bus: &Bus, base: u32, has_bp: bool, has_pc: bool, bytes_per_row: usize) -> String {
    // Markers: `▸` for PC row, `●` for breakpoint row. PC wins since
    // knowing where execution is now is more urgent than which
    // addresses we've decided to stop on.
    let marker = match (has_pc, has_bp) {
        (true, _) => '▸',
        (false, true) => '●',
        (false, false) => ' ',
    };
    let mut out = format!("{marker} {base:08X}  ");
    let mut ascii = String::with_capacity(bytes_per_row);

    for i in 0..bytes_per_row {
        let addr = base.wrapping_add(i as u32);
        match bus.try_read8(addr) {
            Some(b) => {
                out.push_str(&format!("{b:02X} "));
                ascii.push(printable_ascii(b));
            }
            None => {
                out.push_str("-- ");
                ascii.push('.');
            }
        }
        // Mid-row visual gap at the halfway point (16- and 8-wide rows).
        if bytes_per_row >= 8 && i == bytes_per_row / 2 - 1 {
            out.push(' ');
        }
    }
    out.push(' ');
    out.push_str(&ascii);
    out
}

fn printable_ascii(b: u8) -> char {
    if (0x20..0x7F).contains(&b) {
        b as char
    } else {
        '.'
    }
}
