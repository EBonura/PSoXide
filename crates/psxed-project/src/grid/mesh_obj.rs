//! Imported meshes: Wavefront OBJ geometry stored as an asset.
//!
//! The existing importers do not fit a static mesh. `psxed-obj` converts to the
//! PSXM flat-colour format of the model path (no UVs, no materials) and
//! `psxed-gltf` builds rigged models with joints and clips. A grid mesh needs
//! only positions, texels and a material slot per face, so this reads those
//! from an OBJ directly: `v`, `vt`, `f` (triangles and quads kept, larger
//! polygons fanned) and `usemtl` (each new name is the next material slot).
//!
//! Winding is OBJ's, counter-clockwise from the front. Positions are scaled by
//! the import scale into grid units and re-centred so the origin is on the
//! floor at the centre of the base (the asset frame). Texels are `vt * 64`, so
//! one 0..1 unit is one 64 texel tile.

use serde::{Deserialize, Serialize};

use super::mesh::{BuiltMesh, MeshFace, Proxy, MAX_MESH_SLOTS};

/// Collision of an imported mesh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportProxy {
    /// Decoration only.
    #[default]
    None,
    /// A box around the whole mesh, blocking from every side.
    Box,
    /// A box with a standable top.
    BoxTop,
}

/// One face of an imported mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedFace {
    /// Point indices (the fourth unused for a triangle).
    pub v: [u16; 4],
    /// Corners, 3 or 4.
    pub n: u8,
    /// Material slot.
    pub slot: u8,
    /// Texels per corner, material-relative.
    pub uv: [[u8; 2]; 4],
}

/// Imported geometry in the asset frame, grid units.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedMesh {
    /// File it came from (informational).
    #[serde(default)]
    pub source: String,
    /// Points, grid units.
    pub points: Vec<[i16; 3]>,
    /// Faces.
    pub faces: Vec<ImportedFace>,
    /// Material slot names, in first-use order (empty means one slot).
    #[serde(default)]
    pub slot_names: Vec<String>,
    /// Collision.
    #[serde(default)]
    pub proxy: ImportProxy,
}

impl ImportedMesh {
    /// Parse OBJ `text` at `scale` (grid units per OBJ unit).
    pub fn from_obj(
        text: &str,
        scale: f64,
        source: &str,
        proxy: ImportProxy,
    ) -> Result<Self, String> {
        if !(scale.is_finite() && scale > 0.0) {
            return Err("the import scale must be positive".to_string());
        }
        let mut positions: Vec<[f64; 3]> = Vec::new();
        let mut texcoords: Vec<[f64; 2]> = Vec::new();
        let mut slot_names: Vec<String> = Vec::new();
        let mut slot = 0u8;
        // (position indices, texcoord indices, slot) per polygon.
        let mut polygons: Vec<(Vec<usize>, Vec<Option<usize>>, u8)> = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            let mut words = line.split_whitespace();
            let at = || format!("line {}", number + 1);
            match words.next() {
                Some("v") => {
                    let v: Vec<f64> = words.take(3).filter_map(|w| w.parse().ok()).collect();
                    if v.len() != 3 {
                        return Err(format!("{}: a vertex needs x y z", at()));
                    }
                    positions.push([v[0], v[1], v[2]]);
                }
                Some("vt") => {
                    let v: Vec<f64> = words.take(2).filter_map(|w| w.parse().ok()).collect();
                    if v.len() != 2 {
                        return Err(format!("{}: a texture coordinate needs u v", at()));
                    }
                    texcoords.push([v[0], v[1]]);
                }
                Some("usemtl") => {
                    let name = words.next().unwrap_or("default").to_string();
                    slot = match slot_names.iter().position(|n| *n == name) {
                        Some(i) => i as u8,
                        None => {
                            slot_names.push(name);
                            if slot_names.len() > MAX_MESH_SLOTS {
                                return Err(format!(
                                    "{}: more than {MAX_MESH_SLOTS} materials",
                                    at()
                                ));
                            }
                            (slot_names.len() - 1) as u8
                        }
                    };
                }
                Some("f") => {
                    let mut vs = Vec::new();
                    let mut ts = Vec::new();
                    for word in words {
                        let mut parts = word.split('/');
                        let resolve = |s: Option<&str>,
                                       len: usize|
                         -> Result<Option<usize>, String> {
                            match s.filter(|s| !s.is_empty()) {
                                None => Ok(None),
                                Some(s) => {
                                    let i: i64 = s.parse().map_err(|_| {
                                        format!("line {}: bad index {s:?}", number + 1)
                                    })?;
                                    let i = if i < 0 { len as i64 + i } else { i - 1 };
                                    usize::try_from(i)
                                        .ok()
                                        .filter(|i| *i < len)
                                        .map(Some)
                                        .ok_or_else(|| {
                                            format!("line {}: index {s} out of range", number + 1)
                                        })
                                }
                            }
                        };
                        let v = resolve(parts.next(), positions.len())?
                            .ok_or_else(|| format!("{}: a face corner needs a vertex", at()))?;
                        vs.push(v);
                        ts.push(resolve(parts.next(), texcoords.len())?);
                    }
                    if vs.len() < 3 {
                        return Err(format!("{}: a face needs three corners", at()));
                    }
                    polygons.push((vs, ts, slot));
                }
                _ => {}
            }
        }
        if polygons.is_empty() {
            return Err("the OBJ has no faces".to_string());
        }
        let scaled: Vec<[f64; 3]> = positions
            .iter()
            .map(|p| [p[0] * scale, p[1] * scale, p[2] * scale])
            .collect();
        // Re-centre: x and z on the middle of the bounds, y on the floor.
        let used: Vec<usize> = polygons
            .iter()
            .flat_map(|(vs, ..)| vs.iter().copied())
            .collect();
        let lo: [f64; 3] =
            core::array::from_fn(|i| used.iter().map(|&k| scaled[k][i]).fold(f64::MAX, f64::min));
        let hi: [f64; 3] =
            core::array::from_fn(|i| used.iter().map(|&k| scaled[k][i]).fold(f64::MIN, f64::max));
        let shift = [(lo[0] + hi[0]) / 2.0, lo[1], (lo[2] + hi[2]) / 2.0];
        let mut points: Vec<[i16; 3]> = Vec::new();
        let mut remap = vec![u16::MAX; scaled.len()];
        let mut point = |k: usize| -> Result<u16, String> {
            if remap[k] != u16::MAX {
                return Ok(remap[k]);
            }
            let p = scaled[k];
            let q: [i16; 3] = {
                let mut out = [0i16; 3];
                for i in 0..3 {
                    let v = (p[i] - shift[i]).round();
                    out[i] = i16::try_from(v as i64).map_err(|_| {
                        format!(
                            "the mesh is larger than {} grid units at this scale",
                            i16::MAX
                        )
                    })?;
                }
                out
            };
            let id =
                u16::try_from(points.len()).map_err(|_| "more than 65535 points".to_string())?;
            points.push(q);
            remap[k] = id;
            Ok(id)
        };
        let mut faces = Vec::new();
        for (vs, ts, slot) in &polygons {
            // Triangles and quads as they are, larger polygons as a fan.
            let rings: Vec<Vec<usize>> = if vs.len() <= 4 {
                vec![(0..vs.len()).collect()]
            } else {
                (1..vs.len() - 1).map(|i| vec![0, i, i + 1]).collect()
            };
            for ring in rings {
                let mut face = ImportedFace {
                    v: [0; 4],
                    n: ring.len() as u8,
                    slot: *slot,
                    uv: [[0; 2]; 4],
                };
                for (c, &k) in ring.iter().enumerate() {
                    face.v[c] = point(vs[k])?;
                    if let Some(t) = ts[k] {
                        let [u, v] = texcoords[t];
                        face.uv[c] = [
                            (u * 64.0).round().clamp(0.0, 255.0) as u8,
                            ((1.0 - v) * 64.0).round().clamp(0.0, 255.0) as u8,
                        ];
                    }
                }
                faces.push(face);
            }
        }
        Ok(Self {
            source: source.to_string(),
            points,
            faces,
            slot_names,
            proxy,
        })
    }

    /// Bounds of the points, `(min, max)`.
    pub fn bounds(&self) -> ([i32; 3], [i32; 3]) {
        let lo = core::array::from_fn(|i| {
            self.points
                .iter()
                .map(|p| i32::from(p[i]))
                .min()
                .unwrap_or(0)
        });
        let hi = core::array::from_fn(|i| {
            self.points
                .iter()
                .map(|p| i32::from(p[i]))
                .max()
                .unwrap_or(0)
        });
        (lo, hi)
    }

    /// Build the faces and the optional box.
    pub fn build(&self) -> Result<BuiltMesh, String> {
        let mut faces = Vec::with_capacity(self.faces.len());
        for f in &self.faces {
            let mut pts = [[0i32; 3]; 4];
            let mut texels = [[0i32; 2]; 4];
            for c in 0..usize::from(f.n) {
                let p = self
                    .points
                    .get(usize::from(f.v[c]))
                    .ok_or_else(|| "an imported face names a missing point".to_string())?;
                pts[c] = p.map(i32::from);
                texels[c] = f.uv[c].map(i32::from);
            }
            faces.push(MeshFace {
                pts,
                len: f.n.clamp(3, 4),
                texels,
                slot: f.slot,
                double_sided: false,
            });
        }
        let (min, max) = self.bounds();
        let proxies = match self.proxy {
            ImportProxy::None => Vec::new(),
            ImportProxy::Box => vec![Proxy::Box {
                min,
                max,
                top: false,
            }],
            ImportProxy::BoxTop => vec![Proxy::Box {
                min,
                max,
                top: true,
            }],
        };
        let slots = if self.slot_names.is_empty() {
            vec!["surface".to_string()]
        } else {
            self.slot_names.clone()
        };
        Ok(BuiltMesh {
            faces,
            proxies,
            sockets: Vec::new(),
            slots,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE: &str = "\
# a unit cube, two materials
v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
v 0 0 1
v 1 0 1
v 1 1 1
v 0 1 1
vt 0 0
vt 1 0
vt 1 1
vt 0 1
usemtl wall
f 5/1 6/2 7/3 8/4
f 2/1 1/2 4/3 3/4
usemtl floor
f 1/1 2/2 6/3 5/4
f 8/1 7/2 3/3 4/4 # top, a quad
f 1 5 8 4
f 2 3 7 6
";

    #[test]
    fn an_obj_becomes_faces_slots_and_an_origin_on_the_floor() {
        let mesh = ImportedMesh::from_obj(CUBE, 1000.0, "cube.obj", ImportProxy::BoxTop).unwrap();
        assert_eq!(mesh.faces.len(), 6);
        assert_eq!(mesh.slot_names, ["wall", "floor"]);
        let (lo, hi) = mesh.bounds();
        // Centred on x and z, standing on y = 0.
        assert_eq!((lo, hi), ([-500, 0, -500], [500, 1000, 500]));
        let built = mesh.build().unwrap();
        assert_eq!(built.faces.len(), 6);
        assert_eq!(built.slots, ["wall", "floor"]);
        assert!(matches!(built.proxies[0], Proxy::Box { top: true, .. }));
        // Texels are vt * 64, v flipped.
        assert_eq!(built.faces[0].texels[0], [0, 64]);
        assert_eq!(built.faces[0].texels[2], [64, 0]);
    }

    #[test]
    fn polygons_beyond_four_corners_are_fanned_and_negative_indices_work() {
        let text = "v 0 0 0\nv 1 0 0\nv 2 1 0\nv 1 2 0\nv 0 1 0\nf 1 2 3 4 5\nf -1 -2 -3\n";
        let mesh = ImportedMesh::from_obj(text, 100.0, "", ImportProxy::None).unwrap();
        assert_eq!(mesh.faces.len(), 3 + 1);
        assert!(mesh.faces.iter().all(|f| f.n == 3));
    }

    #[test]
    fn bad_files_say_where() {
        assert!(
            ImportedMesh::from_obj("v 1 2\n", 1.0, "", ImportProxy::None)
                .unwrap_err()
                .contains("line 1")
        );
        assert!(
            ImportedMesh::from_obj("v 0 0 0\nf 1 2 3\n", 1.0, "", ImportProxy::None)
                .unwrap_err()
                .contains("out of range")
        );
        assert!(
            ImportedMesh::from_obj("v 0 0 0\n", 1.0, "", ImportProxy::None)
                .unwrap_err()
                .contains("no faces")
        );
        assert!(ImportedMesh::from_obj("v 0 0 0\n", 0.0, "", ImportProxy::None).is_err());
    }

    #[test]
    fn the_document_form_round_trips() {
        let mesh = ImportedMesh::from_obj(CUBE, 1000.0, "cube.obj", ImportProxy::Box).unwrap();
        let text = ron::to_string(&mesh).unwrap();
        assert_eq!(ron::from_str::<ImportedMesh>(&text).unwrap(), mesh);
    }
}
