//! Memory viewer panel -- hex+ASCII, visual map, or disassembly anchored
//! at a user-selectable address.
//!
//! Quick-jump buttons land the window at the canonical entry points
//! for each region (RAM, scratchpad, MMIO, BIOS) and at the current
//! PC. Unmapped rows render as `--` so the viewer doesn't panic when
//! the user scrolls past the end of a region.

use std::collections::BTreeSet;
use std::ops::Range;

use egui::{Color32, Stroke, StrokeKind};
use emulator_core::{Bus, Cpu};

use crate::disasm;
use crate::theme;

const BYTES_PER_ROW: usize = 16;
const ROWS: usize = 64;
const WINDOW_SIZE: u32 = (BYTES_PER_ROW * ROWS) as u32;
const RAM_CANONICAL_BASE: u32 = 0x8000_0000;
const RAM_OVERVIEW_BYTES_PER_CELL: usize = 64;
/// In disasm mode: one row per instruction (4 bytes).
const DISASM_ROWS: usize = 64;

/// Which format the memory panel is displaying.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ViewMode {
    /// Hex+ASCII dump at 16 bytes/row.
    Hex,
    /// Color-map view: full RAM overview for RAM, local window elsewhere.
    Visual,
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
            mode: ViewMode::Visual,
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
            ViewMode::Hex | ViewMode::Visual => !0x0F,
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
        ViewMode::Visual => draw_visual_map(ui, view, bus, breakpoints, cpu.pc()),
        ViewMode::Disasm => draw_disasm(ui, view, bus, breakpoints, cpu.pc()),
    }
}

fn draw_header(
    ui: &mut egui::Ui,
    view: &mut MemoryView,
    cpu: &Cpu,
    breakpoints: &mut BTreeSet<u32>,
) {
    // Two compact rows instead of four stacked strips:
    //   1) address input + page nav + breakpoint toggle
    //   2) region quick-jumps + view mode (wraps as a unit when narrow)
    ui.horizontal(|ui| {
        let resp = ui.add(
            egui::TextEdit::singleline(&mut view.addr_input)
                .desired_width(78.0)
                .font(egui::TextStyle::Monospace),
        );
        if resp.lost_focus() && resp.ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            apply_addr_input(view);
        }
        let step = match view.mode {
            ViewMode::Hex | ViewMode::Visual => 256,
            ViewMode::Disasm => 64, // 16 instructions
        };
        if ui
            .small_button("◀")
            .on_hover_text(format!("-{step}"))
            .clicked()
        {
            view.addr = view.addr.wrapping_sub(step);
            view.addr_input = format!("{:08X}", view.addr);
        }
        if ui
            .small_button("▶")
            .on_hover_text(format!("+{step}"))
            .clicked()
        {
            view.addr = view.addr.wrapping_add(step);
            view.addr_input = format!("{:08X}", view.addr);
        }
        let bp_label = if breakpoints.contains(&view.addr) {
            "Clear BP"
        } else {
            "Set BP"
        };
        if ui.small_button(bp_label).clicked() && !breakpoints.remove(&view.addr) {
            breakpoints.insert(view.addr);
        }
    });

    ui.horizontal_wrapped(|ui| {
        for (label, target) in [
            ("RAM", Some(0x8000_0000)),
            ("Scratch", Some(0x1F80_0000)),
            ("MMIO", Some(0x1F80_1000)),
            ("BIOS", Some(0xBFC0_0000)),
            ("PC", None),
        ] {
            if ui.small_button(label).clicked() {
                view.jump_to(target.unwrap_or_else(|| cpu.pc()));
            }
        }
        ui.separator();
        ui.radio_value(&mut view.mode, ViewMode::Hex, "Hex");
        ui.radio_value(&mut view.mode, ViewMode::Visual, "Visual");
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
        ui.monospace("(no BIOS loaded - Bus unavailable)");
        return;
    };

    // Adapt bytes-per-row to the panel width (16 / 8 / 4) instead of
    // clipping a fixed 78-char line: a hex row costs
    // marker(2) + addr(8) + gap(2) + 3*b hex + mid-gap(1) + gap(1) + b ascii.
    let glyph_w =
        ui.fonts(|fonts| fonts.glyph_width(&egui::FontId::monospace(theme::FONT_SIZE_MONO), '0'));
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

fn draw_visual_map(
    ui: &mut egui::Ui,
    view: &MemoryView,
    bus: Option<&Bus>,
    breakpoints: &BTreeSet<u32>,
    pc: u32,
) {
    let Some(bus) = bus else {
        ui.monospace("(no BIOS loaded - Bus unavailable)");
        return;
    };

    if ram_offset(view.addr, bus.ram().len()).is_some() {
        draw_ram_overview(ui, bus, breakpoints, pc);
    } else {
        draw_local_visual_map(ui, view, bus, breakpoints, pc);
    }
}

fn draw_ram_overview(ui: &mut egui::Ui, bus: &Bus, breakpoints: &BTreeSet<u32>, pc: u32) {
    const GAP: f32 = 1.0;
    const MIN_CELL: f32 = 1.5;
    const MAX_CELL: f32 = 3.0;

    let ram = bus.ram();
    let sample_count = ram_overview_sample_count(ram.len());
    let columns = visual_columns(ui.available_width());
    let rows = sample_count.div_ceil(columns);
    let available_width = ui.available_width().max(1.0);
    let cell = ((available_width - GAP * columns.saturating_sub(1) as f32) / columns as f32)
        .clamp(MIN_CELL, MAX_CELL);
    let grid_size = egui::vec2(
        columns as f32 * cell + columns.saturating_sub(1) as f32 * GAP,
        rows as f32 * cell + rows.saturating_sub(1) as f32 * GAP,
    );

    let (rect, response) = ui.allocate_exact_size(grid_size, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::CONTENT_BG);

    let pc_offset = ram_offset(pc, ram.len());
    for sample_idx in 0..sample_count {
        let col = sample_idx % columns;
        let row = sample_idx / columns;
        let min = rect.min + egui::vec2(col as f32 * (cell + GAP), row as f32 * (cell + GAP));
        let cell_rect = egui::Rect::from_min_size(min, egui::vec2(cell, cell));
        let byte_range = ram_overview_byte_range(sample_idx, ram.len());
        let color = ram_overview_color(ram, sample_idx);
        painter.rect_filled(cell_rect, 0.0, color);

        if pc_offset.is_some_and(|offset| byte_range.contains(&offset)) {
            painter.rect_stroke(
                cell_rect,
                0.0,
                Stroke::new(1.0, Color32::from_rgb(80, 200, 120)),
                StrokeKind::Inside,
            );
        } else if breakpoint_in_ram_range(breakpoints, byte_range, ram.len()) {
            painter.rect_stroke(
                cell_rect,
                0.0,
                Stroke::new(1.0, theme::ACCENT),
                StrokeKind::Inside,
            );
        }
    }

    if let Some(hover) =
        ram_overview_hover_text(ui, rect, cell, GAP, columns, sample_count, ram.len())
    {
        response.on_hover_text(hover);
    }
}

fn draw_local_visual_map(
    ui: &mut egui::Ui,
    view: &MemoryView,
    bus: &Bus,
    breakpoints: &BTreeSet<u32>,
    pc: u32,
) {
    const GAP: f32 = 1.0;
    const MIN_CELL: f32 = 1.5;
    const MAX_CELL: f32 = 3.0;

    let sample_count = WINDOW_SIZE as usize;
    let columns = visual_columns(ui.available_width());
    let rows = sample_count.div_ceil(columns);
    let available_width = ui.available_width().max(1.0);
    let cell = ((available_width - GAP * columns.saturating_sub(1) as f32) / columns as f32)
        .clamp(MIN_CELL, MAX_CELL);
    let grid_size = egui::vec2(
        columns as f32 * cell + columns.saturating_sub(1) as f32 * GAP,
        rows as f32 * cell + rows.saturating_sub(1) as f32 * GAP,
    );

    let (rect, response) = ui.allocate_exact_size(grid_size, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::CONTENT_BG);

    for sample_idx in 0..sample_count {
        let col = sample_idx % columns;
        let row = sample_idx / columns;
        let min = rect.min + egui::vec2(col as f32 * (cell + GAP), row as f32 * (cell + GAP));
        let cell_rect = egui::Rect::from_min_size(min, egui::vec2(cell, cell));
        let addr = visual_sample_addr(view.addr, sample_idx);
        let color = visual_sample_color(bus, addr);
        painter.rect_filled(cell_rect, 0.0, color);

        if addr == pc {
            painter.rect_stroke(
                cell_rect,
                0.0,
                Stroke::new(1.0, Color32::from_rgb(80, 200, 120)),
                StrokeKind::Inside,
            );
        } else if breakpoints.contains(&addr) {
            painter.rect_stroke(
                cell_rect,
                0.0,
                Stroke::new(1.0, theme::ACCENT),
                StrokeKind::Inside,
            );
        }
    }

    if let Some(hover) = visual_hover_text(ui, rect, cell, GAP, columns, sample_count, view, bus) {
        response.on_hover_text(hover);
    }
}

fn ram_offset(virt: u32, ram_len: usize) -> Option<usize> {
    let phys = virt & 0x1FFF_FFFF;
    let mirror_end = (ram_len as u32).saturating_mul(4);
    if phys < mirror_end {
        Some(phys as usize % ram_len)
    } else {
        None
    }
}

fn ram_addr(offset: usize) -> u32 {
    RAM_CANONICAL_BASE.wrapping_add(offset as u32)
}

fn ram_overview_sample_count(ram_len: usize) -> usize {
    ram_len.div_ceil(RAM_OVERVIEW_BYTES_PER_CELL)
}

fn ram_overview_byte_range(sample_idx: usize, ram_len: usize) -> Range<usize> {
    let start = (sample_idx * RAM_OVERVIEW_BYTES_PER_CELL).min(ram_len);
    let end = (start + RAM_OVERVIEW_BYTES_PER_CELL).min(ram_len);
    start..end
}

fn ram_overview_color(ram: &[u8], sample_idx: usize) -> Color32 {
    let range = ram_overview_byte_range(sample_idx, ram.len());
    aggregate_byte_color(&ram[range])
}

fn breakpoint_in_ram_range(
    breakpoints: &BTreeSet<u32>,
    byte_range: Range<usize>,
    ram_len: usize,
) -> bool {
    breakpoints
        .iter()
        .filter_map(|&addr| ram_offset(addr, ram_len))
        .any(|offset| byte_range.contains(&offset))
}

fn ram_overview_hover_text(
    ui: &egui::Ui,
    rect: egui::Rect,
    cell: f32,
    gap: f32,
    columns: usize,
    sample_count: usize,
    ram_len: usize,
) -> Option<String> {
    let pos = ui.ctx().pointer_hover_pos()?;
    if !rect.contains(pos) {
        return None;
    }
    let rel = pos - rect.min;
    let pitch = cell + gap;
    let col = (rel.x / pitch).floor() as usize;
    let row = (rel.y / pitch).floor() as usize;
    if rel.x - col as f32 * pitch > cell || rel.y - row as f32 * pitch > cell {
        return None;
    }
    let sample_idx = row.checked_mul(columns)?.checked_add(col)?;
    if sample_idx >= sample_count {
        return None;
    }

    let byte_range = ram_overview_byte_range(sample_idx, ram_len);
    let start = ram_addr(byte_range.start);
    let end = ram_addr(byte_range.end.saturating_sub(1));
    Some(format!(
        "{start:08X}-{end:08X}  {} bytes/cell",
        byte_range.len()
    ))
}

fn aggregate_byte_color(bytes: &[u8]) -> Color32 {
    let mut hi_hist = [0usize; 16];
    let mut low_hist = [0usize; 16];
    let mut low_sum = 0usize;
    let mut nonzero = 0usize;
    for &byte in bytes {
        if byte == 0 {
            continue;
        }
        nonzero += 1;
        hi_hist[(byte >> 4) as usize] += 1;
        low_hist[(byte & 0x0F) as usize] += 1;
        low_sum += (byte & 0x0F) as usize;
    }

    if nonzero == 0 {
        return byte_color(0);
    }

    let high = dominant_index(&hi_hist);
    let low = if high == 0 {
        dominant_index(&low_hist)
    } else {
        ((low_sum / nonzero).min(15)) as u8
    };
    let representative = (high << 4) | low;
    density_tint(byte_color(representative), nonzero, bytes.len())
}

fn dominant_index(hist: &[usize; 16]) -> u8 {
    hist.iter()
        .enumerate()
        .max_by_key(|&(idx, count)| (*count, idx))
        .map(|(idx, _)| idx as u8)
        .unwrap_or(0)
}

fn density_tint(color: Color32, nonzero: usize, total: usize) -> Color32 {
    let density = nonzero as f32 / total.max(1) as f32;
    color.gamma_multiply(0.35 + density.sqrt() * 0.65)
}

fn visual_columns(width: f32) -> usize {
    if width >= 620.0 {
        192
    } else if width >= 300.0 {
        128
    } else {
        96
    }
}

fn visual_sample_addr(base: u32, sample_idx: usize) -> u32 {
    base.wrapping_add(sample_idx as u32)
}

fn visual_sample_color(bus: &Bus, addr: u32) -> Color32 {
    let Some(byte) = bus.try_read8(addr) else {
        return Color32::from_rgb(8, 8, 11);
    };
    byte_color(byte)
}

fn visual_hover_text(
    ui: &egui::Ui,
    rect: egui::Rect,
    cell: f32,
    gap: f32,
    columns: usize,
    sample_count: usize,
    view: &MemoryView,
    bus: &Bus,
) -> Option<String> {
    let pos = ui.ctx().pointer_hover_pos()?;
    if !rect.contains(pos) {
        return None;
    }
    let rel = pos - rect.min;
    let pitch = cell + gap;
    let col = (rel.x / pitch).floor() as usize;
    let row = (rel.y / pitch).floor() as usize;
    if rel.x - col as f32 * pitch > cell || rel.y - row as f32 * pitch > cell {
        return None;
    }
    let sample_idx = row.checked_mul(columns)?.checked_add(col)?;
    if sample_idx >= sample_count {
        return None;
    }

    let addr = visual_sample_addr(view.addr, sample_idx);
    let Some(byte) = bus.try_read8(addr) else {
        return Some(format!("{addr:08X}  --"));
    };
    Some(format!("{addr:08X}  {byte:02X}"))
}

fn byte_color(byte: u8) -> Color32 {
    if byte == 0 {
        return Color32::from_rgb(4, 4, 7);
    }
    if byte == 0xFF {
        return Color32::from_rgb(238, 238, 226);
    }

    let base = value_family_color(byte >> 4);
    let lift = 72 + (byte & 0x0F) as u16 * 8;
    Color32::from_rgb(
        scale_channel(base.r(), lift),
        scale_channel(base.g(), lift),
        scale_channel(base.b(), lift),
    )
}

fn scale_channel(channel: u8, lift: u16) -> u8 {
    ((channel as u16 * lift) / 192).min(255) as u8
}

fn value_family_color(family: u8) -> Color32 {
    // This 16-color base palette stays readable at tiny cell sizes. The
    // low half of each byte then modulates brightness in `byte_color`.
    const PALETTE: [Color32; 16] = [
        Color32::from_rgb(0x00, 0x00, 0x00),
        Color32::from_rgb(0x1D, 0x2B, 0x53),
        Color32::from_rgb(0x7E, 0x25, 0x53),
        Color32::from_rgb(0x00, 0x87, 0x51),
        Color32::from_rgb(0xAB, 0x52, 0x36),
        Color32::from_rgb(0x5F, 0x57, 0x4F),
        Color32::from_rgb(0xC2, 0xC3, 0xC7),
        Color32::from_rgb(0xFF, 0xF1, 0xE8),
        Color32::from_rgb(0xFF, 0x00, 0x4D),
        Color32::from_rgb(0xFF, 0xA3, 0x00),
        Color32::from_rgb(0xFF, 0xEC, 0x27),
        Color32::from_rgb(0x00, 0xE4, 0x36),
        Color32::from_rgb(0x29, 0xAD, 0xFF),
        Color32::from_rgb(0x83, 0x76, 0x9C),
        Color32::from_rgb(0xFF, 0x77, 0xA8),
        Color32::from_rgb(0xFF, 0xCC, 0xAA),
    ];
    PALETTE[(family & 0x0F) as usize]
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
        ui.monospace("(no BIOS loaded - Bus unavailable)");
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
                    egui::Label::new(egui::RichText::new(text).monospace().color(color)).truncate(),
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
