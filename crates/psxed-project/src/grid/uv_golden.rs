//! Legacy texels never change: a hash of every face's corners and texel
//! bits on the reference projects, pinned from before the UV model landed.

use super::faces::all_faces;

const PROJECTS: [&str; 7] = [
    "grid-cathedral",
    "grid-level008",
    "grid-dungeon",
    "grid-cave",
    "grid-sewers",
    "grid-west",
    "grid-planes",
];

/// The pinned hashes, in `PROJECTS` order.
const GOLDEN: [u64; 7] = [
    0xcf48c80128acabbb,
    0xd4ae97c7eccc539f,
    0xf2d1d43d51a5da31,
    0x8ac4b01036b6e626,
    0xa604cca745d2ac2a,
    0x92129cc4ca69d11f,
    0xdeeb9e077316127b,
];

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *hash ^= u64::from(*b);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

/// The `project.ron` of a reference level: the tracked project, or, for the
/// one that only lives as a fixture (`grid-planes`, which was never tracked
/// under `editor/projects`), the copy under `tests/fixtures`. A test never
/// depends on a project that is not in the repository.
pub(super) fn reference_project_path(name: &str) -> std::path::PathBuf {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .join("project.ron");
    if fixture.is_file() {
        fixture
    } else {
        crate::projects_dir().join(name).join("project.ron")
    }
}

fn texel_hash(name: &str) -> u64 {
    let path = reference_project_path(name);
    let project = crate::ProjectDocument::load_from_path(path).expect("project loads");
    let world = project.active_scene().world.grid().expect("grid project");
    let mut hash = 0xcbf29ce484222325u64;
    for face in all_faces(world) {
        for p in &face.points {
            for c in p {
                fnv(&mut hash, &c.to_le_bytes());
            }
        }
        for t in &face.texels {
            fnv(&mut hash, &t[0].to_bits().to_le_bytes());
            fnv(&mut hash, &t[1].to_bits().to_le_bytes());
        }
        fnv(&mut hash, format!("{:?}", face.material).as_bytes());
    }
    hash
}

#[test]
fn legacy_texels_are_bit_for_bit_what_they_were() {
    let got: Vec<u64> = PROJECTS.iter().map(|p| texel_hash(p)).collect();
    assert_eq!(got, GOLDEN.to_vec(), "texels of a legacy face changed");
}

/// The same pins one level at a time, so a failure names the level: the
/// texels of every legacy face stay what they were whatever the world's
/// default density is for a new world.
#[test]
fn the_reference_levels_keep_their_pinned_texels() {
    for (name, golden) in PROJECTS.iter().zip(GOLDEN) {
        assert!(
            reference_project_path(name).is_file(),
            "{name}: the reference level is missing from the repository"
        );
        assert_eq!(
            texel_hash(name),
            golden,
            "{name}: texels of a legacy face changed"
        );
    }
}
