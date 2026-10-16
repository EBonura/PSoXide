//! Tests of the bonnie-32 level conversion.

use super::bonnie::*;
use super::surface::{GridSplit, WallFacing};
use super::units::GridScale;

/// One 3x2 room: a slope, a split-level floor, a ceiling, a back-facing
/// wall, an `_USER` texture and an empty texture reference.
const LEVEL: &str = r#"(
    rooms: [(
        id: 4,
        position: (x: -2048.0, y: 512.0, z: 1024.0),
        width: 3, depth: 2,
        ambient: 0.25,
        sectors: [
            [Some((floor: Some((heights: (0.0, -0.0, 256.0, 256.0), texture: (pack: "a", name: "Brick"), split_direction: NeSw)),
                   ceiling: Some((heights: (1024.0, 1024.0, 1024.0, 1024.0), texture: (pack: "a", name: "Brick"))),
                   walls_north: [(heights: (0.0, 0.0, 1024.0, 1024.0), texture: (pack: "a", name: "Brick"), normal_mode: Back, solid: false)],
                   walls_nwse: [(heights: (0.0, 0.0, 512.0, 512.0), texture: (pack: "", name: ""))])),
             None],
            [Some((floor: Some((heights: (0.0, 0.0, 0.0, 0.0), texture: (pack: "a", name: "Brick"),
                                heights_2: Some((0.0, 0.0, 512.0, 0.0)), texture_2: Some((pack: "_USER", name: "texture_001")),
                                uv: Some(((x: 0.0, y: 0.0), (x: 1.0, y: 0.0), (x: 1.0, y: 1.0), (x: 0.0, y: 1.0))))))),
             Some((floor: Some((heights: (100.0, 0.0, 0.0, 0.0), texture: (pack: "a", name: "Brick")))))],
            [None, None],
        ],
        objects: [
            (sector_x: 0, sector_z: 0, height: 512.0, object_type: Light(color: (r: 10, g: 20, b: 30, blend: Opaque), intensity: 2.0, radius: 2048.0), name: "", enabled: true),
            (sector_x: 1, sector_z: 1, height: 64.0, object_type: Spawn(PlayerStart), name: "", enabled: true),
            (sector_x: 1, sector_z: 0, height: 0.0, asset_id: 77, name: "", enabled: true),
        ],
        fog: (enabled: true, color: (1.0, 0.0, 0.5), start: 100.0, falloff: 200.0, cull_offset: 5.0),
    )],
    player_settings: (height: 762.0, radius: 100.0),
)"#;

fn convert(text: &str) -> (ConvertedLevel, Vec<TextureRef>) {
    let level = parse_level_text(text).expect("parse");
    let mut seen = Vec::new();
    let mut project = crate::ProjectDocument::new("t");
    let mut ids = std::collections::BTreeMap::new();
    let converted = convert_level(&level, None, &mut |t| {
        if !seen.contains(t) {
            seen.push(t.clone());
        }
        Some(*ids.entry(t.clone()).or_insert_with(|| {
            project.add_resource(
                &t.name,
                crate::ResourceData::Material(crate::MaterialResource::opaque(None)),
            )
        }))
    })
    .expect("convert");
    (converted, seen)
}

#[test]
fn room_geometry_comes_across_in_integers() {
    let (c, _) = convert(LEVEL);
    let room = &c.world.rooms[0];
    assert_eq!(room.name, "Room 4");
    assert_eq!(room.origin, [-2, 1]);
    assert_eq!(room.y_offset, 512);
    assert_eq!((room.width, room.depth), (3, 2));
    assert_eq!(room.ambient, [64; 3]);
    assert_eq!(room.sector_count(), 3);
    // 762 tall player picks 21/16.
    assert_eq!(c.world.scale, GridScale { num: 21, den: 16 });
    let floor = room.sector(0, 0).unwrap().floor.as_ref().unwrap();
    assert_eq!(floor.heights, [0, 0, 256, 256]);
    assert_eq!(floor.split, GridSplit::NeSw);
    assert!(floor.tri_b.is_none() && floor.heights_b.is_none());
    assert!(room.sector(2, 0).is_none());
    // Column-major bonnie storage lands at [x][z].
    assert!(room.sector(1, 1).is_some() && room.sector(1, 0).is_some());
}

#[test]
fn split_level_surfaces_keep_their_second_triangle() {
    let (c, _) = convert(LEVEL);
    let floor = c.world.rooms[0]
        .sector(1, 0)
        .unwrap()
        .floor
        .as_ref()
        .unwrap();
    assert_eq!(floor.heights_b, Some([0, 0, 512, 0]));
    assert!(floor.tri_b.is_some());
    assert_ne!(floor.tri_b.unwrap().material, floor.tri_a.material);
}

#[test]
fn walls_keep_facing_solidity_and_edge() {
    let (c, _) = convert(LEVEL);
    let sector = c.world.rooms[0].sector(0, 0).unwrap();
    assert_eq!(sector.north.len(), 1);
    assert_eq!(sector.north[0].facing, WallFacing::Back);
    assert!(!sector.north[0].solid);
    assert_eq!(sector.nwse.len(), 1);
    assert!(sector.south.is_empty());
    assert_eq!(c.stats.walls, 2);
    assert!(sector.ceiling.is_some());
}

#[test]
fn every_texture_use_goes_through_the_resolver_including_odd_ones() {
    let (_, seen) = convert(LEVEL);
    assert!(seen.iter().any(TextureRef::is_empty));
    assert!(seen.iter().any(TextureRef::is_user));
    assert!(seen.iter().any(|t| t.pack == "a" && t.name == "Brick"));
    let level = parse_level_text(LEVEL).unwrap();
    assert_eq!(level.textures().len(), 3);
}

#[test]
fn level_lights_and_spawn_are_read_in_editor_units() {
    let (c, _) = convert(LEVEL);
    assert_eq!(c.stats.level_lights, 1);
    assert_eq!(c.stats.generated_lights, 0);
    let light = &c.lights[0];
    assert!(!light.generated);
    assert_eq!(light.color, [10, 20, 30]);
    // Sector (0,0) of a room at sector (-2, 1): x centre at -2 sectors
    // plus half, floor mean 128 + 512 lift + 512 above, scaled 21/16.
    assert_eq!(light.position, [-2016, 1512, 1344 + 672]);
    assert_eq!(light.radius, 2688);
    assert_eq!(
        c.spawn,
        Some(ImportedSpawn {
            room: 0,
            cell: [1, 1],
            height: 64
        })
    );
    assert_eq!(c.stats.unresolved_objects, 1);
}

#[test]
fn fog_and_dropped_data_are_reported() {
    let (c, _) = convert(LEVEL);
    let fog = c.world.rooms[0].fog;
    assert!(fog.enabled);
    assert_eq!(fog.color, [255, 0, 128]);
    assert_eq!((fog.start, fog.falloff), (100, 200));
    assert_eq!(c.stats.custom_uv_faces, 1);
    // Only surface heights count towards the lattice check: the 100 unit
    // corner is off it, the fog distances are not heights.
    assert_eq!(c.stats.off_lattice_heights, 1);
}

#[test]
fn levels_without_lights_get_a_generated_lattice() {
    let mut text = String::from(
        "(rooms: [(position: (x: 0.0, y: 0.0, z: 0.0), width: 6, depth: 6, sectors: [",
    );
    for _ in 0..6 {
        text.push('[');
        for _ in 0..6 {
            text.push_str("Some((floor: Some((heights: (0.0, 0.0, 0.0, 0.0), texture: (pack: \"a\", name: \"b\"))))),");
        }
        text.push_str("],");
    }
    text.push_str("])])");
    let (c, _) = convert(&text);
    assert_eq!(c.stats.generated_lights, 1);
    assert_eq!(c.lights.len(), 1);
    assert!(c.lights[0].generated);
    // Centre-most cell of a 6x6 block, 2048 above its floor.
    assert_eq!(c.lights[0].position[1], c.world.scale.to_editor(2048));
    assert_eq!(c.lights[0].radius, c.world.scale.to_editor(6 * 1024));
    // No player height recorded: the default scale applies.
    assert_eq!(c.world.scale, GridScale::DEFAULT);
}

#[test]
fn lamps_stay_below_the_ceiling() {
    let mut text = String::from(
        "(rooms: [(position: (x: 0.0, y: 0.0, z: 0.0), width: 6, depth: 6, sectors: [",
    );
    for _ in 0..6 {
        text.push('[');
        for _ in 0..6 {
            text.push_str("Some((floor: Some((heights: (0.0, 0.0, 0.0, 0.0), texture: (pack: \"a\", name: \"b\"))), ceiling: Some((heights: (1024.0, 1024.0, 1024.0, 1024.0), texture: (pack: \"a\", name: \"b\"))))),");
        }
        text.push_str("],");
    }
    text.push_str("])])");
    let (c, _) = convert(&text);
    let ceiling = c.world.scale.to_editor(1024);
    assert!(c.lights[0].position[1] < ceiling, "{:?}", c.lights[0]);
}

#[test]
fn rooms_off_the_sector_lattice_are_rejected() {
    let text =
        "(rooms: [(position: (x: 100.0, y: 0.0, z: 0.0), width: 1, depth: 1, sectors: [[None]])])";
    let level = parse_level_text(text).unwrap();
    assert!(matches!(
        convert_level(&level, None, &mut |_| None),
        Err(BonnieError::UnalignedRoom { room: 0 })
    ));
}

#[test]
fn mismatched_sector_columns_are_rejected() {
    let text =
        "(rooms: [(position: (x: 0.0, y: 0.0, z: 0.0), width: 2, depth: 1, sectors: [[None]])])";
    let level = parse_level_text(text).unwrap();
    assert!(matches!(
        convert_level(&level, None, &mut |_| None),
        Err(BonnieError::BadRoomShape { room: 0 })
    ));
}

#[test]
fn plain_and_brotli_levels_both_parse() {
    let plain = "(rooms: [])";
    assert_eq!(parse_level(plain.as_bytes()).unwrap().room_count(), 0);
    // Brotli stream for "(rooms: [])", from the reference `brotli` tool.
    let compressed = [
        0x0f, 0x05, 0x80, 0x28, 0x72, 0x6f, 0x6f, 0x6d, 0x73, 0x3a, 0x20, 0x5b, 0x5d, 0x29, 0x03,
    ];
    assert_eq!(parse_level(&compressed).unwrap().room_count(), 0);
    assert!(matches!(
        parse_level(b"\x00\x01\x02"),
        Err(BonnieError::Decompress(_)) | Err(BonnieError::Parse(_))
    ));
}

#[test]
fn player_height_picks_the_scale() {
    assert_eq!(
        scale_for_player_height(1800.0),
        GridScale { num: 9, den: 16 }
    );
    assert_eq!(
        scale_for_player_height(762.0),
        GridScale { num: 21, den: 16 }
    );
    assert_eq!(
        scale_for_player_height(1200.0),
        GridScale { num: 14, den: 16 }
    );
    assert_eq!(
        scale_for_player_height(1024.0),
        GridScale { num: 16, den: 16 }
    );
    assert_eq!(scale_for_player_height(0.0), GridScale::DEFAULT);
}
