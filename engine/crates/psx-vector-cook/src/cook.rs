//! The cooking pipeline: SVG text to a PSVG asset, a report and a reference.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use psx_vector::encode::{encode, EncodeError, GroupSource};
use psx_vector::format::{Vertex, COORD_SHIFT, RUN_QUADS, RUN_TRIANGLES};

use crate::geom::{Affine, Pt};
use crate::paint::{
    parse_blend, parse_color, parse_length, parse_numbers, parse_paint, parse_transform, Cap,
    FillRule, Gradient, Join, Paint, Rgb, Stop, Style,
};
use crate::path::{ellipse_segs, flatten, parse_path, rect_segs, Poly, Seg};
use crate::stroke::{stroke_polyline, StrokeStyle};
use crate::tri::{triangulate, IPt, ITri, TriError};
use crate::xml::{self, Element, XmlError};

/// Triangle limits enforced at cook time.
#[derive(Clone, Debug, Default)]
pub struct Budget {
    /// Limit on the triangles of the whole asset.
    pub total: Option<usize>,
    /// Limit applied to every group not named in `groups`.
    pub per_group: Option<usize>,
    /// Limits for named groups.
    pub groups: BTreeMap<String, usize>,
}

/// Cooker settings.
#[derive(Clone, Debug)]
pub struct CookOptions {
    /// Curve flattening tolerance in output pixels.
    pub tolerance_px: f64,
    /// Triangle budget.
    pub budget: Budget,
}

impl Default for CookOptions {
    fn default() -> Self {
        Self {
            tolerance_px: 0.25,
            budget: Budget::default(),
        }
    }
}

/// Why a cook failed.
#[derive(Debug)]
pub enum CookError {
    /// The SVG is not well-formed XML.
    Xml(XmlError),
    /// The SVG uses something outside the supported subset, or is invalid.
    Svg {
        /// The element at fault, such as `<path id="frame">`.
        context: String,
        /// What is wrong.
        message: String,
    },
    /// One or more triangle budgets were exceeded.
    Budget(Vec<String>),
    /// The asset could not be encoded.
    Encode(EncodeError),
}

impl fmt::Display for CookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CookError::Xml(e) => write!(f, "{e}"),
            CookError::Svg { context, message } => write!(f, "{context}: {message}"),
            CookError::Budget(lines) => write!(f, "triangle budget exceeded: {}", lines.join("; ")),
            CookError::Encode(e) => write!(f, "cannot encode: {e:?}"),
        }
    }
}

impl std::error::Error for CookError {}

/// Per-group cook statistics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupReport {
    /// SVG id (empty for loose shapes).
    pub name: String,
    /// GPU triangles drawn (a quad counts as two).
    pub triangles: usize,
    /// Of those, how many came from quads: the group holds this many quads.
    pub quads: usize,
    /// Vertices after sharing.
    pub vertices: usize,
    /// Blend runs.
    pub runs: usize,
}

/// What a cook produced, for the log and the budget gate.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Report {
    /// One entry per group, in cook order.
    pub groups: Vec<GroupReport>,
    /// Size of the encoded asset in bytes.
    pub bytes: usize,
}

impl Report {
    /// Triangles over all groups.
    pub fn total_triangles(&self) -> usize {
        self.groups.iter().map(|g| g.triangles).sum()
    }

    /// The largest vertex count of any group (the runtime scratch size).
    pub fn max_group_vertices(&self) -> usize {
        self.groups.iter().map(|g| g.vertices).max().unwrap_or(0)
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self
            .groups
            .iter()
            .map(|g| g.name.len())
            .max()
            .unwrap_or(5)
            .max(5);
        writeln!(
            f,
            "{:width$}  {:>9}  {:>5}  {:>8}  {:>5}",
            "group", "triangles", "quads", "vertices", "runs"
        )?;
        for g in &self.groups {
            let name = if g.name.is_empty() { "(root)" } else { &g.name };
            writeln!(
                f,
                "{:width$}  {:>9}  {:>5}  {:>8}  {:>5}",
                name, g.triangles, g.quads, g.vertices, g.runs
            )?;
        }
        write!(
            f,
            "total: {} triangles ({} as quads), {} groups, max {} vertices in a group, {} bytes",
            self.total_triangles(),
            self.groups.iter().map(|g| 2 * g.quads).sum::<usize>(),
            self.groups.len(),
            self.max_group_vertices(),
            self.bytes
        )
    }
}

/// How a reference shape is coloured.
#[derive(Clone, Debug)]
pub enum RefColor {
    /// A flat colour.
    Solid(Rgb),
    /// A linear gradient: `t = ax * x + ay * y + c` in output pixels.
    Linear {
        /// `[ax, ay, c]`.
        t: [f64; 3],
        /// The gradient's stops, offsets ascending.
        gradient: Gradient,
    },
}

/// One painted shape in output pixels, for the analytic reference raster.
#[derive(Clone, Debug)]
pub struct RefShape {
    /// Group the shape drew into.
    pub group: usize,
    /// Closed contours in output pixels.
    pub contours: Vec<Vec<Pt>>,
    /// Fill rule.
    pub rule: FillRule,
    /// Colour source.
    pub color: RefColor,
    /// Colour scale for additive layers (the element alpha), 1 otherwise.
    pub scale: f64,
    /// Blend run code.
    pub blend: u8,
}

/// The result of a cook.
#[derive(Debug)]
pub struct Cooked {
    /// The PSVG asset.
    pub bytes: Vec<u8>,
    /// Statistics.
    pub report: Report,
    /// Canvas size in pixels.
    pub canvas_px: (f64, f64),
    /// Painted shapes in group-then-paint order, for the reference raster.
    pub reference: Vec<RefShape>,
}

/// A coloured vertex: lattice position and colour word.
type CVert = (i32, i32, u32);

/// A triangle (`n == 3`) or quad (`n == 4`, drawn as `0 1 2` then `1 2 3`).
#[derive(Clone, Copy)]
struct Prim {
    v: [CVert; 4],
    n: u8,
    blend: u8,
}

struct GroupBuild {
    name: String,
    prims: Vec<Prim>,
    pivot: Option<IPt>,
}

struct Cooker<'a> {
    opts: &'a CookOptions,
    gradients: HashMap<String, Gradient>,
    groups: Vec<GroupBuild>,
    by_name: HashMap<String, usize>,
    root_group: Option<usize>,
    reference: Vec<RefShape>,
}

fn describe(el: &Element) -> String {
    match el.attr("id") {
        Some(id) => format!("<{} id=\"{id}\">", el.name),
        None => format!("<{}>", el.name),
    }
}

fn svg_err<T>(el: &Element, message: impl Into<String>) -> Result<T, CookError> {
    Err(CookError::Svg {
        context: describe(el),
        message: message.into(),
    })
}

fn to_lattice(p: Pt) -> IPt {
    let s = (1 << COORD_SHIFT) as f64;
    ((p.0 * s).round() as i32, (p.1 * s).round() as i32)
}

/// Elements that carry no pixels and are skipped silently.
fn is_metadata(name: &str) -> bool {
    matches!(name, "title" | "desc" | "metadata") || name.contains(':')
}

/// Cook `svg` into a PSVG asset.
pub fn cook(svg: &str, opts: &CookOptions) -> Result<Cooked, CookError> {
    let root = xml::parse(svg).map_err(CookError::Xml)?;
    if root.name != "svg" {
        return svg_err(&root, "the root element must be <svg>");
    }
    let mut cooker = Cooker {
        opts,
        gradients: HashMap::new(),
        groups: Vec::new(),
        by_name: HashMap::new(),
        root_group: None,
        reference: Vec::new(),
    };
    cooker.collect_gradients(&root)?;
    cooker.resolve_gradient_hrefs(&root)?;

    let (canvas, ctm) = canvas_and_root_transform(&root)?;
    cooker.walk(&root, &ctm, &Style::default(), None, true)?;
    cooker.finish(canvas)
}

fn canvas_and_root_transform(root: &Element) -> Result<((f64, f64), Affine), CookError> {
    let attr_len = |name: &str| -> Result<Option<f64>, CookError> {
        match root.attr(name) {
            Some(v) => parse_length(v).map(Some).or_else(|m| svg_err(root, m)),
            None => Ok(None),
        }
    };
    let view_box = match root.attr("viewBox") {
        Some(v) => {
            let n = parse_numbers(v).or_else(|m| svg_err(root, m))?;
            if n.len() != 4 || n[2] <= 0.0 || n[3] <= 0.0 {
                return svg_err(root, "viewBox must be four numbers with positive size");
            }
            Some([n[0], n[1], n[2], n[3]])
        }
        None => None,
    };
    let (width, height) = (attr_len("width")?, attr_len("height")?);
    let canvas = match (width, height, view_box) {
        (Some(w), Some(h), _) => (w, h),
        (_, _, Some(vb)) => (vb[2], vb[3]),
        _ => return svg_err(root, "needs width and height, or a viewBox"),
    };
    let ctm = match view_box {
        Some(vb) => Affine::scale(canvas.0 / vb[2], canvas.1 / vb[3])
            .compose(&Affine::translate(-vb[0], -vb[1])),
        None => Affine::IDENTITY,
    };
    Ok((canvas, ctm))
}

fn parse_unit_or_percent(s: &str) -> Result<f64, String> {
    match s.trim().strip_suffix('%') {
        Some(p) => p
            .trim()
            .parse::<f64>()
            .map(|v| v / 100.0)
            .map_err(|_| format!("bad number `{s}`")),
        None => s
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("bad number `{s}`")),
    }
}

/// `property` from the element's attributes, overridden by its `style`.
fn property<'a>(
    el: &'a Element,
    style_decls: &'a [(String, String)],
    name: &str,
) -> Option<&'a str> {
    style_decls
        .iter()
        .rev()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .or_else(|| el.attr(name))
}

fn style_declarations(el: &Element) -> Vec<(String, String)> {
    el.attr("style")
        .map(|s| {
            s.split(';')
                .filter_map(|d| d.split_once(':'))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

impl Cooker<'_> {
    // ---- gradients ----

    fn collect_gradients(&mut self, el: &Element) -> Result<(), CookError> {
        if el.name == "linearGradient" {
            let Some(id) = el.attr("id") else {
                return svg_err(el, "a gradient needs an id");
            };
            let units = el.attr("gradientUnits").unwrap_or("objectBoundingBox");
            let bbox_units = match units {
                "objectBoundingBox" => true,
                "userSpaceOnUse" => false,
                other => return svg_err(el, format!("unknown gradientUnits `{other}`")),
            };
            let coord = |name: &str, default: &str| -> Result<f64, CookError> {
                let v = el.attr(name).unwrap_or(default);
                if !bbox_units && v.contains('%') {
                    return svg_err(el, "percentages need objectBoundingBox units");
                }
                parse_unit_or_percent(v).or_else(|m| svg_err(el, m))
            };
            let transform = match el.attr("gradientTransform") {
                Some(t) => parse_transform(t).or_else(|m| svg_err(el, m))?,
                None => Affine::IDENTITY,
            };
            let mut stops = Vec::new();
            for child in el.children.iter().filter(|c| c.name == "stop") {
                let cd = style_declarations(child);
                let offset = parse_unit_or_percent(child.attr("offset").unwrap_or("0"))
                    .or_else(|m| svg_err(child, m))?
                    .clamp(0.0, 1.0);
                let color = match property(child, &cd, "stop-color") {
                    Some(c) => parse_color(c)
                        .or_else(|m| svg_err(child, m))?
                        .unwrap_or(Rgb(0, 0, 0)),
                    None => Rgb(0, 0, 0),
                };
                if let Some(op) = property(child, &cd, "stop-opacity") {
                    let op: f64 = op
                        .trim()
                        .parse()
                        .or_else(|_| svg_err(child, "bad stop-opacity"))?;
                    if (op - 1.0).abs() > 1e-9 {
                        return svg_err(
                            child,
                            "stop-opacity is not supported (the GPU has no per-vertex alpha); \
                             fade to black on a data-blend=\"add\" shape instead",
                        );
                    }
                }
                let offset = stops.last().map_or(offset, |s: &Stop| offset.max(s.offset));
                stops.push(Stop { offset, color });
            }
            let gradient = Gradient {
                p1: (coord("x1", "0%")?, coord("y1", "0%")?),
                p2: (coord("x2", "100%")?, coord("y2", "0%")?),
                bbox_units,
                transform,
                stops,
            };
            self.gradients.insert(id.to_string(), gradient);
        }
        for child in &el.children {
            self.collect_gradients(child)?;
        }
        Ok(())
    }

    /// Give a stop-less gradient the stops of the gradient its `href` names.
    fn resolve_gradient_hrefs(&mut self, root: &Element) -> Result<(), CookError> {
        let mut hrefs = Vec::new();
        collect_hrefs(root, &mut hrefs);
        for (id, target) in hrefs {
            let stops = self.gradients.get(&target).map(|g| g.stops.clone());
            if let (Some(stops), Some(g)) = (stops, self.gradients.get_mut(&id)) {
                if g.stops.is_empty() {
                    g.stops = stops;
                }
            }
        }
        for (id, g) in &self.gradients {
            if g.stops.is_empty() {
                return Err(CookError::Svg {
                    context: format!("<linearGradient id=\"{id}\">"),
                    message: "has no stops".into(),
                });
            }
        }
        Ok(())
    }

    // ---- tree walk ----

    fn group_index(&mut self, current: Option<usize>) -> usize {
        match current {
            Some(i) => i,
            None => *self.root_group.get_or_insert_with(|| {
                self.groups.push(GroupBuild {
                    name: String::new(),
                    prims: Vec::new(),
                    pivot: None,
                });
                self.by_name.insert(String::new(), self.groups.len() - 1);
                self.groups.len() - 1
            }),
        }
    }

    fn walk(
        &mut self,
        el: &Element,
        parent_ctm: &Affine,
        parent_style: &Style,
        parent_group: Option<usize>,
        is_root: bool,
    ) -> Result<(), CookError> {
        let decls = style_declarations(el);
        if property(el, &decls, "display") == Some("none") {
            return Ok(());
        }
        let ctm = match el.attr("transform") {
            Some(t) if !is_root => {
                parent_ctm.compose(&parse_transform(t).or_else(|m| svg_err(el, m))?)
            }
            _ => *parent_ctm,
        };
        let style = self.cascade(el, &decls, parent_style)?;

        // The nearest element with an id owns the shapes below it.
        let mut group = parent_group;
        if !is_root {
            if let Some(id) = el.attr("id") {
                if matches!(
                    el.name.as_str(),
                    "g" | "path" | "rect" | "circle" | "ellipse" | "polygon" | "polyline" | "line"
                ) {
                    if self.by_name.contains_key(id) {
                        return svg_err(el, format!("duplicate group id `{id}`"));
                    }
                    self.groups.push(GroupBuild {
                        name: id.to_string(),
                        prims: Vec::new(),
                        pivot: None,
                    });
                    let idx = self.groups.len() - 1;
                    self.by_name.insert(id.to_string(), idx);
                    if let Some(p) = el.attr("data-pivot") {
                        let n = parse_numbers(p).or_else(|m| svg_err(el, m))?;
                        if n.len() != 2 {
                            return svg_err(el, "data-pivot must be `x y`");
                        }
                        self.groups[idx].pivot = Some(to_lattice(ctm.apply((n[0], n[1]))));
                    }
                    group = Some(idx);
                }
            }
        }

        match el.name.as_str() {
            "svg" | "g" | "a" | "switch" => {
                for child in &el.children {
                    self.walk(child, &ctm, &style, group, false)?;
                }
                Ok(())
            }
            "defs" | "linearGradient" | "stop" => Ok(()),
            "path" | "rect" | "circle" | "ellipse" | "polygon" | "polyline" | "line" => {
                let segs = self.shape_segs(el)?;
                self.paint_shape(el, &segs, &ctm, &style, group)
            }
            name if is_metadata(name) => Ok(()),
            other => svg_err(
                el,
                format!(
                    "<{other}> is not supported (supported: svg, g, path, rect, circle, ellipse, \
                     polygon, polyline, line, defs, linearGradient)"
                ),
            ),
        }
    }

    fn cascade(
        &self,
        el: &Element,
        decls: &[(String, String)],
        parent: &Style,
    ) -> Result<Style, CookError> {
        let mut s = parent.clone();
        let get = |name: &str| property(el, decls, name);
        let number = |name: &str| -> Result<Option<f64>, CookError> {
            match get(name) {
                Some(v) => v
                    .trim()
                    .parse::<f64>()
                    .map(Some)
                    .or_else(|_| svg_err(el, format!("bad {name} `{v}`"))),
                None => Ok(None),
            }
        };
        if let Some(v) = get("fill") {
            s.fill = parse_paint(v).or_else(|m| svg_err(el, m))?;
        }
        if let Some(v) = get("stroke") {
            s.stroke = parse_paint(v).or_else(|m| svg_err(el, m))?;
        }
        if let Some(v) = get("fill-rule") {
            s.fill_rule = match v.trim() {
                "nonzero" => FillRule::NonZero,
                "evenodd" => FillRule::EvenOdd,
                other => return svg_err(el, format!("bad fill-rule `{other}`")),
            };
        }
        if let Some(v) = number("fill-opacity")? {
            s.fill_opacity = v.clamp(0.0, 1.0);
        }
        if let Some(v) = number("stroke-opacity")? {
            s.stroke_opacity = v.clamp(0.0, 1.0);
        }
        if let Some(v) = get("stroke-width") {
            s.stroke_width = parse_length(v).or_else(|m| svg_err(el, m))?;
        }
        if let Some(v) = number("stroke-miterlimit")? {
            s.miter_limit = v.max(1.0);
        }
        if let Some(v) = get("stroke-linejoin") {
            s.join = match v.trim() {
                "miter" | "miter-clip" | "arcs" => Join::Miter,
                "bevel" => Join::Bevel,
                "round" => Join::Round,
                other => return svg_err(el, format!("bad stroke-linejoin `{other}`")),
            };
        }
        if let Some(v) = get("stroke-linecap") {
            s.cap = match v.trim() {
                "butt" => Cap::Butt,
                "square" => Cap::Square,
                "round" => Cap::Round,
                other => return svg_err(el, format!("bad stroke-linecap `{other}`")),
            };
        }
        if let Some(v) = number("opacity")? {
            s.opacity *= v.clamp(0.0, 1.0);
        }
        // Per-corner colours belong to one element and are not inherited.
        s.corners = match el.attr("data-corners") {
            Some(list) => Some(
                list.split_whitespace()
                    .map(|c| match parse_color(c) {
                        Ok(Some(rgb)) => Ok(rgb),
                        Ok(None) => svg_err(el, "data-corners colours cannot be none"),
                        Err(m) => svg_err(el, m),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            None => None,
        };
        if let Some(v) = get("data-blend").or_else(|| get("mix-blend-mode")) {
            s.blend = parse_blend(v).or_else(|m| svg_err(el, m))?;
        }
        Ok(s)
    }

    fn shape_segs(&self, el: &Element) -> Result<Vec<Seg>, CookError> {
        let num = |name: &str, default: f64| -> Result<f64, CookError> {
            match el.attr(name) {
                Some(v) => parse_length(v).or_else(|m| svg_err(el, m)),
                None => Ok(default),
            }
        };
        match el.name.as_str() {
            "path" => parse_path(el.attr("d").unwrap_or("")).or_else(|m| svg_err(el, m)),
            "rect" => {
                let (x, y, w, h) = (
                    num("x", 0.0)?,
                    num("y", 0.0)?,
                    num("width", 0.0)?,
                    num("height", 0.0)?,
                );
                let (mut rx, mut ry) = (num("rx", -1.0)?, num("ry", -1.0)?);
                if rx < 0.0 {
                    rx = if ry < 0.0 { 0.0 } else { ry };
                }
                if ry < 0.0 {
                    ry = rx;
                }
                Ok(rect_segs(x, y, w, h, rx, ry))
            }
            "circle" => {
                let r = num("r", 0.0)?;
                Ok(ellipse_segs(num("cx", 0.0)?, num("cy", 0.0)?, r, r))
            }
            "ellipse" => Ok(ellipse_segs(
                num("cx", 0.0)?,
                num("cy", 0.0)?,
                num("rx", 0.0)?,
                num("ry", 0.0)?,
            )),
            "polygon" | "polyline" => {
                let n =
                    parse_numbers(el.attr("points").unwrap_or("")).or_else(|m| svg_err(el, m))?;
                if n.len() % 2 != 0 || n.len() < 4 {
                    return svg_err(el, "points needs at least two coordinate pairs");
                }
                let mut segs = vec![Seg::Move((n[0], n[1]))];
                segs.extend(n[2..].chunks(2).map(|p| Seg::Line((p[0], p[1]))));
                if el.name == "polygon" {
                    segs.push(Seg::Close);
                }
                Ok(segs)
            }
            "line" => Ok(vec![
                Seg::Move((num("x1", 0.0)?, num("y1", 0.0)?)),
                Seg::Line((num("x2", 0.0)?, num("y2", 0.0)?)),
            ]),
            _ => unreachable!("shape_segs is only called for shape elements"),
        }
    }

    fn paint_shape(
        &mut self,
        el: &Element,
        segs: &[Seg],
        ctm: &Affine,
        style: &Style,
        group: Option<usize>,
    ) -> Result<(), CookError> {
        let group = self.group_index(group);
        let scale = ctm.max_scale().max(1e-9);
        let polys = flatten(segs, self.opts.tolerance_px / scale);
        if polys.is_empty() {
            return Ok(());
        }
        let bbox = user_bbox(&polys);

        if style.fill != Paint::None && !matches!(el.name.as_str(), "line") {
            let contours: Vec<Vec<Pt>> = polys
                .iter()
                .filter(|p| p.pts.len() >= 3)
                .map(|p| p.pts.iter().map(|&q| ctm.apply(q)).collect())
                .collect();
            if !contours.is_empty() {
                let lattice: Vec<Vec<IPt>> = contours
                    .iter()
                    .map(|c| c.iter().map(|&p| to_lattice(p)).collect())
                    .collect();
                let tris = triangulate(&lattice, style.fill_rule).map_err(|e| match e {
                    TriError::Crossing => CookError::Svg {
                        context: describe(el),
                        message: "contours cross each other or themselves; split the path into \
                                  simple shapes"
                            .into(),
                    },
                    TriError::Stuck => CookError::Svg {
                        context: describe(el),
                        message: "could not triangulate (touching or degenerate contours)".into(),
                    },
                })?;
                let corners = match &style.corners {
                    None => None,
                    Some(colors) => {
                        if contours[0].len() != colors.len() {
                            return svg_err(
                                el,
                                format!(
                                    "data-corners lists {} colours but the shape has {} vertices",
                                    colors.len(),
                                    contours[0].len()
                                ),
                            );
                        }
                        Some(
                            contours[0]
                                .iter()
                                .zip(colors)
                                .map(|(&p, &c)| (to_lattice(p), c))
                                .collect::<HashMap<IPt, Rgb>>(),
                        )
                    }
                };
                self.emit(
                    el,
                    group,
                    tris,
                    &style.fill,
                    style.fill_opacity * style.opacity,
                    style.blend,
                    ctm,
                    bbox,
                    Some((contours, style.fill_rule)),
                    corners.as_ref(),
                    Merge::Anywhere,
                )?;
            }
        }

        if style.stroke != Paint::None && style.stroke_width > 0.0 {
            let st = StrokeStyle {
                width: style.stroke_width,
                join: style.join,
                cap: style.cap,
                miter_limit: style.miter_limit,
                tolerance: self.opts.tolerance_px / scale,
            };
            let mut tris: Vec<ITri> = Vec::new();
            let mut ref_contours: Vec<Vec<Pt>> = Vec::new();
            for poly in &polys {
                for t in stroke_polyline(&poly.pts, poly.closed, &st) {
                    let out = [ctm.apply(t[0]), ctm.apply(t[1]), ctm.apply(t[2])];
                    let mut tri = [to_lattice(out[0]), to_lattice(out[1]), to_lattice(out[2])];
                    let a = (tri[1].0 as i64 - tri[0].0 as i64)
                        * (tri[2].1 as i64 - tri[0].1 as i64)
                        - (tri[1].1 as i64 - tri[0].1 as i64) * (tri[2].0 as i64 - tri[0].0 as i64);
                    if a == 0 {
                        continue;
                    }
                    // Counter-clockwise, so the reference's non-zero union holds.
                    let mut pts = out.to_vec();
                    if a < 0 {
                        tri.swap(1, 2);
                        pts.swap(1, 2);
                    }
                    ref_contours.push(pts);
                    tris.push(tri);
                }
            }
            self.emit(
                el,
                group,
                tris,
                &style.stroke,
                style.stroke_opacity * style.opacity,
                style.blend,
                ctm,
                bbox,
                Some((ref_contours, FillRule::NonZero)),
                None,
                Merge::Neighbours,
            )?;
        }
        Ok(())
    }

    /// Colour `tris` and append them to `group`, splitting at gradient stops.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        el: &Element,
        group: usize,
        tris: Vec<ITri>,
        paint: &Paint,
        alpha: f64,
        blend: u8,
        ctm: &Affine,
        bbox: [f64; 4],
        reference: Option<(Vec<Vec<Pt>>, FillRule)>,
        corners: Option<&HashMap<IPt, Rgb>>,
        merge: Merge,
    ) -> Result<(), CookError> {
        if alpha <= 0.0 || tris.is_empty() {
            return Ok(());
        }
        // Without a stated blend, an alpha of one half is the GPU's average
        // mode; other fractions have no native equivalent. With an additive
        // blend, alpha scales the colour (a dimmed glow).
        let (blend, colour_scale) = if alpha >= 1.0 - 1e-9 {
            (blend, 1.0)
        } else if blend == 0 && (alpha - 0.5).abs() <= 0.05 {
            (1, 1.0)
        } else if matches!(blend, 2..=4) {
            (blend, alpha)
        } else {
            return svg_err(
                el,
                format!(
                    "opacity {alpha} has no native PS1 equivalent: use 0.5 (average) or \
                     data-blend=\"add\" with the colour pre-scaled"
                ),
            );
        };
        let scale_rgb = |c: Rgb| {
            let k = |v: u8| (v as f64 * colour_scale).round().clamp(0.0, 255.0) as u8;
            Rgb(k(c.0), k(c.1), k(c.2))
        };
        let (ref_color, shaded): (RefColor, Box<dyn Fn(IPt) -> Rgb + '_>) = match paint {
            Paint::None => return Ok(()),
            // Per-corner colours replace the paint: the colour of a vertex is
            // the one listed for the nearest authored corner.
            _ if corners.is_some() => {
                let map = corners.cloned().unwrap_or_default();
                (
                    RefColor::Solid(Rgb(0, 0, 0)),
                    Box::new(move |p: IPt| {
                        map.get(&p).copied().unwrap_or_else(|| {
                            map.iter()
                                .min_by_key(|(q, _)| {
                                    let (dx, dy) =
                                        (q.0 as i64 - p.0 as i64, q.1 as i64 - p.1 as i64);
                                    (dx * dx + dy * dy, q.0, q.1)
                                })
                                .map_or(Rgb(0, 0, 0), |(_, c)| *c)
                        })
                    }),
                )
            }
            Paint::Solid(c) => (RefColor::Solid(*c), Box::new(move |_| *c)),
            Paint::Url(id) => {
                let Some(grad) = self.gradients.get(id).cloned() else {
                    return svg_err(el, format!("fill references unknown gradient `{id}`"));
                };
                let map = gradient_map(el, &grad, ctm, bbox)?;
                let g2 = grad.clone();
                (
                    RefColor::Linear {
                        t: map,
                        gradient: grad,
                    },
                    Box::new(move |p: IPt| {
                        let s = (1 << COORD_SHIFT) as f64;
                        let t = map[0] * (p.0 as f64 / s) + map[1] * (p.1 as f64 / s) + map[2];
                        g2.color_at(t)
                    }),
                )
            }
        };
        let thresholds: Vec<f64> = match (paint, &ref_color) {
            (Paint::Url(_), RefColor::Linear { gradient, .. }) => {
                let mut t: Vec<f64> = gradient.stops.iter().map(|s| s.offset).collect();
                t.dedup();
                t
            }
            _ => Vec::new(),
        };
        // Per-corner shapes have no analytic reference (it would only restate
        // the triangulation), so they are left out of it.
        if let (Some((contours, rule)), None) = (&reference, corners) {
            self.reference.push(RefShape {
                group,
                contours: contours.clone(),
                rule: *rule,
                color: ref_color.clone(),
                scale: colour_scale,
                blend,
            });
        }
        let t_of = |p: IPt| -> f64 {
            match &ref_color {
                RefColor::Linear { t, .. } => {
                    let s = (1 << COORD_SHIFT) as f64;
                    t[0] * (p.0 as f64 / s) + t[1] * (p.1 as f64 / s) + t[2]
                }
                RefColor::Solid(_) => 0.0,
            }
        };
        let mut pieces: Vec<ITri> = Vec::new();
        for tri in tris {
            split_at_thresholds(tri, &thresholds, &t_of, &mut pieces);
        }
        let mut coloured: Vec<[CVert; 3]> = Vec::with_capacity(pieces.len());
        for tri in pieces {
            let area = (tri[1].0 as i64 - tri[0].0 as i64) * (tri[2].1 as i64 - tri[0].1 as i64)
                - (tri[1].1 as i64 - tri[0].1 as i64) * (tri[2].0 as i64 - tri[0].0 as i64);
            if area == 0 {
                continue;
            }
            let v = |p: IPt| (p.0, p.1, scale_rgb(shaded(p)).word());
            coloured.push([v(tri[0]), v(tri[1]), v(tri[2])]);
        }
        self.groups[group]
            .prims
            .extend(merge_into_prims(&coloured, blend, merge));
        Ok(())
    }

    // ---- output ----

    fn finish(self, canvas: (f64, f64)) -> Result<Cooked, CookError> {
        let q4 = (1 << COORD_SHIFT) as f64;
        let canvas_q4 = [
            (canvas.0 * q4).round().clamp(0.0, u16::MAX as f64) as u16,
            (canvas.1 * q4).round().clamp(0.0, u16::MAX as f64) as u16,
        ];
        let mut sources = Vec::with_capacity(self.groups.len());
        let mut report = Report::default();
        for g in &self.groups {
            let mut index: HashMap<(i32, i32, u32), u16> = HashMap::new();
            let mut vertices: Vec<Vertex> = Vec::new();
            let mut triangles: Vec<[u16; 3]> = Vec::new();
            let mut quads: Vec<[u16; 4]> = Vec::new();
            let mut runs: Vec<(u8, u8, u16)> = Vec::new();
            for prim in &g.prims {
                let mut ids = [0u16; 4];
                for (slot, &(x, y, color)) in ids.iter_mut().zip(&prim.v[..prim.n as usize]) {
                    *slot = match index.get(&(x, y, color)) {
                        Some(&i) => i,
                        None => {
                            let (Ok(xi), Ok(yi)) = (i16::try_from(x), i16::try_from(y)) else {
                                return Err(CookError::Svg {
                                    context: format!("group `{}`", g.name),
                                    message: format!(
                                        "vertex ({}, {}) px is outside the +-2047 px range of \
                                         a Q4 coordinate",
                                        x as f64 / q4,
                                        y as f64 / q4
                                    ),
                                });
                            };
                            let Ok(i) = u16::try_from(vertices.len()) else {
                                return Err(CookError::Encode(EncodeError::TooManyVertices(
                                    g.name.clone(),
                                )));
                            };
                            vertices.push(Vertex {
                                x: xi,
                                y: yi,
                                color,
                            });
                            index.insert((x, y, color), i);
                            i
                        }
                    };
                }
                let kind = if prim.n == 4 {
                    quads.push(ids);
                    RUN_QUADS
                } else {
                    triangles.push([ids[0], ids[1], ids[2]]);
                    RUN_TRIANGLES
                };
                match runs.last_mut() {
                    Some((b, k, n)) if *b == prim.blend && *k == kind && *n < u16::MAX => *n += 1,
                    _ => runs.push((prim.blend, kind, 1)),
                }
            }
            report.groups.push(GroupReport {
                name: g.name.clone(),
                triangles: triangles.len() + 2 * quads.len(),
                quads: quads.len(),
                vertices: vertices.len(),
                runs: runs.len(),
            });
            sources.push(GroupSource {
                name: g.name.clone(),
                vertices,
                triangles,
                quads,
                runs,
                pivot: g.pivot.map(|p| [p.0 as i16, p.1 as i16]),
            });
        }
        check_budget(&self.opts.budget, &report)?;
        let bytes = encode(canvas_q4, &sources).map_err(CookError::Encode)?;
        report.bytes = bytes.len();
        let mut reference = self.reference;
        reference.sort_by_key(|r| r.group);
        Ok(Cooked {
            bytes,
            report,
            canvas_px: canvas,
            reference,
        })
    }
}

/// How far apart two triangles may be in a shape and still become one quad.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Merge {
    /// Any two triangles that share an edge. For fills, whose triangles do
    /// not overlap, so reordering them within the shape changes nothing.
    Anywhere,
    /// Only neighbours in paint order, for strokes whose pieces overlap.
    Neighbours,
}

/// The vertex shared by position and colour, if `a` and `b` share exactly an
/// edge: returns the shared edge's endpoints in `a` and the apex of each.
fn shared_edge(a: &[CVert; 3], b: &[CVert; 3]) -> Option<([CVert; 2], CVert, CVert)> {
    let mut shared = Vec::with_capacity(3);
    for (i, va) in a.iter().enumerate() {
        if b.contains(va) {
            shared.push(i);
        }
    }
    if shared.len() != 2 {
        return None;
    }
    let apex_a = (0..3).find(|i| !shared.contains(i))?;
    let apex_b = b.iter().find(|v| !a.contains(v))?;
    Some(([a[shared[0]], a[shared[1]]], a[apex_a], *apex_b))
}

/// Pair up triangles that share an edge into quads. A GPU quad `p q r s` is
/// the triangles `p q r` and `q r s`, so a pair with the shared edge `q r` and
/// apexes `p` and `s` is exactly that quad, convex or not.
fn merge_into_prims(tris: &[[CVert; 3]], blend: u8, merge: Merge) -> Vec<Prim> {
    let mut used = vec![false; tris.len()];
    let mut out = Vec::with_capacity(tris.len());
    for i in 0..tris.len() {
        if used[i] {
            continue;
        }
        used[i] = true;
        let last = match merge {
            Merge::Anywhere => tris.len(),
            Merge::Neighbours => (i + 2).min(tris.len()),
        };
        let partner = (i + 1..last)
            .filter(|&j| !used[j])
            .find_map(|j| shared_edge(&tris[i], &tris[j]).map(|e| (j, e)));
        match partner {
            Some((j, (edge, apex_a, apex_b))) => {
                used[j] = true;
                out.push(Prim {
                    v: [apex_a, edge[0], edge[1], apex_b],
                    n: 4,
                    blend,
                });
            }
            None => out.push(Prim {
                v: [tris[i][0], tris[i][1], tris[i][2], tris[i][2]],
                n: 3,
                blend,
            }),
        }
    }
    out
}

fn collect_hrefs(el: &Element, out: &mut Vec<(String, String)>) {
    if el.name == "linearGradient" {
        if let (Some(id), Some(href)) = (
            el.attr("id"),
            el.attr("href").or_else(|| el.attr("xlink:href")),
        ) {
            if let Some(target) = href.strip_prefix('#') {
                out.push((id.to_string(), target.to_string()));
            }
        }
    }
    for c in &el.children {
        collect_hrefs(c, out);
    }
}

fn user_bbox(polys: &[Poly]) -> [f64; 4] {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in polys.iter().flat_map(|p| p.pts.iter()) {
        b = [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)];
    }
    b
}

/// `[ax, ay, c]` with `t = ax * x + ay * y + c` in output pixels.
fn gradient_map(
    el: &Element,
    g: &Gradient,
    ctm: &Affine,
    bbox: [f64; 4],
) -> Result<[f64; 3], CookError> {
    let Some(to_user) = ctm.inverse() else {
        return svg_err(el, "singular transform");
    };
    let Some(from_gradient) = g.transform.inverse() else {
        return svg_err(el, "singular gradientTransform");
    };
    let (bw, bh) = (bbox[2] - bbox[0], bbox[3] - bbox[1]);
    if g.bbox_units && (bw <= 0.0 || bh <= 0.0) {
        return svg_err(
            el,
            "an objectBoundingBox gradient needs a shape with width and height",
        );
    }
    let (dx, dy) = (g.p2.0 - g.p1.0, g.p2.1 - g.p1.1);
    let len2 = dx * dx + dy * dy;
    let t_at = |q: Pt| -> f64 {
        let u = to_user.apply(q);
        let space = if g.bbox_units {
            ((u.0 - bbox[0]) / bw, (u.1 - bbox[1]) / bh)
        } else {
            u
        };
        let gp = from_gradient.apply(space);
        if len2 <= 1e-18 {
            return 1.0;
        }
        ((gp.0 - g.p1.0) * dx + (gp.1 - g.p1.1) * dy) / len2
    };
    let t0 = t_at((0.0, 0.0));
    Ok([t_at((1.0, 0.0)) - t0, t_at((0.0, 1.0)) - t0, t0])
}

/// Split `tri` (a lattice triangle) along every gradient isoline in
/// `thresholds` and append the convex pieces as triangles.
fn split_at_thresholds(
    tri: ITri,
    thresholds: &[f64],
    t_of: &dyn Fn(IPt) -> f64,
    out: &mut Vec<ITri>,
) {
    let mut polys: Vec<Vec<IPt>> = vec![tri.to_vec()];
    for &s in thresholds {
        let mut next = Vec::with_capacity(polys.len() + 1);
        for poly in polys {
            let ts: Vec<f64> = poly.iter().map(|&p| t_of(p)).collect();
            let (lo, hi) = ts
                .iter()
                .fold((f64::MAX, f64::MIN), |(l, h), &t| (l.min(t), h.max(t)));
            if lo >= s - 1e-9 || hi <= s + 1e-9 {
                next.push(poly);
                continue;
            }
            let (mut below, mut above) = (Vec::new(), Vec::new());
            for i in 0..poly.len() {
                let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                let (ta, tb) = (ts[i], ts[(i + 1) % poly.len()]);
                if ta <= s {
                    below.push(a);
                }
                if ta >= s {
                    above.push(a);
                }
                if (ta < s && tb > s) || (ta > s && tb < s) {
                    let u = (s - ta) / (tb - ta);
                    let p = (
                        (a.0 as f64 + (b.0 - a.0) as f64 * u).round() as i32,
                        (a.1 as f64 + (b.1 - a.1) as f64 * u).round() as i32,
                    );
                    below.push(p);
                    above.push(p);
                }
            }
            for part in [below, above] {
                if part.len() >= 3 {
                    next.push(part);
                }
            }
        }
        polys = next;
    }
    for poly in polys {
        for i in 1..poly.len() - 1 {
            out.push([poly[0], poly[i], poly[i + 1]]);
        }
    }
}

fn check_budget(budget: &Budget, report: &Report) -> Result<(), CookError> {
    let mut problems = Vec::new();
    for g in &report.groups {
        let limit = budget.groups.get(&g.name).copied().or(budget.per_group);
        if let Some(limit) = limit {
            if g.triangles > limit {
                let name = if g.name.is_empty() { "(root)" } else { &g.name };
                problems.push(format!(
                    "group {name}: {} triangles, budget {limit}",
                    g.triangles
                ));
            }
        }
    }
    if let Some(total) = budget.total {
        if report.total_triangles() > total {
            problems.push(format!(
                "total: {} triangles, budget {total}",
                report.total_triangles()
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(CookError::Budget(problems))
    }
}
