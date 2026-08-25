use std::path::PathBuf;

use psx_bsp::pxbsp::material_flags;
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::SliceReader;
use psx_level::sky_flags;
use psxed_project::playtest::PlaytestWorldGeometry;
use psxed_project::{
    MaterialResource, NodeKind, ProjectDocument, ResourceData, SkyMode, SkyVisibility,
};

fn directional_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../archive/fixtures/brush-directional-sky")
}

struct TempCookDir(PathBuf);

impl TempCookDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("psoxide-scene-sky-contract-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }
}

impl Drop for TempCookDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn legacy_material_projection_migrates_to_world_sky_and_aperture() {
    let mut project = ProjectDocument::new("legacy directional sky");
    let mut material = MaterialResource::opaque(Some("assets/sky.psxt".to_string()));
    material.directional_sky = true;
    let sky_material = project.add_resource("Legacy Sky", ResourceData::Material(material));
    let mut brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    for face in &mut brush.faces {
        face.material = Some(sky_material);
    }
    project.active_scene_mut().brushes.push(brush);
    let root = project.active_scene().root;
    let NodeKind::World { sky, .. } = &mut project
        .active_scene_mut()
        .node_mut(root)
        .expect("world")
        .kind
    else {
        panic!("root is not a World");
    };
    sky.mode = SkyMode::Off;

    project.normalize_loaded();

    let ResourceData::Material(material) = &project.resource(sky_material).unwrap().data else {
        panic!("resource changed kind");
    };
    assert!(material.sky_aperture);
    assert!(!material.layered_sky);
    assert!(!material.directional_sky);
    let NodeKind::World { sky, .. } = &project.active_scene().node(root).unwrap().kind else {
        panic!("root is not a World");
    };
    assert_eq!(sky.mode, SkyMode::Cube);
    assert_eq!(sky.visibility, SkyVisibility::ThroughSkySurfaces);
    assert_eq!(sky.texture, Some(sky_material));
}

#[test]
fn directional_fixture_cooks_one_scene_sky_and_textureless_apertures() {
    let fixture_dir = directional_fixture_dir();
    let project = ProjectDocument::load_from_path(fixture_dir.join("project.ron"))
        .expect("directional fixture loads");
    let (package, report) = psxed_project::playtest::build_package(&project, &fixture_dir);
    assert!(report.is_ok(), "directional package: {:?}", report.errors);
    let package = package.expect("directional package");
    let manifest = psxed_project::playtest::render_manifest_source(&package);
    assert!(manifest.contains("texture_asset: AssetId("));
    let output = TempCookDir::new();
    psxed_project::playtest::write_package(&package, &output.0)
        .expect("directional package writes complete generated output");
    let written_manifest = std::fs::read_to_string(output.0.join("level_manifest.cooked.rs"))
        .expect("written directional manifest");
    assert_eq!(written_manifest, manifest);
    assert!(output.0.join("brush_world.pxbsp").is_file());
    let sky = package.rooms[0].sky.clone();
    assert_eq!(
        sky.flags & (sky_flags::ENABLED | sky_flags::CUBE | sky_flags::THROUGH_SKY_SURFACES),
        sky_flags::ENABLED | sky_flags::CUBE | sky_flags::THROUGH_SKY_SURFACES
    );
    let sky_asset = sky.texture_asset_index.expect("one scene sky texture");
    let PlaytestWorldGeometry::Pxbsp(world) = &package.world_geometry else {
        panic!("fixture did not cook PXBSP");
    };
    assert!(!world.texture_asset_indices.contains(&sky_asset));

    let mut map = PxbspResidentMap::with_capacity(world.bytes.len());
    map.load(0, &mut SliceReader::new(&world.bytes))
        .expect("resident directional map");
    let apertures = map
        .materials()
        .iter()
        .filter(|material| material.flags & material_flags::SKY_APERTURE != 0)
        .collect::<Vec<_>>();
    assert!(!apertures.is_empty());
    assert!(apertures
        .iter()
        .all(|material| material.texture_asset == u16::MAX));
}
