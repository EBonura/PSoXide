//! From rays cast down a model to the plan cells that hold a floating solid.
//!
//! The model is sampled as vertical rays, [`COLUMNS_PER_SECTOR`] to a sector
//! each way, each recording up to a handful of surface heights from the top
//! down. Heights pair up into solid intervals (a top and the underside under
//! it; a lone surface at the bottom stands on the ground). The solid that
//! stands on the ground, and anything within [`FloatConfig::clearance`] of
//! it, is what the ground fit already built. What is left above that is
//! floating: a gantry, a bridge, a lintel.
//!
//! Columns are gathered into quarter-sector plan cells (a click, the
//! finest leaf), each keeping the intervals that enough of its columns
//! agree on.

use super::npy::NpyF32;
use super::FloatConfig;
use crate::grid::units::{CLICK_UNITS, FINE_UNITS, SECTOR_UNITS};

/// Rays cast per sector along each axis.
pub const COLUMNS_PER_SECTOR: usize = 16;
/// Rays along one side of a plan cell (a click).
pub const COLUMNS_PER_CELL: usize =
    COLUMNS_PER_SECTOR * CLICK_UNITS as usize / SECTOR_UNITS as usize;
/// Plan cells along a sector.
pub const CELLS_PER_SECTOR: i32 = SECTOR_UNITS / CLICK_UNITS;
/// Heights the lattice of heights snaps to, grid units.
pub const SNAP: i32 = FINE_UNITS;

/// The model's ground level and sector size, in the model's own units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Height of the ground in the model.
    pub ground: f64,
    /// Model units in one sector.
    pub sector: f64,
}

impl Frame {
    /// Read `{"ground": g, "sector": s}`.
    ///
    /// # Errors
    ///
    /// Returns a message when either number is missing or `sector` is not
    /// positive.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let number = |key: &str| -> Result<f64, String> {
            let at = text
                .find(&format!("\"{key}\""))
                .ok_or_else(|| format!("frame.json has no {key}"))?;
            let rest = text[at..]
                .split_once(':')
                .ok_or_else(|| format!("frame.json: no value for {key}"))?
                .1;
            let end = rest.find([',', '}']).unwrap_or(rest.len());
            rest[..end]
                .trim()
                .parse::<f64>()
                .map_err(|_| format!("frame.json: {key} is not a number"))
        };
        let frame = Self {
            ground: number("ground")?,
            sector: number("sector")?,
        };
        if frame.sector > 0.0 {
            Ok(frame)
        } else {
            Err("frame.json: the sector size must be positive".to_string())
        }
    }

    /// Height of `hit` above the ground, grid units.
    pub fn grid_units(&self, hit: f32) -> f64 {
        (f64::from(hit) - self.ground) / self.sector * f64::from(SECTOR_UNITS)
    }
}

/// Surface heights of every ray: `rows` by `cols` columns, up to `depth`
/// hits each, `NaN` for none.
#[derive(Clone, Debug)]
pub struct HitField {
    rows: usize,
    cols: usize,
    depth: usize,
    heights: Vec<f32>,
}

impl HitField {
    /// Wrap a parsed array of shape `[rows, cols, depth]`.
    ///
    /// # Errors
    ///
    /// Returns a message when the array is not three dimensional.
    pub fn from_npy(npy: NpyF32) -> Result<Self, String> {
        match npy.shape[..] {
            [rows, cols, depth] => Ok(Self {
                rows,
                cols,
                depth,
                heights: npy.data,
            }),
            _ => Err(format!(
                "the hit array must be rows x columns x hits, got {:?}",
                npy.shape
            )),
        }
    }

    /// A field of `rows` by `cols` rays, each the hits `hits(row, col)` lists
    /// (at most `depth`, the rest `NaN`).
    pub fn from_fn(
        rows: usize,
        cols: usize,
        depth: usize,
        hits: impl Fn(usize, usize) -> Vec<f32>,
    ) -> Self {
        let mut heights = Vec::with_capacity(rows * cols * depth);
        for row in 0..rows {
            for col in 0..cols {
                let mut list = hits(row, col);
                list.resize(depth, f32::NAN);
                heights.extend(list);
            }
        }
        Self {
            rows,
            cols,
            depth,
            heights,
        }
    }

    /// Rows of rays (along z).
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Columns of rays (along x).
    pub fn cols(&self) -> usize {
        self.cols
    }

    fn column(&self, row: usize, col: usize) -> &[f32] {
        let start = (row * self.cols + col) * self.depth;
        &self.heights[start..start + self.depth]
    }
}

/// A solid between two heights above the ground, grid units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Solid {
    /// Underside.
    pub bottom: i32,
    /// Top.
    pub top: i32,
    /// Open space under it, to the solid below or to the ground.
    pub clearance: i32,
}

/// The solid intervals of one ray, ascending, overlapping ones merged.
fn intervals(hits: &[f32], frame: &Frame) -> Vec<(i32, i32)> {
    let mut heights: Vec<i32> = hits
        .iter()
        .filter(|h| h.is_finite())
        .map(|&h| frame.grid_units(h).round() as i32)
        .collect();
    heights.sort_unstable_by(|a, b| b.cmp(a));
    let mut out: Vec<(i32, i32)> = heights
        .chunks(2)
        .map(|pair| (pair.get(1).copied().unwrap_or(0).max(0), pair[0]))
        .filter(|(bottom, top)| top > bottom)
        .collect();
    out.sort_unstable();
    let mut merged: Vec<(i32, i32)> = Vec::with_capacity(out.len());
    for (bottom, top) in out {
        match merged.last_mut() {
            Some(last) if bottom <= last.1 => last.1 = last.1.max(top),
            _ => merged.push((bottom, top)),
        }
    }
    merged
}

/// The floating solids of one ray: what stands more than
/// [`FloatConfig::clearance`] over the ground run and is thick enough.
pub fn floating_in_column(hits: &[f32], frame: &Frame, config: &FloatConfig) -> Vec<Solid> {
    let mut ground_top = 0;
    let mut out: Vec<Solid> = Vec::new();
    for (bottom, top) in intervals(hits, frame) {
        match out.last_mut() {
            // Above something already floating: join it across a small gap.
            Some(last) if bottom - last.top < config.merge_gap => last.top = last.top.max(top),
            Some(last) => {
                let clearance = bottom - last.top;
                out.push(Solid {
                    bottom,
                    top,
                    clearance,
                });
            }
            // Still the ground's: close to the run so far, it belongs to it.
            None if bottom - ground_top < config.clearance => ground_top = ground_top.max(top),
            None => out.push(Solid {
                bottom,
                top,
                clearance: bottom - ground_top,
            }),
        }
    }
    out.retain(|s| s.top - s.bottom >= config.min_thickness);
    out
}

/// A floating solid agreed on by the rays of one plan cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanCell {
    /// Cell along x, in clicks from the model's corner.
    pub x: i32,
    /// Cell along z, in clicks from the model's corner.
    pub z: i32,
    /// Underside, snapped to [`SNAP`].
    pub bottom: i32,
    /// Top, snapped to [`SNAP`].
    pub top: i32,
    /// Open space under it.
    pub clearance: i32,
}

/// Round to the height lattice.
pub fn snap(value: i32) -> i32 {
    (f64::from(value) / f64::from(SNAP)).round() as i32 * SNAP
}

fn median(values: &mut [i32]) -> i32 {
    values.sort_unstable();
    values[values.len() / 2]
}

/// The floating solids of every plan cell of the field. A cell keeps an
/// interval when at least [`FloatConfig::min_cover_percent`] percent of its
/// rays have one within [`FloatConfig::tolerance`] of it at both ends.
pub fn plan_cells(field: &HitField, frame: &Frame, config: &FloatConfig) -> Vec<PlanCell> {
    let rays = COLUMNS_PER_CELL * COLUMNS_PER_CELL;
    let need = (rays * config.min_cover_percent as usize)
        .div_ceil(100)
        .max(1);
    let mut out = Vec::new();
    for cz in 0..field.rows() / COLUMNS_PER_CELL {
        for cx in 0..field.cols() / COLUMNS_PER_CELL {
            let mut found: Vec<Solid> = Vec::new();
            for row in cz * COLUMNS_PER_CELL..(cz + 1) * COLUMNS_PER_CELL {
                for col in cx * COLUMNS_PER_CELL..(cx + 1) * COLUMNS_PER_CELL {
                    found.extend(floating_in_column(field.column(row, col), frame, config));
                }
            }
            if found.len() < need {
                continue;
            }
            for cluster in clusters(&found, config.tolerance) {
                if cluster.len() < need {
                    continue;
                }
                let mut bottoms: Vec<i32> = cluster.iter().map(|s| s.bottom).collect();
                let mut tops: Vec<i32> = cluster.iter().map(|s| s.top).collect();
                let mut clearances: Vec<i32> = cluster.iter().map(|s| s.clearance).collect();
                out.push(PlanCell {
                    x: cx as i32,
                    z: cz as i32,
                    bottom: snap(median(&mut bottoms)),
                    top: snap(median(&mut tops)),
                    clearance: median(&mut clearances),
                });
            }
        }
    }
    out
}

/// Group solids whose ends all lie within `tolerance` of the group's first.
fn clusters(solids: &[Solid], tolerance: i32) -> Vec<Vec<Solid>> {
    let mut out: Vec<Vec<Solid>> = Vec::new();
    for &solid in solids {
        let home = out.iter_mut().find(|c| {
            (c[0].bottom - solid.bottom).abs() <= tolerance
                && (c[0].top - solid.top).abs() <= tolerance
        });
        match home {
            Some(cluster) => cluster.push(solid),
            None => out.push(vec![solid]),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One sector per model unit, ground at zero: hits are grid units / 1024.
    pub(crate) const UNIT: Frame = Frame {
        ground: 0.0,
        sector: 1.0,
    };

    pub(crate) fn hit(units: i32) -> f32 {
        units as f32 / 1024.0
    }

    #[test]
    fn the_frame_reads_the_conversion_file() {
        let frame = Frame::from_json("{\"ground\": -0.3233, \"sector\": 0.0226}").unwrap();
        assert_eq!(frame.ground, -0.3233);
        assert!((frame.grid_units(-0.3233 + 0.0226) - 1024.0).abs() < 1e-2);
        assert!(Frame::from_json("{\"ground\": 1}").is_err());
        assert!(Frame::from_json("{\"ground\": 1, \"sector\": 0}").is_err());
    }

    #[test]
    fn hits_pair_into_solids_and_a_lone_hit_stands_on_the_ground() {
        let hits = [hit(5120), hit(4096), hit(512), f32::NAN];
        assert_eq!(intervals(&hits, &UNIT), vec![(0, 512), (4096, 5120)]);
    }

    #[test]
    fn a_deck_over_open_ground_floats_and_a_lintel_does_not() {
        let config = FloatConfig::default();
        // Ground to 512, a deck 4096 to 5120 (3584 of air under it).
        let deck = [hit(5120), hit(4096), hit(512)];
        let found = floating_in_column(&deck, &UNIT, &config);
        assert_eq!(
            found,
            vec![Solid {
                bottom: 4096,
                top: 5120,
                clearance: 3584
            }]
        );
        // A lintel 1024 over the ground run is the ground's own solid.
        let lintel = [hit(3072), hit(1536), hit(512)];
        assert!(floating_in_column(&lintel, &UNIT, &config).is_empty());
        // A sliver thinner than the minimum is dropped.
        let sliver = [hit(4300), hit(4096), hit(512)];
        assert!(floating_in_column(&sliver, &UNIT, &config).is_empty());
    }

    #[test]
    fn solids_stacked_with_a_small_gap_are_one() {
        let config = FloatConfig::default();
        let stack = [hit(6144), hit(5632), hit(5376), hit(4096), hit(512)];
        let found = floating_in_column(&stack, &UNIT, &config);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].bottom, found[0].top), (4096, 6144));
    }

    #[test]
    fn snapping_rounds_to_the_lattice() {
        assert_eq!(snap(4120), 4096);
        assert_eq!(snap(4150), 4160);
        assert_eq!(snap(-30), 0);
    }
}
