//! The strict check: exact pixel comparison, not footprints.
//!
//! Round 1 compared the connected components of each cell, so anything that is not a component
//! (the dark dividers, the plate between and around the cells) was never compared, and a shape
//! could pass while its right half stepped differently from its left. This check compares every
//! pixel of a full-height strip:
//!
//! * **Periodic strips.** The bar is cut into strips one pitch wide. Two strips whose
//!   neighbourhood is the same (same lit or empty state for the cell in the strip and the one
//!   before it, whose last columns spill into this strip) must be equal pixel for pixel, whatever
//!   the pixels belong to: cell body, bevel, divider, plate.
//! * **Mirror symmetry.** Parts that are symmetric by design (the emblem and its core, each gem)
//!   must equal their own reflection across the vertical axis through the centre of their
//!   rectangle, colour for colour.
//!
//! A layout (which strips, which rows, which rectangles) comes from a text file written next to the
//! art, or one of the built-in round-1 presets.

use crate::frame::Frame;

/// One mirrored rectangle, in HUD-origin coordinates, `[x0, x1) x [y0, y1)`.
pub struct Mirror {
    pub name: String,
    pub rect: (i32, i32, i32, i32),
}

/// Where everything is, relative to a HUD's origin.
pub struct Layout {
    pub cols: Vec<i32>,
    pub y0: i32,
    pub pitch_y: i32,
    pub levels: Vec<usize>,
    pub count: usize,
    /// Left edge of strip 0 of the front bar.
    pub cell_x0: i32,
    pub pitch: i32,
    /// Rows of the front bar, `[y0, y1)`.
    pub front_band: (i32, i32),
    /// Visible rows of the bar behind, when it shows cells.
    pub back_band: Option<(i32, i32)>,
    pub back_lit: usize,
    /// Strip pitch and left edge of strip 0 of the bar behind, when it is not the front bar's size.
    pub back_pitch: i32,
    pub back_cell_x0: i32,
    /// Left edge of strip 0 of the gems.
    pub gem_x0: i32,
    pub gem_band: Option<(i32, i32)>,
    /// Distance between gems.
    pub gem_pitch: i32,
    /// Gem strips compared: `[gem_from, gem_to)`.
    pub gem_from: usize,
    pub gem_to: usize,
    pub mirrors: Vec<Mirror>,
}

impl Layout {
    /// The round-1 variants A, B and C (`back == false`) and D (`back == true`).
    pub fn legacy(back: bool) -> Self {
        let mirrors = [
            ("emblem core", (6, 8, 20, 20)),
            ("emblem top", (0, 2, 26, 8)),
            ("emblem bottom", (0, 23, 26, 27)),
        ]
        .into_iter()
        .map(|(name, rect)| Mirror {
            name: name.into(),
            rect,
        })
        .collect();
        Self {
            cols: vec![2, 162],
            y0: 8,
            pitch_y: 40,
            levels: vec![10, 6, 3, 0],
            count: 10,
            cell_x0: 30,
            pitch: 12,
            front_band: (8, 22),
            back_band: back.then_some((1, 8)),
            back_lit: 4,
            gem_x0: 31,
            gem_band: Some((22, 32)),
            gem_pitch: 12,
            back_pitch: 12,
            back_cell_x0: 30,
            // The rail ends one column into the last gem's strip.
            gem_from: 0,
            gem_to: 9,
            mirrors,
        }
    }

    /// Parse a `key = value` layout file.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut l = Self::legacy(false);
        l.mirrors.clear();
        l.back_band = None;
        l.gem_band = None;
        l.back_pitch = 0;
        l.back_cell_x0 = i32::MIN;
        l.gem_from = 0;
        l.gem_to = usize::MAX;
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| format!("layout line {}: expected key = value", n + 1))?;
            let (key, value) = (key.trim(), value.trim());
            let nums = |v: &str| -> Result<Vec<i32>, String> {
                v.split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        s.parse::<i32>()
                            .map_err(|_| format!("layout line {}: bad number {s:?}", n + 1))
                    })
                    .collect()
            };
            let pair = |v: &str| -> Result<(i32, i32), String> {
                match nums(v)?.as_slice() {
                    [a, b] => Ok((*a, *b)),
                    _ => Err(format!("layout line {}: expected two numbers", n + 1)),
                }
            };
            match key {
                "cols" => l.cols = nums(value)?,
                "y0" => l.y0 = nums(value)?[0],
                "pitch_y" => l.pitch_y = nums(value)?[0],
                "levels" => l.levels = nums(value)?.into_iter().map(|v| v as usize).collect(),
                "count" => l.count = nums(value)?[0] as usize,
                "cell_x0" => l.cell_x0 = nums(value)?[0],
                "pitch" => l.pitch = nums(value)?[0],
                "front_band" => l.front_band = pair(value)?,
                "back_band" => l.back_band = Some(pair(value)?),
                "back_lit" => l.back_lit = nums(value)?[0] as usize,
                "gem_x0" => l.gem_x0 = nums(value)?[0],
                "gem_band" => l.gem_band = Some(pair(value)?),
                "gem_pitch" => l.gem_pitch = nums(value)?[0],
                "back_pitch" => l.back_pitch = nums(value)?[0],
                "back_cell_x0" => l.back_cell_x0 = nums(value)?[0],
                "gem_range" => {
                    let (a, b) = pair(value)?;
                    l.gem_from = a as usize;
                    l.gem_to = b as usize;
                }
                "mirror" => {
                    let (name, rest) = value
                        .split_once(char::is_whitespace)
                        .ok_or_else(|| format!("layout line {}: mirror needs a name", n + 1))?;
                    match nums(rest)?.as_slice() {
                        [a, b, c, d] => l.mirrors.push(Mirror {
                            name: name.into(),
                            rect: (*a, *b, *c, *d),
                        }),
                        _ => return Err(format!("layout line {}: mirror needs 4 numbers", n + 1)),
                    }
                }
                other => return Err(format!("layout line {}: unknown key {other:?}", n + 1)),
            }
        }
        if l.back_pitch == 0 {
            l.back_pitch = l.pitch;
        }
        if l.back_cell_x0 == i32::MIN {
            l.back_cell_x0 = l.cell_x0;
        }
        if l.gem_to == usize::MAX {
            l.gem_to = l.count;
        }
        Ok(l)
    }
}

/// Collected mismatching pixels, for the marked frame.
#[derive(Default)]
pub struct Marks(pub Vec<(i32, i32)>);

fn strip_pixels(f: &Frame, x0: i32, band: (i32, i32), width: i32) -> Result<Vec<[u8; 3]>, String> {
    if !f.contains(x0, band.0) || !f.contains(x0 + width - 1, band.1 - 1) {
        return Err(format!(
            "strip x {x0}..{} rows {}..{} is outside the frame",
            x0 + width,
            band.0,
            band.1
        ));
    }
    let mut v = Vec::with_capacity((width * (band.1 - band.0)) as usize);
    for y in band.0..band.1 {
        for x in x0..x0 + width {
            v.push(f.at(x, y));
        }
    }
    Ok(v)
}

const EMPTY: u8 = 0;
const LIT: u8 = 1;
const EDGE: u8 = 2;

fn state_name(s: u8) -> char {
    match s {
        EMPTY => 'E',
        LIT => 'L',
        _ => '|',
    }
}

/// Compare strips `from..to` of one row of strips against the first strip of their class.
#[allow(clippy::too_many_arguments)]
fn check_strips(
    label: &str,
    f: &Frame,
    x0: i32,
    pitch: i32,
    band: (i32, i32),
    states: &[u8],
    range: (usize, usize),
    neighbour: bool,
    marks: &mut Marks,
) -> Result<(bool, String), String> {
    let mut classes: Vec<((u8, u8), Vec<usize>)> = Vec::new();
    for k in range.0..range.1.min(states.len()) {
        let prev = if neighbour && k > 0 {
            states[k - 1]
        } else {
            EDGE
        };
        let key = (prev, states[k]);
        match classes.iter_mut().find(|c| c.0 == key) {
            Some(c) => c.1.push(k),
            None => classes.push((key, vec![k])),
        }
    }
    let mut ok = true;
    let mut parts = Vec::new();
    let mut compared = 0usize;
    let mut bad_px = 0usize;
    for (key, members) in &classes {
        let tag = if neighbour {
            format!("{}{}", state_name(key.0), state_name(key.1))
        } else {
            format!("{}", state_name(key.1))
        };
        let reference = strip_pixels(f, x0 + pitch * members[0] as i32, band, pitch)?;
        let mut class_bad = 0usize;
        for &k in &members[1..] {
            let strip = strip_pixels(f, x0 + pitch * k as i32, band, pitch)?;
            compared += 1;
            for (i, (a, b)) in reference.iter().zip(&strip).enumerate() {
                if a != b {
                    class_bad += 1;
                    let (px, py) = ((i as i32) % pitch, (i as i32) / pitch);
                    marks.0.push((x0 + pitch * k as i32 + px, band.0 + py));
                }
            }
        }
        if class_bad > 0 {
            ok = false;
            bad_px += class_bad;
        }
        parts.push(format!(
            "{tag} x{}{}",
            members.len(),
            if class_bad > 0 {
                format!(" ({class_bad} px differ)")
            } else {
                String::new()
            }
        ));
    }
    let text = format!(
        "  {label}: {compared} strip comparisons in {} classes [{}], {}",
        classes.len(),
        parts.join(", "),
        if ok {
            "all identical".to_string()
        } else {
            format!("{bad_px} pixels differ")
        }
    );
    Ok((ok, text))
}

/// Compare a rectangle with its own mirror image.
fn check_mirror(
    f: &Frame,
    name: &str,
    rect: (i32, i32, i32, i32),
    marks: &mut Marks,
) -> Result<(bool, String), String> {
    let (x0, y0, x1, y1) = rect;
    if !f.contains(x0, y0) || !f.contains(x1 - 1, y1 - 1) {
        return Err(format!("mirror {name} is outside the frame"));
    }
    let mut bad = 0usize;
    let mut total = 0usize;
    for y in y0..y1 {
        for x in x0..(x0 + x1) / 2 {
            let m = x0 + x1 - 1 - x;
            total += 1;
            if f.at(x, y) != f.at(m, y) {
                bad += 1;
                marks.0.push((x, y));
                marks.0.push((m, y));
            }
        }
    }
    let ok = bad == 0;
    let text = format!(
        "  mirror {name}: {total} pixel pairs, {}",
        if ok {
            "symmetric".to_string()
        } else {
            format!("{bad} pairs differ")
        }
    );
    Ok((ok, text))
}

/// Run the strict check over every HUD of the grid. Returns whether everything passed.
pub fn run(f: &Frame, l: &Layout, marks: &mut Marks) -> Result<bool, String> {
    let mut all_ok = true;
    for (ci, &ox) in l.cols.iter().enumerate() {
        for (ri, &lvl) in l.levels.iter().enumerate() {
            let oy = l.y0 + l.pitch_y * ri as i32;
            println!(
                "HUD col {ci} row {ri} (origin {ox},{oy}), lit {lvl}/{}",
                l.count
            );
            let lit_states: Vec<u8> = (0..l.count)
                .map(|k| if k < lvl { LIT } else { EMPTY })
                .collect();
            let mut results = Vec::new();
            results.push(check_strips(
                "front bar",
                f,
                ox + l.cell_x0,
                l.pitch,
                (oy + l.front_band.0, oy + l.front_band.1),
                &lit_states,
                (1, l.count),
                true,
                marks,
            )?);
            if let Some(b) = l.back_band {
                let back_states: Vec<u8> = (0..l.count)
                    .map(|k| if k < l.back_lit { LIT } else { EMPTY })
                    .collect();
                results.push(check_strips(
                    "back bar",
                    f,
                    ox + l.back_cell_x0,
                    l.back_pitch,
                    (oy + b.0, oy + b.1),
                    &back_states,
                    (1, l.count),
                    true,
                    marks,
                )?);
            }
            if let Some(band) = l.gem_band {
                results.push(check_strips(
                    "gems",
                    f,
                    ox + l.gem_x0,
                    l.gem_pitch,
                    (oy + band.0, oy + band.1),
                    &lit_states,
                    (l.gem_from, l.gem_to),
                    false,
                    marks,
                )?);
                // Each gem against its own reflection (strips are centred on the gem).
                let mut gem_bad = 0usize;
                for k in l.gem_from..l.gem_to.min(l.count) {
                    let gx = ox + l.gem_x0 + l.gem_pitch * k as i32;
                    let r = (gx + 1, oy + band.0, gx + l.gem_pitch - 1, oy + band.1);
                    let (ok, _) = check_mirror(f, "gem", r, marks)?;
                    if !ok {
                        gem_bad += 1;
                    }
                }
                results.push((
                    gem_bad == 0,
                    format!(
                        "  mirror gems: {} gems, {}",
                        l.gem_to.min(l.count) - l.gem_from,
                        if gem_bad == 0 {
                            "all symmetric".to_string()
                        } else {
                            format!("{gem_bad} not symmetric")
                        }
                    ),
                ));
            }

            for m in &l.mirrors {
                let r = (ox + m.rect.0, oy + m.rect.1, ox + m.rect.2, oy + m.rect.3);
                results.push(check_mirror(f, &m.name, r, marks)?);
            }
            for (ok, text) in results {
                println!("{text}  -> {}", if ok { "PASS" } else { "FAIL" });
                all_ok &= ok;
            }
        }
    }
    Ok(all_ok)
}
