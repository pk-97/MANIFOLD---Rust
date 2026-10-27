//! Water (SCENE_DOCUMENT_DESIGN.md section 7.10 (Water, Water Source,
//! Ocean)): a liquid whose Transform is its domain box, with its Water
//! Sources as children. Solver (GPU FLIP or MPM) and Draw As (Surface or
//! Particles) are setup fields; the solver, the surface, the whitewater and
//! the dots come from the water family template the water crate registers
//! for each pair ([`WaterTemplate`]); the compiler splices one copy per
//! Water, its node ids `<water id>:<template node id>`. Its look is its
//! Material, as for any object.
//!
//! The template's params are a family, like a Material's: every param of a
//! template node keeps the template's tuned value (Coarse Solve, Max
//! Iterations, Sheet Fill Rate and the rest are automatic), except the ones a
//! row below names. The card is the rows of section 8: the domain box, the
//! Cell Size, Speed, the solver's dials, the Surface dials and the
//! whitewater Amount.

use std::sync::Arc;

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::fields::{
    Choice, FieldControl, FieldRow, FieldValue, MATERIAL_BINDING, always, enabled, is_on, on_off,
    parse_on, wrong_value,
};
use super::{Exposure, PropertyRow, Reach, follows, perform, row, setup};
use crate::effect_graph_def::{EffectGraphNode, EffectGraphWire, SerializedParamValue};
use crate::fluid_domain::{CELL_SIZE_METRES, DEFAULT_CELL_SIZE, MIN_DOMAIN_SIDE_METRES};
use crate::scene_doc::{Item, ItemFields, SourceRole, WaterDraw, WaterFields, WaterSolver};

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

/// Registered by the crate that owns the water solvers, one per solver and
/// draw.
pub struct WaterTemplate {
    pub solver: WaterSolver,
    pub draw: WaterDraw,
    pub family: fn() -> Arc<WaterFamily>,
}
inventory::collect!(WaterTemplate);

/// The registered water family for `solver` drawn as `draw`, or `None` when
/// no water crate is linked.
pub fn water_family(solver: WaterSolver, draw: WaterDraw) -> Option<Arc<WaterFamily>> {
    inventory::iter::<WaterTemplate>
        .into_iter()
        .find(|template| template.solver == solver && template.draw == draw)
        .map(|template| (template.family)())
}

/// The template node a water's look is.
pub const WATER_MATERIAL_ROLE: &str = "water_material";

/// The template node that draws the water itself.
pub const WATER_OBJECT_ROLE: &str = "water_object";

/// The water look the template draws with: its water material's values,
/// which a new Water's own Material starts at.
pub fn water_look() -> Vec<(String, f32)> {
    let Some(family) = water_family(WaterSolver::Flip, WaterDraw::Surface) else {
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
/// inputs, the water's foam cover, its mist and its wetness. Each follows the
/// World's Render Mode, on only in Physics View, so the side that is off is
/// never computed.
pub const PHYSICS_VIEW_SWITCHES: [&str; 12] = [
    "water_field_switch",
    "water_velocity_switch",
    "water_nodes_x_switch",
    "water_nodes_y_switch",
    "water_nodes_z_switch",
    "water_visible",
    "dots_visible",
    "dots_instances_switch",
    "dots_mesh_switch",
    "water_foam_switch",
    "water_mist_switch",
    "water_wet_switch",
];

/// The template nodes Surface Detail sets together.
pub const SURFACE_DETAIL_NODES: [&str; 2] = ["liquid_volume", "liquid_bricks"];

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
const DOMAIN: Reach = Reach::Own(DOMAIN_ROLE);
const SPEED: Reach = Reach::Own("speed");
const CELL_SIZE: Reach = Reach::Own("cell_size");
const DETAIL: Reach = Reach::Own("surface_detail");
const BLOBS: Reach = Reach::Own("liquid_blobs");
const SMOOTHNESS: Reach = Reach::Own("liquid_smoothness");
const RIPPLES: Reach = Reach::Own("water_ripple_detail");
const FOAM: Reach = Reach::Own("water_foam_cover");
const MIST: Reach = Reach::Own("water_mist");
const WETNESS: Reach = Reach::Own("water_wetness");
use ControlGroup::{Domain, Simulation, Surface, Whitewater};

/// The domain box is axis-aligned: the solvers take no rotation.
const AXIS_ALIGNED: &str = "the domain box is axis-aligned; the solvers take no rotation";

/// Each side of the box, within what the solvers hold.
const SIZE: HardLimit = HardLimit::AtLeast(MIN_DOMAIN_SIDE_METRES);

/// The Dam Break tank: four metres a side, standing on the floor.
const DOMAIN_SIDE: f32 = 4.0;

fn water(fields: &ItemFields) -> Option<&WaterFields> {
    match fields {
        ItemFields::Water(water) => Some(water),
        _ => None,
    }
}

/// The Water's own box and walls are on: the size and wall rows exist.
fn bounded(fields: &ItemFields) -> bool {
    water(fields).is_some_and(|water| water.bounds)
}

fn flip(fields: &ItemFields) -> bool {
    water(fields).is_some_and(|water| water.solver == WaterSolver::Flip)
}

fn mpm(fields: &ItemFields) -> bool {
    water(fields).is_some_and(|water| water.solver == WaterSolver::Mpm)
}

fn surface(fields: &ItemFields) -> bool {
    water(fields).is_some_and(|water| water.draw == WaterDraw::Surface)
}

/// A drawn surface on the solver that makes whitewater, which is where foam,
/// spray mist and wetness are.
fn flip_surface(fields: &ItemFields) -> bool {
    flip(fields) && surface(fields)
}

pub static WATER_ROWS: [PropertyRow; 41] = [
    row("pos_x", T, Domain, setup())
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited),
    row("pos_y", T, Domain, setup())
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited)
        .default(DOMAIN_SIDE / 2.0),
    row("pos_z", T, Domain, setup())
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited),
    row("scale_x", T, Domain, setup())
        .name("Size X")
        .unit(Unit::Metres)
        .limit(SIZE)
        .default(DOMAIN_SIDE)
        .when(bounded),
    row("scale_y", T, Domain, setup())
        .name("Size Y")
        .unit(Unit::Metres)
        .limit(SIZE)
        .default(DOMAIN_SIDE)
        .when(bounded),
    row("scale_z", T, Domain, setup())
        .name("Size Z")
        .unit(Unit::Metres)
        .limit(SIZE)
        .default(DOMAIN_SIDE)
        .when(bounded),
    row("rot_x", T, Domain, Exposure::Internal(AXIS_ALIGNED)),
    row("rot_y", T, Domain, Exposure::Internal(AXIS_ALIGNED)),
    row("rot_z", T, Domain, Exposure::Internal(AXIS_ALIGNED)),
    row("billboard", T, Domain, Exposure::Internal(AXIS_ALIGNED)),
    row("closed_neg_x", DOMAIN, Domain, setup())
        .name("Left Wall")
        .when(bounded),
    row("closed_pos_x", DOMAIN, Domain, setup())
        .name("Right Wall")
        .when(bounded),
    row("closed_neg_y", DOMAIN, Domain, setup())
        .name("Floor")
        .when(bounded),
    row("closed_pos_y", DOMAIN, Domain, setup())
        .name("Ceiling")
        .when(bounded),
    row("closed_neg_z", DOMAIN, Domain, setup())
        .name("Back Wall")
        .when(bounded),
    row("closed_pos_z", DOMAIN, Domain, setup())
        .name("Front Wall")
        .when(bounded),
    // The cell edge in metres; the box's cells follow from its size.
    row("cell_size", CELL_SIZE, Domain, setup())
        .param("value")
        .name("Cell Size")
        .unit(Unit::Metres)
        .default(DEFAULT_CELL_SIZE)
        .limit(HardLimit::Between(CELL_SIZE_METRES.0, CELL_SIZE_METRES.1)),
    row("speed", SPEED, Simulation, perform(CardBand::Scene, 0))
        .param("b")
        .name("Speed")
        .default(1.0)
        .limit(HardLimit::Between(0.0, 4.0)),
    row(
        "speed_world",
        SPEED,
        Simulation,
        Exposure::Automatic("the World's Speed, through its value node"),
    )
    .param("a"),
    row(
        "speed_op",
        SPEED,
        Simulation,
        Exposure::Automatic("Speed times the World's Speed"),
    )
    .param("op"),
    // MPM's dials, as main's Matter scenes showed them.
    row("stiffness", DOMAIN, Simulation, perform(CardBand::Scene, 2))
        .name("Stiffness")
        .when(mpm),
    row("cohesion", DOMAIN, Simulation, perform(CardBand::Scene, 3))
        .name("Cohesion")
        .when(mpm),
    row(
        "liveliness",
        DOMAIN,
        Simulation,
        perform(CardBand::Scene, 4),
    )
    .name("FLIP Ratio")
    .when(mpm),
    row("points_per_cell", DOMAIN, Simulation, setup())
        .name("Particles per Cell")
        .when(mpm),
    row("seed", DOMAIN, Simulation, setup())
        .name("Seed")
        .when(mpm),
    row(
        "domain_size",
        DOMAIN,
        Domain,
        Exposure::Internal("the domain box's Size sets the box"),
    ),
    row(
        "fill_height",
        DOMAIN,
        Domain,
        Exposure::Internal("a Fill source fills the water"),
    )
    .default(0.0),
    // Shown from 0, FLIP Fluids' subdivision 1; the surface nodes take it plus one.
    row("surface_detail", DETAIL, Surface, setup())
        .param("value")
        .name("Surface Detail")
        .default(0.0)
        .range(0.0, 2.0)
        .when(surface),
    row("surface_particle_scale", BLOBS, Surface, setup())
        .param("particle_scale")
        .name("Particle Scale")
        .when(surface),
    // Times the smoothing's leash (FIELD_LEASH_CELLS in manifold-water-liquid): 1 is the tuned surface, 0 the raw particle field.
    row("surface_smoothness", SMOOTHNESS, Surface, setup())
        .param("value")
        .name("Smoothness")
        .default(1.0)
        .range(0.0, 2.0)
        .when(surface),
    // Times the automatic ripples on the surface: 1 is their natural strength, 0 none.
    row("surface_ripple_detail", RIPPLES, Surface, setup())
        .param("value")
        .name("Ripple Detail")
        .default(1.0)
        .range(0.0, 2.0)
        .when(surface),
    // Times the foam's area: 1 is the foam the particles make, 0 none.
    row("surface_foam", FOAM, Surface, setup())
        .param("strength")
        .name("Foam")
        .default(1.0)
        .range(0.0, 4.0)
        .when(flip_surface),
    // Times the spray's droplet extinction: 1 is the mist the spray makes, 0 none.
    row("surface_mist", MIST, Surface, setup())
        .param("strength")
        .name("Mist")
        .default(1.0)
        .range(0.0, 4.0)
        .when(flip_surface),
    // Times still-air evaporation: 1 dries a soaked floor in about half an hour, 100 in about 17 s, 0 never.
    row("surface_drying", WETNESS, Surface, setup())
        .param("drying")
        .name("Drying")
        .default(1.0)
        .range(0.0, 1000.0)
        .when(flip_surface),
    row(
        "whitewater_amount",
        Reach::Own("whitewater"),
        Whitewater,
        perform(CardBand::Scene, 1),
    )
    .param("amount")
    .name("Amount")
    .when(flip),
    follows(WORLD_FOLLOWS[0], "the World's Gravity X"),
    follows(WORLD_FOLLOWS[1], "the World's Gravity Y"),
    follows(WORLD_FOLLOWS[2], "the World's Gravity Z"),
    follows(WORLD_FOLLOWS[3], "the World's Air Resistance"),
    follows(WORLD_FOLLOWS[4], "the World's Speed"),
    follows(WORLD_FOLLOWS[5], "the World's Reset"),
];

/// The roles the Water's rows reach inside the template, which the compiler
/// adopts after splicing it.
pub fn template_roles() -> impl Iterator<Item = &'static str> {
    WATER_ROWS.iter().filter_map(|row| match row.reach {
        Reach::Own(role)
            if !matches!(role, "transform" | "speed" | "cell_size" | "surface_detail")
                && !WORLD_FOLLOWS.contains(&role) =>
        {
            Some(role)
        }
        _ => None,
    })
}

const SOLVERS: &[Choice] = &[
    Choice {
        key: "flip",
        label: "FLIP",
    },
    Choice {
        key: "mpm",
        label: "MPM",
    },
];

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

/// Why `item` cannot take the solver or draw `key`, or `None`.
pub fn choice_unavailable(item: &Item, field: &str, key: &str) -> Option<&'static str> {
    let fields = water(&item.fields)?;
    match (field, key) {
        ("solver", "mpm") if fields.solver != WaterSolver::Mpm => {
            let flows = item
                .children
                .iter()
                .any(|child| matches!(&child.fields, ItemFields::WaterSource(source) if source.role != SourceRole::Fill));
            flows.then_some("MPM fills only; Inflow and Drain run on FLIP. Remove them first")
        }
        _ => None,
    }
}

pub static WATER_FIELDS: [FieldRow; 4] = [
    FieldRow {
        id: "bounds",
        label: "Bounds",
        section: ControlGroup::Domain,
        control: on_off(),
        applies: always,
        disabled: enabled,
        read: |fields| is_on(matches!(fields, ItemFields::Water(w) if w.bounds)),
        write: |fields, value| {
            let on = parse_on("bounds", value)?;
            match fields {
                ItemFields::Water(w) => w.bounds = on,
                _ => return Err(wrong_value("bounds", value)),
            }
            Ok(())
        },
    },
    FieldRow {
        id: "solver",
        label: "Solver",
        section: ControlGroup::Simulation,
        control: FieldControl::Closed(SOLVERS),
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Water(w) => FieldValue::Choice(
                if w.solver == WaterSolver::Mpm {
                    "mpm"
                } else {
                    "flip"
                }
                .to_owned(),
            ),
            _ => FieldValue::Choice(String::new()),
        },
        write: |fields, value| {
            let (ItemFields::Water(w), FieldValue::Choice(key)) = (fields, value) else {
                return Err(wrong_value("solver", value));
            };
            w.solver = match key.as_str() {
                "flip" => WaterSolver::Flip,
                "mpm" => WaterSolver::Mpm,
                _ => return Err(wrong_value("solver", value)),
            };
            Ok(())
        },
    },
    FieldRow {
        id: "draw",
        label: "Draw As",
        section: ControlGroup::Surface,
        control: FieldControl::Closed(DRAWS),
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Water(w) => FieldValue::Choice(
                if w.draw == WaterDraw::Particles {
                    "particles"
                } else {
                    "surface"
                }
                .to_owned(),
            ),
            _ => FieldValue::Choice(String::new()),
        },
        write: |fields, value| {
            let (ItemFields::Water(w), FieldValue::Choice(key)) = (fields, value) else {
                return Err(wrong_value("draw", value));
            };
            w.draw = match key.as_str() {
                "surface" => WaterDraw::Surface,
                "particles" => WaterDraw::Particles,
                _ => return Err(wrong_value("draw", value)),
            };
            Ok(())
        },
    },
    MATERIAL_BINDING,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_schema::applicable;

    fn applies(fields: &ItemFields, property: &str) -> bool {
        applicable(fields).any(|row| row.property == property)
    }

    /// The box size and the six walls are the Bounds switch's rows: they
    /// exist with Bounds on and not with it off.
    #[test]
    fn size_and_wall_rows_follow_bounds() {
        let on = ItemFields::Water(WaterFields {
            bounds: true,
            ..WaterFields::default()
        });
        let off = ItemFields::Water(WaterFields::default());
        for property in [
            "scale_x",
            "scale_y",
            "scale_z",
            "closed_neg_x",
            "closed_pos_z",
        ] {
            assert!(applies(&on, property), "{property} with Bounds on");
            assert!(!applies(&off, property), "{property} with Bounds off");
        }
        assert!(applies(&off, "cell_size"));
    }
}
