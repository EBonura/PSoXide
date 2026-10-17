//! Collision records of placed meshes.
//!
//! Each proxy of an asset is turned by its instance into the record the grid
//! already has for free planes (`psx_grid::ColPlane`), so a body, the camera and
//! the hook all see a mesh through the native collision provider and the
//! runtime needs nothing new. The shapes are the plane shapes:
//!
//! * a **platform** under an upward surface (a slab: the thickness is the
//!   proxy's, so a thick arch band is solid right through),
//! * a vertical **wall** segment of zero thickness,
//! * a **blocker** box, which over-blocks when the shape is not a box.
//!
//! A box proxy stays one blocker when the instance is turned by a multiple of
//! 90 degrees (exact), becomes four walls (exact, plus a platform for a
//! standable top) at any other yaw, and a bounding box when pitched or rolled
//! (over-blocks; the cook says so). Corners go through the same rounding as the
//! faces, so what is drawn is what is stood on.

use psx_grid::{plane_kind, ColPlane};

use super::plane_collision::{blocker, platform, wall, ExtraRecord, PLATFORM_THICKNESS};
use super::units::RuntimeUnits;
use crate::grid::mesh::{BuiltMesh, GridMesh, MeshAsset, Proxy};
use crate::grid::plane::{PLATFORM_NORMAL_Y, WALL_NORMAL_Y};
use crate::grid::room::GridRoom;

type P3 = [i32; 3];

/// The collision records of every enabled, colliding instance of `room`, with
/// warnings for shapes that over-block.
pub fn extra_records(
    units: &RuntimeUnits,
    room: &GridRoom,
    assets: &[MeshAsset],
    origin: P3,
    warnings: &mut Vec<String>,
) -> Result<Vec<ExtraRecord>, String> {
    let mut out = Vec::new();
    for mesh in room.meshes.iter().filter(|m| m.enabled && m.collide) {
        let Some(asset) = assets.get(usize::from(mesh.asset)) else {
            continue;
        };
        let built = asset
            .build()
            .map_err(|e| format!("mesh {}: {e}", mesh.id))?;
        proxies(units, room, mesh, &built, origin, &mut out, warnings)
            .map_err(|e| format!("mesh {} ({}): {e}", mesh.id, asset.name))?;
    }
    Ok(out)
}

/// Runtime, room-local position of an asset point of `mesh`.
fn rt_point(units: &RuntimeUnits, room: &GridRoom, mesh: &GridMesh, p: P3, origin: P3) -> P3 {
    let g = mesh.place(p);
    [
        units.rt(g[0]) - origin[0],
        units.rt(room.y_offset + g[1]) - origin[1],
        units.rt(g[2]) - origin[2],
    ]
}

fn extra(record: ColPlane, range: (i32, i32), mesh: &GridMesh, what: &str) -> ExtraRecord {
    ExtraRecord {
        record,
        range,
        label: format!("mesh {} {what}", mesh.id),
    }
}

fn normal_y(c: &[P3; 4]) -> f64 {
    let a = [0, 1, 2].map(|i| f64::from(c[1][i] - c[0][i]));
    let b = [0, 1, 2].map(|i| f64::from(c[3][i] - c[0][i]));
    let n = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len == 0.0 {
        0.0
    } else {
        n[1] / len
    }
}

fn proxies(
    units: &RuntimeUnits,
    room: &GridRoom,
    mesh: &GridMesh,
    built: &BuiltMesh,
    origin: P3,
    out: &mut Vec<ExtraRecord>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let rt = |p: P3| rt_point(units, room, mesh, p, origin);
    let mut leaning = 0usize;
    for proxy in &built.proxies {
        match *proxy {
            Proxy::Slab { corners, thickness } => {
                let c = corners.map(rt);
                let thick = i16::try_from(units.rt(thickness).max(1)).unwrap_or(i16::MAX);
                let ny = normal_y(&c).abs();
                let record = if ny >= PLATFORM_NORMAL_Y {
                    platform(&c, 0, thick)?
                } else if ny <= WALL_NORMAL_Y {
                    match wall(&c, 0)? {
                        Some(record) => record,
                        None => continue,
                    }
                } else {
                    leaning += 1;
                    blocker(&c, 0)?
                };
                let range = match record.kind {
                    plane_kind::PLATFORM => (
                        c.iter().map(|p| p[1]).min().unwrap_or(0) - i32::from(record.extent),
                        c.iter().map(|p| p[1]).max().unwrap_or(0),
                    ),
                    _ => (
                        i32::from(record.y0),
                        i32::from(record.y0) + i32::from(record.extent),
                    ),
                };
                out.push(extra(record, range, mesh, "slab"));
            }
            Proxy::Box { min, max, top } => {
                box_proxy(mesh, (min, max, top), out, warnings, &rt)?;
            }
        }
    }
    if leaning > 0 {
        warnings.push(format!(
            "grid world: mesh {}: {leaning} slab(s) lean too far for a wall and not flat enough for a platform and block as boxes, which over-blocks the space under them",
            mesh.id
        ));
    }
    Ok(())
}

/// A box proxy `(min, max, top)` of `mesh`.
///
/// A box left upright and either standable (`top`) or turned off a quarter
/// turn is four vertical walls (and a platform for the top), exact at any
/// yaw; an unturned or quarter-turned box with no top is one blocker, exact
/// and cheaper; a pitched or rolled box is its bounding box, which over-blocks.
fn box_proxy(
    mesh: &GridMesh,
    (min, max, top): (P3, P3, bool),
    out: &mut Vec<ExtraRecord>,
    warnings: &mut Vec<String>,
    rt: &dyn Fn(P3) -> P3,
) -> Result<(), String> {
    let corner = |x: i32, y: i32, z: i32| rt([x, y, z]);
    let bottom = [
        corner(min[0], min[1], min[2]),
        corner(max[0], min[1], min[2]),
        corner(max[0], min[1], max[2]),
        corner(min[0], min[1], max[2]),
    ];
    let upper = [
        corner(min[0], max[1], min[2]),
        corner(max[0], max[1], min[2]),
        corner(max[0], max[1], max[2]),
        corner(min[0], max[1], max[2]),
    ];
    let aabb = |out: &mut Vec<ExtraRecord>| -> Result<(), String> {
        let all: Vec<P3> = bottom.iter().chain(&upper).copied().collect();
        let lo: P3 = core::array::from_fn(|i| all.iter().map(|p| p[i]).min().unwrap_or(0));
        let hi: P3 = core::array::from_fn(|i| all.iter().map(|p| p[i]).max().unwrap_or(0));
        let quad = [lo, [hi[0], lo[1], lo[2]], hi, [lo[0], hi[1], hi[2]]];
        out.push(extra(blocker(&quad, 0)?, (lo[1], hi[1]), mesh, "box"));
        Ok(())
    };
    if !mesh.upright() {
        warnings.push(format!(
            "grid world: mesh {}: a box turned off the vertical blocks as its bounding box, which over-blocks",
            mesh.id
        ));
        return aabb(out);
    }
    if !top && mesh.quarter_turned() {
        return aabb(out);
    }
    for i in 0..4 {
        let j = (i + 1) % 4;
        let quad = [bottom[i], bottom[j], upper[j], upper[i]];
        if let Some(record) = wall(&quad, 0)? {
            let range = (
                i32::from(record.y0),
                i32::from(record.y0) + i32::from(record.extent),
            );
            out.push(extra(record, range, mesh, "box side"));
        }
    }
    if top {
        let record = platform(&upper, 0, PLATFORM_THICKNESS)?;
        let y = upper[0][1];
        out.push(extra(
            record,
            (y - i32::from(PLATFORM_THICKNESS), y),
            mesh,
            "box top",
        ));
    }
    Ok(())
}
