//! Geometry-only importer for classic Quake `.map` source files.
//!
//! The import boundary is deliberately narrow: worldspawn and `func_*`
//! brushes become editable PSoXide brushes, gameplay/trigger entities are
//! ignored, and every imported face receives one caller-supplied material.
//! No Quake texture pixels or gameplay data cross this boundary.

use std::collections::BTreeMap;
use std::fmt;

use crate::brush::{Brush, BrushContents, BrushFace};
use crate::ResourceId;

/// PSoXide editor coordinates are sixteen times runtime/Quake map units.
pub const QUAKE_TO_EDITOR_SCALE: i32 = 16;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuakeMapImportStats {
    pub source_entities: usize,
    pub source_brushes: usize,
    pub source_faces: usize,
    pub imported_brushes: usize,
    pub imported_faces: usize,
    pub skipped_non_geometry_brushes: usize,
    pub skipped_helper_brushes: usize,
    pub skipped_invalid_brushes: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuakeMapGeometry {
    pub brushes: Vec<Brush>,
    /// Converted location of the first `info_player_start`, still at Quake's
    /// centre-height origin. A character with a feet pivot should subtract
    /// `24 * QUAKE_TO_EDITOR_SCALE` from Y.
    pub player_start: Option<[i32; 3]>,
    pub stats: QuakeMapImportStats,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuakeMapImportError {
    pub line: usize,
    pub message: String,
}

impl QuakeMapImportError {
    fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for QuakeMapImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "{}", self.message)
        } else {
            write!(f, "line {}: {}", self.line, self.message)
        }
    }
}

impl std::error::Error for QuakeMapImportError {}

#[derive(Clone, Debug, Default)]
struct QuakeEntity {
    properties: BTreeMap<String, String>,
    brushes: Vec<QuakeBrush>,
}

#[derive(Clone, Debug, Default)]
struct QuakeBrush {
    faces: Vec<QuakeFace>,
}

#[derive(Clone, Debug)]
struct QuakeFace {
    line: usize,
    points: [[f64; 3]; 3],
    texture: String,
}

/// Parse a classic Quake map and convert only visible brush geometry.
///
/// The coordinate adapter is right-handed and winding-preserving after the
/// source's point order is reversed: `(quake_x, quake_y, quake_z)` becomes
/// `(x, y, z) = (quake_x, quake_z, -quake_y) * 16`.
pub fn import_quake_map_geometry(
    source: &str,
    material: Option<ResourceId>,
) -> Result<QuakeMapGeometry, QuakeMapImportError> {
    import_quake_map_geometry_scaled(source, material, QUAKE_TO_EDITOR_SCALE)
}

/// Parse geometry with an explicit integer coordinate scale.
///
/// This is useful for full-map performance fixtures that uniformly scale the
/// imported world and their actors down together to fit the PSX face/vertex
/// budgets without changing gameplay proportions.
pub fn import_quake_map_geometry_scaled(
    source: &str,
    material: Option<ResourceId>,
    scale: i32,
) -> Result<QuakeMapGeometry, QuakeMapImportError> {
    if scale <= 0 {
        return Err(QuakeMapImportError::new(
            0,
            "Quake import scale must be positive",
        ));
    }
    let entities = parse_entities(source)?;
    let mut stats = QuakeMapImportStats {
        source_entities: entities.len(),
        source_brushes: entities.iter().map(|entity| entity.brushes.len()).sum(),
        source_faces: entities
            .iter()
            .flat_map(|entity| &entity.brushes)
            .map(|brush| brush.faces.len())
            .sum(),
        ..QuakeMapImportStats::default()
    };
    let player_start = entities
        .iter()
        .find(|entity| entity.classname() == "info_player_start")
        .and_then(|entity| entity.properties.get("origin"))
        .map(|origin| parse_origin(origin))
        .transpose()?
        .map(|origin| transform_point_checked(origin, scale, 0))
        .transpose()?;

    let mut brushes = Vec::new();
    for entity in entities {
        if !entity.has_importable_geometry() {
            stats.skipped_non_geometry_brushes += entity.brushes.len();
            continue;
        }
        for source_brush in entity.brushes {
            if source_brush.is_helper_only() {
                stats.skipped_helper_brushes += 1;
                continue;
            }
            let contents = source_brush.contents();
            let mut faces = Vec::with_capacity(source_brush.faces.len());
            for source_face in source_brush.faces {
                // Original Quake derives the face normal with the opposite
                // cross-product order to BrushFace. Swap B/C so the converted
                // plane normal points out of the solid as the kernel expects.
                let [a, b, c] = source_face.points;
                let points = [a, c, b]
                    .map(|point| transform_point_checked(point, scale, source_face.line))
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()?;
                faces.push(BrushFace {
                    points: [points[0], points[1], points[2]],
                    material,
                    uv: Default::default(),
                });
            }
            let brush = Brush {
                faces,
                contents,
                mover: None,
            };
            let solved = brush.solve();
            if !solved.is_valid() || !solved.within_extent(crate::brush::BRUSH_EDIT_EXTENT_LIMIT) {
                stats.skipped_invalid_brushes += 1;
                continue;
            }
            stats.imported_faces += brush.faces.len();
            stats.imported_brushes += 1;
            brushes.push(brush);
        }
    }

    Ok(QuakeMapGeometry {
        brushes,
        player_start,
        stats,
    })
}

impl QuakeEntity {
    fn classname(&self) -> &str {
        self.properties
            .get("classname")
            .map(String::as_str)
            .unwrap_or("")
    }

    fn has_importable_geometry(&self) -> bool {
        self.classname() == "worldspawn" || self.classname().starts_with("func_")
    }
}

impl QuakeBrush {
    fn is_helper_only(&self) -> bool {
        !self.faces.is_empty()
            && self
                .faces
                .iter()
                .all(|face| is_helper_texture(&face.texture))
    }

    fn contents(&self) -> BrushContents {
        if self
            .faces
            .iter()
            .any(|face| face.texture.to_ascii_uppercase().starts_with("*LAVA"))
        {
            BrushContents::Lava
        } else if self
            .faces
            .iter()
            .any(|face| face.texture.to_ascii_uppercase().starts_with("*SLIME"))
        {
            BrushContents::Slime
        } else if self
            .faces
            .iter()
            .any(|face| face.texture.to_ascii_uppercase().starts_with("*WATER"))
        {
            BrushContents::Water
        } else {
            BrushContents::Solid
        }
    }
}

fn is_helper_texture(texture: &str) -> bool {
    matches!(
        texture.to_ascii_uppercase().as_str(),
        "CLIP" | "TRIGGER" | "HINT" | "SKIP" | "ORIGIN"
    )
}

fn parse_entities(source: &str) -> Result<Vec<QuakeEntity>, QuakeMapImportError> {
    let mut entities = Vec::new();
    let mut entity: Option<QuakeEntity> = None;
    let mut brush: Option<QuakeBrush> = None;

    for (zero_line, raw) in source.lines().enumerate() {
        let line = zero_line + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        match trimmed {
            "{" if entity.is_none() => entity = Some(QuakeEntity::default()),
            "{" if brush.is_none() => brush = Some(QuakeBrush::default()),
            "{" => return Err(QuakeMapImportError::new(line, "unexpected nested block")),
            "}" if brush.is_some() => {
                let finished = brush.take().expect("checked above");
                entity
                    .as_mut()
                    .expect("brush cannot exist outside entity")
                    .brushes
                    .push(finished);
            }
            "}" if entity.is_some() => {
                entities.push(entity.take().expect("checked above"));
            }
            "}" => return Err(QuakeMapImportError::new(line, "unmatched closing brace")),
            _ if entity.is_none() => {
                return Err(QuakeMapImportError::new(line, "content outside an entity"));
            }
            _ if brush.is_some() => {
                brush
                    .as_mut()
                    .expect("checked above")
                    .faces
                    .push(parse_face(trimmed, line)?);
            }
            _ => {
                let (key, value) = parse_property(trimmed, line)?;
                entity
                    .as_mut()
                    .expect("checked above")
                    .properties
                    .insert(key, value);
            }
        }
    }
    if brush.is_some() {
        return Err(QuakeMapImportError::new(0, "map ended inside a brush"));
    }
    if entity.is_some() {
        return Err(QuakeMapImportError::new(0, "map ended inside an entity"));
    }
    Ok(entities)
}

fn parse_property(line: &str, line_number: usize) -> Result<(String, String), QuakeMapImportError> {
    let (key, rest) = take_quoted(line, line_number)?;
    let (value, trailing) = take_quoted(rest.trim_start(), line_number)?;
    if !trailing.trim().is_empty() {
        return Err(QuakeMapImportError::new(
            line_number,
            "unexpected text after entity property",
        ));
    }
    Ok((key, value))
}

fn take_quoted(input: &str, line_number: usize) -> Result<(String, &str), QuakeMapImportError> {
    let Some(rest) = input.strip_prefix('"') else {
        return Err(QuakeMapImportError::new(
            line_number,
            "expected quoted entity property",
        ));
    };
    let Some(end) = rest.find('"') else {
        return Err(QuakeMapImportError::new(
            line_number,
            "unterminated quoted entity property",
        ));
    };
    Ok((rest[..end].to_string(), &rest[end + 1..]))
}

fn parse_face(line: &str, line_number: usize) -> Result<QuakeFace, QuakeMapImportError> {
    let mut rest = line;
    let mut points = [[0.0; 3]; 3];
    for point in &mut points {
        let (parsed, trailing) = take_point(rest, line_number)?;
        *point = parsed;
        rest = trailing.trim_start();
    }
    let texture = rest
        .split_whitespace()
        .next()
        .filter(|texture| !texture.is_empty())
        .ok_or_else(|| QuakeMapImportError::new(line_number, "face has no texture name"))?;
    Ok(QuakeFace {
        line: line_number,
        points,
        texture: texture.to_string(),
    })
}

fn take_point(input: &str, line_number: usize) -> Result<([f64; 3], &str), QuakeMapImportError> {
    let input = input.trim_start();
    let Some(rest) = input.strip_prefix('(') else {
        return Err(QuakeMapImportError::new(
            line_number,
            "face point must start with '('",
        ));
    };
    let Some(end) = rest.find(')') else {
        return Err(QuakeMapImportError::new(
            line_number,
            "unterminated face point",
        ));
    };
    let values: Vec<f64> = rest[..end]
        .split_whitespace()
        .map(|value| {
            value.parse::<f64>().map_err(|_| {
                QuakeMapImportError::new(line_number, format!("invalid coordinate '{value}'"))
            })
        })
        .collect::<Result<_, _>>()?;
    let point: [f64; 3] = values.try_into().map_err(|values: Vec<f64>| {
        QuakeMapImportError::new(
            line_number,
            format!("face point has {} coordinates, expected 3", values.len()),
        )
    })?;
    Ok((point, &rest[end + 1..]))
}

fn parse_origin(origin: &str) -> Result<[f64; 3], QuakeMapImportError> {
    let values: Vec<f64> = origin
        .split_whitespace()
        .map(|value| {
            value.parse::<f64>().map_err(|_| {
                QuakeMapImportError::new(0, format!("invalid player origin '{origin}'"))
            })
        })
        .collect::<Result<_, _>>()?;
    values.try_into().map_err(|values: Vec<f64>| {
        QuakeMapImportError::new(
            0,
            format!("player origin has {} coordinates, expected 3", values.len()),
        )
    })
}

fn transform_point_checked(
    point: [f64; 3],
    scale: i32,
    line: usize,
) -> Result<[i32; 3], QuakeMapImportError> {
    let converted = [point[0], point[2], -point[1]];
    converted
        .map(|value| {
            let scaled = value * f64::from(scale);
            if !scaled.is_finite()
                || scaled < f64::from(i32::MIN)
                || scaled > f64::from(i32::MAX)
                || (scaled - scaled.round()).abs() > 1.0e-6
            {
                Err(QuakeMapImportError::new(
                    line,
                    format!("coordinate {value} cannot be represented on the editor grid"),
                ))
            } else {
                Ok(scaled.round() as i32)
            }
        })
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map(|values| [values[0], values[1], values[2]])
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE: &str = r#"
{
"classname" "worldspawn"
{
( 0 64 64 ) ( 0 0 64 ) ( 0 0 0 ) STONE 0 0 0 1 1
( 64 64 64 ) ( 0 64 64 ) ( 0 64 0 ) STONE 0 0 0 1 1
( 64 0 64 ) ( 64 64 64 ) ( 64 64 0 ) STONE 0 0 0 1 1
( 0 0 64 ) ( 64 0 64 ) ( 64 0 0 ) STONE 0 0 0 1 1
( 0 0 64 ) ( 0 64 64 ) ( 64 64 64 ) STONE 0 0 0 1 1
( 64 64 0 ) ( 0 64 0 ) ( 0 0 0 ) STONE 0 0 0 1 1
}
}
{
"classname" "info_player_start"
"origin" "32 16 88"
}
"#;

    #[test]
    fn imports_quake_cube_with_y_up_scale_and_outward_winding() {
        let geometry = import_quake_map_geometry(CUBE, Some(ResourceId(7))).unwrap();
        assert_eq!(geometry.stats.source_brushes, 1);
        assert_eq!(geometry.stats.imported_brushes, 1);
        assert_eq!(geometry.stats.imported_faces, 6);
        assert_eq!(geometry.player_start, Some([512, 1408, -256]));
        let solved = geometry.brushes[0].solve();
        assert!(solved.is_valid());
        assert_eq!(solved.min, [0.0, 0.0, -1024.0]);
        assert_eq!(solved.max, [1024.0, 1024.0, 0.0]);
        assert!(geometry.brushes[0]
            .faces
            .iter()
            .all(|face| face.material == Some(ResourceId(7))));
    }

    #[test]
    fn explicit_scale_uniformly_resizes_geometry_and_player_origin() {
        let geometry = import_quake_map_geometry_scaled(CUBE, None, 4).unwrap();
        assert_eq!(geometry.player_start, Some([128, 352, -64]));
        let solved = geometry.brushes[0].solve();
        assert!(solved.is_valid());
        assert_eq!(solved.min, [0.0, 0.0, -256.0]);
        assert_eq!(solved.max, [256.0, 256.0, 0.0]);
    }

    #[test]
    fn explicit_scale_rejects_non_positive_values() {
        let error = import_quake_map_geometry_scaled(CUBE, None, 0).unwrap_err();
        assert_eq!(error.line, 0);
        assert!(error.message.contains("positive"));
    }

    #[test]
    fn imports_func_geometry_but_not_triggers_or_clip_helpers() {
        let source = CUBE.replace(
            "\n}\n{\n\"classname\" \"info_player_start\"",
            r#"
}
{
"classname" "func_door"
{
( 0 0 64 ) ( 0 64 64 ) ( 0 64 0 ) CLIP 0 0 0 1 1
}
}
{
"classname" "trigger_once"
{
( 0 0 64 ) ( 0 64 64 ) ( 0 64 0 ) TRIGGER 0 0 0 1 1
}
}
{
"classname" "info_player_start""#,
        );
        let geometry = import_quake_map_geometry(&source, None).unwrap();
        assert_eq!(geometry.stats.source_brushes, 3);
        assert_eq!(geometry.stats.imported_brushes, 1);
        assert_eq!(geometry.stats.skipped_helper_brushes, 1);
        assert_eq!(geometry.stats.skipped_non_geometry_brushes, 1);
    }

    #[test]
    fn liquid_texture_marks_non_solid_contents() {
        let geometry = import_quake_map_geometry(&CUBE.replace("STONE", "*SLIME0"), None).unwrap();
        assert_eq!(geometry.brushes[0].contents, BrushContents::Slime);
    }
}
