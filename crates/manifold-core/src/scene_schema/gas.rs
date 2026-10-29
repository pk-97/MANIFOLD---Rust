//! Smoke and Fire (docs/SMOKE_AND_FIRE_MAP.md): one gas solver, `node.gas_volume`,
//! in a box of cells, drawn by a volume `node.scene_object`. The item's
//! Transform is the box, sitting on the ground, and its Quality picks the
//! cells on the World's grid ([`Quality::cell_scale`]). The simulation rows
//! reach the gas node and the look rows reach the object; a Smoke and a Fire
//! are the same card with their own starting values, and only a Fire has the
//! burning and flame rows. Both follow the World's gravity, speed and reset.
//! A gas pushes nothing back, so no force or collider reaches it.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, RgbChannel, Unit};

use super::fields::FieldRow;
use super::water::{
    BILLBOARD, CELL_SIZE_ROW, FOLLOW_GRAVITY_X, FOLLOW_GRAVITY_Y, FOLLOW_GRAVITY_Z, FOLLOW_RESET,
    FOLLOW_SPEED, POS_X, POS_Z, QUALITY_FIELD, ROT_X, ROT_Y, ROT_Z, SIZE,
};
use super::{Exposure, PropertyRow, Reach, perform, row, setup};

const T: Reach = Reach::Own("transform");
const GAS: Reach = Reach::Own("gas");
const DRAW: Reach = Reach::Own("object");
use ControlGroup::{Domain, Look, Simulation};

const NO_BURN: &str = "a Smoke does not burn";
const NO_FLAME: &str = "a Smoke has no flames";
const NO_SHADOWS: &str = "a gas casts no shadows on the scene";
const NO_MESH: &str = "a gas draws a volume, not a mesh with instances and a material";
const NO_WATER_LOOK: &str = "a look of a water surface, and a gas has none";

/// What a kind's dials and box start at.
struct Start {
    /// The box's width and depth in metres.
    side: f32,
    /// The box's height in metres.
    height: f32,
    source_size: f32,
    rate: f32,
    inflow: f32,
    source_heat: f32,
    buoyancy: f32,
    weight: f32,
    vorticity: f32,
    turbulence: f32,
    dissipation: f32,
    cooling: f32,
    smoke: [f32; 3],
    density: f32,
    albedo: f32,
    detail: f32,
    /// A Fire burns: it has the burning and flame rows.
    burns: bool,
    burn_rate: f32,
    heat_release: f32,
    soot: f32,
    expansion: f32,
    flame_brightness: f32,
    kelvin_cool: f32,
    kelvin_hot: f32,
    glow: f32,
    light: f32,
}

const SMOKE: Start = Start {
    side: 2.0,
    height: 4.0,
    source_size: 0.6,
    rate: 4.0,
    inflow: 1.0,
    source_heat: 0.6,
    buoyancy: 2.0,
    weight: 0.2,
    vorticity: 0.3,
    turbulence: 0.3,
    dissipation: 0.15,
    cooling: 0.6,
    smoke: [0.85, 0.85, 0.85],
    density: 30.0,
    albedo: 0.9,
    detail: 0.0,
    burns: false,
    burn_rate: 0.0,
    heat_release: 0.0,
    soot: 0.0,
    expansion: 0.0,
    flame_brightness: 0.0,
    kelvin_cool: 900.0,
    kelvin_hot: 1800.0,
    glow: 0.0,
    light: 0.0,
};

const FIRE: Start = Start {
    side: 1.5,
    height: 3.0,
    source_size: 0.5,
    rate: 6.0,
    inflow: 1.5,
    source_heat: 1.0,
    buoyancy: 6.0,
    weight: 0.1,
    vorticity: 1.0,
    turbulence: 1.0,
    dissipation: 0.3,
    cooling: 1.8,
    smoke: [0.25, 0.24, 0.23],
    density: 25.0,
    albedo: 0.55,
    detail: 0.0,
    burns: true,
    burn_rate: 6.0,
    heat_release: 2.0,
    soot: 0.6,
    expansion: 1.5,
    flame_brightness: 6.0,
    kelvin_cool: 900.0,
    kelvin_hot: 1800.0,
    glow: 0.6,
    light: 1.0,
};

/// The box stands on the ground: its centre is half its height up.
const fn pos_y(height: f32) -> PropertyRow {
    row("pos_y", T, Domain, setup())
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited)
        .default(height / 2.0)
}

const fn size(
    property: &'static str,
    name: &'static str,
    default: f32,
    tip: &'static str,
) -> PropertyRow {
    row(property, T, Domain, setup())
        .name(name)
        .tip(tip)
        .unit(Unit::Metres)
        .limit(SIZE)
        .default(default)
}

/// A simulation dial on the gas node, in the Scene tab (`exposure` setup) or
/// on the performance card.
const fn dial(
    property: &'static str,
    exposure: Exposure,
    name: &'static str,
    default: f32,
    range: (f32, f32),
    tip: &'static str,
) -> PropertyRow {
    row(property, GAS, Simulation, exposure)
        .name(name)
        .tip(tip)
        .default(default)
        .range(range.0, range.1)
}

/// A burning dial: a Fire's, left alone on a Smoke.
const fn burning(
    burns: bool,
    property: &'static str,
    name: &'static str,
    default: f32,
    range: (f32, f32),
    tip: &'static str,
) -> PropertyRow {
    if burns {
        dial(property, setup(), name, default, range, tip)
    } else {
        row(property, GAS, Simulation, Exposure::Internal(NO_BURN))
    }
}

/// A look dial on the scene object. The slider range is the object's own.
const fn look(property: &'static str, name: &'static str, default: f32) -> PropertyRow {
    row(property, DRAW, Look, setup())
        .name(name)
        .default(default)
}

/// A flame look dial: a Fire's, left alone on a Smoke.
const fn flame(
    burns: bool,
    property: &'static str,
    name: &'static str,
    default: f32,
    tip: &'static str,
) -> PropertyRow {
    if burns {
        look(property, name, default).tip(tip)
    } else {
        row(property, DRAW, Look, Exposure::Internal(NO_FLAME))
    }
}

/// An object param a gas leaves at the node's default.
const fn unused(property: &'static str, why: &'static str) -> PropertyRow {
    row(property, DRAW, Look, Exposure::Internal(why))
}

const fn rows(s: Start) -> [PropertyRow; 60] {
    [
        POS_X,
        pos_y(s.height),
        POS_Z,
        size(
            "scale_x",
            "Size X",
            s.side,
            "The box's width in metres. The gas fades out where it reaches the sides.",
        ),
        size(
            "scale_y",
            "Size Y",
            s.height,
            "The box's height in metres. The gas fades out at the top, so a taller box lets it rise further.",
        ),
        size(
            "scale_z",
            "Size Z",
            s.side,
            "The box's depth in metres. The gas fades out where it reaches the sides.",
        ),
        ROT_X,
        ROT_Y,
        ROT_Z,
        BILLBOARD,
        CELL_SIZE_ROW,
        row(
            "cell_metres",
            GAS,
            Domain,
            Exposure::Automatic("wired from the cell-size node"),
        ),
        row(
            "seed",
            GAS,
            Simulation,
            Exposure::Internal("every gas starts from the same noise"),
        ),
        row(
            "gas_kind",
            GAS,
            Simulation,
            Exposure::Automatic("the item's kind: Smoke or Fire"),
        )
        .param("kind"),
        row(
            "sim_quality",
            GAS,
            Simulation,
            Exposure::Automatic("the Quality field"),
        )
        .param("quality"),
        dial(
            "source_size",
            setup(),
            "Source Size",
            s.source_size,
            (0.02, 20.0),
            "How wide the spot is where the gas comes in, at the middle of the box floor.",
        )
        .unit(Unit::Metres),
        dial(
            "rate",
            perform(CardBand::Scene, 0),
            "Rate",
            s.rate,
            (0.0, 100.0),
            "How much the source puts out. More makes a thicker plume or a bigger fire.",
        ),
        dial(
            "inflow",
            setup(),
            "Speed",
            s.inflow,
            (0.0, 30.0),
            "How fast the gas leaves the source. It sets how hard the plume shoots up before it rises on its own.",
        )
        .unit(Unit::MetresPerSecond),
        dial(
            "source_heat",
            perform(CardBand::Scene, 1),
            "Heat",
            s.source_heat,
            (0.0, 2.0),
            "How hot the gas is when it comes in. Hotter gas rises faster.",
        ),
        dial(
            "buoyancy",
            perform(CardBand::Scene, 2),
            "Rise",
            s.buoyancy,
            (0.0, 50.0),
            "How strongly hot gas floats upward. Higher makes a plume that climbs quickly.",
        ),
        dial(
            "weight",
            setup(),
            "Weight",
            s.weight,
            (0.0, 20.0),
            "How strongly the smoke itself sinks. Higher makes heavier smoke that hangs lower.",
        ),
        dial(
            "vorticity",
            setup(),
            "Swirl",
            s.vorticity,
            (0.0, 20.0),
            "How much the gas keeps curling into small eddies instead of smoothing out.",
        ),
        dial(
            "turbulence",
            perform(CardBand::Scene, 3),
            "Turbulence",
            s.turbulence,
            (0.0, 30.0),
            "How hard random gusts push the gas around. Higher breaks the plume up and makes it flicker.",
        ),
        dial(
            "dissipation",
            setup(),
            "Fade",
            s.dissipation,
            (0.0, 10.0),
            "How fast the smoke thins out and disappears.",
        )
        .unit(Unit::PerSecond),
        dial(
            "cooling",
            setup(),
            "Cooling",
            s.cooling,
            (0.0, 20.0),
            "How fast the gas loses its heat. Faster cooling makes a shorter plume.",
        )
        .unit(Unit::PerSecond),
        burning(
            s.burns,
            "burn_rate",
            "Burn Rate",
            s.burn_rate,
            (0.0, 50.0),
            "How fast fuel burns once it is hot enough. Higher makes shorter, brighter flames.",
        ),
        burning(
            s.burns,
            "heat_release",
            "Flame Heat",
            s.heat_release,
            (0.0, 10.0),
            "How much heat burning adds. Higher makes the flames climb taller.",
        ),
        burning(
            s.burns,
            "soot",
            "Smoke Amount",
            s.soot,
            (0.0, 5.0),
            "How much smoke the flames give off as they burn.",
        ),
        burning(
            s.burns,
            "expansion",
            "Expansion",
            s.expansion,
            (0.0, 10.0),
            "How hard burning pushes the gas outward. Higher makes a more explosive flare.",
        ),
        look("gas_smoke_r", "Smoke Colour", s.smoke[0])
            .tip("The colour the smoke scatters back to the camera.")
            .colour(RgbChannel::R),
        look("gas_smoke_g", "Smoke Green", s.smoke[1]).colour(RgbChannel::G),
        look("gas_smoke_b", "Smoke Blue", s.smoke[2]).colour(RgbChannel::B),
        look("gas_density", "Smoke Density", s.density)
            .tip("How thick the smoke is. Higher blocks more light and looks more solid."),
        look("gas_albedo", "Brightness", s.albedo).tip(
            "How much of the light that hits the smoke bounces on instead of being absorbed. Low is dark soot, high is white steam.",
        ),
        look("gas_detail", "Detail", s.detail).tip(
            "How much fine swirling texture the smoke gets below the simulation's cells, carried along by the flow. 0 shows the simulation as it is and costs nothing; at High the simulation has its own detail, so raise it for close-up renders.",
        ),
        flame(
            s.burns,
            "gas_flame_brightness",
            "Flame Brightness",
            s.flame_brightness,
            "How brightly the flames glow. The scene's exposure and bloom treat them as light.",
        ),
        flame(
            s.burns,
            "gas_kelvin_cool",
            "Flame Cool",
            s.kelvin_cool,
            "The flame's colour temperature where it is coolest. Lower is redder.",
        )
        .unit(Unit::Kelvin),
        flame(
            s.burns,
            "gas_kelvin_hot",
            "Flame Hot",
            s.kelvin_hot,
            "The flame's colour temperature where it is hottest. Higher turns yellow, then white.",
        )
        .unit(Unit::Kelvin),
        flame(
            s.burns,
            "gas_glow",
            "Glow",
            s.glow,
            "How strongly the flames light their own smoke.",
        ),
        flame(
            s.burns,
            "gas_light_brightness",
            "Light",
            s.light,
            "How strongly the fire lights the rest of the scene. 0 adds no light.",
        ),
        row(
            "gas_quality",
            DRAW,
            Look,
            Exposure::Automatic("the Quality field"),
        ),
        row("visible", DRAW, Look, setup()).toggle(),
        row(
            "parent_visible",
            DRAW,
            Look,
            Exposure::Automatic("the enclosing group's visibility"),
        ),
        unused("cast_shadows", NO_SHADOWS),
        unused("emission_strength", NO_MESH),
        unused("gain", NO_MESH),
        unused("instances_space", NO_MESH),
        unused("closed", NO_MESH),
        unused("foam", NO_WATER_LOOK),
        unused("foam_coverage", NO_WATER_LOOK),
        unused("mist", NO_WATER_LOOK),
        unused("wet_surfaces", NO_WATER_LOOK),
        unused("ripples", NO_WATER_LOOK),
        unused("caustics", NO_WATER_LOOK),
        unused("caustic_intensity", NO_WATER_LOOK),
        FOLLOW_GRAVITY_X,
        FOLLOW_GRAVITY_Y,
        FOLLOW_GRAVITY_Z,
        FOLLOW_SPEED,
        FOLLOW_RESET,
    ]
}

pub static SMOKE_ROWS: [PropertyRow; 60] = rows(SMOKE);
pub static FIRE_ROWS: [PropertyRow; 60] = rows(FIRE);

/// The Quality field, shared with Water and Sand.
pub static GAS_FIELDS: [FieldRow; 1] = [QUALITY_FIELD];

#[cfg(test)]
mod tests {
    use super::super::{FieldValue, add_entry, field_for, row_for};
    use crate::scene_doc::{ItemFields, Quality};

    /// A Fire from the Add menu starts at High with the Fire's own dials and
    /// look, and a Smoke at the Smoke's, whatever the node would default to.
    /// The Quality field reads and writes both.
    #[test]
    fn a_new_fire_starts_at_high_with_its_own_look() {
        let fire = add_entry("fire").unwrap().fields.unwrap()();
        let mut smoke = add_entry("smoke").unwrap().fields.unwrap()();
        assert!(matches!(&fire, ItemFields::Fire(gas) if gas.quality == Quality::High));
        assert!(matches!(&smoke, ItemFields::Smoke(gas) if gas.quality == Quality::High));
        let default = |fields: &ItemFields, property: &str| {
            row_for(fields, property)
                .unwrap_or_else(|| panic!("no row {property}"))
                .default
        };
        assert_eq!(default(&fire, "gas_flame_brightness"), Some(6.0));
        assert_eq!(default(&fire, "pos_y"), Some(1.5));
        assert_eq!(default(&fire, "scale_y"), Some(3.0));
        assert_eq!(default(&smoke, "pos_y"), Some(2.0));
        assert_eq!(default(&smoke, "gas_density"), Some(30.0));
        // A Smoke has no flames and does not burn.
        assert!(
            row_for(&smoke, "gas_flame_brightness")
                .is_some_and(|row| matches!(row.exposure, super::Exposure::Internal(_)))
        );
        assert!(
            row_for(&smoke, "burn_rate")
                .is_some_and(|row| matches!(row.exposure, super::Exposure::Internal(_)))
        );
        assert!(
            row_for(&fire, "burn_rate")
                .is_some_and(|row| matches!(row.exposure, super::Exposure::Card(_)))
        );

        let quality = field_for(&smoke, "quality").expect("a Smoke has a Quality field");
        let extra_high = FieldValue::Choice("extra_high".to_owned());
        (quality.write)(&mut smoke, &extra_high).unwrap();
        assert!(matches!(&smoke, ItemFields::Smoke(gas) if gas.quality == Quality::ExtraHigh));
        assert_eq!((quality.read)(&smoke), extra_high);
    }
}
