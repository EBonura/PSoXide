//! The stance HUD composed from `assets/hud/stance_hud.psvg`: which groups are
//! drawn, in what order, with what tint, for a given HUD state.
//!
//! Shared by the `vector-hud-proof` feature of this crate and by the
//! `showcase-vector` example (which includes this file by path), so both draw
//! the same composition.
//!
//! The composition is written once against a `draw` callback, so the same code
//! drives an immediate GPU draw, a packet build for an ordering table, and a
//! CPU-only cost probe.

use psx_vector::{GroupId, Placement, Style, VectorAsset};

/// Scratch capacity: the HUD asset's largest group has 14 vertices.
pub const SCRATCH_VERTICES: usize = 64;

/// Group handles looked up once at load.
pub struct HudIds {
    plate: GroupId,
    health: [GroupId; 10],
    emblem: GroupId,
    emblem_core: GroupId,
    rail: GroupId,
    energy: [GroupId; 10],
}

impl HudIds {
    /// Resolve every group the HUD draws. `None` if the asset lacks one.
    pub fn new(asset: &VectorAsset<'_>) -> Option<Self> {
        let numbered = |prefix: &str| -> Option<[GroupId; 10]> {
            let mut out = [asset.group("plate")?; 10];
            let mut name = [0u8; 12];
            for (i, slot) in out.iter_mut().enumerate() {
                let p = prefix.as_bytes();
                name[..p.len()].copy_from_slice(p);
                name[p.len()] = b'0' + i as u8;
                let text = core::str::from_utf8(&name[..p.len() + 1]).ok()?;
                *slot = asset.group(text)?;
            }
            Some(out)
        };
        Some(Self {
            plate: asset.group("plate")?,
            health: numbered("health_")?,
            emblem: asset.group("emblem")?,
            emblem_core: asset.group("emblem_core")?,
            rail: asset.group("rail")?,
            energy: numbered("energy_")?,
        })
    }
}

/// One stance's colour and how much of its bar is filled.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    /// Stance colour (the tint multiplied into the grey-authored art).
    pub rgb: [u8; 3],
    /// Lit cells, 0 to 10.
    pub cells: u8,
}

/// Everything the HUD shows; the cached packets are valid while it is equal.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HudState {
    /// The stance in front.
    pub active: Bar,
    /// The stance behind, dimmed.
    pub inactive: Bar,
    /// Lit energy crystals, 0 to 10.
    pub energy: u8,
    /// Also draw the inactive stance's cells (dimmed) on the back bar.
    pub back_cells: bool,
}

const AMBER_EMPTY: [u8; 3] = [70, 44, 24];

fn scaled(rgb: [u8; 3], k: u16) -> [u8; 3] {
    [
        ((rgb[0] as u16 * k) >> 8) as u8,
        ((rgb[1] as u16 * k) >> 8) as u8,
        ((rgb[2] as u16 * k) >> 8) as u8,
    ]
}

/// Draw the cluster at `origin` (top-left of the asset's canvas), back to
/// front: the inactive plate dimmed and offset up, the active plate, its
/// cells, the emblem, then the rail and the energy crystals.
pub fn draw_hud(
    ids: &HudIds,
    origin: (i32, i32),
    state: &HudState,
    draw: &mut impl FnMut(GroupId, &Placement, &Style),
) {
    let front = Placement::at(origin.0, origin.1);
    let back = Placement::at(origin.0, origin.1 - 7);
    let dim = Style {
        tint: scaled(state.inactive.rgb, 110),
        saturation: 110,
        ..Style::DEFAULT
    };
    draw(ids.plate, &back, &dim);
    if state.back_cells {
        let lit = scaled(state.inactive.rgb, 96);
        let off = scaled(state.inactive.rgb, 40);
        for (i, &cell) in ids.health.iter().enumerate() {
            let tint = if (i as u8) < state.inactive.cells {
                lit
            } else {
                off
            };
            draw(cell, &back, &Style::tinted(tint));
        }
    }
    draw(ids.plate, &front, &Style::tinted(state.active.rgb));
    let empty = scaled(state.active.rgb, 52);
    for (i, &cell) in ids.health.iter().enumerate() {
        let tint = if (i as u8) < state.active.cells {
            state.active.rgb
        } else {
            empty
        };
        draw(cell, &front, &Style::tinted(tint));
    }
    draw(ids.emblem, &front, &Style::tinted(state.active.rgb));
    draw(ids.emblem_core, &front, &Style::tinted(state.inactive.rgb));
    draw(ids.rail, &front, &Style::DEFAULT);
    for (i, &gem) in ids.energy.iter().enumerate() {
        let tint = if (i as u8) < state.energy {
            [255, 255, 255]
        } else {
            AMBER_EMPTY
        };
        draw(gem, &front, &Style::tinted(tint));
    }
}
