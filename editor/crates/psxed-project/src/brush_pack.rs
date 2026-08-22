//! PS1 record packing for compiled brush BSP geometry.

use crate::brush::{paraxial_uv, BRUSH_UV_UNITS_PER_TEXEL};
use crate::brush_compile::{
    pack_plane, BspChild, BspLeafContents, CompiledSurface, CompiledSurfaceBsp,
};
use crate::brush_portal::CompiledPortal;
use crate::brush_vis::{quake_portal_fast_rows, quake_portal_flow_rows};
use crate::ResourceId;

use psx_bsp::{
    encode_node_bound_max, encode_node_bound_min, FACE_BACKSIDE, FACE_BAKED_LIGHT,
    FACE_PAGE_LOCAL_UV, FACE_TWO_SIDED,
};
use psx_render_contract::CookedDrawSurface;

const CONTENTS_SOLID: i16 = psx_bsp::collision::CONTENTS_SOLID;
const FULLBRIGHT_RGB: u32 = 0x00ff_ffff;
const MAX_RENDER_FACES: usize = 32_767;
const MAX_FACE_VERTICES: usize = 39;
const MAX_VISIBLE_LEAVES: usize = 8 * 1024;

/// Per-surface vertex colors used by the release light bake.
pub enum BspLighting<'a> {
    /// Fast Play cook with no light pass.
    Fullbright,
    /// Packed RGB24 colors matching every BSP surface vertex.
    Baked(&'a [Vec<u32>]),
}

/// Visibility quality selected by the project cook mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BspVisibility {
    /// Fast, conservative connectivity rows for interactive editor cooks.
    #[default]
    PortalComponents,
    /// Quake-style base-portal `mightsee` rows, equivalent to fast VIS.
    PortalFast,
    /// Original Quake-style directed portal/separator flow for release cooks.
    PortalFlow,
}

/// Runtime record lumps produced from one classified surface BSP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackedBspGeometry {
    pub vertices: Vec<u8>,
    pub planes: Vec<u8>,
    pub faces: Vec<u8>,
    pub mark_surfaces: Vec<u8>,
    pub visibility: Vec<u8>,
    pub leaves: Vec<u8>,
    pub nodes: Vec<u8>,
    /// First-seen material order used by packed face texture indices.
    pub material_slots: Vec<Option<ResourceId>>,
    pub root_node: i16,
    pub visible_leaves: i16,
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
}

/// A cooked BSP value exceeded a fixed XBSP or PS1 representation limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrushPackError {
    EmptyWorld,
    UnclassifiedLeaf(usize),
    LimitExceeded {
        kind: &'static str,
        count: usize,
        max: usize,
    },
    NonFiniteVertex {
        surface: usize,
        vertex: usize,
    },
    VertexOutOfRange {
        surface: usize,
        vertex: usize,
        axis: usize,
        rounded: i64,
    },
    BadLightingSurfaceCount {
        expected: usize,
        found: usize,
    },
    BadLightingVertexCount {
        surface: usize,
        expected: usize,
        found: usize,
    },
    InvalidPlane(usize),
}

/// Pack a classified BSP into the record layouts read by `psx-bsp`.
pub fn pack_bsp_geometry(
    bsp: &CompiledSurfaceBsp,
    portals: &[CompiledPortal],
    lighting: BspLighting<'_>,
    texture_dims: &std::collections::HashMap<Option<crate::ResourceId>, [u16; 2]>,
) -> Result<PackedBspGeometry, BrushPackError> {
    pack_bsp_geometry_with_visibility(
        bsp,
        portals,
        lighting,
        BspVisibility::PortalComponents,
        texture_dims,
    )
}

/// Pack geometry with an explicit visibility quality.
pub fn pack_bsp_geometry_with_visibility(
    bsp: &CompiledSurfaceBsp,
    portals: &[CompiledPortal],
    lighting: BspLighting<'_>,
    visibility_mode: BspVisibility,
    texture_dims: &std::collections::HashMap<Option<crate::ResourceId>, [u16; 2]>,
) -> Result<PackedBspGeometry, BrushPackError> {
    validate_limits(bsp)?;
    validate_lighting(bsp, &lighting)?;

    let mut plane_records: Vec<[u8; 14]> = Vec::new();
    let mut node_plane_indices = Vec::with_capacity(bsp.nodes.len());
    let mut node_plane_flipped = Vec::with_capacity(bsp.nodes.len());
    for (node_index, node) in bsp.nodes.iter().enumerate() {
        let (record, flipped) =
            pack_plane(&node.plane).ok_or(BrushPackError::InvalidPlane(node_index))?;
        node_plane_indices.push(intern_plane(&mut plane_records, record)?);
        node_plane_flipped.push(flipped);
    }

    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    let mut material_slots = Vec::new();
    for (surface_index, surface) in bsp.surfaces.iter().enumerate() {
        let first_vertex = vertices.len() / 12;
        let (plane_record, plane_flipped) =
            pack_plane(&surface.plane).ok_or(BrushPackError::InvalidPlane(surface_index))?;
        let plane_index = intern_plane(&mut plane_records, plane_record)?;
        let texture_index = intern_material(&mut material_slots, surface.material)?;
        // Whole-surface texel UVs, rebased by whole texture repeats so
        // no polygon straddles the u8 wrap (a straddling surface
        // rasterizes a backwards texture gradient in-game). Surfaces
        // whose material has no known dimensions keep the historic
        // per-vertex wrap.
        let mut surface_uvs: Vec<[f64; 2]> = surface
            .vertices
            .iter()
            .map(|&vertex| {
                let raw_uv = paraxial_uv(&surface.plane, vertex);
                surface.uv.apply([
                    raw_uv[0] / BRUSH_UV_UNITS_PER_TEXEL,
                    raw_uv[1] / BRUSH_UV_UNITS_PER_TEXEL,
                ])
            })
            .collect();
        let page_local_uv = if let Some(dims) = texture_dims.get(&surface.material) {
            crate::brush::rebase_texel_uvs(
                &mut surface_uvs,
                [f64::from(dims[0].max(1)), f64::from(dims[1].max(1))],
            );
            surface_uvs.iter().all(|uv| {
                uv.iter().zip(dims).all(|(&value, &size)| {
                    value.is_finite()
                        && value.round() >= 0.0
                        && value.round() < f64::from(size.max(1))
                })
            })
        } else {
            false
        };
        for (vertex_index, vertex) in surface.vertices.iter().copied().enumerate() {
            pack_vertex(
                &mut vertices,
                surface_index,
                vertex_index,
                vertex,
                surface_uvs[vertex_index],
                vertex_light(&lighting, surface_index, vertex_index),
            )?;
        }
        let flags = FACE_BAKED_LIGHT
            | if page_local_uv { FACE_PAGE_LOCAL_UV } else { 0 }
            | if plane_flipped { FACE_BACKSIDE } else { 0 }
            | if surface.contents.is_solid() {
                0
            } else {
                FACE_TWO_SIDED
            };
        faces.extend_from_slice(
            &CookedDrawSurface {
                plane: plane_index as u16,
                first_corner: first_vertex as u16,
                material: texture_index as u16,
                flags: flags as u8,
                corner_count: surface.vertices.len() as u8,
                light_styles: [0, 64],
            }
            .encode(),
        );
    }

    let (leaf_mapping, visible_leaves) = runtime_leaf_mapping(bsp)?;
    let (visibility, visibility_offsets) = match visibility_mode {
        BspVisibility::PortalComponents => {
            portal_component_visibility(bsp, portals, &leaf_mapping, visible_leaves)
        }
        BspVisibility::PortalFast => {
            portal_fast_visibility(bsp, portals, &leaf_mapping, visible_leaves)
        }
        BspVisibility::PortalFlow => {
            portal_flow_visibility(bsp, portals, &leaf_mapping, visible_leaves)
        }
    };
    limit("visibility", visibility.len(), u16::MAX as usize)?;
    let mut mark_surfaces = Vec::new();
    let mut leaves = Vec::new();
    pack_leaf_record(&mut leaves, CONTENTS_SOLID, -1, 0, 0)?;
    for (host_leaf, leaf) in bsp.leaves.iter().enumerate() {
        if !leaf.contents.is_visible() {
            continue;
        }
        let first_mark = mark_surfaces.len() / 2;
        for &surface in &leaf.mark_surfaces {
            push_u16(&mut mark_surfaces, surface as u16);
        }
        pack_leaf_record(
            &mut leaves,
            leaf.contents.runtime_contents(),
            visibility_offsets[host_leaf],
            first_mark as u16,
            leaf.mark_surfaces.len() as u16,
        )?;
    }

    let node_bounds = node_render_bounds(bsp);
    let mut nodes = Vec::new();
    for (node_index, node) in bsp.nodes.iter().enumerate() {
        push_u16(&mut nodes, node_plane_indices[node_index] as u16);
        let mut children = [
            pack_child(node.front, &leaf_mapping)?,
            pack_child(node.back, &leaf_mapping)?,
        ];
        if node_plane_flipped[node_index] {
            children.swap(0, 1);
        }
        push_i16(&mut nodes, children[0]);
        push_i16(&mut nodes, children[1]);
        let (mins, maxs) = node_bounds[node_index].packed();
        for value in mins {
            nodes.push(encode_node_bound_min(value) as u8);
        }
        for value in maxs {
            nodes.push(encode_node_bound_max(value) as u8);
        }
        push_u16(&mut nodes, node.first_surface as u16);
        push_u16(&mut nodes, node.surface_count as u16);
    }

    let root_node = match bsp.root {
        BspChild::Node(index) => index as i16,
        BspChild::Leaf(_) => return Err(BrushPackError::EmptyWorld),
    };
    let world_bounds = surface_bounds(&bsp.surfaces);
    Ok(PackedBspGeometry {
        vertices,
        planes: plane_records.into_iter().flatten().collect(),
        faces,
        mark_surfaces,
        visibility,
        leaves,
        nodes,
        material_slots,
        root_node,
        visible_leaves: visible_leaves as i16,
        mins: world_bounds.0,
        maxs: world_bounds.1,
    })
}

fn portal_flow_visibility(
    bsp: &CompiledSurfaceBsp,
    portals: &[CompiledPortal],
    leaf_mapping: &[i16],
    visible_leaves: usize,
) -> (Vec<u8>, Vec<i32>) {
    let rows = quake_portal_flow_rows(bsp, portals, leaf_mapping, visible_leaves);
    pack_visibility_rows(bsp, rows)
}

fn portal_fast_visibility(
    bsp: &CompiledSurfaceBsp,
    portals: &[CompiledPortal],
    leaf_mapping: &[i16],
    visible_leaves: usize,
) -> (Vec<u8>, Vec<i32>) {
    let rows = quake_portal_fast_rows(bsp, portals, leaf_mapping, visible_leaves);
    pack_visibility_rows(bsp, rows)
}

fn pack_visibility_rows(
    bsp: &CompiledSurfaceBsp,
    rows: Vec<Option<Vec<u8>>>,
) -> (Vec<u8>, Vec<i32>) {
    let mut visibility = Vec::new();
    let mut offsets = vec![-1; bsp.leaves.len()];
    let mut interned = std::collections::HashMap::<Vec<u8>, i32>::new();
    for (host_leaf, row) in rows.into_iter().enumerate() {
        let Some(row) = row else {
            continue;
        };
        let compressed = compress_visibility(&row);
        let offset = if let Some(&offset) = interned.get(&compressed) {
            offset
        } else {
            let offset = visibility.len() as i32;
            visibility.extend_from_slice(&compressed);
            interned.insert(compressed, offset);
            offset
        };
        offsets[host_leaf] = offset;
    }
    (visibility, offsets)
}

fn validate_limits(bsp: &CompiledSurfaceBsp) -> Result<(), BrushPackError> {
    limit("nodes", bsp.nodes.len(), i16::MAX as usize + 1)?;
    limit("leaves", bsp.leaves.len() + 1, i16::MAX as usize + 1)?;
    limit("faces", bsp.surfaces.len(), MAX_RENDER_FACES)?;
    let vertices = bsp
        .surfaces
        .iter()
        .map(|surface| surface.vertices.len())
        .sum();
    // Compact Face stores first_vertex as u16; its public i32 semantic field
    // does not reduce that physical domain.
    limit("vertices", vertices, u16::MAX as usize)?;
    for (surface, compiled) in bsp.surfaces.iter().enumerate() {
        limit("face vertices", compiled.vertices.len(), MAX_FACE_VERTICES)?;
        if compiled.vertices.len() < 3 {
            return Err(BrushPackError::LimitExceeded {
                kind: "face vertices",
                count: compiled.vertices.len(),
                max: MAX_FACE_VERTICES,
            });
        }
        if surface > u16::MAX as usize {
            return Err(BrushPackError::LimitExceeded {
                kind: "mark surface index",
                count: surface,
                max: u16::MAX as usize,
            });
        }
    }
    for (index, leaf) in bsp.leaves.iter().enumerate() {
        if leaf.contents == BspLeafContents::Unclassified {
            return Err(BrushPackError::UnclassifiedLeaf(index));
        }
        limit(
            "leaf mark surfaces",
            leaf.mark_surfaces.len(),
            u16::MAX as usize,
        )?;
    }
    for node in &bsp.nodes {
        let end = node.first_surface.saturating_add(node.surface_count);
        limit("node first face", node.first_surface, u16::MAX as usize)?;
        limit("node faces", node.surface_count, u16::MAX as usize)?;
        if end > bsp.surfaces.len() {
            return Err(BrushPackError::LimitExceeded {
                kind: "node face range",
                count: end,
                max: bsp.surfaces.len(),
            });
        }
    }
    let mark_surfaces = bsp
        .leaves
        .iter()
        .filter(|leaf| leaf.contents.is_visible())
        .map(|leaf| leaf.mark_surfaces.len())
        .sum();
    limit("mark surfaces", mark_surfaces, u16::MAX as usize)?;
    Ok(())
}

fn validate_lighting(
    bsp: &CompiledSurfaceBsp,
    lighting: &BspLighting<'_>,
) -> Result<(), BrushPackError> {
    let BspLighting::Baked(colors) = lighting else {
        return Ok(());
    };
    if colors.len() != bsp.surfaces.len() {
        return Err(BrushPackError::BadLightingSurfaceCount {
            expected: bsp.surfaces.len(),
            found: colors.len(),
        });
    }
    for (surface, (colors, geometry)) in colors.iter().zip(&bsp.surfaces).enumerate() {
        if colors.len() != geometry.vertices.len() {
            return Err(BrushPackError::BadLightingVertexCount {
                surface,
                expected: geometry.vertices.len(),
                found: colors.len(),
            });
        }
    }
    Ok(())
}

fn limit(kind: &'static str, count: usize, max: usize) -> Result<(), BrushPackError> {
    if count > max {
        Err(BrushPackError::LimitExceeded { kind, count, max })
    } else {
        Ok(())
    }
}

fn intern_plane(planes: &mut Vec<[u8; 14]>, record: [u8; 14]) -> Result<i16, BrushPackError> {
    let index = planes
        .iter()
        .position(|plane| *plane == record)
        .unwrap_or_else(|| {
            let index = planes.len();
            planes.push(record);
            index
        });
    limit("planes", index + 1, i16::MAX as usize + 1)?;
    Ok(index as i16)
}

fn intern_material(
    materials: &mut Vec<Option<ResourceId>>,
    material: Option<ResourceId>,
) -> Result<i16, BrushPackError> {
    let index = materials
        .iter()
        .position(|slot| *slot == material)
        .unwrap_or_else(|| {
            let index = materials.len();
            materials.push(material);
            index
        });
    limit("materials", index + 1, i16::MAX as usize + 1)?;
    Ok(index as i16)
}

fn pack_vertex(
    output: &mut Vec<u8>,
    surface_index: usize,
    vertex_index: usize,
    vertex: [f64; 3],
    uv: [f64; 2],
    light: u32,
) -> Result<(), BrushPackError> {
    if !vertex.into_iter().all(f64::is_finite) {
        return Err(BrushPackError::NonFiniteVertex {
            surface: surface_index,
            vertex: vertex_index,
        });
    }
    for (axis, value) in vertex.into_iter().enumerate() {
        let rounded = value.round() as i64;
        if !(i16::MIN as i64..=i16::MAX as i64).contains(&rounded) {
            return Err(BrushPackError::VertexOutOfRange {
                surface: surface_index,
                vertex: vertex_index,
                axis,
                rounded,
            });
        }
        push_i16(output, rounded as i16);
    }
    if !uv.into_iter().all(f64::is_finite) {
        return Err(BrushPackError::NonFiniteVertex {
            surface: surface_index,
            vertex: vertex_index,
        });
    }
    output.extend(uv.map(|value| (value.round() as i64).rem_euclid(256) as u8));
    output.extend_from_slice(&(light & 0x00ff_ffff).to_le_bytes());
    Ok(())
}

fn vertex_light(lighting: &BspLighting<'_>, surface: usize, vertex: usize) -> u32 {
    match lighting {
        BspLighting::Fullbright => FULLBRIGHT_RGB,
        BspLighting::Baked(colors) => colors[surface][vertex],
    }
}

fn runtime_leaf_mapping(bsp: &CompiledSurfaceBsp) -> Result<(Vec<i16>, usize), BrushPackError> {
    let mut next_empty = 1usize;
    let mut mapping = Vec::with_capacity(bsp.leaves.len());
    for leaf in &bsp.leaves {
        let runtime = match leaf.contents {
            BspLeafContents::Solid => 0,
            BspLeafContents::Empty
            | BspLeafContents::Water
            | BspLeafContents::Slime
            | BspLeafContents::Lava => {
                let index = next_empty;
                next_empty += 1;
                index
            }
            BspLeafContents::Unclassified => unreachable!("validated above"),
        };
        mapping.push(runtime as i16);
    }
    limit("visible leaves", next_empty - 1, MAX_VISIBLE_LEAVES)?;
    Ok((mapping, next_empty - 1))
}

fn pack_child(child: BspChild, leaf_mapping: &[i16]) -> Result<i16, BrushPackError> {
    match child {
        BspChild::Node(index) => {
            limit("node child", index, i16::MAX as usize)?;
            Ok(index as i16)
        }
        BspChild::Leaf(index) => Ok(-1 - leaf_mapping[index]),
    }
}

/// Build a conservative first-stage PVS from the exact empty-leaf portal
/// graph. Every empty leaf in the same portal-connected component remains
/// visible; sealed components are omitted. This deliberately stops short of
/// portal-frustum flow, but unlike the former all-visible draft row it gives
/// sealed rooms and disconnected world volumes independent visibility sets
/// without risking through-an-open-portal false negatives.
fn portal_component_visibility(
    bsp: &CompiledSurfaceBsp,
    portals: &[CompiledPortal],
    leaf_mapping: &[i16],
    visible_leaves: usize,
) -> (Vec<u8>, Vec<i32>) {
    let mut adjacency = vec![Vec::new(); bsp.leaves.len()];
    for portal in portals {
        if !bsp.leaves[portal.front_leaf].contents.is_visible()
            || !bsp.leaves[portal.back_leaf].contents.is_visible()
        {
            continue;
        }
        adjacency[portal.front_leaf].push(portal.back_leaf);
        adjacency[portal.back_leaf].push(portal.front_leaf);
    }

    let row_bytes = visible_leaves.div_ceil(8);
    let mut visibility = Vec::new();
    let mut offsets = vec![-1; bsp.leaves.len()];
    let mut component = Vec::new();
    let mut pending = Vec::new();

    for root in 0..bsp.leaves.len() {
        if !bsp.leaves[root].contents.is_visible() || offsets[root] >= 0 {
            continue;
        }

        component.clear();
        pending.clear();
        pending.push(root);
        // Mark membership while discovering so cycles and duplicate portals
        // cannot enqueue the same leaf repeatedly. `i32::MAX` is only a host
        // scratch sentinel and never reaches the cooked output.
        offsets[root] = i32::MAX;
        while let Some(leaf) = pending.pop() {
            component.push(leaf);
            for &adjacent in &adjacency[leaf] {
                if offsets[adjacent] < 0 {
                    offsets[adjacent] = i32::MAX;
                    pending.push(adjacent);
                }
            }
        }

        let mut row = vec![0; row_bytes];
        for &host_leaf in &component {
            let runtime_leaf = leaf_mapping[host_leaf];
            debug_assert!(runtime_leaf > 0);
            let visible_index = runtime_leaf as usize - 1;
            row[visible_index >> 3] |= 1 << (visible_index & 7);
        }
        let offset = visibility.len() as i32;
        visibility.extend_from_slice(&compress_visibility(&row));
        for &host_leaf in &component {
            offsets[host_leaf] = offset;
        }
    }

    (visibility, offsets)
}

fn compress_visibility(row: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut cursor = 0;
    while cursor < row.len() {
        if row[cursor] != 0 {
            output.push(row[cursor]);
            cursor += 1;
            continue;
        }
        let start = cursor;
        while cursor < row.len() && row[cursor] == 0 && cursor - start < u8::MAX as usize {
            cursor += 1;
        }
        output.extend_from_slice(&[0, (cursor - start) as u8]);
    }
    output
}

fn surface_bounds(surfaces: &[CompiledSurface]) -> ([i16; 3], [i16; 3]) {
    Bounds::from_surfaces(surfaces).packed()
}

fn node_render_bounds(bsp: &CompiledSurfaceBsp) -> Vec<Bounds> {
    let leaf_bounds: Vec<_> = bsp
        .leaves
        .iter()
        .map(|leaf| {
            let mut bounds = Bounds::empty();
            for &surface in &leaf.mark_surfaces {
                if let Some(surface) = bsp.surfaces.get(surface) {
                    bounds.include_bounds(Bounds::from_surfaces(core::slice::from_ref(surface)));
                }
            }
            bounds
        })
        .collect();
    let mut output = vec![Bounds::empty(); bsp.nodes.len()];
    for (index, node) in bsp.nodes.iter().enumerate().rev() {
        let end = node.first_surface + node.surface_count;
        let mut bounds = Bounds::from_surfaces(&bsp.surfaces[node.first_surface..end]);
        for child in [node.front, node.back] {
            match child {
                BspChild::Node(child) => bounds.include_bounds(output[child]),
                BspChild::Leaf(child) => bounds.include_bounds(leaf_bounds[child]),
            }
        }
        output[index] = bounds;
    }
    output
}

#[derive(Clone, Copy)]
struct Bounds {
    min: [f64; 3],
    max: [f64; 3],
}

impl Bounds {
    fn empty() -> Self {
        Self {
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
        }
    }

    fn from_surfaces(surfaces: &[CompiledSurface]) -> Self {
        let mut bounds = Self::empty();
        for surface in surfaces {
            for &vertex in &surface.vertices {
                bounds.include(vertex);
            }
        }
        bounds
    }

    fn include(&mut self, vertex: [f64; 3]) {
        for (axis, value) in vertex.into_iter().enumerate() {
            self.min[axis] = self.min[axis].min(value);
            self.max[axis] = self.max[axis].max(value);
        }
    }

    fn include_bounds(&mut self, other: Self) {
        if !other.min[0].is_finite() {
            return;
        }
        self.include(other.min);
        self.include(other.max);
    }

    fn packed(self) -> ([i16; 3], [i16; 3]) {
        if !self.min[0].is_finite() {
            return ([0; 3], [0; 3]);
        }
        (
            self.min.map(|value| clamp_i16(value.floor())),
            self.max.map(|value| clamp_i16(value.ceil())),
        )
    }
}

fn clamp_i16(value: f64) -> i16 {
    value.clamp(i16::MIN as f64, i16::MAX as f64) as i16
}

fn pack_leaf_record(
    output: &mut Vec<u8>,
    contents: i16,
    visibility_offset: i32,
    first_mark: u16,
    mark_count: u16,
) -> Result<(), BrushPackError> {
    let contents = i8::try_from(contents).map_err(|_| BrushPackError::LimitExceeded {
        kind: "leaf contents",
        count: contents as usize,
        max: i8::MAX as usize,
    })?;
    if visibility_offset < -1 {
        return Err(BrushPackError::LimitExceeded {
            kind: "visibility offset",
            count: 0,
            max: i32::MAX as usize,
        });
    }
    // Compact leaf: contents i8, pad, mark count u16, visibility offset i32
    // (-1 = none), first mark u16, lightmap, light styles.
    output.push(contents as u8);
    output.push(0);
    push_u16(output, mark_count);
    output.extend_from_slice(&visibility_offset.to_le_bytes());
    push_u16(output, first_mark);
    output.extend_from_slice(&[0, 0, 0, 64]);
    Ok(())
}

fn push_i16(output: &mut Vec<u8>, value: i16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::{Brush, BrushContents};
    use crate::brush_compile::{build_surface_bsp, compile_csg_surfaces};
    use crate::brush_portal::{classify_bsp_leaves, point_leaf_index, portalize_surface_bsp};
    use psx_bsp::{Face, Leaf, Node, Plane, RecordSlice, Vertex};

    fn packed(brushes: &[Brush]) -> (CompiledSurfaceBsp, PackedBspGeometry) {
        let surfaces = compile_csg_surfaces(brushes);
        let mut bsp = build_surface_bsp(&surfaces);
        let portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &portals, brushes);
        let packed =
            pack_bsp_geometry(&bsp, &portals, BspLighting::Fullbright, &Default::default())
                .expect("pack");
        (bsp, packed)
    }

    #[test]
    fn straddling_surface_uvs_rebase_onto_the_u8_window() {
        // World x/z 3968..4224 = texels 248..264: every big face
        // straddles the 256 wrap. Packed without texture dimensions the
        // wrap survives (some face spans nearly the whole u8 range);
        // with 64x64 dims each face's UVs rebase into one contiguous
        // window so the GPU never rasterizes a backwards gradient.
        let brushes = [Brush::cuboid([3968, 0, 3968], [4224, 128, 4224])];
        let surfaces = compile_csg_surfaces(&brushes);
        let mut bsp = build_surface_bsp(&surfaces);
        let portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &portals, &brushes);

        let face_uv_spans = |packed: &PackedBspGeometry| -> Vec<[i32; 2]> {
            let vertices = RecordSlice::<Vertex>::new(&packed.vertices).expect("vertices");
            let faces = RecordSlice::<Face>::new(&packed.faces).expect("faces");
            (0..faces.len())
                .map(|index| {
                    let face = faces.get(index).expect("face");
                    let mut min = [i32::MAX; 2];
                    let mut max = [i32::MIN; 2];
                    for offset in 0..face.vertex_count {
                        let vertex = vertices
                            .get((face.first_vertex + i32::from(offset)) as usize)
                            .expect("vertex");
                        let uv = [i32::from(vertex.texture.x), i32::from(vertex.texture.y)];
                        for axis in 0..2 {
                            min[axis] = min[axis].min(uv[axis]);
                            max[axis] = max[axis].max(uv[axis]);
                        }
                    }
                    [max[0] - min[0], max[1] - min[1]]
                })
                .collect()
        };

        let wrapped =
            pack_bsp_geometry(&bsp, &portals, BspLighting::Fullbright, &Default::default())
                .expect("pack");
        assert!(
            face_uv_spans(&wrapped)
                .iter()
                .any(|span| span[0] > 128 || span[1] > 128),
            "control: without dims the straddle wraps"
        );

        let mut dims = std::collections::HashMap::new();
        dims.insert(None, [64u16, 64u16]);
        let rebased =
            pack_bsp_geometry(&bsp, &portals, BspLighting::Fullbright, &dims).expect("pack");
        for span in face_uv_spans(&rebased) {
            assert!(
                span[0] <= 64 && span[1] <= 64,
                "rebased face UVs must stay contiguous: span {span:?}"
            );
        }
    }

    #[test]
    fn faces_inside_one_texture_copy_are_marked_page_local() {
        // Offset from a repeat seam so every signed paraxial axis remains
        // strictly inside one 64x64 copy after whole-repeat rebasing.
        let brushes = [Brush::cuboid([16, 16, 16], [144, 80, 272])];
        let surfaces = compile_csg_surfaces(&brushes);
        let mut bsp = build_surface_bsp(&surfaces);
        let portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &portals, &brushes);
        let mut dims = std::collections::HashMap::new();
        dims.insert(None, [64u16, 64u16]);
        let packed =
            pack_bsp_geometry(&bsp, &portals, BspLighting::Fullbright, &dims).expect("pack");
        let faces = RecordSlice::<Face>::new(&packed.faces).expect("faces");
        assert!(faces
            .iter()
            .all(|face| face.flags & FACE_PAGE_LOCAL_UV != 0));
    }

    #[test]
    fn cuboid_packs_checked_runtime_records() {
        let (bsp, packed) = packed(&[Brush::cuboid([0, 0, 0], [128, 64, 256])]);
        let vertices = RecordSlice::<Vertex>::new(&packed.vertices).expect("vertices");
        let planes = RecordSlice::<Plane>::new(&packed.planes).expect("planes");
        let faces = RecordSlice::<Face>::new(&packed.faces).expect("faces");
        let leaves = RecordSlice::<Leaf>::new(&packed.leaves).expect("leaves");
        let nodes = RecordSlice::<Node>::new(&packed.nodes).expect("nodes");
        assert_eq!(vertices.len(), 24);
        assert_eq!(faces.len(), bsp.surfaces.len());
        assert_eq!(nodes.len(), bsp.nodes.len());
        for (index, source) in bsp.nodes.iter().enumerate() {
            let node = nodes.get(index).expect("node");
            assert_eq!(usize::from(node.first_face), source.first_surface);
            assert_eq!(usize::from(node.face_count), source.surface_count);
            assert!(node.mins.x <= node.maxs.x);
            assert!(node.mins.y <= node.maxs.y);
            assert!(node.mins.z <= node.maxs.z);
        }
        let BspChild::Node(root) = bsp.root else {
            panic!("cuboid has a render root");
        };
        let root = nodes.get(root).expect("root node");
        for axis in 0..3 {
            let mins = [root.mins.x, root.mins.y, root.mins.z];
            let maxs = [root.maxs.x, root.maxs.y, root.maxs.z];
            assert!(mins[axis] <= packed.mins[axis]);
            assert!(maxs[axis] >= packed.maxs[axis]);
        }
        assert_eq!(leaves.get(0).expect("solid leaf").contents, CONTENTS_SOLID);
        assert!(planes.len() <= bsp.nodes.len() + bsp.surfaces.len());
        assert_eq!(packed.mins, [0, 0, 0]);
        assert_eq!(packed.maxs, [128, 64, 256]);
    }

    #[test]
    fn solid_host_leaves_share_runtime_leaf_zero() {
        let brushes = [
            Brush::cuboid([0, 0, 0], [128, 128, 128]),
            Brush::cuboid([512, 0, 0], [640, 128, 128]),
        ];
        let (bsp, packed) = packed(&brushes);
        let solid_count = bsp
            .leaves
            .iter()
            .filter(|leaf| leaf.contents == BspLeafContents::Solid)
            .count();
        assert_eq!(solid_count, 2);
        let leaves = RecordSlice::<Leaf>::new(&packed.leaves).expect("leaves");
        assert_eq!(
            leaves
                .iter()
                .filter(|leaf| leaf.contents == CONTENTS_SOLID)
                .count(),
            1
        );
    }

    #[test]
    fn liquid_leaf_codes_and_visibility_survive_runtime_packing() {
        for (contents, expected) in [
            (BrushContents::Water, psx_bsp::collision::CONTENTS_WATER),
            (BrushContents::Slime, psx_bsp::collision::CONTENTS_SLIME),
            (BrushContents::Lava, psx_bsp::collision::CONTENTS_LAVA),
        ] {
            let mut brush = Brush::cuboid([0, 0, 0], [128, 64, 256]);
            brush.contents = contents;
            let (bsp, packed) = packed(&[brush]);
            let host_leaf = point_leaf_index(&bsp, [64.0, 32.0, 128.0]);
            assert_eq!(
                bsp.leaves[host_leaf].contents,
                BspLeafContents::from_brush(contents)
            );
            let (mapping, _) = runtime_leaf_mapping(&bsp).expect("mapping");
            let runtime_leaf = mapping[host_leaf];
            assert!(runtime_leaf > 0);
            let leaves = RecordSlice::<Leaf>::new(&packed.leaves).expect("leaves");
            let leaf = leaves.get(runtime_leaf as usize).expect("liquid leaf");
            assert_eq!(leaf.contents, expected);
            assert!(leaf.visibility_offset >= 0);
            let faces = RecordSlice::<Face>::new(&packed.faces).expect("faces");
            assert!(faces.iter().all(|face| face.flags & FACE_TWO_SIDED != 0));
        }
    }

    #[test]
    fn nested_and_partial_liquid_precedence_survives_render_bsp_and_packing() {
        let mut water = Brush::cuboid([0, 0, 0], [512, 256, 512]);
        water.contents = BrushContents::Water;
        let mut slime = Brush::cuboid([64, 32, 64], [448, 224, 448]);
        slime.contents = BrushContents::Slime;
        let mut lava = Brush::cuboid([128, 64, 128], [384, 192, 384]);
        lava.contents = BrushContents::Lava;
        for brushes in [
            vec![water.clone(), slime.clone(), lava.clone()],
            vec![lava.clone(), water.clone(), slime.clone()],
            vec![slime.clone(), lava.clone(), water.clone()],
        ] {
            assert_packed_contents(
                &brushes,
                [32.0, 128.0, 32.0],
                psx_bsp::collision::CONTENTS_WATER,
            );
            assert_packed_contents(
                &brushes,
                [96.0, 128.0, 96.0],
                psx_bsp::collision::CONTENTS_SLIME,
            );
            assert_packed_contents(
                &brushes,
                [256.0, 128.0, 256.0],
                psx_bsp::collision::CONTENTS_LAVA,
            );
        }

        let mut partial_water = Brush::cuboid([0, 0, 0], [256, 256, 256]);
        partial_water.contents = BrushContents::Water;
        let mut partial_lava = Brush::cuboid([64, 0, 64], [384, 256, 384]);
        partial_lava.contents = BrushContents::Lava;
        for brushes in [
            vec![partial_water.clone(), partial_lava.clone()],
            vec![partial_lava.clone(), partial_water.clone()],
        ] {
            assert_packed_contents(
                &brushes,
                [32.0, 128.0, 32.0],
                psx_bsp::collision::CONTENTS_WATER,
            );
            assert_packed_contents(
                &brushes,
                [128.0, 128.0, 128.0],
                psx_bsp::collision::CONTENTS_LAVA,
            );
            assert_packed_contents(
                &brushes,
                [320.0, 128.0, 320.0],
                psx_bsp::collision::CONTENTS_LAVA,
            );
            assert_packed_contents(
                &brushes,
                [448.0, 128.0, 64.0],
                psx_bsp::collision::CONTENTS_EMPTY,
            );
            let (bsp, _) = packed(&brushes);
            let water_index = brushes
                .iter()
                .position(|brush| brush.contents == BrushContents::Water)
                .expect("water brush");
            for surface in bsp
                .surfaces
                .iter()
                .filter(|surface| surface.source_brush == water_index)
            {
                let inverse_count = 1.0 / surface.vertices.len() as f64;
                let centroid = surface.vertices.iter().fold([0.0; 3], |mut sum, vertex| {
                    for axis in 0..3 {
                        sum[axis] += vertex[axis] * inverse_count;
                    }
                    sum
                });
                let buried_by_lava = (64.0..=384.0).contains(&centroid[0])
                    && (0.0..=256.0).contains(&centroid[1])
                    && (64.0..=384.0).contains(&centroid[2]);
                assert!(
                    !buried_by_lava,
                    "weaker coplanar Water face survived under Lava: {centroid:?}"
                );
            }
        }

        let mut outer_lava = Brush::cuboid([0, 0, 0], [512, 256, 512]);
        outer_lava.contents = BrushContents::Lava;
        let mut inner_water = Brush::cuboid([128, 64, 128], [384, 192, 384]);
        inner_water.contents = BrushContents::Water;
        for brushes in [
            vec![outer_lava.clone(), inner_water.clone()],
            vec![inner_water.clone(), outer_lava.clone()],
        ] {
            let (bsp, _) = packed(&brushes);
            for point in [[32.0, 128.0, 32.0], [256.0, 128.0, 256.0]] {
                let host_leaf = point_leaf_index(&bsp, point);
                assert_eq!(
                    bsp.leaves[host_leaf].contents,
                    BspLeafContents::Lava,
                    "a weaker nested liquid must not create a false transition"
                );
            }
            assert!(bsp
                .leaves
                .iter()
                .all(|leaf| leaf.contents != BspLeafContents::Water));
        }
    }

    fn assert_packed_contents(brushes: &[Brush], point: [f64; 3], expected: i16) {
        let (bsp, packed) = packed(brushes);
        let host_leaf = point_leaf_index(&bsp, point);
        assert_eq!(
            bsp.leaves[host_leaf].contents.runtime_contents(),
            expected,
            "host render leaf at {point:?}, brush order {:?}",
            brushes
                .iter()
                .map(|brush| brush.contents)
                .collect::<Vec<_>>()
        );
        let (mapping, _) = runtime_leaf_mapping(&bsp).expect("leaf mapping");
        let leaves = RecordSlice::<Leaf>::new(&packed.leaves).expect("leaves");
        assert_eq!(
            leaves
                .get(mapping[host_leaf] as usize)
                .expect("packed point leaf")
                .contents,
            expected
        );
    }

    #[test]
    fn portal_component_pvs_marks_every_leaf_in_one_open_component() {
        let (_, packed) = packed(&[Brush::cuboid([0, 0, 0], [128, 64, 256])]);
        let row_bytes = (packed.visible_leaves as usize).div_ceil(8);
        assert_eq!(packed.visibility.len(), row_bytes);
        for leaf in 0..packed.visible_leaves as usize {
            assert_ne!(packed.visibility[leaf >> 3] & (1 << (leaf & 7)), 0);
        }
        let leaves = RecordSlice::<Leaf>::new(&packed.leaves).expect("leaves");
        assert!(leaves
            .iter()
            .skip(1)
            .all(|leaf| leaf.visibility_offset == 0));
    }

    #[test]
    fn portal_component_pvs_separates_sealed_room_interiors() {
        let mut brushes = Brush::cuboid([0, 0, 0], [256, 256, 256])
            .hollow(32)
            .expect("first room");
        brushes.extend(
            Brush::cuboid([512, 0, 0], [768, 256, 256])
                .hollow(32)
                .expect("second room"),
        );
        let surfaces = compile_csg_surfaces(&brushes);
        let mut bsp = build_surface_bsp(&surfaces);
        let portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &portals, &brushes);
        let (mapping, visible_leaves) = runtime_leaf_mapping(&bsp).expect("leaf mapping");
        let (visibility, offsets) =
            portal_component_visibility(&bsp, &portals, &mapping, visible_leaves);

        let first_host = point_leaf_index(&bsp, [128.0, 128.0, 128.0]);
        let second_host = point_leaf_index(&bsp, [640.0, 128.0, 128.0]);
        assert_eq!(bsp.leaves[first_host].contents, BspLeafContents::Empty);
        assert_eq!(bsp.leaves[second_host].contents, BspLeafContents::Empty);
        assert_ne!(offsets[first_host], offsets[second_host]);

        let row_bytes = visible_leaves.div_ceil(8);
        let first_row = decompress_visibility_row(&visibility, offsets[first_host], row_bytes);
        let second_row = decompress_visibility_row(&visibility, offsets[second_host], row_bytes);
        let first_bit = mapping[first_host] as usize - 1;
        let second_bit = mapping[second_host] as usize - 1;
        assert_ne!(first_row[first_bit >> 3] & (1 << (first_bit & 7)), 0);
        assert_eq!(first_row[second_bit >> 3] & (1 << (second_bit & 7)), 0);
        assert_ne!(second_row[second_bit >> 3] & (1 << (second_bit & 7)), 0);
        assert_eq!(second_row[first_bit >> 3] & (1 << (first_bit & 7)), 0);
    }

    #[test]
    fn release_portal_flow_culls_a_hidden_corner_that_draft_keeps() {
        use crate::brush::Plane as BrushPlane;

        let brushes = [Brush::cuboid([0, 0, 0], [128, 64, 256])];
        let surfaces = compile_csg_surfaces(&brushes);
        let mut bsp = build_surface_bsp(&surfaces);
        let classification_portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &classification_portals, &brushes);
        let visible_hosts: Vec<_> = bsp
            .leaves
            .iter()
            .enumerate()
            .filter_map(|(index, leaf)| leaf.contents.is_visible().then_some(index))
            .take(4)
            .collect();
        assert_eq!(visible_hosts.len(), 4);
        let x_portal = |back_leaf, front_leaf, x: i64| CompiledPortal {
            plane: BrushPlane {
                normal: [1, 0, 0],
                dist: x,
            },
            front_leaf,
            back_leaf,
            vertices: vec![
                [x as f64, -1.0, -1.0],
                [x as f64, 1.0, -1.0],
                [x as f64, 1.0, 1.0],
                [x as f64, -1.0, 1.0],
            ],
        };
        let portals = vec![
            x_portal(visible_hosts[0], visible_hosts[1], 0),
            x_portal(visible_hosts[1], visible_hosts[2], 10),
            CompiledPortal {
                plane: BrushPlane {
                    normal: [0, 1, 0],
                    dist: 10,
                },
                front_leaf: visible_hosts[3],
                back_leaf: visible_hosts[2],
                vertices: vec![
                    [9.0, 10.0, -1.0],
                    [9.0, 10.0, 1.0],
                    [11.0, 10.0, 1.0],
                    [11.0, 10.0, -1.0],
                ],
            },
        ];
        let pack = |visibility| {
            pack_bsp_geometry_with_visibility(
                &bsp,
                &portals,
                BspLighting::Fullbright,
                visibility,
                &Default::default(),
            )
            .expect("pack")
        };
        let draft = pack(BspVisibility::PortalComponents);
        let release = pack(BspVisibility::PortalFlow);
        let draft_leaves = RecordSlice::<Leaf>::new(&draft.leaves).expect("draft leaves");
        let release_leaves = RecordSlice::<Leaf>::new(&release.leaves).expect("release leaves");
        let (mapping, visible_leaf_count) = runtime_leaf_mapping(&bsp).expect("leaf mapping");
        let source_leaf = mapping[visible_hosts[0]] as usize;
        let hidden_bit = mapping[visible_hosts[3]] as usize - 1;
        let row_bytes = visible_leaf_count.div_ceil(8);
        let draft_row = decompress_visibility_row(
            &draft.visibility,
            draft_leaves
                .get(source_leaf)
                .expect("draft source leaf")
                .visibility_offset,
            row_bytes,
        );
        let release_row = decompress_visibility_row(
            &release.visibility,
            release_leaves
                .get(source_leaf)
                .expect("release source leaf")
                .visibility_offset,
            row_bytes,
        );

        assert_ne!(draft_row[hidden_bit >> 3] & (1 << (hidden_bit & 7)), 0);
        assert_eq!(release_row[hidden_bit >> 3] & (1 << (hidden_bit & 7)), 0);
        for &visible_host in &visible_hosts[..3] {
            let visible_bit = mapping[visible_host] as usize - 1;
            assert_ne!(release_row[visible_bit >> 3] & (1 << (visible_bit & 7)), 0);
        }
    }

    fn decompress_visibility_row(input: &[u8], offset: i32, row_bytes: usize) -> Vec<u8> {
        let mut output = vec![0; row_bytes];
        let mut source = offset as usize;
        let mut destination = 0usize;
        while destination < row_bytes {
            let byte = input[source];
            source += 1;
            if byte != 0 {
                output[destination] = byte;
                destination += 1;
            } else {
                let run = input[source] as usize;
                source += 1;
                destination += run;
            }
        }
        output
    }

    #[test]
    fn face_uv_and_baked_light_reach_vertex_records() {
        let mut brush = Brush::cuboid([0, 0, 0], [128, 64, 256]);
        brush.faces[5].uv.offset_texels = [17, -9];
        let surfaces = compile_csg_surfaces(&[brush.clone()]);
        let mut bsp = build_surface_bsp(&surfaces);
        let portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &portals, &[brush]);
        let colors: Vec<Vec<u32>> = bsp
            .surfaces
            .iter()
            .map(|surface| vec![0x0012_3456; surface.vertices.len()])
            .collect();
        let packed = pack_bsp_geometry(
            &bsp,
            &portals,
            BspLighting::Baked(&colors),
            &Default::default(),
        )
        .expect("pack");
        let vertices = RecordSlice::<Vertex>::new(&packed.vertices).expect("vertices");
        assert!(vertices.iter().all(|vertex| vertex.light == 0x0012_3456));
        let authored = bsp
            .surfaces
            .iter()
            .position(|surface| surface.source_face == 5)
            .expect("authored face");
        let face = RecordSlice::<Face>::new(&packed.faces)
            .expect("faces")
            .get(authored)
            .expect("face");
        let first = vertices
            .get(face.first_vertex as usize)
            .expect("first vertex");
        assert_ne!(first.texture.x, 0);
    }

    #[test]
    fn packed_node_descent_preserves_solid_and_empty_cells() {
        let brush = Brush::cuboid([0, 0, 0], [128, 64, 256]);
        let (bsp, packed) = packed(&[brush]);
        assert_eq!(
            bsp.leaves[point_leaf_index(&bsp, [64.0, 32.0, 128.0])].contents,
            BspLeafContents::Solid
        );
        let nodes = RecordSlice::<Node>::new(&packed.nodes).expect("nodes");
        let planes = RecordSlice::<Plane>::new(&packed.planes).expect("planes");
        let descend = |point: [i32; 3]| {
            let mut child = packed.root_node;
            while child >= 0 {
                let node = nodes.get(child as usize).expect("node");
                let plane = planes.get(node.plane as usize).expect("plane");
                let point_q12 = point.map(|value| value * 4096);
                let dot = match plane.kind {
                    0 => point_q12[0],
                    1 => point_q12[1],
                    2 => point_q12[2],
                    _ => {
                        ((point_q12[0] as i64 * plane.normal.x as i64
                            + point_q12[1] as i64 * plane.normal.y as i64
                            + point_q12[2] as i64 * plane.normal.z as i64)
                            >> 12) as i32
                    }
                };
                child = node.children[usize::from(dot - plane.distance <= 0)];
            }
            -1 - child
        };
        assert_eq!(descend([64, 32, 128]), 0);
        assert!(descend([-64, 32, 128]) > 0);
    }

    #[test]
    fn packed_face_flags_preserve_authored_outward_winding() {
        let brush = Brush::cuboid([0, 0, 0], [128, 64, 256]);
        let surfaces = compile_csg_surfaces(&[brush.clone()]);
        let mut bsp = build_surface_bsp(&surfaces);
        let portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &portals, &[brush]);
        let packed =
            pack_bsp_geometry(&bsp, &portals, BspLighting::Fullbright, &Default::default())
                .expect("pack");
        let faces = RecordSlice::<Face>::new(&packed.faces).expect("faces");
        let planes = RecordSlice::<Plane>::new(&packed.planes).expect("planes");
        for (index, surface) in bsp.surfaces.iter().enumerate() {
            let face = faces.get(index).expect("face");
            let plane = planes.get(face.plane as usize).expect("plane");
            let inverse_count = 1.0 / surface.vertices.len() as f64;
            let centroid = surface.vertices.iter().fold([0.0; 3], |mut sum, vertex| {
                for axis in 0..3 {
                    sum[axis] += vertex[axis] * inverse_count;
                }
                sum
            });
            let (normal, _) = crate::brush_compile::normalized_plane(surface.plane);
            let outside = [
                centroid[0] + normal[0],
                centroid[1] + normal[1],
                centroid[2] + normal[2],
            ];
            let point = outside.map(|value| (value * 4096.0).round() as i32);
            let distance = match plane.kind {
                0 => point[0],
                1 => point[1],
                2 => point[2],
                _ => {
                    ((point[0] as i64 * plane.normal.x as i64
                        + point[1] as i64 * plane.normal.y as i64
                        + point[2] as i64 * plane.normal.z as i64)
                        >> 12) as i32
                }
            } - plane.distance;
            let behind = distance < 0;
            assert_eq!(behind, face.flags & FACE_BACKSIDE != 0);
            assert_eq!(face.flags & FACE_TWO_SIDED, 0);
        }
    }
}
