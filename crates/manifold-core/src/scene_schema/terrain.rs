//! Terrain: an Object whose Shape is Terrain. `node.terrain` lifts a square
//! grid of points by noise and carves it with simulated rain on the GPU
//! whenever the land changes; `node.make_triangles` turns the grid into the
//! triangles the Object draws.
//!
//! Size, Height, Feature Size, Roughness, Weathering and Seed are performance
//! rows, so the land can morph on the beat. The grid's density is fixed setup.
//! The object's own material is the ground; the Cliff and Peak Material
//! fields pick the layers `look_terrain.wgsl` blends over it, by the slope
//! and snow line rows.

use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::fields::{FieldControl, FieldRow, FieldValue, enabled, is_material, wrong_value};
use super::{Exposure, PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::{ItemFields, ItemId, Shape};

const S: ControlGroup = ControlGroup::Shape;

fn terrain(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if matches!(o.shape, Shape::Terrain { .. }))
}

/// A Terrain's layer material: cliff (0) or peak (1).
fn layer(fields: &ItemFields, which: usize) -> Option<ItemId> {
    match fields {
        ItemFields::Object(o) => match &o.shape {
            Shape::Terrain { cliff, peak } => [cliff, peak][which].clone(),
            _ => None,
        },
        _ => None,
    }
}

fn set_layer(fields: &mut ItemFields, value: &FieldValue, which: usize) -> Result<(), String> {
    let (ItemFields::Object(o), FieldValue::Item(material)) = (fields, value) else {
        return Err(wrong_value("terrain layer", value));
    };
    let Shape::Terrain { cliff, peak } = &mut o.shape else {
        return Err("only a Terrain has layers".to_owned());
    };
    *[cliff, peak][which] = material.clone();
    Ok(())
}

/// The material on ground steeper than Cliff Starts.
pub(super) const CLIFF_MATERIAL: FieldRow = FieldRow {
    id: "cliff_material",
    label: "Cliff Material",
    section: ControlGroup::Material,
    control: FieldControl::Item {
        items: is_material,
        none: "None",
    },
    applies: terrain,
    disabled: enabled,
    read: |fields| FieldValue::Item(layer(fields, 0)),
    write: |fields, value| set_layer(fields, value, 0),
};

/// The material above the Snow Line, where the ground is gentle enough to
/// hold it.
pub(super) const PEAK_MATERIAL: FieldRow = FieldRow {
    id: "peak_material",
    label: "Peak Material",
    section: ControlGroup::Material,
    control: FieldControl::Item {
        items: is_material,
        none: "None",
    },
    applies: terrain,
    disabled: enabled,
    read: |fields| FieldValue::Item(layer(fields, 1)),
    write: |fields, value| set_layer(fields, value, 1),
};

/// A layer blend row on the object's draw.
const fn blend(property: &'static str) -> PropertyRow {
    row(
        property,
        Reach::Own("object"),
        ControlGroup::Material,
        setup(),
    )
    .when(terrain)
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

static ROWS: [PropertyRow; 15] = [
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
    fixed("terrain_triangle_columns", "triangles", "src_cols").default(RESOLUTION),
    fixed("terrain_triangle_rows", "triangles", "src_rows").default(RESOLUTION),
    row(
        "terrain_layers",
        Reach::Own("object"),
        S,
        Exposure::Internal("a Terrain draws its layers"),
    )
    .param("terrain")
    .default(1.0)
    .when(terrain),
    blend("terrain_cliff_start")
        .name("Cliff Starts")
        .range(0.0, 1.0)
        .default(0.3),
    blend("terrain_cliff_end")
        .name("Cliff Whole")
        .range(0.0, 1.0)
        .default(0.5),
    blend("terrain_snow_line")
        .name("Snow Line")
        .unit(Unit::Metres)
        .range(-500.0, 2000.0)
        .default(120.0),
    blend("terrain_snow_softness")
        .name("Snow Softness")
        .unit(Unit::Metres)
        .range(0.0, 200.0)
        .default(20.0),
    blend("terrain_snow_slope")
        .name("Snow Slides Off")
        .range(0.0, 1.0)
        .default(0.45),
];

/// Every table a Terrain's rows come from.
pub static TERRAIN_TABLES: [&[PropertyRow]; 1] = [&ROWS];
