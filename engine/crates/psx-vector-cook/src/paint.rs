//! Colours, gradients and the style properties the cooker reads.

use crate::geom::{Affine, Pt};

/// An 8-bit sRGB colour.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Pack as a PSVG colour word.
    pub fn word(self) -> u32 {
        psx_vector::format::pack_rgb(self.0, self.1, self.2)
    }
}

/// What a shape is painted with.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    /// Not painted.
    None,
    /// A flat colour.
    Solid(Rgb),
    /// `url(#id)`, resolved to a gradient at cook time.
    Url(String),
}

/// A gradient stop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    /// Position along the gradient vector, 0 to 1.
    pub offset: f64,
    /// Colour at the stop.
    pub color: Rgb,
}

/// A linear gradient as authored.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    /// Start of the gradient vector.
    pub p1: Pt,
    /// End of the gradient vector.
    pub p2: Pt,
    /// True when the vector is in fractions of the shape's bounding box.
    pub bbox_units: bool,
    /// `gradientTransform`.
    pub transform: Affine,
    /// Stops in ascending offset order.
    pub stops: Vec<Stop>,
}

impl Gradient {
    /// The colour at gradient parameter `t`, padded outside 0..1.
    pub fn color_at(&self, t: f64) -> Rgb {
        let stops = &self.stops;
        let first = stops[0];
        let last = stops[stops.len() - 1];
        if t <= first.offset {
            return first.color;
        }
        if t >= last.offset {
            return last.color;
        }
        for pair in stops.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if t <= b.offset {
                let span = b.offset - a.offset;
                if span <= 0.0 {
                    return b.color;
                }
                let u = (t - a.offset) / span;
                let mix = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * u).round() as u8;
                return Rgb(
                    mix(a.color.0, b.color.0),
                    mix(a.color.1, b.color.1),
                    mix(a.color.2, b.color.2),
                );
            }
        }
        last.color
    }
}

/// SVG fill rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FillRule {
    /// Non-zero winding.
    NonZero,
    /// Even-odd crossings.
    EvenOdd,
}

/// Stroke line join.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Join {
    /// Pointed corner up to the miter limit, else bevelled.
    Miter,
    /// Flat corner.
    Bevel,
    /// Circular corner.
    Round,
}

/// Stroke line cap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cap {
    /// Flat at the end point.
    Butt,
    /// Flat, half a width past the end point.
    Square,
    /// Semicircle.
    Round,
}

/// The cascaded style of one element.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    /// Fill paint.
    pub fill: Paint,
    /// Fill rule.
    pub fill_rule: FillRule,
    /// Fill opacity, already multiplied by group opacities.
    pub fill_opacity: f64,
    /// Stroke paint.
    pub stroke: Paint,
    /// Stroke width in user units.
    pub stroke_width: f64,
    /// Stroke opacity, already multiplied by group opacities.
    pub stroke_opacity: f64,
    /// Stroke join.
    pub join: Join,
    /// Stroke cap.
    pub cap: Cap,
    /// Miter limit.
    pub miter_limit: f64,
    /// Blend run code, from `data-blend`.
    pub blend: u8,
    /// Accumulated `opacity` of the element and its ancestors.
    pub opacity: f64,
    /// Per-vertex fill colours from `data-corners`, in path order. Not inherited.
    pub corners: Option<Vec<Rgb>>,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fill: Paint::Solid(Rgb(0, 0, 0)),
            fill_rule: FillRule::NonZero,
            fill_opacity: 1.0,
            stroke: Paint::None,
            stroke_width: 1.0,
            stroke_opacity: 1.0,
            join: Join::Miter,
            cap: Cap::Butt,
            miter_limit: 4.0,
            blend: 0,
            opacity: 1.0,
            corners: None,
        }
    }
}

/// Parse a number with an optional `px` unit.
pub fn parse_length(s: &str) -> Result<f64, String> {
    let t = s.trim();
    let t = t.strip_suffix("px").unwrap_or(t);
    t.trim()
        .parse::<f64>()
        .map_err(|_| format!("`{s}` is not a length (plain numbers and px only)"))
}

/// Parse a list of numbers separated by spaces and commas.
pub fn parse_numbers(s: &str) -> Result<Vec<f64>, String> {
    s.split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .map(|t| {
            t.parse::<f64>()
                .map_err(|_| format!("`{t}` is not a number"))
        })
        .collect()
}

/// Parse an SVG colour or `none`: `#rgb`, `#rrggbb`, `rgb(...)` and a few names.
pub fn parse_color(s: &str) -> Result<Option<Rgb>, String> {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    if lower == "none" || lower == "transparent" {
        return Ok(None);
    }
    if let Some(hex) = lower.strip_prefix('#') {
        let digit = |c: char| c.to_digit(16).map(|d| d as u8);
        let ds: Option<Vec<u8>> = hex.chars().map(digit).collect();
        return match ds.as_deref() {
            Some([r, g, b]) => Ok(Some(Rgb(r * 17, g * 17, b * 17))),
            Some([r1, r2, g1, g2, b1, b2]) => {
                Ok(Some(Rgb(r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2)))
            }
            _ => Err(format!("bad colour `{s}`")),
        };
    }
    if let Some(args) = lower.strip_prefix("rgb(").and_then(|a| a.strip_suffix(')')) {
        let parts: Vec<&str> = args.split([',', ' ']).filter(|p| !p.is_empty()).collect();
        if parts.len() != 3 {
            return Err(format!("bad colour `{s}`"));
        }
        let mut c = [0u8; 3];
        for (slot, part) in c.iter_mut().zip(parts) {
            let v = if let Some(p) = part.strip_suffix('%') {
                p.parse::<f64>().map(|v| v * 2.55)
            } else {
                part.parse::<f64>()
            }
            .map_err(|_| format!("bad colour `{s}`"))?;
            *slot = v.round().clamp(0.0, 255.0) as u8;
        }
        return Ok(Some(Rgb(c[0], c[1], c[2])));
    }
    let named = match lower.as_str() {
        "black" => Rgb(0, 0, 0),
        "white" => Rgb(255, 255, 255),
        "red" => Rgb(255, 0, 0),
        "lime" => Rgb(0, 255, 0),
        "green" => Rgb(0, 128, 0),
        "blue" => Rgb(0, 0, 255),
        "yellow" => Rgb(255, 255, 0),
        "cyan" | "aqua" => Rgb(0, 255, 255),
        "magenta" | "fuchsia" => Rgb(255, 0, 255),
        "gray" | "grey" => Rgb(128, 128, 128),
        "orange" => Rgb(255, 165, 0),
        _ => {
            return Err(format!(
                "unsupported colour `{s}` (use #rrggbb, rgb() or a basic name)"
            ))
        }
    };
    Ok(Some(named))
}

/// Parse a paint value.
pub fn parse_paint(s: &str) -> Result<Paint, String> {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("url(") {
        let id = rest
            .split(')')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches(|c| c == '\'' || c == '"')
            .strip_prefix('#')
            .ok_or_else(|| format!("unsupported paint reference `{s}`"))?;
        return Ok(Paint::Url(id.to_string()));
    }
    Ok(match parse_color(t)? {
        Some(c) => Paint::Solid(c),
        None => Paint::None,
    })
}

/// Parse an SVG `transform` list.
pub fn parse_transform(s: &str) -> Result<Affine, String> {
    let mut m = Affine::IDENTITY;
    let mut rest = s.trim();
    while !rest.is_empty() {
        let open = rest
            .find('(')
            .ok_or_else(|| format!("bad transform `{s}`"))?;
        let close = rest
            .find(')')
            .ok_or_else(|| format!("bad transform `{s}`"))?;
        let name = rest[..open].trim();
        let args = parse_numbers(&rest[open + 1..close])?;
        let t = match (name, args.as_slice()) {
            ("matrix", [a, b, c, d, e, f]) => Affine {
                a: *a,
                b: *b,
                c: *c,
                d: *d,
                e: *e,
                f: *f,
            },
            ("translate", [x]) => Affine::translate(*x, 0.0),
            ("translate", [x, y]) => Affine::translate(*x, *y),
            ("scale", [s]) => Affine::scale(*s, *s),
            ("scale", [x, y]) => Affine::scale(*x, *y),
            ("rotate", [a]) => Affine::rotate(*a),
            ("rotate", [a, cx, cy]) => Affine::translate(*cx, *cy)
                .compose(&Affine::rotate(*a))
                .compose(&Affine::translate(-*cx, -*cy)),
            ("skewX", [a]) => Affine {
                c: a.to_radians().tan(),
                ..Affine::IDENTITY
            },
            ("skewY", [a]) => Affine {
                b: a.to_radians().tan(),
                ..Affine::IDENTITY
            },
            _ => return Err(format!("unsupported transform `{name}` in `{s}`")),
        };
        m = m.compose(&t);
        rest = rest[close + 1..].trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
    }
    Ok(m)
}

/// Map a `data-blend` value to a run code.
pub fn parse_blend(s: &str) -> Result<u8, String> {
    match s.trim() {
        "opaque" | "none" | "normal" => Ok(0),
        "avg" | "average" | "mix" => Ok(1),
        "add" | "plus-lighter" => Ok(2),
        "sub" | "subtract" => Ok(3),
        "quarter" | "add-quarter" => Ok(4),
        other => Err(format!(
            "unknown data-blend `{other}` (opaque, avg, add, sub, quarter)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_and_transforms() {
        assert_eq!(parse_color("#fa0").unwrap(), Some(Rgb(255, 170, 0)));
        assert_eq!(
            parse_color("rgb(10, 20, 30)").unwrap(),
            Some(Rgb(10, 20, 30))
        );
        assert_eq!(parse_color("none").unwrap(), None);
        let m = parse_transform("translate(10 20) scale(2)").unwrap();
        assert_eq!(m.apply((1.0, 1.0)), (12.0, 22.0));
    }

    #[test]
    fn gradient_interpolates_between_stops() {
        let g = Gradient {
            p1: (0.0, 0.0),
            p2: (1.0, 0.0),
            bbox_units: true,
            transform: Affine::IDENTITY,
            stops: vec![
                Stop {
                    offset: 0.0,
                    color: Rgb(0, 0, 0),
                },
                Stop {
                    offset: 1.0,
                    color: Rgb(200, 100, 0),
                },
            ],
        };
        assert_eq!(g.color_at(0.5), Rgb(100, 50, 0));
        assert_eq!(g.color_at(2.0), Rgb(200, 100, 0));
    }
}
