//! Collision-only cook/replay diagnostic; no render/world package writes.
use psx_bsp::collision::{CollisionHull, Trace, TraceScratch};
use psx_bsp::{ClipNode, Plane, RecordSlice, Vec3I32};
use psxed_project::{
    brush_collision_hulls::{
        compile_collision_hulls_with_target, CollisionHullBounds, CollisionHullStrategy,
    },
    ProjectDocument,
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let mut project = ProjectDocument::from_ron_str(&std::fs::read_to_string(&args[1])?)?;
    psxed_project::units::scale_project_to_engine_units(&mut project);
    let brushes: Vec<_> = project
        .active_scene()
        .brushes
        .iter()
        .filter(|b| b.mover.is_none())
        .cloned()
        .collect();
    // Verified against this frozen Cortex manifest; arguments keep helper explicit.
    let radius: i32 = args[4].parse()?;
    let height: i32 = args[5].parse()?;
    let bounds = CollisionHullBounds {
        mins: [-radius, 0, -radius],
        maxs: [radius, height, radius],
    };
    // Same strategy choice as the playtest cook.
    let strategy = if project.collision_hull_bsp {
        CollisionHullStrategy::HullBsp
    } else {
        CollisionHullStrategy::SpatialChains
    };
    let compiled = compile_collision_hulls_with_target(
        &brushes,
        &[bounds, bounds],
        strategy,
        project.bsp_collision_leaf_target,
    )
    .map_err(|e| format!("{e:?}"))?;
    let prefix = &args[3];
    std::fs::write(format!("{prefix}.planes"), &compiled.planes)?;
    std::fs::write(format!("{prefix}.nodes"), &compiled.clipnodes)?;
    std::fs::write(
        format!("{prefix}.heads"),
        format!("{:?}", compiled.head_nodes),
    )?;
    eprintln!(
        "brushes={} planes={} nodes={} heads={:?}",
        brushes.len(),
        compiled.planes.len() / 14,
        compiled.clipnodes.len() / 6,
        compiled.head_nodes
    );
    let hull = CollisionHull::new(
        RecordSlice::<Plane>::new(&compiled.planes).ok_or("planes")?,
        RecordSlice::<ClipNode>::new(&compiled.clipnodes).ok_or("nodes")?,
        compiled.head_nodes[0],
    )
    .ok_or("bad hull")?;
    let rays = std::fs::read(&args[2])?;
    let mut scratch = TraceScratch::new();
    let mut out = String::new();
    for (i, r) in rays.chunks_exact(32).enumerate() {
        let v: [i32; 8] =
            core::array::from_fn(|j| i32::from_le_bytes(r[j * 4..j * 4 + 4].try_into().unwrap()));
        let a = Vec3I32 {
            x: v[1],
            y: v[2],
            z: v[3],
        };
        let b = Vec3I32 {
            x: v[4],
            y: v[5],
            z: v[6],
        };
        let mut t = Trace::default();
        let ok = hull.trace_into(&a, &b, &mut scratch, &mut t);
        let mut hash = 0xcbf29ce484222325u64;
        for x in format!("{ok:?}:{t:?}").bytes() {
            hash = (hash ^ x as u64).wrapping_mul(0x100000001b3);
        }
        out.push_str(&format!("{i},{hash:016x},{t:?}\n"));
    }
    std::fs::write(format!("{prefix}.trace"), out)?;
    Ok(())
}
