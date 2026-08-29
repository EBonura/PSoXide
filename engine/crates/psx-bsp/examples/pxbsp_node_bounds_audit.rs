//! Audit whether PXBSP render-node bounds contain the geometry the render
//! traversal marks under them.
//!
//! The hierarchical frustum optimisation in `select_frame_pxbsp_faces` may
//! only propagate a "node wholly inside the frustum" proof down to faces if
//! every vertex of those faces lies inside the node's own box. This tool
//! answers that question for a cooked map, separately for the two ways a face
//! can be marked: a node's own `first_face..face_count` range, and a leaf's
//! mark-surface list.
//!
//! ```sh
//! cargo run -p psx-bsp --example pxbsp_node_bounds_audit -- brush_world.pxbsp
//! ```

use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::SliceReader;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: pxbsp_node_bounds_audit <map.pxbsp>");
        std::process::exit(2);
    }
    for path in &args {
        let bytes = std::fs::read(path).expect("read map");
        let mut map = PxbspResidentMap::default();
        let mut reader = SliceReader::new(&bytes);
        map.load(0, &mut reader).expect("load pxbsp");
        audit(path, &map);
        prove_report(&map);
    }
}

fn audit(path: &str, map: &PxbspResidentMap) {
    let nodes = map.nodes();
    let faces = map.faces();
    let leaves = map.leaves();
    let marks = map.mark_surfaces();
    let vertex_data = map.vertex_data();
    let stride = 12usize; // ClassicAffineWordSourceVertex

    let face_bounds = |index: usize| -> Option<([i16; 3], [i16; 3])> {
        let face = faces.get(index)?;
        let count = face.vertex_count as usize;
        if count == 0 {
            return None;
        }
        let first = face.first_vertex as usize;
        let read = |v: usize| -> [i16; 3] {
            let base = (first + v) * stride;
            [
                i16::from_le_bytes([vertex_data[base], vertex_data[base + 1]]),
                i16::from_le_bytes([vertex_data[base + 2], vertex_data[base + 3]]),
                i16::from_le_bytes([vertex_data[base + 4], vertex_data[base + 5]]),
            ]
        };
        let mut mins = read(0);
        let mut maxs = mins;
        for v in 1..count {
            let p = read(v);
            for axis in 0..3 {
                mins[axis] = mins[axis].min(p[axis]);
                maxs[axis] = maxs[axis].max(p[axis]);
            }
        }
        Some((mins, maxs))
    };

    let mut node_faces = 0usize;
    let mut node_face_violations = 0usize;
    let mut node_worst = 0i32;
    let mut leaf_marks = 0usize;
    let mut leaf_mark_violations = 0usize;
    let mut leaf_worst = 0i32;
    // A leaf mark is checked against every node on the path to it, so record
    // the deepest ancestor whose box still contains the face.
    let mut stack: Vec<(u32, usize)> = Vec::new();
    let root = map
        .brush_models()
        .get(0)
        .expect("validated world model")
        .head_nodes[0];
    if root < 0 {
        println!("{path}: world model head node is a leaf; nothing to audit");
        return;
    }

    let contains = |node_mins: [i16; 3], node_maxs: [i16; 3], mins: [i16; 3], maxs: [i16; 3]| {
        let mut worst = 0i32;
        for axis in 0..3 {
            worst = worst.max(node_mins[axis] as i32 - mins[axis] as i32);
            worst = worst.max(maxs[axis] as i32 - node_maxs[axis] as i32);
        }
        worst
    };

    // Ancestor boxes must nest, otherwise "inside" cannot be inherited.
    let mut nesting_violations = 0usize;
    let mut nesting_worst = 0i32;
    // Every ancestor on the path down, so the check is transitive rather than
    // relying on nesting alone.
    let mut ancestors: Vec<([i16; 3], [i16; 3])> = Vec::new();
    let mut walk: Vec<(i16, usize)> = vec![(root, 0)];
    while let Some((child, depth)) = walk.pop() {
        ancestors.truncate(depth);
        if child < 0 {
            let leaf_index = (-1i32 - child as i32) as usize;
            let Some(leaf) = leaves.get(leaf_index) else {
                continue;
            };
            let s = leaf.first_mark_surface as usize;
            let e = s + leaf.mark_surface_count as usize;
            for mark in s..e {
                let Some(face) = marks.get(mark) else {
                    continue;
                };
                let Some((mins, maxs)) = face_bounds(face as usize) else {
                    continue;
                };
                for (amins, amaxs) in &ancestors {
                    let worst = contains(*amins, *amaxs, mins, maxs);
                    if worst > 0 {
                        nesting_violations += 1;
                        nesting_worst = nesting_worst.max(worst);
                    }
                }
            }
            continue;
        }
        let Some(node) = nodes.get(child as usize) else {
            continue;
        };
        let nmins = [node.mins.x, node.mins.y, node.mins.z];
        let nmaxs = [node.maxs.x, node.maxs.y, node.maxs.z];
        ancestors.push((nmins, nmaxs));
        walk.push((node.children[0], depth + 1));
        walk.push((node.children[1], depth + 1));
    }

    stack.push((root as u32, 0));
    let mut visited = 0usize;
    while let Some((node_index, _depth)) = stack.pop() {
        let Some(node) = nodes.get(node_index as usize) else {
            continue;
        };
        visited += 1;
        let nmins = [node.mins.x, node.mins.y, node.mins.z];
        let nmaxs = [node.maxs.x, node.maxs.y, node.maxs.z];
        let start = node.first_face as usize;
        let end = start + node.face_count as usize;
        for face in start..end {
            let Some((mins, maxs)) = face_bounds(face) else {
                continue;
            };
            node_faces += 1;
            let worst = contains(nmins, nmaxs, mins, maxs);
            if worst > 0 {
                node_face_violations += 1;
                node_worst = node_worst.max(worst);
            }
        }
        for child in node.children {
            if child >= 0 {
                stack.push((child as u32, 0));
            } else {
                let leaf_index = (-1i32 - child as i32) as usize;
                let Some(leaf) = leaves.get(leaf_index) else {
                    continue;
                };
                let s = leaf.first_mark_surface as usize;
                let e = s + leaf.mark_surface_count as usize;
                for mark in s..e {
                    let Some(face) = marks.get(mark) else {
                        continue;
                    };
                    let Some((mins, maxs)) = face_bounds(face as usize) else {
                        continue;
                    };
                    leaf_marks += 1;
                    let worst = contains(nmins, nmaxs, mins, maxs);
                    if worst > 0 {
                        leaf_mark_violations += 1;
                        leaf_worst = leaf_worst.max(worst);
                    }
                }
            }
        }
    }

    println!("{path}");
    println!("  nodes visited            {visited} / {}", nodes.len());
    println!("  faces                    {}", faces.len());
    println!(
        "  node-range faces         {node_faces}, outside their node box: {node_face_violations} (worst overshoot {node_worst} units)"
    );
    println!(
        "  leaf mark surfaces       {leaf_marks}, outside their PARENT node box: {leaf_mark_violations} (worst overshoot {leaf_worst} units)"
    );
    println!(
        "  (face, ancestor) pairs where the face escapes an ANCESTOR node box: {nesting_violations} (worst overshoot {nesting_worst} units)"
    );
}

/// Mirror of `Renderer::prove_pxbsp_node_bounds`, reporting WHY it fails.
fn prove_report(map: &PxbspResidentMap) {
    let nodes = map.nodes();
    let leaves = map.leaves();
    let marks = map.mark_surfaces();
    let faces = map.faces();
    let vertex_data = map.vertex_data();
    let stride = 12usize;
    let mut nest_fail = 0usize;
    let mut mark_fail = 0usize;
    let mut range_fail = 0usize;
    let mut missing = 0usize;
    for index in 0..nodes.len() {
        let node = nodes.get(index).expect("node");
        let start = node.first_face as usize;
        for face in start..start + node.face_count as usize {
            if !face_inside(faces, vertex_data, stride, node.mins, node.maxs, face) {
                range_fail += 1;
            }
        }
        for child in node.children {
            if child >= 0 {
                let Some(c) = nodes.get(child as usize) else {
                    missing += 1;
                    continue;
                };
                if c.mins.x < node.mins.x
                    || c.mins.y < node.mins.y
                    || c.mins.z < node.mins.z
                    || c.maxs.x > node.maxs.x
                    || c.maxs.y > node.maxs.y
                    || c.maxs.z > node.maxs.z
                {
                    nest_fail += 1;
                }
                continue;
            }
            let leaf_index = (-1i32 - child as i32) as usize;
            let Some(leaf) = leaves.get(leaf_index) else {
                missing += 1;
                continue;
            };
            let first = leaf.first_mark_surface as usize;
            for m in first..first + leaf.mark_surface_count as usize {
                let Some(face) = marks.get(m) else {
                    missing += 1;
                    continue;
                };
                if !face_inside(
                    faces,
                    vertex_data,
                    stride,
                    node.mins,
                    node.maxs,
                    face as usize,
                ) {
                    mark_fail += 1;
                }
            }
        }
    }
    println!(
        "  runtime proof over ALL {} nodes: child-box nesting failures {nest_fail}, leaf-mark failures {mark_fail}, node-range failures {range_fail}, missing records {missing}",
        nodes.len()
    );
}

fn face_inside(
    faces: psx_bsp::RecordSlice<'_, psx_bsp::Face>,
    vertex_data: &[u8],
    stride: usize,
    mins: psx_bsp::Vec3I16,
    maxs: psx_bsp::Vec3I16,
    index: usize,
) -> bool {
    let Some(face) = faces.get(index) else {
        return false;
    };
    let count = face.vertex_count as usize;
    let first = face.first_vertex as usize;
    if (first + count) * stride > vertex_data.len() {
        return false;
    }
    for v in 0..count {
        let b = (first + v) * stride;
        let p = [
            i16::from_le_bytes([vertex_data[b], vertex_data[b + 1]]),
            i16::from_le_bytes([vertex_data[b + 2], vertex_data[b + 3]]),
            i16::from_le_bytes([vertex_data[b + 4], vertex_data[b + 5]]),
        ];
        if p[0] < mins.x || p[1] < mins.y || p[2] < mins.z {
            return false;
        }
        if p[0] > maxs.x || p[1] > maxs.y || p[2] > maxs.z {
            return false;
        }
    }
    true
}
