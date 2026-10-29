//! Water (SCENE_DOCUMENT_DESIGN.md section 7.10 (Water, Water Source,
//! Ocean)): a GPU FLIP liquid whose Transform is its domain box, with its
//! Water Sources as children. A Sand (`sand.rs`) is the same kind of domain
//! on Matter. Draw As (Surface or Particles) is a setup field; the solver,
//! the surface, the whitewater and the dots come from the family template
//! the water crate registers for each kind and draw ([`WaterTemplate`]); the
//! compiler splices one copy per Water or Sand, its node ids
//! `<item id>:<template node id>`. Its look is its Material, as for any
//! object.
//!
//! The template's params are a family, like a Material's: every param of a
//! template node keeps the template's tuned value (Coarse Solve, Max
//! Iterations, Sheet Fill Rate and the rest are automatic), except the ones a
//! row below names. The card is the rows of section 8: the domain box, the
//! Cell Size, the solver's dials, the Surface dials and the
//! whitewater Amount.

use std::sync::Arc;

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Feature, Gate, Unit};

use super::fields::{
    Choice, FieldControl, FieldRow, FieldValue, MATERIAL_BINDING, always, enabled, is_on, on_off,
    parse_on, wrong_value,
};
use super::{Exposure, PropertyRow, Reach, follows, perform, row, setup};
use crate::effect_graph_def::{EffectGraphNode, EffectGraphWire, SerializedParamValue};
use crate::fluid_domain::{CELL_SIZE_METRES, DEFAULT_CELL_SIZE, MIN_DOMAIN_SIDE_METRES};
use crate::scene_doc::{ItemFields, ItemKind, WaterDraw};

/// The water family the scene compiler splices for each Water: the solver,
/// its display and its looks, with no source and no host wiring.
#[derive(Debug, Clone)]
pub struct WaterFamily {
    pub nodes: Vec<EffectGraphNode>,
    pub wires: Vec<EffectGraphWire>,
    /// The node ids of the scene objects it draws, in render order: the
    /// water, then spray and bubbles where the solver has whitewater,
    /// then Physics View's dots where it draws a surface.
    pub objects: Vec<String>,
}

impl WaterFamily {
    /// Whether `node_id` names a node of the template, at any depth.
    pub fn holds(&self, node_id: &str) -> bool {
        fn find(nodes: &[EffectGraphNode], node_id: &str) -> bool {
            nodes.iter().any(|node| {
                node.node_id.as_str() == node_id
                    || node
                        .group
                        .as_deref()
                        .is_some_and(|body| find(&body.nodes, node_id))
            })
        }
        find(&self.nodes, node_id)
    }
}

/// Registered by the crate that owns the solvers, one per kind (Water or
/// Sand), draw and Narrow Band.
pub struct WaterTemplate {
    pub kind: ItemKind,
    pub draw: WaterDraw,
    /// GPU FLIP's Narrow Band variant; Sand registers only `false`.
    pub narrow_band: bool,
    pub family: fn() -> Arc<WaterFamily>,
}
inventory::collect!(WaterTemplate);

/// The registered family for a Water's or Sand's fields, or `None` for any
/// other item or when no water crate is linked.
pub fn water_family(fields: &ItemFields) -> Option<Arc<WaterFamily>> {
    let (kind, draw, narrow_band) = match fields {
        ItemFields::Water(w) => (ItemKind::Water, w.draw, w.narrow_band),
        ItemFields::Sand(s) => (ItemKind::Sand, s.draw, false),
        _ => return None,
    };
    inventory::iter::<WaterTemplate>
        .into_iter()
        .find(|template| {
            template.kind == kind && template.draw == draw && template.narrow_band == narrow_band
        })
        .map(|template| (template.family)())
}

/// The template node a water's look is.
pub const WATER_MATERIAL_ROLE: &str = "water_material";

/// The template node that draws the water itself.
pub const WATER_OBJECT_ROLE: &str = "water_object";

/// The look a new Water's or Sand's template draws with: its material's
/// values, which the new item's own Material starts at.
pub fn water_look(kind: ItemKind) -> Vec<(String, f32)> {
    let Some(family) = water_family(&ItemFields::default_for(kind)) else {
        return Vec::new();
    };
    let Some(material) = family
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == WATER_MATERIAL_ROLE)
    else {
        return Vec::new();
    };
    material
        .params
        .iter()
        .filter_map(|(name, value)| match value {
            SerializedParamValue::Float { value } => Some((name.clone(), *value)),
            SerializedParamValue::Int { value } => Some((name.clone(), *value as f32)),
            _ => None,
        })
        .collect()
}

/// Why a template param no row names is left alone.
pub const WATER_FAMILY_REASON: &str = "the water template's tuned value";

/// The template node the domain is.
pub const DOMAIN_ROLE: &str = "domain";

/// The template's Physics View switches: the field's inputs (its level set,
/// its velocity lattice and its node counts, which come from the surface's
/// volume), the water's visibility, the dots' visibility, the dots' two
/// inputs, the water's mist, its wetness and its surface mesh. The foam needs
/// none: it draws only on the field, which Physics View takes away.
/// Each follows the World's Render Mode, on only in Physics View, so the side
/// that is off is never computed.
pub const PHYSICS_VIEW_SWITCHES: [&str; 11] = [
    "water_field_switch",
    "water_velocity_switch",
    "water_tiles_switch",
    "water_extent_switch",
    "water_visible",
    "dots_visible",
    "dots_instances_switch",
    "dots_mesh_switch",
    "water_mist_switch",
    "water_wet_switch",
    "water_vertices_switch",
];

/// The template nodes Surface Detail sets: the display's world tiles, whose
/// cell size every drawn field shares.
pub const SURFACE_DETAIL_NODES: [&str; 1] = ["surface_tiles"];

/// Surface Detail 0 is one surface lattice node per cell, FLIP Fluids' default.
pub const SURFACE_DETAIL_OFFSET: f32 = 1.0;

/// The value node that follows each World control the domain takes, in
/// `scene_world_controls::WORLD_INPUTS` order.
pub const WORLD_FOLLOWS: [&str; 6] = [
    "world_gravity_x",
    "world_gravity_y",
    "world_gravity_z",
    "world_air_resistance",
    "world_speed",
    "world_reset",
];

const T: Reach = Reach::Own("transform");
pub(super) const DOMAIN: Reach = Reach::Own(DOMAIN_ROLE);
const CELL_SIZE: Reach = Reach::Own("cell_size");
const DETAIL: Reach = Reach::Own("surface_detail");
const PARTICLE_SCALE: Reach = Reach::Own("liquid_particle_scale");
const SMOOTHNESS: Reach = Reach::Own("liquid_smoothness");
const CLOSE_GAPS: Reach = Reach::Own("liquid_close_gaps");
const RIPPLES: Reach = Reach::Own("water_ripple_detail");
const MIST: Reach = Reach::Own("water_mist");
const WETNESS: Reach = Reach::Own("water_wetness");
const LOOKS: Reach = Reach::Own(WATER_OBJECT_ROLE);
use ControlGroup::{Domain, Surface, Whitewater};

/// The domain box is axis-aligned: the solvers take no rotation.
const AXIS_ALIGNED: &str = "the domain box is axis-aligned; the solvers take no rotation";

/// Each side of the box, within what the solvers hold.
const SIZE: HardLimit = HardLimit::AtLeast(MIN_DOMAIN_SIDE_METRES);

/// The Dam Break tank: four metres a side, standing on the floor.
const DOMAIN_SIDE: f32 = 4.0;

const WALL_TIP: &str = "On: this face of the box is a wall that holds the contents in. Off: they flow out through it and are removed once they leave the box";

/// A Water's or Sand's Bounds switch and draw.
fn domain(fields: &ItemFields) -> Option<(bool, WaterDraw)> {
    match fields {
        ItemFields::Water(w) => Some((w.bounds, w.draw)),
        ItemFields::Sand(s) => Some((s.bounds, s.draw)),
        _ => None,
    }
}

/// The item's own box and walls are on: the size and wall rows exist.
fn bounded(fields: &ItemFields) -> bool {
    domain(fields).is_some_and(|(bounds, _)| bounds)
}

pub(super) fn surface(fields: &ItemFields) -> bool {
    domain(fields).is_some_and(|(_, draw)| draw == WaterDraw::Surface)
}

// The rows a Water and a Sand share: the box, its walls, the Cell Size, the
// internal domain params, the surface build and the World controls.
pub(super) const POS_X: PropertyRow = row("pos_x", T, Domain, setup())
    .unit(Unit::Metres)
    .limit(HardLimit::Unlimited);
pub(super) const POS_Y: PropertyRow = row("pos_y", T, Domain, setup())
    .unit(Unit::Metres)
    .limit(HardLimit::Unlimited)
    .default(DOMAIN_SIDE / 2.0);
pub(super) const POS_Z: PropertyRow = row("pos_z", T, Domain, setup())
    .unit(Unit::Metres)
    .limit(HardLimit::Unlimited);
pub(super) const SCALE_X: PropertyRow = row("scale_x", T, Domain, setup())
    .name("Size X")
    .tip("The Bounds box's width in metres: the space its walls enclose")
    .unit(Unit::Metres)
    .limit(SIZE)
    .default(DOMAIN_SIDE)
    .when(bounded);
pub(super) const SCALE_Y: PropertyRow = row("scale_y", T, Domain, setup())
    .name("Size Y")
    .tip("The Bounds box's height in metres: the space its walls enclose")
    .unit(Unit::Metres)
    .limit(SIZE)
    .default(DOMAIN_SIDE)
    .when(bounded);
pub(super) const SCALE_Z: PropertyRow = row("scale_z", T, Domain, setup())
    .name("Size Z")
    .tip("The Bounds box's depth in metres: the space its walls enclose")
    .unit(Unit::Metres)
    .limit(SIZE)
    .default(DOMAIN_SIDE)
    .when(bounded);
pub(super) const ROT_X: PropertyRow = row("rot_x", T, Domain, Exposure::Internal(AXIS_ALIGNED));
pub(super) const ROT_Y: PropertyRow = row("rot_y", T, Domain, Exposure::Internal(AXIS_ALIGNED));
pub(super) const ROT_Z: PropertyRow = row("rot_z", T, Domain, Exposure::Internal(AXIS_ALIGNED));
pub(super) const BILLBOARD: PropertyRow =
    row("billboard", T, Domain, Exposure::Internal(AXIS_ALIGNED));
pub(super) const LEFT_WALL: PropertyRow = row("closed_neg_x", DOMAIN, Domain, setup())
    .name("Left Wall")
    .tip(WALL_TIP)
    .when(bounded);
pub(super) const RIGHT_WALL: PropertyRow = row("closed_pos_x", DOMAIN, Domain, setup())
    .name("Right Wall")
    .tip(WALL_TIP)
    .when(bounded);
pub(super) const FLOOR: PropertyRow = row("closed_neg_y", DOMAIN, Domain, setup())
    .name("Floor")
    .tip(WALL_TIP)
    .when(bounded);
pub(super) const CEILING: PropertyRow = row("closed_pos_y", DOMAIN, Domain, setup())
    .name("Ceiling")
    .tip(WALL_TIP)
    .when(bounded);
pub(super) const BACK_WALL: PropertyRow = row("closed_neg_z", DOMAIN, Domain, setup())
    .name("Back Wall")
    .tip(WALL_TIP)
    .when(bounded);
pub(super) const FRONT_WALL: PropertyRow = row("closed_pos_z", DOMAIN, Domain, setup())
    .name("Front Wall")
    .tip(WALL_TIP)
    .when(bounded);
// The cell edge in metres; the box's cells follow from its size.
pub(super) const CELL_SIZE_ROW: PropertyRow = row("cell_size", CELL_SIZE, Domain, setup())
    .param("value")
    .name("Cell Size")
    .tip("The simulation's grid cell in metres. Smaller cells show finer detail but cost far more: half the size holds eight times as many cells")
    .unit(Unit::Metres)
    .default(DEFAULT_CELL_SIZE)
    .limit(HardLimit::Between(CELL_SIZE_METRES.0, CELL_SIZE_METRES.1));
pub(super) const DOMAIN_SIZE: PropertyRow = row(
    "domain_size",
    DOMAIN,
    Domain,
    Exposure::Internal("the domain box's Size sets the box"),
);
pub(super) const FILL_HEIGHT: PropertyRow = row(
    "fill_height",
    DOMAIN,
    Domain,
    Exposure::Internal("a Fill source fills the domain"),
)
.default(0.0);
// Shown from 0, FLIP Fluids' subdivision 1; the surface nodes take it plus one.
pub(super) const SURFACE_DETAIL: PropertyRow = row("surface_detail", DETAIL, Surface, setup())
    .param("value")
    .name("Surface Detail")
    .tip("How finely the surface is built: 0 at the simulation's cell size, 1 at half, 2 at a third. Finer shows smaller details and sharper edges, and costs more")
    .default(0.0)
    .range(0.0, 2.0)
    .when(surface);
pub(super) const SURFACE_PARTICLE_SCALE: PropertyRow =
    row("surface_particle_scale", PARTICLE_SCALE, Surface, setup())
        .param("value")
        .name("Particle Scale")
        .tip("How large each simulation particle is drawn, times its true size. Larger closes gaps in thin sheets but rounds the surface off; smaller keeps edges crisp but shows single particles")
        .when(surface);
// Times the smoothing's leash (FIELD_LEASH_CELLS in manifold-water-liquid): 1 is the tuned surface, 0 the raw particle field.
pub(super) const SURFACE_SMOOTHNESS: PropertyRow =
    row("surface_smoothness", SMOOTHNESS, Surface, setup())
        .param("value")
        .name("Smoothness")
        .tip("How much the surface is smoothed between particles: 0 shows each particle's bumps, 2 is the smoothest")
        .default(1.0)
        .range(0.0, 2.0)
        .when(surface);
// The surface group's switch between the raw level set and its closing.
pub(super) const SURFACE_CLOSE_GAPS: PropertyRow =
    row("surface_close_gaps", CLOSE_GAPS, Surface, setup())
        .param("selector")
        .name("Close Gaps")
        .tip("Closes holes and gaps in the surface up to two surface cells across, so a thin sheet holds together instead of breaking into beads. Pieces closer than that join up")
        .toggle()
        .default(1.0)
        .when(surface);
pub(super) const FOLLOW_GRAVITY_X: PropertyRow = follows(WORLD_FOLLOWS[0], "the World's Gravity X");
pub(super) const FOLLOW_GRAVITY_Y: PropertyRow = follows(WORLD_FOLLOWS[1], "the World's Gravity Y");
pub(super) const FOLLOW_GRAVITY_Z: PropertyRow = follows(WORLD_FOLLOWS[2], "the World's Gravity Z");
pub(super) const FOLLOW_AIR: PropertyRow = follows(WORLD_FOLLOWS[3], "the World's Air Resistance");
pub(super) const FOLLOW_SPEED: PropertyRow = follows(WORLD_FOLLOWS[4], "the World's Speed");
pub(super) const FOLLOW_RESET: PropertyRow = follows(WORLD_FOLLOWS[5], "the World's Reset");

pub static WATER_ROWS: [PropertyRow; 42] = [
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
    SURFACE_DETAIL,
    SURFACE_PARTICLE_SCALE,
    SURFACE_SMOOTHNESS,
    SURFACE_CLOSE_GAPS,
    // Each optional look is a switch on the water object, which leaves the
    // look and its passes out while off, with its tuning row folded under it.
    row("look_ripples", LOOKS, Surface, setup())
        .param("ripples")
        .name("Surface Ripples")
        .tip("Small ripples on the surface, carried by the flow. They catch the light and bend the sun into caustics; still water keeps none")
        .switch(Feature::Ripples)
        .toggle()
        .default(1.0)
        .when(surface),
    // Times the automatic ripples on the surface: 1 is their natural strength, 0 none.
    row("surface_ripple_detail", RIPPLES, Surface, setup())
        .param("value")
        .name("Ripple Strength")
        .tip("How strong the ripples are, times their natural strength for how fast the water flows")
        .gate(Gate::Fold(Feature::Ripples))
        .default(1.0)
        .range(0.0, 2.0)
        .when(surface),
    row("look_foam", LOOKS, Surface, setup())
        .param("foam")
        .name("Foam")
        .tip("White foam lying on the surface where the water churns, from the simulation's foam particles")
        .switch(Feature::Foam)
        .toggle()
        .default(1.0)
        .when(surface),
    // Times the foam's area: 1 is the foam the particles make, 0 none.
    row("surface_foam", LOOKS, Surface, setup())
        .param("foam_coverage")
        .name("Foam Coverage")
        .tip("How much of the surface the foam covers, times what the simulation makes")
        .gate(Gate::Fold(Feature::Foam))
        .default(1.0)
        .range(0.0, 4.0)
        .when(surface),
    row("look_mist", LOOKS, Surface, setup())
        .param("mist")
        .name("Spray Mist")
        .tip("A fine haze where the spray is dense, lit by the lights and the sky. Off by default: it costs 2 to 4 ms a frame on the Dam Break")
        .switch(Feature::Mist)
        .toggle()
        .default(0.0)
        .when(surface),
    // Times the spray's droplet extinction: 1 is the mist the spray makes, 0 none.
    row("surface_mist", MIST, Surface, setup())
        .param("strength")
        .name("Mist Density")
        .tip("How thick the haze is, times what the spray's drops would make")
        .gate(Gate::Fold(Feature::Mist))
        .default(1.0)
        .range(0.0, 4.0)
        .when(surface),
    row("look_wet", LOOKS, Surface, setup())
        .param("wet_surfaces")
        .name("Wet Surfaces")
        .tip("Surfaces the water touches turn darker and glossier, then dry")
        .switch(Feature::WetSurfaces)
        .toggle()
        .default(1.0)
        .when(surface),
    // Times the stage drying rate: at 1 a splash fades to a third in 15 s, 0 never dries.
    row("surface_drying", WETNESS, Surface, setup())
        .param("drying")
        .name("Drying Speed")
        .tip("How fast wet surfaces dry: at 1 a splash fades to a third in 15 s, at 0 they stay wet")
        .gate(Gate::Fold(Feature::WetSurfaces))
        .default(1.0)
        .range(0.0, 10.0)
        .when(surface),
    row("look_caustics", LOOKS, Surface, setup())
        .param("caustics")
        .name("Caustics")
        .tip("Bright moving light the surface focuses onto what lies under the water. Needs a Sun that casts shadows")
        .switch(Feature::Caustics)
        .toggle()
        .default(1.0)
        .when(surface),
    row("caustic_intensity", LOOKS, Surface, setup())
        .param("caustic_intensity")
        .name("Caustic Intensity")
        .tip("How strongly the caustics show: 1 is physical, higher exaggerates them, 0 leaves the sunlight under the water plain")
        .gate(Gate::Fold(Feature::Caustics))
        .default(1.0)
        .range(0.0, 4.0)
        .when(surface),
    row(
        "whitewater_amount",
        Reach::Own("whitewater"),
        Whitewater,
        perform(CardBand::Scene, 1),
    )
    .param("amount")
    .name("Whitewater Amount")
    .tip("How much spray, foam and bubbles the simulation spawns, times its default rate"),
    row("spray_visible", Reach::Own("spray_object"), Whitewater, setup())
        .param("visible")
        .name("Spray Drops")
        .tip("Draws the drops splashes throw into the air")
        .toggle()
        .default(1.0),
    row("bubble_visible", Reach::Own("bubble_object"), Whitewater, setup())
        .param("visible")
        .name("Bubbles")
        .tip("Draws the air bubbles carried under the surface, seen through the water")
        .toggle()
        .default(1.0),
    FOLLOW_GRAVITY_X,
    FOLLOW_GRAVITY_Y,
    FOLLOW_GRAVITY_Z,
    FOLLOW_AIR,
    FOLLOW_SPEED,
    FOLLOW_RESET,
];

/// The roles a Water's or Sand's rows (`rows`) reach inside its template,
/// which the compiler adopts after splicing it.
pub fn template_roles(rows: &'static [PropertyRow]) -> impl Iterator<Item = &'static str> {
    rows.iter().filter_map(|row| match row.reach {
        Reach::Own(role)
            if !matches!(role, "transform" | "cell_size" | "surface_detail")
                && !WORLD_FOLLOWS.contains(&role) =>
        {
            Some(role)
        }
        _ => None,
    })
}

const DRAWS: &[Choice] = &[
    Choice {
        key: "surface",
        label: "Surface",
    },
    Choice {
        key: "particles",
        label: "Particles",
    },
];

/// The Bounds switch, shared with Sand.
pub(super) const BOUNDS_FIELD: FieldRow = FieldRow {
    id: "bounds",
    label: "Bounds",
    section: ControlGroup::Domain,
    control: on_off(),
    applies: always,
    disabled: enabled,
    read: |fields| is_on(bounded(fields)),
    write: |fields, value| {
        let on = parse_on("bounds", value)?;
        match fields {
            ItemFields::Water(w) => w.bounds = on,
            ItemFields::Sand(s) => s.bounds = on,
            _ => return Err(wrong_value("bounds", value)),
        }
        Ok(())
    },
};

/// Draw As, shared with Sand.
pub(super) const DRAW_FIELD: FieldRow = FieldRow {
    id: "draw",
    label: "Draw As",
    section: ControlGroup::Surface,
    control: FieldControl::Closed(DRAWS),
    applies: always,
    disabled: enabled,
    read: |fields| match domain(fields) {
        Some((_, draw)) => FieldValue::Choice(
            if draw == WaterDraw::Particles {
                "particles"
            } else {
                "surface"
            }
            .to_owned(),
        ),
        None => FieldValue::Choice(String::new()),
    },
    write: |fields, value| {
        let FieldValue::Choice(key) = value else {
            return Err(wrong_value("draw", value));
        };
        let draw = match key.as_str() {
            "surface" => WaterDraw::Surface,
            "particles" => WaterDraw::Particles,
            _ => return Err(wrong_value("draw", value)),
        };
        match fields {
            ItemFields::Water(w) => w.draw = draw,
            ItemFields::Sand(s) => s.draw = draw,
            _ => return Err(wrong_value("draw", value)),
        }
        Ok(())
    },
};

pub static WATER_FIELDS: [FieldRow; 4] = [
    BOUNDS_FIELD,
    FieldRow {
        id: "narrow_band",
        label: "Narrow Band",
        section: ControlGroup::Simulation,
        control: on_off(),
        applies: always,
        disabled: enabled,
        read: |fields| is_on(matches!(fields, ItemFields::Water(w) if w.narrow_band)),
        write: |fields, value| {
            let on = parse_on("narrow_band", value)?;
            match fields {
                ItemFields::Water(w) => w.narrow_band = on,
                _ => return Err(wrong_value("narrow_band", value)),
            }
            Ok(())
        },
    },
    DRAW_FIELD,
    MATERIAL_BINDING,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_schema::applicable;

    fn applies(fields: &ItemFields, property: &str) -> bool {
        applicable(fields).any(|row| row.property == property)
    }

    /// The box size and the six walls are the Bounds switch's rows, on a
    /// Water and a Sand alike: they exist with Bounds on and not with it off.
    #[test]
    fn size_and_wall_rows_follow_bounds() {
        use crate::scene_doc::{SandFields, WaterFields};
        let pairs = [
            (
                ItemFields::Water(WaterFields {
                    bounds: true,
                    ..WaterFields::default()
                }),
                ItemFields::Water(WaterFields::default()),
            ),
            (
                ItemFields::Sand(SandFields::default()),
                ItemFields::Sand(SandFields {
                    bounds: false,
                    ..SandFields::default()
                }),
            ),
        ];
        for (on, off) in &pairs {
            for property in [
                "scale_x",
                "scale_y",
                "scale_z",
                "closed_neg_x",
                "closed_pos_z",
            ] {
                assert!(applies(on, property), "{property} with Bounds on");
                assert!(!applies(off, property), "{property} with Bounds off");
            }
            assert!(applies(off, "cell_size"));
        }
    }
}
