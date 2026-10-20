//! The cooked bytes (level core and every room chunk) of each tracked
//! reference project, pinned from before the world's texel density became a
//! setting: a world stored without it keeps its 32 texels per sector, so
//! its cook must not move.

use super::tests::{cook, load_reference};

const PROJECTS: [(&str, u64); 12] = [
    ("default", 0x180d290ce913116b),
    ("graybox-reach", 0xf7509ab5c833de99),
    ("grid-cathedral", 0xf9db58002a1552e8),
    ("grid-cave", 0xf359de563dfea216),
    ("grid-corridor", 0x3fa796c1727a94a9),
    ("grid-dungeon", 0xe8f923fe4ba491d0),
    ("grid-gate", 0x1f23bd2aac942081),
    ("grid-level008", 0xe2645a177a13de04),
    ("grid-sewers", 0x347b75214eb56048),
    ("grid-starter", 0xbf7b01fcbb654729),
    ("grid-tower", 0xdfc513b70bb820ea),
    ("grid-west", 0xb6bb9c4ed48a31e5),
];

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *hash ^= u64::from(*b);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

fn cook_hash(name: &str) -> u64 {
    let project = load_reference(name).expect("a tracked project");
    let Some(world) = project.active_scene().world.grid() else {
        return 1;
    };
    let mut hash = 0xcbf29ce484222325u64;
    match cook(world) {
        Ok(cooked) => {
            fnv(&mut hash, &cooked.core);
            for chunk in &cooked.chunks {
                fnv(&mut hash, &(chunk.len() as u64).to_le_bytes());
                fnv(&mut hash, chunk);
            }
        }
        Err(e) => fnv(&mut hash, e.as_bytes()),
    }
    hash
}

#[test]
fn the_reference_projects_cook_to_the_bytes_they_always_did() {
    let got: Vec<(&str, u64)> = PROJECTS.iter().map(|(n, _)| (*n, cook_hash(n))).collect();
    if std::env::var("PRINT_COOK_PINS").is_ok() {
        for (n, h) in &got {
            eprintln!("    (\"{n}\", {h:#018x}),");
        }
    }
    assert_eq!(got, PROJECTS.to_vec(), "a legacy world's cook changed");
}

mod density {
    use super::super::tests::cook;
    use crate::face_uv::FaceUv;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridSurface;
    use crate::grid::units::{GridScale, SectorTexels};
    use crate::grid::world::GridWorld;
    use crate::ResourceId;

    fn floor(texels: SectorTexels, uv: FaceUv) -> GridWorld {
        let rid: ResourceId = ron::from_str("(7)").expect("a resource id");
        let mut room = GridRoom::new("r", 1, 1);
        let mut surface = GridSurface::flat(0, Some(rid));
        surface.tri_a.uv = uv;
        room.sector_or_insert(0, 0)
            .unwrap()
            .set_ground_floor(Some(surface));
        GridWorld {
            scale: GridScale::DEFAULT,
            texels_per_sector: texels,
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    /// The four texture corners of the lone quad, sorted.
    fn corners(world: &GridWorld) -> Vec<(u8, u8)> {
        let cooked = cook(world).unwrap();
        let quads = &cooked.tables[0].quads;
        assert_eq!(quads.len(), 1);
        let mut c: Vec<(u8, u8)> = quads[0]
            .uv
            .iter()
            .map(|uv| ((uv & 0xff) as u8, (uv >> 8) as u8))
            .collect();
        c.sort();
        c
    }

    /// A new world's default sector holds one whole 64 by 64 texture; a
    /// world stored with 32 holds a quarter of it, as it always did.
    #[test]
    fn a_new_worlds_sector_cooks_as_one_whole_texture() {
        assert_eq!(
            corners(&floor(SectorTexels::DEFAULT, FaceUv::default())),
            vec![(0, 0), (0, 63), (63, 0), (63, 63)]
        );
        assert_eq!(
            corners(&floor(SectorTexels::LEGACY, FaceUv::default())),
            vec![(0, 0), (0, 32), (32, 0), (32, 32)]
        );
    }

    /// Scale 2 shows the texture twice as big: half of it across a sector.
    /// Scale 0.5 shows it half as big: two repeats.
    #[test]
    fn a_scaled_face_cooks_with_the_scale() {
        let at = |q8: i16| {
            corners(&floor(
                SectorTexels::DEFAULT,
                FaceUv {
                    scale_q8: [q8, q8],
                    ..FaceUv::default()
                },
            ))
        };
        // The texels the sector spans along u.
        let span = |c: &[(u8, u8)]| {
            let u: Vec<u8> = c.iter().map(|p| p.0).collect();
            u.iter().max().unwrap() - u.iter().min().unwrap()
        };
        assert_eq!(span(&at(256)), 63);
        assert_eq!(span(&at(512)), 32);
        assert_eq!(span(&at(1024)), 16);
        assert!(span(&at(128)) >= 127);
    }
}
