//! Sand: sand, snow, clay, mud, goo or jelly on Matter (MLS-MPM), a domain
//! like a Water's (`water.rs`) whose Transform is its box and whose one Fill
//! fills it. The compiler splices the Matter family the water crate
//! registers for Sand; the card is the shared domain and surface rows plus
//! the dials of what it is made of (MATTER_SAND_DESIGN.md).

use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::fields::{FieldRow, MATERIAL_BINDING};
use super::water::{
    BACK_WALL, BILLBOARD, BOUNDS_FIELD, CEILING, CELL_SIZE_ROW, DOMAIN, DOMAIN_SIZE, DRAW_FIELD,
    FILL_HEIGHT, FLOOR, FOLLOW_AIR, FOLLOW_GRAVITY_X, FOLLOW_GRAVITY_Y, FOLLOW_GRAVITY_Z,
    FOLLOW_RESET, FOLLOW_SPEED, FRONT_WALL, LEFT_WALL, POS_X, POS_Y, POS_Z, RIGHT_WALL, ROT_X,
    ROT_Y, ROT_Z, SCALE_X, SCALE_Y, SCALE_Z, SURFACE_CLOSE_GAPS, SURFACE_DETAIL,
    SURFACE_PARTICLE_SCALE, SURFACE_SMOOTHNESS,
};
use super::{PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::{ItemFields, Matter, WaterDraw};

use ControlGroup::{Simulation, Surface};

/// The Sand draws as grains.
fn grains(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Sand(s) if s.draw == WaterDraw::Particles)
}

fn made_of(fields: &ItemFields, matter: Matter) -> bool {
    matches!(fields, ItemFields::Sand(s) if s.matter == matter)
}
fn sand(fields: &ItemFields) -> bool {
    made_of(fields, Matter::Sand)
}
fn snow(fields: &ItemFields) -> bool {
    made_of(fields, Matter::Snow)
}
fn clay(fields: &ItemFields) -> bool {
    made_of(fields, Matter::Clay)
}
fn mud(fields: &ItemFields) -> bool {
    made_of(fields, Matter::Mud)
}
fn goo(fields: &ItemFields) -> bool {
    made_of(fields, Matter::Goo)
}
fn jelly(fields: &ItemFields) -> bool {
    made_of(fields, Matter::Jelly)
}

/// Each material's Density row: one domain param, its own starting value.
const fn density(property: &'static str, default: f32, tip: &'static str) -> PropertyRow {
    row(property, DOMAIN, Simulation, setup())
        .param("density")
        .name("Density")
        .unit(Unit::KilogramsPerCubicMetre)
        .tip(tip)
        .default(default)
}

/// Clay's, mud's and goo's Firmness: the shear yield stress, below which it
/// holds its shape.
const fn firmness(property: &'static str, default: f32, range: (f32, f32)) -> PropertyRow {
    row(property, DOMAIN, Simulation, perform(CardBand::Scene, 3))
        .param("yield_stress")
        .name("Firmness")
        .unit(Unit::Pascals)
        .tip(
            "How hard it pushes back before it gives. Higher holds sharper shapes and taller piles",
        )
        .default(default)
        .range(range.0, range.1)
}

/// Clay's, mud's and goo's Thickness: the plastic viscosity once it flows.
const fn thickness(property: &'static str, default: f32, range: (f32, f32)) -> PropertyRow {
    row(property, DOMAIN, Simulation, perform(CardBand::Scene, 4))
        .param("viscosity")
        .name("Thickness")
        .unit(Unit::PascalSeconds)
        .tip("How slowly it flows once it gives. Runny honey is about 10, toothpaste about 100, cold caramel thousands; a metre of anything thinner than that spreads within seconds")
        .default(default)
        .range(range.0, range.1)
}

pub static SAND_ROWS: [PropertyRow; 48] = [
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
        .tip("How stiff it is. Higher is firmer, wobbles faster and costs more steps a frame"),
    row("friction_angle", DOMAIN, Simulation, perform(CardBand::Scene, 3))
        .name("Friction Angle")
        .unit(Unit::Degrees)
        .tip("How steep a pile holds. Sand flows below it and stops at it")
        .when(sand),
    row("cohesion", DOMAIN, Simulation, perform(CardBand::Scene, 4))
        .name("Cohesion")
        .unit(Unit::Kilopascals)
        .tip("0 is dry sand. Higher holds a cliff, about half a metre per kPa; drop it live to bring the cliff down")
        .when(sand),
    density("density", 1600.0, "How heavy the sand is, which sets how bodies sink into it").when(sand),
    row("hardening", DOMAIN, Simulation, perform(CardBand::Scene, 3))
        .name("Hardening")
        .tip("How much stiffer snow gets as it packs. Around 10 packs into firm clumps that break off; 0 stays loose powder")
        .default(10.0)
        .range(0.0, 20.0)
        .when(snow),
    density("snow_density", 400.0, "How packed the snow is: about 100 for fresh powder, 400 for settled snow, 600 for wet snow").when(snow),
    firmness("clay_firmness", 1.0e4, (1.0e3, 5.0e4)).when(clay),
    thickness("clay_thickness", 20.0, (0.0, 500.0)).when(clay),
    density("clay_density", 1800.0, "How heavy the clay is, which sets how it sags and how bodies sink into it").when(clay),
    firmness("mud_firmness", 1500.0, (20.0, 5.0e3)).when(mud),
    thickness("mud_thickness", 100.0, (1.0, 1.0e3)).when(mud),
    density("mud_density", 1700.0, "How heavy the mud is, which sets how it spreads and how bodies sink into it").when(mud),
    firmness("goo_firmness", 50.0, (0.0, 500.0)).when(goo),
    thickness("goo_thickness", 2000.0, (1.0, 1.0e4)).when(goo),
    density("goo_density", 1400.0, "How heavy the goo is, which sets how fast it pours").when(goo),
    density("jelly_density", 1050.0, "How heavy the jelly is, which sets how far it sags and how long it wobbles").when(jelly),
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

/// What a new item of `matter` looks like on its own Material, over the
/// Sand template's look, each from what the material is made of:
/// - Snow: ice grains (IOR 1.31) a fraction of a millimetre across at about
///   0.9 albedo, glinting. Light runs millimetres to a centimetre inside,
///   blue the furthest, so creases and thin edges glow faintly blue; a
///   little light stays at the surface for its crust.
/// - Clay: terracotta, satin, its light spreading a few millimetres and red
///   the furthest.
/// - Mud: dark wet soil under a film of water (a clear coat at water's
///   IOR), gritty below it.
/// - Goo: honey, clear (transmission 1) with its amber from absorption
///   through its own thickness, at honey's IOR 1.49.
/// - Jelly: gelatin, clear and absorbing red, at IOR 1.34.
pub fn matter_look(matter: Matter) -> &'static [(&'static str, f32)] {
    match matter {
        Matter::Sand => &[],
        Matter::Snow => &[
            ("color_r", 0.9),
            ("color_g", 0.92),
            ("color_b", 0.95),
            ("roughness", 0.35),
            ("specular", 0.5),
            ("ior", 1.31),
            ("grain", 0.4),
            ("subsurface_weight", 0.85),
            ("subsurface_radius_r", 0.004),
            ("subsurface_radius_g", 0.006),
            ("subsurface_radius_b", 0.012),
            ("subsurface_color_r", 0.9),
            ("subsurface_color_g", 0.94),
            ("subsurface_color_b", 0.98),
        ],
        Matter::Clay => &[
            ("color_r", 0.42),
            ("color_g", 0.16),
            ("color_b", 0.085),
            ("roughness", 0.6),
            ("specular", 0.35),
            ("grain", 0.0),
            ("subsurface_weight", 1.0),
            ("subsurface_radius_r", 0.006),
            ("subsurface_radius_g", 0.003),
            ("subsurface_radius_b", 0.002),
            ("subsurface_color_r", 0.42),
            ("subsurface_color_g", 0.16),
            ("subsurface_color_b", 0.085),
        ],
        Matter::Mud => &[
            ("color_r", 0.075),
            ("color_g", 0.05),
            ("color_b", 0.03),
            ("roughness", 0.65),
            ("ior", 1.33),
            ("grain", 1.0),
            ("clearcoat", 0.8),
            ("clearcoat_roughness", 0.12),
        ],
        Matter::Goo => &[
            ("color_r", 1.0),
            ("color_g", 0.9),
            ("color_b", 0.7),
            ("roughness", 0.02),
            ("ior", 1.49),
            ("grain", 0.0),
            ("transmission", 1.0),
            ("volume_geometry", 1.0),
            ("volume_thickness", 1.0),
            ("volume_attenuation_color_r", 0.95),
            ("volume_attenuation_color_g", 0.55),
            ("volume_attenuation_color_b", 0.08),
            ("volume_attenuation_distance", 0.25),
        ],
        Matter::Jelly => &[
            ("color_r", 1.0),
            ("color_g", 0.8),
            ("color_b", 0.8),
            ("roughness", 0.02),
            ("ior", 1.34),
            ("grain", 0.0),
            ("transmission", 1.0),
            ("volume_geometry", 1.0),
            ("volume_thickness", 1.0),
            ("volume_attenuation_color_r", 0.85),
            ("volume_attenuation_color_g", 0.05),
            ("volume_attenuation_color_b", 0.1),
            ("volume_attenuation_distance", 0.25),
        ],
    }
}
