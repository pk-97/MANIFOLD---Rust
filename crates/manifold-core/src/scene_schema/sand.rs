//! Sand: sand, snow, clay, mud, goo or jelly on Matter (MLS-MPM), a domain
//! like a Water's (`water.rs`) whose Transform is its box and whose one Fill
//! fills it. The compiler splices the Matter family the water crate
//! registers for Sand; the card is the shared domain and surface rows plus
//! the dials of what it is made of (MATTER_SAND_DESIGN.md).

use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::fields::{
    Choice, FieldControl, FieldRow, FieldValue, MATERIAL_BINDING, always, enabled, wrong_value,
};
use super::water::{
    BACK_WALL, BILLBOARD, BOUNDS_FIELD, CEILING, CELL_SIZE_ROW, DOMAIN, DOMAIN_SIZE, DRAW_FIELD,
    FILL_HEIGHT, FLOOR, FOLLOW_AIR, FOLLOW_GRAVITY_X, FOLLOW_GRAVITY_Y, FOLLOW_GRAVITY_Z,
    FOLLOW_RESET, FOLLOW_SPEED, FRONT_WALL, LEFT_WALL, POS_X, POS_Y, POS_Z, QUALITY_FIELD,
    RIGHT_WALL, ROT_X, ROT_Y, ROT_Z, SCALE_X, SCALE_Y, SCALE_Z, SURFACE_CLOSE_GAPS, SURFACE_DETAIL,
    SURFACE_PARTICLE_SCALE, SURFACE_SMOOTHNESS,
};
use super::{PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::{ItemFields, Matter, WaterDraw};

use ControlGroup::{Simulation, Surface};

/// The Sand template's `node.matter_material`, whose dials are what it is
/// made of.
const MATERIAL: Reach = Reach::Own("material");

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

/// Each material's Density row: one material param, its own starting value.
const fn density(property: &'static str, default: f32, tip: &'static str) -> PropertyRow {
    row(property, MATERIAL, Simulation, setup())
        .param("density")
        .name("Density")
        .unit(Unit::KilogramsPerCubicMetre)
        .tip(tip)
        .default(default)
}

/// Clay's, mud's and goo's Firmness: the shear yield stress, below which it
/// holds its shape.
const fn firmness(property: &'static str, default: f32, range: (f32, f32)) -> PropertyRow {
    row(property, MATERIAL, Simulation, perform(CardBand::Scene, 3))
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
    row(property, MATERIAL, Simulation, perform(CardBand::Scene, 4))
        .param("viscosity")
        .name("Thickness")
        .unit(Unit::PascalSeconds)
        .tip("How slowly it flows once it gives. Runny honey is about 10, toothpaste about 100, cold caramel thousands; a metre of anything thinner than that spreads within seconds")
        .default(default)
        .range(range.0, range.1)
}

pub static SAND_ROWS: [PropertyRow; 49] = [
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
    row("stiffness", MATERIAL, Simulation, perform(CardBand::Scene, 2))
        .name("Stiffness")
        .tip("How stiff it is. Higher is firmer, wobbles faster and costs more steps a frame"),
    row("friction_angle", MATERIAL, Simulation, perform(CardBand::Scene, 3))
        .name("Friction Angle")
        .unit(Unit::Degrees)
        .tip("How steep a pile holds. Sand flows below it and stops at it")
        .when(sand),
    row("cohesion", MATERIAL, Simulation, perform(CardBand::Scene, 4))
        .name("Cohesion")
        .unit(Unit::Kilopascals)
        .tip("0 is dry sand. Higher holds a cliff, about half a metre per kPa; drop it live to bring the cliff down")
        .when(sand),
    density("density", 1600.0, "How heavy the sand is, which sets how bodies sink into it").when(sand),
    row("hardening", MATERIAL, Simulation, perform(CardBand::Scene, 3))
        .name("Hardening")
        .tip("How much stiffer snow gets as it packs. Around 10 packs into firm clumps that break off; 0 stays loose powder")
        .default(10.0)
        .range(0.0, 20.0)
        .when(snow),
    density("snow_density", 400.0, "How packed the snow is, which sets how heavy and how stiff it is: about 100 for fresh powder that slumps, 400 for settled snow that holds a wall, 600 for wet snow").when(snow),
    firmness("clay_firmness", 3.0e4, (1.0e3, 5.0e4)).when(clay),
    thickness("clay_thickness", 20.0, (0.0, 500.0)).when(clay),
    density("clay_density", 1800.0, "How heavy the clay is, which sets how it sags and how bodies sink into it").when(clay),
    firmness("mud_firmness", 1500.0, (20.0, 5.0e3)).when(mud),
    thickness("mud_thickness", 100.0, (1.0, 1.0e3)).when(mud),
    density("mud_density", 1700.0, "How heavy the mud is, which sets how it spreads and how bodies sink into it").when(mud),
    firmness("goo_firmness", 50.0, (0.0, 500.0)).when(goo),
    thickness("goo_thickness", 2000.0, (1.0, 1.0e4)).when(goo),
    density("goo_density", 1400.0, "How heavy the goo is, which sets how fast it pours").when(goo),
    density("jelly_density", 1050.0, "How heavy the jelly is, which sets how far it sags and how long it wobbles").when(jelly),
    row("jelly_damping", MATERIAL, Simulation, perform(CardBand::Scene, 3))
        .param("viscosity")
        .name("Damping")
        .unit(Unit::PascalSeconds)
        .tip("How fast a wobble dies away. Gelatin is about 80, which settles in a few bounces; 0 wobbles on and on, a few hundred barely wobbles. High values cost more steps a frame at small Cell Sizes")
        .default(80.0)
        .range(0.0, 500.0)
        .when(jelly),
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
        .tip("How big each grain is, at any Cell Size. Halving it draws about four times as many grains at the surface, so the cost follows the grain size and the sand's surface area")
        .when(grains),
    FOLLOW_GRAVITY_X,
    FOLLOW_GRAVITY_Y,
    FOLLOW_GRAVITY_Z,
    FOLLOW_AIR,
    FOLLOW_SPEED,
    FOLLOW_RESET,
];

/// What the Sand is made of, in [`Matter::ALL`] order. The key is the
/// project file's name for it.
const MATTERS: &[Choice] = &[
    Choice {
        key: "sand",
        label: "Sand",
    },
    Choice {
        key: "snow",
        label: "Snow",
    },
    Choice {
        key: "clay",
        label: "Clay",
    },
    Choice {
        key: "mud",
        label: "Mud",
    },
    Choice {
        key: "goo",
        label: "Goo",
    },
    Choice {
        key: "jelly",
        label: "Jelly",
    },
];

/// What the Sand is made of. Changing it keeps the domain, its cells and
/// the fill; the app swaps the look and surface with it (`scene_edit`).
const MATTER_FIELD: FieldRow = FieldRow {
    id: "matter",
    label: "Matter",
    section: Simulation,
    control: FieldControl::Closed(MATTERS),
    applies: always,
    disabled: enabled,
    read: |fields| match fields {
        ItemFields::Sand(s) => {
            FieldValue::Choice(MATTERS[s.matter.index() as usize].key.to_owned())
        }
        _ => FieldValue::Choice(String::new()),
    },
    write: |fields, value| {
        let (ItemFields::Sand(s), FieldValue::Choice(key)) = (&mut *fields, value) else {
            return Err(wrong_value("matter", value));
        };
        let at = MATTERS
            .iter()
            .position(|choice| choice.key == key)
            .ok_or_else(|| wrong_value("matter", value))?;
        s.matter = Matter::ALL[at];
        // Sand and snow draw their loose part as grains and crumbs, the rest
        // as their surface.
        s.draw = if matches!(s.matter, Matter::Sand | Matter::Snow) {
            WaterDraw::Particles
        } else {
            WaterDraw::Surface
        };
        Ok(())
    },
};

pub static SAND_FIELDS: [FieldRow; 5] = [
    MATTER_FIELD,
    BOUNDS_FIELD,
    QUALITY_FIELD,
    DRAW_FIELD,
    MATERIAL_BINDING,
];

/// The look a Sand of `matter` gives its own Material: the Sand template's
/// look with what it is made of over it ([`matter_look`]).
pub fn sand_look(matter: Matter) -> Vec<(String, f32)> {
    let mut look = super::water_look(crate::scene_doc::ItemKind::Sand);
    // A scanned set's pictures carry the colour and roughness, so the
    // Material starts at the file's for them.
    if matter_set(matter).is_some() {
        look.retain(|(property, _)| !FROM_SET.contains(&property.as_str()));
    }
    for (property, value) in matter_look(matter) {
        match look.iter_mut().find(|(name, _)| name == property) {
            Some((_, old)) => *old = *value,
            None => look.push(((*property).to_owned(), *value)),
        }
    }
    look
}

/// The rows a Matter's scanned set gives its Material, which its look
/// leaves to the file.
const FROM_SET: [&str; 4] = ["color_r", "color_g", "color_b", "roughness"];

/// The scanned material set a Sand of `matter` starts with, a folder of the
/// starter materials: its colour, roughness, normal and height pictures,
/// box mapped at the ground's real size and carved by parallax. Sand: dune
/// ripples. Snow: soft drifted snow. Clay: worked clay with finger smears.
/// Mud: damp rippled mud. Goo and jelly are clear and draw no picture.
pub fn matter_set(matter: Matter) -> Option<&'static str> {
    match matter {
        Matter::Sand => Some("Ground097"),
        Matter::Snow => Some("Snow010A"),
        Matter::Clay => Some("Clay006"),
        Matter::Mud => Some("Ground094C"),
        Matter::Goo | Matter::Jelly => None,
    }
}

/// What a Sand of `matter` looks like over the Sand template's look, each
/// from what the material is made of. Sand, snow, clay and mud take their
/// colour, roughness and grain from their scanned set ([`matter_set`]), so
/// the procedural grain is off and a colour here tints the picture:
/// - Snow: ice (IOR 1.31). Light runs millimetres to a centimetre inside,
///   blue the furthest, so creases and thin edges glow faintly blue; a
///   little light stays at the surface for its crust.
/// - Clay: satin, its light spreading a few millimetres, red the furthest,
///   tinted as its picture.
/// - Mud: soil darkened as water darkens it (its picture's colour times
///   about 0.4), under a film of water (a clear coat at water's IOR).
/// - Goo: honey, clear (transmission 1) with its amber from absorption
///   through its own thickness, at honey's IOR 1.49. It takes its full tint
///   over a metre, so a thick blob reads amber rather than red.
/// - Jelly: gelatin, clear and absorbing red, at IOR 1.34.
fn matter_look(matter: Matter) -> &'static [(&'static str, f32)] {
    match matter {
        Matter::Sand => &[("grain", 0.0)],
        Matter::Snow => &[
            ("specular", 0.5),
            ("ior", 1.31),
            ("grain", 0.0),
            ("subsurface_weight", 0.85),
            ("subsurface_radius_r", 0.004),
            ("subsurface_radius_g", 0.006),
            ("subsurface_radius_b", 0.012),
            ("subsurface_color_r", 0.9),
            ("subsurface_color_g", 0.94),
            ("subsurface_color_b", 0.98),
        ],
        Matter::Clay => &[
            ("specular", 0.35),
            ("grain", 0.0),
            ("subsurface_weight", 1.0),
            ("subsurface_radius_r", 0.006),
            ("subsurface_radius_g", 0.003),
            ("subsurface_radius_b", 0.002),
            ("subsurface_color_r", 0.62),
            ("subsurface_color_g", 0.47),
            ("subsurface_color_b", 0.36),
        ],
        Matter::Mud => &[
            ("color_r", 0.4),
            ("color_g", 0.36),
            ("color_b", 0.33),
            ("ior", 1.33),
            ("grain", 0.0),
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
            ("volume_attenuation_distance", 1.0),
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
