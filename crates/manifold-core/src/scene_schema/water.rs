//! Water (SCENE_DOCUMENT_DESIGN.md section 7.10 (Water, Water Source,
//! Ocean)): a GPU FLIP liquid whose Transform is its domain box, with its
//! Water Sources as children. The solver, the surface and the whitewater
//! come from the water family template the water crate registers
//! ([`WaterTemplate`]); the compiler splices one copy per Water, its node ids
//! `<water id>:<template node id>`.
//!
//! The template's params are a family, like a Material's: every param of a
//! template node keeps the template's tuned value (Coarse Solve, Max
//! Iterations, Sheet Fill Rate and the rest are automatic), except the ones a
//! row below names. The card is the rows of section 8: the domain box, the
//! Resolution, Speed, the Surface dials and the whitewater Amount.

use std::sync::Arc;

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::{Exposure, PropertyRow, Reach, perform, row, setup};
use crate::effect_graph_def::{EffectGraphNode, EffectGraphWire};
use crate::fluid_domain::DOMAIN_EXTENT_METRES;

/// The water family the scene compiler splices for each Water: the solver,
/// its display and its looks, with no source and no host wiring.
#[derive(Debug, Clone)]
pub struct WaterFamily {
    pub nodes: Vec<EffectGraphNode>,
    pub wires: Vec<EffectGraphWire>,
    /// The node ids of the scene objects it draws, in render order: the
    /// water, foam, spray, bubbles, then Physics View's dots.
    pub objects: Vec<String>,
}

impl WaterFamily {
    /// Whether `node_id` names a node of the template, at any depth.
    pub fn holds(&self, node_id: &str) -> bool {
        fn find(nodes: &[EffectGraphNode], node_id: &str) -> bool {
            nodes.iter().any(|node| {
                node.node_id.as_str() == node_id || node.group.as_deref().is_some_and(|body| find(&body.nodes, node_id))
            })
        }
        find(&self.nodes, node_id)
    }
}

/// Registered by the crate that owns the water solver.
pub struct WaterTemplate {
    pub family: fn() -> Arc<WaterFamily>,
}
inventory::collect!(WaterTemplate);

/// The registered water family, or `None` when no water crate is linked.
pub fn water_family() -> Option<Arc<WaterFamily>> {
    inventory::iter::<WaterTemplate>.into_iter().next().map(|source| (source.family)())
}

/// Why a template param no row names is left alone.
pub const WATER_FAMILY_REASON: &str = "the water template's tuned value";

/// The template node the domain is.
pub const DOMAIN_ROLE: &str = "domain";

/// The template's Physics View switches: the surface's two inputs, the dots'
/// visibility and the dots' two inputs. Each follows the World's Render
/// Mode, on only in Physics View, so the side that is off is never computed.
pub const PHYSICS_VIEW_SWITCHES: [&str; 5] =
    ["water_vertices_switch", "water_indices_switch", "dots_visible", "dots_instances_switch", "dots_mesh_switch"];

/// The template nodes Surface Detail sets together; the first owns the row.
pub const SURFACE_DETAIL_NODES: [&str; 3] = ["liquid_volume", "liquid_mesh", "liquid_bricks"];

/// Whitewater particles the pool holds per cell across the domain's longest
/// side: 102,400 at the default Resolution of 64. A full pool replaces its
/// oldest particles.
pub const WHITEWATER_PER_CELL: f32 = 1600.0;

/// The value node that follows each World control the domain takes, in
/// `scene_world_controls::WORLD_INPUTS` order.
pub const WORLD_FOLLOWS: [&str; 6] =
    ["world_gravity_x", "world_gravity_y", "world_gravity_z", "world_air_resistance", "world_speed", "world_reset"];

const T: Reach = Reach::Own("transform");
const DOMAIN: Reach = Reach::Own(DOMAIN_ROLE);
const SPEED: Reach = Reach::Own("speed");
const RESOLUTION: Reach = Reach::Own("resolution");
const CAPACITY: Reach = Reach::Own("whitewater_capacity");
const BLOBS: Reach = Reach::Own("liquid_blobs");
use ControlGroup::{Domain, Simulation, Surface, Whitewater};

/// The domain box is axis-aligned: the solvers take no rotation.
const AXIS_ALIGNED: &str = "the domain box is axis-aligned; the solvers take no rotation";

/// Each side of the box, within what the solvers hold.
const SIZE: HardLimit = HardLimit::Between(DOMAIN_EXTENT_METRES.0, DOMAIN_EXTENT_METRES.1);

/// The Dam Break tank: four metres a side, standing on the floor.
const DOMAIN_SIDE: f32 = 4.0;

const fn follows(role: &'static str, why: &'static str) -> PropertyRow {
    row(role, Reach::Own(role), Simulation, Exposure::Automatic(why)).param("value")
}

pub static WATER_ROWS: [PropertyRow; 32] = [
    row("pos_x", T, Domain, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("pos_y", T, Domain, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited).default(DOMAIN_SIDE / 2.0),
    row("pos_z", T, Domain, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("scale_x", T, Domain, setup()).name("Size X").unit(Unit::Metres).limit(SIZE).default(DOMAIN_SIDE),
    row("scale_y", T, Domain, setup()).name("Size Y").unit(Unit::Metres).limit(SIZE).default(DOMAIN_SIDE),
    row("scale_z", T, Domain, setup()).name("Size Z").unit(Unit::Metres).limit(SIZE).default(DOMAIN_SIDE),
    row("rot_x", T, Domain, Exposure::Internal(AXIS_ALIGNED)),
    row("rot_y", T, Domain, Exposure::Internal(AXIS_ALIGNED)),
    row("rot_z", T, Domain, Exposure::Internal(AXIS_ALIGNED)),
    row("billboard", T, Domain, Exposure::Internal(AXIS_ALIGNED)),
    // Cells across the longest side; the solver takes 8 to 512.
    row("resolution", RESOLUTION, Domain, setup()).param("value").name("Resolution").default(64.0).limit(HardLimit::Between(8.0, 512.0)),
    row("speed", SPEED, Simulation, perform(CardBand::Scene, 0)).param("b").name("Speed").default(1.0).limit(HardLimit::Between(0.0, 4.0)),
    row("speed_world", SPEED, Simulation, Exposure::Automatic("the World's Speed, through its value node")).param("a"),
    row("speed_op", SPEED, Simulation, Exposure::Automatic("Speed times the World's Speed")).param("op"),
    row("whitewater_capacity_cells", CAPACITY, Whitewater, Exposure::Automatic("the Resolution")).param("a"),
    row("whitewater_capacity_per_cell", CAPACITY, Whitewater, Exposure::Internal("whitewater per cell across the longest side")).param("b").default(WHITEWATER_PER_CELL),
    row("whitewater_capacity_op", CAPACITY, Whitewater, Exposure::Automatic("Resolution times whitewater per cell")).param("op"),
    row("domain_size", DOMAIN, Domain, Exposure::Internal("the domain box's Size sets the box")),
    row("fill_height", DOMAIN, Domain, Exposure::Internal("a Fill source fills the water")).default(0.0),
    row("surface_detail", Reach::Own(SURFACE_DETAIL_NODES[0]), Surface, setup())
        .param("resolution_scale")
        .name("Surface Detail")
        .range(1.0, 3.0),
    row("surface_particle_scale", BLOBS, Surface, setup()).param("particle_scale").name("Particle Scale"),
    row("surface_stretch", BLOBS, Surface, setup()).param("stretch").name("Stretch"),
    row("surface_centre_smoothing", BLOBS, Surface, setup()).param("smoothing").name("Centre Smoothing"),
    row("surface_fill_pits", Reach::Own("liquid_fill_pits"), Surface, setup()).param("value").name("Fill Pits"),
    row("surface_smoothing", Reach::Own("liquid_smooth_mesh"), Surface, setup()).param("iterations").name("Smoothing"),
    row("whitewater_amount", Reach::Own("whitewater"), Whitewater, perform(CardBand::Scene, 1)).param("amount").name("Amount"),
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
        Reach::Own(role) if !matches!(role, "transform" | "speed" | "resolution" | "whitewater_capacity") && !WORLD_FOLLOWS.contains(&role) => {
            Some(role)
        }
        _ => None,
    })
}
