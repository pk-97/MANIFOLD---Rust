//! Sand: sand and soil on Matter (MLS-MPM), a domain like a Water's
//! (`water.rs`) whose Transform is its box and whose one Fill fills it.
//! The compiler splices the Matter family the water crate registers for
//! Sand; the card is the shared domain and surface rows plus Matter's dials
//! (MATTER_SAND_DESIGN.md).

use manifold_foundation::scene_surface::{CardBand, ControlGroup};

use super::fields::{FieldRow, MATERIAL_BINDING};
use super::water::{
    BACK_WALL, BILLBOARD, BOUNDS_FIELD, CEILING, CELL_SIZE_ROW, DOMAIN, DOMAIN_SIZE, DRAW_FIELD,
    FILL_HEIGHT, FLOOR, FOLLOW_AIR, FOLLOW_GRAVITY_X, FOLLOW_GRAVITY_Y, FOLLOW_GRAVITY_Z,
    FOLLOW_RESET, FOLLOW_SPEED, FRONT_WALL, LEFT_WALL, POS_X, POS_Y, POS_Z, RIGHT_WALL, ROT_X,
    ROT_Y, ROT_Z, SCALE_X, SCALE_Y, SCALE_Z, SURFACE_CLOSE_GAPS, SURFACE_DETAIL,
    SURFACE_PARTICLE_SCALE, SURFACE_SMOOTHNESS,
};
use super::{PropertyRow, perform, row, setup};

use ControlGroup::Simulation;

pub static SAND_ROWS: [PropertyRow; 32] = [
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
    row("points_per_cell", DOMAIN, Simulation, setup()).name("Particles per Cell"),
    row("seed", DOMAIN, Simulation, setup()).name("Seed"),
    SURFACE_DETAIL,
    SURFACE_PARTICLE_SCALE,
    SURFACE_SMOOTHNESS,
    SURFACE_CLOSE_GAPS,
    FOLLOW_GRAVITY_X,
    FOLLOW_GRAVITY_Y,
    FOLLOW_GRAVITY_Z,
    FOLLOW_AIR,
    FOLLOW_SPEED,
    FOLLOW_RESET,
];

pub static SAND_FIELDS: [FieldRow; 3] = [BOUNDS_FIELD, DRAW_FIELD, MATERIAL_BINDING];
