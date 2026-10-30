//! Terrain: an Object whose Shape is Terrain. `node.terrain` lifts a square
//! grid of points by noise and carves it with simulated rain on the GPU
//! whenever the land changes, with each point's normal; `node.grid_indices`
//! lists the grid's triangles for the Object's indexed draw.
//!
//! Size, Height, Feature Size, Roughness, Weathering and Seed are performance
//! rows, so the land can morph on the beat. The grid's density is fixed setup.
//! Rock on the slopes and snow up high are the Material's Covers.

use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::{Exposure, PropertyRow, Reach, perform, row};
use crate::scene_doc::{ItemFields, Shape};

const S: ControlGroup = ControlGroup::Shape;

fn terrain(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if matches!(o.shape, Shape::Terrain))
}

/// Points along each side of the grid.
pub const TERRAIN_RESOLUTION: u32 = 512;

const GRID: &str = "the terrain's grid is a fixed density";

const fn fixed(property: &'static str, role: &'static str, param: &'static str) -> PropertyRow {
    row(property, Reach::Own(role), S, Exposure::Internal(GRID))
        .param(param)
        .when(terrain)
}

const fn dial(property: &'static str, order: u8) -> PropertyRow {
    row(
        property,
        Reach::Own("terrain"),
        S,
        perform(CardBand::Scene, order),
    )
    .when(terrain)
}

const RESOLUTION: f32 = TERRAIN_RESOLUTION as f32;

static ROWS: [PropertyRow; 9] = [
    dial("terrain_size", 0)
        .param("size")
        .name("Size")
        .unit(Unit::Metres)
        .range(50.0, 4000.0)
        .default(600.0),
    dial("terrain_height", 1)
        .param("height")
        .name("Height")
        .unit(Unit::Metres)
        .range(0.0, 1000.0)
        .default(120.0),
    dial("feature_size", 2)
        .unit(Unit::Metres)
        .range(10.0, 2000.0)
        .default(250.0),
    dial("roughness", 3).range(0.3, 0.7).default(0.5),
    dial("weathering", 4).range(0.0, 4.0).default(1.0),
    dial("terrain_seed", 5)
        .param("seed")
        .name("Seed")
        .range(0.0, 999.0)
        .default(1.0),
    fixed("terrain_resolution", "terrain", "resolution").default(RESOLUTION),
    fixed("terrain_index_columns", "indices", "cols").default(RESOLUTION),
    fixed("terrain_index_rows", "indices", "rows").default(RESOLUTION),
];

/// Every table a Terrain's rows come from.
pub static TERRAIN_TABLES: [&[PropertyRow]; 1] = [&ROWS];
