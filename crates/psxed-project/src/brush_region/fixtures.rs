//! Synthetic fixtures for the partitioner: a terrain heightfield project.
//! The terrain pack projects under `editor/projects` are local-only, so the
//! 16 x 16 case is rebuilt from the same generator the editor uses.

use crate::terrain::{Terrain, TerrainShape};
use crate::{MaterialResource, ProjectDocument, ResourceData};

/// A project holding one sealed terrain patch of `cells` x `cells` cells.
///
/// Units are authored (16 per engine unit). `spacing` and `amplitude` are
/// authored units too. Five sky-seal solids close the sides and top.
pub fn terrain_project(cells: usize, spacing: i32, amplitude: i32, seed: u32) -> ProjectDocument {
    let mut project = ProjectDocument::new("Terrain fixture");
    let material = project.add_resource(
        "Terrain",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let terrain = Terrain::generate(
        [cells, cells],
        [spacing, spacing],
        [0, 0, 0],
        amplitude,
        seed,
        TerrainShape::Hills,
        0.5,
    )
    .expect("terrain fixture parameters are valid");
    let ceiling = amplitude + 2048;
    let mut brushes = terrain
        .brushes(Some(material))
        .expect("terrain brushes build");
    brushes.extend(
        terrain
            .sky_enclosure(ceiling, material)
            .expect("terrain enclosure builds"),
    );
    project.active_scene_mut().brushes = brushes;
    project
}
