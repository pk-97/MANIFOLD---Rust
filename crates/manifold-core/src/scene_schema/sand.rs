//! Sand: sand and soil on Matter (MLS-MPM), a domain like a Water's
//! (`water.rs`) whose Transform is its box and whose one Fill fills it.
//! The compiler splices the Matter family the water crate registers for
//! Sand; the card is the shared domain and surface rows plus Matter's dials
//! (MATTER_SAND_DESIGN.md).

use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::fields::{FieldRow, MATERIAL_BINDING};
use crate::scene_doc::{ItemFields, WaterDraw};
use super::water::{
    BACK_WALL, BILLBOARD, BOUNDS_FIELD, CEILING, CELL_SIZE_ROW, DOMAIN, DOMAIN_SIZE, DRAW_FIELD,
    FILL_HEIGHT, FLOOR, FOLLOW_AIR, FOLLOW_GRAVITY_X, FOLLOW_GRAVITY_Y, FOLLOW_GRAVITY_Z,
    FOLLOW_RESET, FOLLOW_SPEED, FRONT_WALL, LEFT_WALL, POS_X, POS_Y, POS_Z, RIGHT_WALL, ROT_X,
    ROT_Y, ROT_Z, SCALE_X, SCALE_Y, SCALE_Z, SURFACE_CLOSE_GAPS, SURFACE_DETAIL,
    SURFACE_PARTICLE_SCALE, SURFACE_SMOOTHNESS,
};
use super::{PropertyRow, Reach, perform, row, setup};

use ControlGroup::{Simulation, Surface};

/// The Sand draws as grains.
fn grains(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Sand(s) if s.draw == WaterDraw::Particles)
}

pub static SAND_ROWS: [PropertyRow; 36] = [
    POS_X,
    POS_Y,
    POS_Z,
    SCALE_X,
    SCALE_Y,
    SCALE_Z,
    ROT_X,
    ROT_Y,
    ROT_Z,
    BILLBOARD,
    LEFT_WALL,
    RIGHT_WALL,
    FLOOR,
    CEILING,
    BACK_WALL,
    FRONT_WALL,
    CELL_SIZE_ROW,
    DOMAIN_SIZE,
    FILL_HEIGHT,
    row("stiffness", DOMAIN, Simulation, perform(CardBand::Scene, 2))
        .name("Stiffness")
        .tip("How stiff the grains are. Higher is firmer and costs more steps a frame"),
    row("friction_angle", DOMAIN, Simulation, perform(CardBand::Scene, 3))
        .name("Friction Angle")
        .unit(Unit::Degrees)
        .tip("How steep a pile holds. Sand flows below it and stops at it"),
    row("cohesion", DOMAIN, Simulation, perform(CardBand::Scene, 4))
        .name("Cohesion")
        .unit(Unit::Kilopascals)
        .tip("0 is dry sand. Higher holds a cliff, about half a metre per kPa; drop it live to bring the cliff down"),
    row("density", DOMAIN, Simulation, setup())
        .name("Density")
        .unit(Unit::KilogramsPerCubicMetre)
        .tip("How heavy the sand is, which sets how bodies sink into it"),
    row("points_per_cell", DOMAIN, Simulation, setup()).name("Particles per Cell"),
    row("seed", DOMAIN, Simulation, setup()).name("Seed"),
    SURFACE_DETAIL,
    SURFACE_PARTICLE_SCALE,
    // Sand reads as clean slopes, with its grain from the material.
    SURFACE_SMOOTHNESS.default(2.0),
    SURFACE_CLOSE_GAPS,
    row("grain_size", Reach::Own("sand_grains"), Surface, setup())
        .name("Grain Size")
        .unit(Unit::Metres)
        .tip("How big each grain is, apart from the Cell Size. Smaller grains draw more of them at the surface, up to 16 a particle; past that they grow instead of leaving gaps")
        .when(grains),
    FOLLOW_GRAVITY_X,
    FOLLOW_GRAVITY_Y,
    FOLLOW_GRAVITY_Z,
    FOLLOW_AIR,
    FOLLOW_SPEED,
    FOLLOW_RESET,
];

pub static SAND_FIELDS: [FieldRow; 3] = [BOUNDS_FIELD, DRAW_FIELD, MATERIAL_BINDING];
