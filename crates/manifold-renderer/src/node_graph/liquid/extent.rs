//! The CPU extent proof (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.7
//! (Safety rails), rule 1). Before any GPU run, a liquid preset's graph is
//! walked in plan order, and every buffer an atom dispatches over or indexes
//! into is checked to cover that reach. Nothing here runs on the GPU.
//!
//! Array sizes follow the planner and the executor's growth rule: an output
//! is sized by its node's `array_output_capacity` from the inputs it
//! actually receives. Storage a node provides itself, and the lattice and
//! count wires the domains publish, come from the node type's rule, which
//! calls the same functions the node sizes and dispatches with. Scalar
//! plumbing (values, math, transforms) runs through the real nodes. Every
//! node that touches an array or the GPU needs a rule; a missing rule fails
//! by name.
//!
//! The walk is over the unfused graph. What a fused region allocates is the
//! freeze compiler's contract (BUG-2efy (fused output capacity probe)).

use std::cell::RefCell;
use std::mem::size_of;

use ahash::AHashMap;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::{FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID, is_liquid_domain};
use manifold_core::{Beats, Seconds};

use crate::generators::mesh_common::{InstanceTransform, MeshVertex};
use crate::node_graph::physics::MAX_COPIES;
use crate::node_graph::fluid_particles::{CellRange, FluidBlob, FluidParticle, MAX_BINS, bin_counts, bin_total, searched_bins};
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::freeze::classify::fusion_kind_str;
use crate::node_graph::liquid::EXACT_F32_COUNT;
use crate::node_graph::liquid::bodies::{LiquidBody, LiquidShape};
use crate::node_graph::liquid::clock::MAX_LIVE_TICKS;
use crate::node_graph::liquid::fields::{FieldFrame, FieldLattice, STAGING_SLOTS as FIELD_STAGING_SLOTS};
use crate::node_graph::liquid::frame_ring::RING;
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::matter::{
    ACCUM_WORDS_PER_NODE, MatterGridNode, MatterPoint, REACTION_WORDS, STATS_WORDS, grid_accum_bytes, grid_bytes,
    lattice_blocks, lattice_nodes, solid_bytes,
};
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::ports::PortType;
use crate::node_graph::primitives::fluid_surface::{boundary_collisions, fluid_settings};
use crate::node_graph::primitives::matter_domain::{fill_region, matter_geometry};
use crate::node_graph::primitives::matter_fill::{fill_cells, fill_count};
use crate::node_graph::primitives::particle_volume::{refined_nodes, volume_scale};
use crate::node_graph::primitives::sort_particles_into_cells::range_storage_bytes;
use crate::node_graph::primitives::volume_surface_mesh::mesh_capacity;
use crate::node_graph::resource_allocation::plan_array_allocations;
use crate::node_graph::transform::Transform;
use crate::node_graph::{
    Backend, EffectGraphDefExt, EffectNodeContext, ExecutionPlan, ExecutionStep, FrameTime, Graph, MockBackend,
    NodeInputs, NodeInstance, NodeOutputs, ParamValues, PrimitiveRegistry, ResourceId, Slot, compile,
};

/// The canvas canvas-sized arrays are planned at: the stage.
const CANVAS: (u32, u32) = (1920, 1080);

/// A resolved scalar or transform wire.
#[derive(Clone, Copy, Debug)]
pub enum Wire {
    F32(f32),
    Transform(Transform),
}

/// Why a rule stopped.
#[derive(Debug)]
pub enum Verdict {
    /// The node refuses this setup by name before any GPU work; nothing
    /// downstream sees it.
    Refused(String),
    /// A buffer does not cover what the node reaches.
    Uncovered(String),
}

/// One node type's extent rule. `check` reads the node's resolved wires,
/// params and bound array sizes; a node that provides storage or publishes
/// lattice and count wires states them through [`AtomExtent::provide`] and
/// [`AtomExtent::publish`].
#[derive(Clone, Copy)]
pub struct ExtentRule {
    pub type_id: &'static str,
    pub check: fn(&mut AtomExtent<'_>) -> Result<(), Verdict>,
}

#[derive(Debug)]
pub enum ExtentError {
    Build(String),
    Refused { node: String, reason: String },
    Uncovered { node: String, detail: String },
    NoRule { node: String, type_id: String },
    Unresolved { node: String, port: String },
}

impl std::fmt::Display for ExtentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Build(error) => write!(f, "build: {error}"),
            Self::Refused { node, reason } => write!(f, "{node} refuses: {reason}"),
            Self::Uncovered { node, detail } => write!(f, "{node}: {detail}"),
            Self::NoRule { node, type_id } => write!(f, "{node}: no extent rule for {type_id}"),
            Self::Unresolved { node, port } => write!(f, "{node}.{port}: the wire's value is not resolved on the CPU"),
        }
    }
}

/// A checked graph: the nodes ruled on, and the device bytes the scene holds
/// once every array has grown to what the walk reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtentReport {
    pub checked: usize,
    pub scene_bytes: u64,
}

/// One node's view during the walk.
pub struct AtomExtent<'a> {
    node: &'a NodeInstance,
    step: &'a ExecutionStep,
    plan: &'a ExecutionPlan,
    wires: &'a AHashMap<ResourceId, Wire>,
    bytes: &'a AHashMap<ResourceId, u64>,
    unresolved: RefCell<Option<&'static str>>,
    provided: Vec<(&'static str, u64)>,
    published: Vec<(&'static str, Wire)>,
    held: u64,
}

impl AtomExtent<'_> {
    fn name(&self) -> String {
        format!("{} ({})", self.node.node_id.as_str(), self.node.node.type_id().as_str())
    }

    fn input(&self, port: &str) -> Option<ResourceId> {
        self.step.inputs.iter().find(|(name, _)| *name == port).map(|&(_, resource)| resource)
    }

    fn output(&self, port: &str) -> Option<ResourceId> {
        self.step.outputs.iter().find(|(name, _)| *name == port).map(|&(_, resource)| resource)
    }

    pub fn params(&self) -> &ParamValues {
        &self.node.params
    }

    /// `param_f32`: a Float param, else `default`.
    pub fn param(&self, name: &str, default: f32) -> f32 {
        match self.node.params.get(name) {
            Some(ParamValue::Float(value)) => *value,
            _ => default,
        }
    }

    pub fn wired(&self, port: &str) -> bool {
        self.input(port).is_some()
    }

    /// `scalar_or_param`: the wire, else a Float param, else `default`.
    pub fn scalar(&self, name: &str, default: f32) -> f32 {
        let Some(resource) = self.input(name) else { return self.param(name, default) };
        match self.wires.get(&resource) {
            Some(Wire::F32(value)) => *value,
            _ => {
                self.unresolved.borrow_mut().get_or_insert(self.step.inputs.iter().find(|(n, _)| *n == name).expect("wired").0);
                default
            }
        }
    }

    pub fn transform(&self, name: &str) -> Option<Transform> {
        let resource = self.input(name)?;
        match self.wires.get(&resource) {
            Some(Wire::Transform(value)) => Some(*value),
            _ => {
                self.unresolved.borrow_mut().get_or_insert(self.step.inputs.iter().find(|(n, _)| *n == name).expect("wired").0);
                None
            }
        }
    }

    /// A count carried on an f32 wire, refused past the range f32 holds
    /// exactly.
    pub fn count(&self, name: &str, default: f32) -> Result<u32, Verdict> {
        let value = self.scalar(name, default).round().max(0.0);
        if value > EXACT_F32_COUNT as f32 {
            return Err(self.uncovered(format!("{name} {value} exceeds the exact f32 range ({EXACT_F32_COUNT})")));
        }
        Ok(value as u32)
    }

    /// The lattice the node reads from its wires, as `LiquidLattice::from_wires`.
    pub fn lattice(&self) -> LiquidLattice {
        LiquidLattice::from_scalars(|name, default| self.scalar(name, default))
    }

    /// Three whole-number wires, as the surface atoms round them.
    pub fn nodes(&self, names: [&str; 3]) -> [f32; 3] {
        names.map(|name| self.scalar(name, 2.0).round())
    }

    /// Bytes bound to an input or output array port.
    pub fn bytes(&self, port: &str) -> Option<u64> {
        if let Some(&(_, bytes)) = self.provided.iter().find(|(name, _)| *name == port) {
            return Some(bytes);
        }
        let resource = self.input(port).or_else(|| self.output(port))?;
        self.bytes.get(&resource).copied()
    }

    /// Records an array port holds.
    pub fn items(&self, port: &str) -> Option<u64> {
        let resource = self.input(port).or_else(|| self.output(port))?;
        match self.plan.resource_type(resource) {
            Some(PortType::Array(layout)) => Some(self.bytes(port)? / u64::from(layout.item_size)),
            _ => None,
        }
    }

    /// The port is bound and holds at least `need` bytes.
    pub fn covers(&self, port: &str, need: u64) -> Result<(), Verdict> {
        match self.bytes(port) {
            Some(have) if have >= need => Ok(()),
            Some(have) => Err(self.uncovered(format!("{port} holds {have} bytes; the dispatch reaches {need}"))),
            None => Err(self.uncovered(format!("{port} is unbound"))),
        }
    }

    /// As [`Self::covers`] when the port is bound; an unbound optional port
    /// is never written.
    pub fn covers_if_bound(&self, port: &str, need: u64) -> Result<(), Verdict> {
        if self.bytes(port).is_some() { self.covers(port, need) } else { Ok(()) }
    }

    pub fn uncovered(&self, detail: String) -> Verdict {
        Verdict::Uncovered(detail)
    }

    /// Size of storage this node provides on `port`, as consumers see it.
    pub fn provide(&mut self, port: &'static str, bytes: u64) {
        self.provided.push((port, bytes));
    }

    /// Device bytes this node holds for itself (provided storage with its
    /// ring slots, private scratch).
    pub fn hold(&mut self, bytes: u64) {
        self.held += bytes;
    }

    pub fn publish(&mut self, port: &'static str, value: f32) {
        self.published.push((port, Wire::F32(value)));
    }

    pub fn publish_transform(&mut self, port: &'static str, value: Transform) {
        self.published.push((port, Wire::Transform(value)));
    }
}

/// Scalar plumbing whose outputs the walk takes from the real node.
const PLUMBING: &[&str] = &["node.value", "node.math", "node.transform_components", "node.transform_3d", "node.lfo"];

/// A node needs a rule when it touches an array or does GPU work.
fn needs_rule(node: &NodeInstance) -> bool {
    let array = |ty: &PortType| matches!(ty, PortType::Array(_));
    node.node.inputs().iter().any(|port| array(&port.ty))
        || node.node.outputs().iter().any(|port| array(&port.ty))
        || fusion_kind_str(node.node.as_ref()) != "boundary:non_gpu"
}

/// Walk `graph` under `plan` with `rules`.
pub fn check_graph(graph: &mut Graph, plan: &ExecutionPlan, rules: &[ExtentRule]) -> Result<ExtentReport, ExtentError> {
    let planned = plan_array_allocations(graph, plan, CANVAS, &AHashMap::default())
        .map_err(|error| ExtentError::Build(error.to_string()))?;
    let mut bytes: AHashMap<ResourceId, u64> = AHashMap::default();
    let mut provided: AHashMap<ResourceId, u64> = AHashMap::default();
    // State captures read what a later step writes: the second pass sees them.
    let mut report = ExtentReport { checked: 0, scene_bytes: 0 };
    for pass in 0..2 {
        let last = pass == 1;
        let mut wires: AHashMap<ResourceId, Wire> = AHashMap::default();
        let mut held = 0u64;
        let mut checked = 0;
        for step in plan.steps() {
            let node = graph.get_node(step.node).expect("plan node");
            let type_id = node.node.type_id().as_str();
            // Outputs the planner and the growth rule size.
            let mut capacities = Vec::new();
            for &(port, resource) in &step.inputs {
                if let Some(PortType::Array(layout)) = plan.resource_type(resource)
                    && let Some(&have) = bytes.get(&resource)
                {
                    capacities.push((port, u32::try_from(have / u64::from(layout.item_size)).unwrap_or(u32::MAX)));
                }
            }
            for &(port, resource) in &step.outputs {
                let Some(PortType::Array(layout)) = plan.resource_type(resource) else { continue };
                if node.node.provides_array_output(port) {
                    continue;
                }
                let planned_bytes = planned.storage.get(&resource).map_or(0, |storage| storage.bytes);
                let aliased = node.node.aliased_array_io().iter().find(|(_, output)| *output == port).and_then(|(input, _)| {
                    step.inputs.iter().find(|(name, _)| name == input).map(|&(_, resource)| resource)
                });
                let size = if let Some(input) = aliased {
                    bytes.get(&input).copied().unwrap_or(planned_bytes)
                } else if node.node.canvas_sized_array_outputs().contains(&port) {
                    planned_bytes
                } else {
                    let grown = node
                        .node
                        .array_output_capacity(port, &node.params, &capacities)
                        .map_or(0, |count| u64::from(count) * u64::from(layout.item_size));
                    grown.max(planned_bytes)
                };
                bytes.insert(resource, size);
            }
            if let Some(rule) = rules.iter().find(|rule| rule.type_id == type_id) {
                let mut atom = AtomExtent {
                    node,
                    step,
                    plan,
                    wires: &wires,
                    bytes: &bytes,
                    unresolved: RefCell::new(None),
                    provided: Vec::new(),
                    published: Vec::new(),
                    held: 0,
                };
                let verdict = (rule.check)(&mut atom);
                let name = atom.name();
                if let Some(port) = atom.unresolved.take() {
                    return Err(ExtentError::Unresolved { node: name, port: port.to_string() });
                }
                match verdict {
                    Err(Verdict::Refused(reason)) => return Err(ExtentError::Refused { node: name, reason }),
                    Err(Verdict::Uncovered(detail)) if last => return Err(ExtentError::Uncovered { node: name, detail }),
                    _ => {}
                }
                let AtomExtent { provided: sizes, published, held: node_held, .. } = atom;
                for &(port, resource) in &step.outputs {
                    if !matches!(plan.resource_type(resource), Some(PortType::Array(_))) || !node.node.provides_array_output(port) {
                        continue;
                    }
                    let Some(&(_, size)) = sizes.iter().find(|(name, _)| *name == port) else {
                        return Err(ExtentError::Uncovered { node: name, detail: format!("the rule does not size provided {port}") });
                    };
                    bytes.insert(resource, size);
                    provided.insert(resource, size);
                }
                for (port, value) in published {
                    if let Some(&(_, resource)) = step.outputs.iter().find(|(name, _)| *name == port) {
                        wires.insert(resource, value);
                    }
                }
                held += node_held;
                checked += 1;
            } else if needs_rule(node) {
                return Err(ExtentError::NoRule { node: node.node_id.as_str().to_string(), type_id: type_id.to_string() });
            } else if PLUMBING.contains(&type_id) {
                evaluate_plumbing(graph, plan, step, &mut wires);
            }
        }
        if last {
            // Every physical root once, at the largest size any resource on
            // it reached; provided storage is counted by its holder.
            let mut roots: AHashMap<ResourceId, u64> = AHashMap::default();
            for (resource, storage) in &planned.storage {
                if provided.contains_key(resource) {
                    continue;
                }
                let size = bytes.get(resource).copied().unwrap_or(storage.bytes);
                let root = roots.entry(storage.root).or_default();
                *root = (*root).max(size);
            }
            report = ExtentReport { checked, scene_bytes: roots.values().sum::<u64>() + held };
        }
    }
    Ok(report)
}

/// Run a plumbing node on its resolved inputs and record its scalar and
/// transform outputs. A node with an unresolved input publishes nothing, so
/// a rule reading through it fails by name.
fn evaluate_plumbing(graph: &mut Graph, plan: &ExecutionPlan, step: &ExecutionStep, wires: &mut AHashMap<ResourceId, Wire>) {
    let mut backend = MockBackend::new();
    let mut inputs = Vec::new();
    for &(port, resource) in &step.inputs {
        let Some(wire) = wires.get(&resource) else { return };
        let Some(ty) = plan.resource_type(resource) else { return };
        let slot = backend.acquire(resource, ty, None, (0, 0));
        match *wire {
            Wire::F32(value) => backend.set_scalar(slot, ParamValue::Float(value)),
            Wire::Transform(value) => backend.set_transform(slot, value),
        }
        inputs.push((port, slot));
    }
    let mut outputs: Vec<(&'static str, Slot)> = Vec::new();
    let mut resources: AHashMap<u32, ResourceId> = AHashMap::default();
    for &(port, resource) in &step.outputs {
        let Some(ty) = plan.resource_type(resource) else { continue };
        if matches!(ty, PortType::Scalar(_) | PortType::Transform) {
            let slot = backend.acquire(resource, ty, None, (0, 0));
            resources.insert(slot.0, resource);
            outputs.push((port, slot));
        }
    }
    let (mut scalars, mut cameras, mut lights, mut materials, mut transforms, mut atmospheres, mut modes, mut objects) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    {
        let node = graph.get_node_mut(step.node).expect("plan node");
        let params = node.params.clone();
        let time = FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 };
        let node_inputs = NodeInputs::new(&inputs, &backend, &[]);
        let node_outputs = NodeOutputs::new(
            &outputs,
            &backend,
            &mut scalars,
            &mut cameras,
            &mut lights,
            &mut materials,
            &mut transforms,
            &mut atmospheres,
            &mut modes,
            &mut objects,
        );
        let mut ctx = EffectNodeContext::new(time, &params, node_inputs, node_outputs, None);
        node.node.evaluate(&mut ctx);
    }
    for (slot, value) in scalars {
        if let (Some(&resource), ParamValue::Float(value)) = (resources.get(&slot.0), value) {
            wires.insert(resource, Wire::F32(value));
        }
    }
    for (slot, value) in transforms {
        if let Some(&resource) = resources.get(&slot.0) {
            wires.insert(resource, Wire::Transform(value));
        }
    }
}

/// Build a preset at one resolution of its liquid domain and walk it.
pub fn check_preset_extents(def: &EffectGraphDef, resolution: u32) -> Result<ExtentReport, ExtentError> {
    let mut preset = LiquidPreset::build(def)?;
    preset.check(resolution)
}

/// A liquid preset built once, checked at any resolution of its domain.
pub struct LiquidPreset {
    graph: Graph,
    plan: ExecutionPlan,
    domains: Vec<crate::node_graph::NodeInstanceId>,
}

impl LiquidPreset {
    pub fn build(def: &EffectGraphDef) -> Result<Self, ExtentError> {
        let registry = PrimitiveRegistry::with_builtin();
        let build = |error: String| ExtentError::Build(error);
        let expanded = crate::node_graph::scene_modifier_expand::expand_scene_modifiers(def, &registry)
            .map_err(|error| build(error.to_string()))?;
        let flat = manifold_core::flatten::flatten_groups(&expanded).map_err(|error| build(error.to_string()))?;
        let graph = flat.into_graph(&registry, &Default::default()).map_err(|error| build(format!("{error:?}")))?;
        let plan = compile(&graph).map_err(|error| build(format!("{error:?}")))?;
        let domains: Vec<_> = graph.nodes().filter(|node| is_liquid_domain(node.node.type_id().as_str())).map(|node| node.id).collect();
        if domains.is_empty() {
            return Err(build("no liquid domain".into()));
        }
        for step in plan.steps() {
            if domains.contains(&step.node) && step.inputs.iter().any(|(port, _)| *port == "resolution") {
                return Err(build("a liquid domain's resolution is wired; the walk sets the param".into()));
            }
        }
        Ok(Self { graph, plan, domains })
    }

    /// Every resolution the domains' Resolution control admits.
    pub fn resolutions(&self) -> std::ops::RangeInclusive<u32> {
        let range = |id| {
            let node = self.graph.get_node(id).expect("domain");
            let def = node.node.parameters().iter().find(|p| p.name == "resolution").expect("a Resolution control");
            let (low, high) = def.range.expect("Resolution has a range");
            (low as u32, high as u32)
        };
        let (low, high) = self.domains.iter().map(|&id| range(id)).fold((0, u32::MAX), |(a, b), (c, d)| (a.max(c), b.min(d)));
        low..=high
    }

    pub fn check(&mut self, resolution: u32) -> Result<ExtentReport, ExtentError> {
        for &id in &self.domains {
            self.graph
                .set_param(id, "resolution", ParamValue::Float(resolution as f32))
                .map_err(|error| ExtentError::Build(format!("{error:?}")))?;
        }
        self.check_authored()
    }

    /// The graph as its def and card set it.
    pub fn check_authored(&mut self) -> Result<ExtentReport, ExtentError> {
        check_graph(&mut self.graph, &self.plan, LIQUID_EXTENT_RULES)
    }

    /// The type ids and Resolution of the domains, as built.
    pub fn domains(&self) -> Vec<(&str, u32)> {
        self.domains
            .iter()
            .map(|&id| {
                let node = self.graph.get_node(id).expect("domain");
                let resolution = node.params.get("resolution").and_then(ParamValue::as_scalar).unwrap_or(0.0);
                (node.node.type_id().as_str(), resolution.round() as u32)
            })
            .collect()
    }
}

// ── Rules ──────────────────────────────────────────────────────────────────

/// Every node type a liquid preset may hold that touches an array or the GPU.
pub const LIQUID_EXTENT_RULES: &[ExtentRule] = &[
    ExtentRule { type_id: MATTER_DOMAIN_TYPE_ID, check: matter_domain },
    ExtentRule { type_id: FLIP_DOMAIN_TYPE_ID, check: fluid_surface },
    ExtentRule { type_id: "node.matter_fill", check: matter_fill },
    ExtentRule { type_id: "node.matter_state", check: matter_state },
    ExtentRule { type_id: "node.zero_array", check: in_place },
    ExtentRule { type_id: "node.matter_move_bodies", check: matter_move_bodies },
    ExtentRule { type_id: "node.matter_to_grid", check: matter_to_grid },
    ExtentRule { type_id: "node.matter_grid_update", check: matter_grid_update },
    ExtentRule { type_id: "node.matter_body_reaction", check: matter_body_reaction },
    ExtentRule { type_id: "node.grid_to_matter", check: grid_to_matter },
    ExtentRule { type_id: "node.matter_stats", check: matter_stats },
    ExtentRule { type_id: "node.liquid_solid_distance", check: liquid_solid_distance },
    ExtentRule { type_id: "node.matter_frame", check: matter_frame },
    ExtentRule { type_id: "node.sort_particles_into_cells", check: sort_particles_into_cells },
    ExtentRule { type_id: "node.shape_particle_blobs", check: shape_particle_blobs },
    ExtentRule { type_id: "node.particle_volume", check: particle_volume },
    ExtentRule { type_id: "node.smooth_lattice", check: smooth_lattice },
    ExtentRule { type_id: "node.clamp_liquid_to_solids", check: clamp_liquid_to_solids },
    ExtentRule { type_id: "node.count_surface_triangles", check: count_surface_triangles },
    ExtentRule { type_id: "node.running_total", check: running_total },
    ExtentRule { type_id: "node.volume_surface_mesh", check: volume_surface_mesh },
    ExtentRule { type_id: "node.render_scene", check: size_bounded },
    ExtentRule { type_id: "node.scene_object", check: size_bounded },
    ExtentRule { type_id: "node.physics_world", check: physics_world },
    ExtentRule { type_id: "node.cube_mesh", check: size_bounded },
    ExtentRule { type_id: "node.platonic_solid_mesh", check: size_bounded },
    ExtentRule { type_id: "node.bake_environment", check: texture_only },
    ExtentRule { type_id: "node.exposure", check: texture_only },
    ExtentRule { type_id: "node.hdri_source", check: texture_only },
    ExtentRule { type_id: "node.switch_texture", check: texture_only },
    ExtentRule { type_id: "node.tone_map", check: texture_only },
];

fn nodes_total(nodes: [f32; 3]) -> u64 {
    nodes.iter().map(|&n| n.max(0.0) as u64).product()
}

fn lattice_total(nodes: [u32; 3]) -> u64 {
    lattice_nodes(nodes)
}

fn whole(x: &AtomExtent<'_>, name: &str, default: f32) -> u32 {
    x.scalar(name, default).round().max(0.0) as u32
}

/// Texture-only GPU work: no array extent to prove. A node of these types
/// that grows an array port needs a real rule.
fn texture_only(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let array = |ty: &PortType| matches!(ty, PortType::Array(_));
    if x.node.node.inputs().iter().any(|p| array(&p.ty)) || x.node.node.outputs().iter().any(|p| array(&p.ty)) {
        return Err(x.uncovered("an array port on a texture-only rule".into()));
    }
    Ok(())
}

/// Every dispatch or draw is bounded by the size of the buffer it reads or
/// writes: mesh sources write their own output's slots, scene objects hand
/// their arrays to render_scene, which draws each up to its size or its live
/// extent clamped to it.
fn size_bounded(_: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    Ok(())
}

/// The instance upload writes at most one record per rigid copy.
fn physics_world(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers_if_bound("instances", MAX_COPIES as u64 * size_of::<InstanceTransform>() as u64)
}

/// Dispatches over its own input, in place.
fn in_place(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.bytes("in").unwrap_or(0))
}

fn matter_domain(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let geometry = matter_geometry(
        |name, default| x.scalar(name, default),
        x.params(),
        x.transform("domain"),
        x.transform("initial_volume"),
    )
    .map_err(Verdict::Refused)?;
    for (name, value) in geometry.outputs() {
        x.publish(name, value);
    }
    // The walk takes a live frame's most force lattices and an impulse tick,
    // so the field reads are checked.
    let field = FieldFrame {
        lattice: FieldLattice::of(&geometry.setup.lattice),
        force_lattices: MAX_LIVE_TICKS,
        impulse_tick: Some(0),
    };
    let forces = u64::from(MAX_LIVE_TICKS) * field.lattice.bytes();
    for (name, value) in field.outputs() {
        x.publish(name, value);
    }
    // Body kernels clamp rows and body counts to the bodies array, so the
    // walk takes a scene without colliders; the reaction slot is sized for
    // every body a liquid holds.
    let reaction = MAX_FLUID_ROLES as u64 * u64::from(REACTION_WORDS) * 4;
    for (port, bytes) in [
        ("bodies", size_of::<LiquidBody>() as u64),
        ("shapes", size_of::<LiquidShape>() as u64),
        ("atlas", 4),
        ("reaction", reaction),
        ("forces", forces),
        ("impulses", field.lattice.bytes()),
    ] {
        x.provide(port, bytes);
        x.hold(bytes);
    }
    // The staging ring: the impulse lattice and the force lattices per slot.
    x.hold(FIELD_STAGING_SLOTS as u64 * (field.lattice.bytes() + forces));
    Ok(())
}

/// Field reads clamp to the field lattices the scalars name, so the wired
/// buffers must hold them.
fn field_reads(x: &AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = ["field_nodes_x", "field_nodes_y", "field_nodes_z"].map(|name| whole(x, name, 2.0).max(2));
    let bytes = nodes.iter().map(|&n| u64::from(n)).product::<u64>() * 16;
    x.covers_if_bound("forces", u64::from(whole(x, "force_lattices", 0.0)) * bytes)?;
    x.covers_if_bound("impulses", bytes)
}

fn fluid_surface(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let faces = boundary_collisions(x.params()).map_err(Verdict::Refused)?;
    let settings = fluid_settings(
        |name, default| x.scalar(name, default),
        x.params(),
        x.transform("domain"),
        x.transform("initial_volume"),
        faces,
        0,
    );
    let layout = settings.domain_layout().map_err(Verdict::Refused)?;
    layout.admit_flip_grid(x.param("grid_budget_mcells", 8.0)).map_err(Verdict::Refused)?;
    settings.validate().map_err(Verdict::Refused)?;
    let (bounds, nodes) = layout.solid_lattice();
    // The ring grows a slot to each frame it captures, so the published
    // count never exceeds its slot. The walk takes the fill FLIP seeds, eight
    // per cell; emission grows it at run time.
    let (pool, column) = fill_region(&layout, settings.fill_height, settings.initial_volume)
        .map_err(|error| x.uncovered(format!("the fill model disagrees with FLIP's settings: {error}")))?;
    let count = fill_cells(layout.cells, pool, column) * 8;
    x.publish("count_a", count as f32);
    x.publish("count_b", count as f32);
    x.publish_transform("grid_bounds", bounds);
    for (port, n) in ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"].into_iter().zip(nodes) {
        x.publish(port, n as f32);
    }
    let particles = count.max(1) * size_of::<FluidParticle>() as u64;
    let solid = lattice_total(nodes) * 4;
    x.provide("particles_a", particles);
    x.provide("particles_b", particles);
    x.provide("solid_a", solid);
    x.provide("solid_b", solid);
    x.hold(crate::node_graph::fluid::particle_ring::RING_SLOTS as u64 * (particles + solid));
    // The CPU mesh grows its buffer by half again when a surface needs more,
    // through device admission; whitewater uploads stop at their capacity.
    let vertices = u64::from((x.param("max_capacity", 786_432.0).clamp(3.0, 3_145_728.0) as u32 / 3) * 3) * size_of::<MeshVertex>() as u64;
    x.provide("vertices", vertices);
    x.hold(vertices);
    Ok(())
}

fn matter_fill(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice();
    let cells = lattice.cells();
    let pool = whole(x, "pool_cells", 3.0).min(cells[1]);
    let column = [["column_x0", "column_x1"], ["column_y0", "column_y1"], ["column_z0", "column_z1"]]
        .map(|[lo, hi]| [whole(x, lo, 0.0), whole(x, hi, 0.0)]);
    let column = std::array::from_fn(|d| [column[d][0].min(cells[d]), column[d][1].min(cells[d])]);
    let ppc = if whole(x, "points_per_cell", 8.0) >= 27 { 27 } else { 8 };
    let count = fill_count(cells, pool, column, ppc).map_err(Verdict::Refused)?;
    x.publish("count", count as f32);
    let points = u64::from(count.max(1)) * size_of::<MatterPoint>() as u64;
    x.provide("points", points);
    x.hold(points);
    Ok(())
}

fn matter_state(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| whole(x, name, 71.0));
    let count = u64::from(x.count("count", 0.0)?);
    let (accum, grid) = (grid_accum_bytes(nodes).max(32), grid_bytes(nodes).max(32));
    x.provide("grid_accum", accum);
    x.provide("grid", grid);
    x.hold(accum + grid + 4 * u64::from(STATS_WORDS) * 4);
    // A tick's first and last substep: the atoms gated on them run.
    x.publish("tick_start", 1.0);
    x.publish("tick_end", 1.0);
    // A new epoch copies the fill into the state.
    x.covers("seed", count * size_of::<MatterPoint>() as u64)?;
    x.covers("out", count * size_of::<MatterPoint>() as u64)?;
    x.covers("stats", u64::from(STATS_WORDS) * 4)
}

/// Lattice-wide node kernels read the accumulator and grid through the node
/// count; P2G's word index is an i32 node index times four.
fn node_extent(x: &AtomExtent<'_>, lattice: &LiquidLattice) -> Result<u64, Verdict> {
    let nodes = lattice_total(lattice.nodes());
    if nodes > i32::MAX as u64 || nodes * u64::from(ACCUM_WORDS_PER_NODE) > u64::from(u32::MAX) {
        return Err(x.uncovered(format!("{nodes} nodes overflow the accumulator's 32-bit word index")));
    }
    Ok(nodes)
}

fn matter_move_bodies(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // Rows and bodies clamp to the bodies arrays; the reaction is read only
    // when it covers every body.
    x.covers("bodies_out", size_of::<LiquidBody>() as u64)
}

fn matter_to_grid(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice();
    node_extent(x, &lattice)?;
    x.covers("accum", grid_accum_bytes(lattice.nodes()))?;
    if x.wired("order") && x.wired("ranges") {
        let blocks = ["blocks_x", "blocks_y", "blocks_z"].map(|name| whole(x, name, 18.0).max(1));
        if blocks != lattice_blocks(&lattice) {
            return Err(x.uncovered(format!("blocks {blocks:?} are not the lattice's {:?}", lattice_blocks(&lattice))));
        }
        x.covers("ranges", bin_total(blocks) * size_of::<CellRange>() as u64)?;
        // Active points never exceed the points array; the order holds one
        // entry per point slot.
        x.covers("order", x.items("points").unwrap_or(0) * 4)?;
    }
    Ok(())
}

fn matter_grid_update(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice();
    let nodes = node_extent(x, &lattice)?;
    x.covers("grid", grid_bytes(lattice.nodes()))?;
    x.covers("accum", nodes * 16)?;
    field_reads(x)
}

fn matter_body_reaction(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice();
    node_extent(x, &lattice)?;
    x.covers("grid", grid_bytes(lattice.nodes()))?;
    field_reads(x)
}

fn grid_to_matter(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice();
    node_extent(x, &lattice)?;
    // Active points clamp to the points array.
    x.covers("grid", grid_bytes(lattice.nodes()))
}

fn matter_stats(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice();
    let nodes = node_extent(x, &lattice)?;
    x.covers("stats", u64::from(STATS_WORDS) * 4)?;
    x.covers("grid", nodes * size_of::<MatterGridNode>() as u64)?;
    x.covers("accum", nodes * 16)
}

fn liquid_solid_distance(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // One thread per lattice node over storage sized from the same lattice.
    let solid = solid_bytes(x.lattice().nodes());
    x.provide("solid", solid);
    x.hold(solid);
    Ok(())
}

fn matter_frame(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice();
    let count = x.count("count", 0.0)?;
    x.covers("points", u64::from(count) * size_of::<MatterPoint>() as u64)?;
    x.covers("stats", u64::from(STATS_WORDS) * 4)?;
    let particles = u64::from(count.max(1)) * size_of::<FluidParticle>() as u64;
    let solid = solid_bytes(lattice.nodes());
    if x.wired("solid") {
        x.covers("solid", solid)?;
        x.hold(RING as u64 * solid);
    } else {
        x.hold(solid);
    }
    x.provide("particles_a", particles);
    x.provide("particles_b", particles);
    x.provide("solid_a", solid);
    x.provide("solid_b", solid);
    x.hold(RING as u64 * particles);
    x.publish("count_a", count as f32);
    x.publish("count_b", count as f32);
    x.publish_transform("grid_bounds", lattice.bounds());
    for (port, n) in ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"].into_iter().zip(lattice.nodes()) {
        x.publish(port, n as f32);
    }
    Ok(())
}

/// The sort's bin grid is searched with exactly the ranges it allocates, and
/// its last bin index stays inside them in i32.
fn search_fits(x: &AtomExtent<'_>, bins: [u32; 3], range_bytes: u64) -> Result<(), Verdict> {
    let searched = searched_bins(bins.map(|n| n as f32), range_bytes, "search").map_err(|error| x.uncovered(error))?;
    let [bx, by, bz] = bins.map(u64::from);
    let last = (bx - 1) + bx * ((by - 1) + by * (bz - 1));
    if searched != bins || last >= range_bytes / size_of::<CellRange>() as u64 || last > i32::MAX as u64 {
        return Err(x.uncovered(format!("bin {last} of {bins:?} lies past the ranges")));
    }
    Ok(())
}

fn sort_particles_into_cells(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let capacity = x.items("particles").unwrap_or(0);
    x.covers_if_bound("sorted", capacity * size_of::<FluidParticle>() as u64)?;
    x.covers_if_bound("order", capacity * 4)?;
    x.hold(2 * capacity.max(1) * 4);
    if x.scalar("enabled", 1.0) <= 0.5 {
        x.provide("cell_ranges", size_of::<CellRange>() as u64);
        return Ok(());
    }
    let size = ["size_x", "size_y", "size_z"].map(|name| x.scalar(name, 4.0));
    let cell_size = x.scalar("cell_size", 0.0625);
    let center = ["center_x", "center_y", "center_z"].map(|name| x.scalar(name, 0.0));
    if !(cell_size.is_finite() && cell_size > 0.0) || center.iter().chain(&size).any(|v| !v.is_finite()) || size.iter().any(|v| *v <= 0.0) {
        return Err(x.uncovered(format!("box {center:?} {size:?} and cell size {cell_size} must be finite and positive")));
    }
    let bins = bin_counts(size, cell_size);
    if bin_total(bins) > MAX_BINS {
        return Err(Verdict::Refused(format!(
            "Sort Particles Into Cells: a {}×{}×{} bin grid is more than the {MAX_BINS} bins a search can index. Raise the cell size.",
            bins[0], bins[1], bins[2]
        )));
    }
    let range_bytes = range_storage_bytes(bins);
    search_fits(x, bins, range_bytes)?;
    x.provide("cell_ranges", range_bytes);
    x.hold(range_bytes + bin_total(bins) * 4);
    for (port, n) in ["bins_x", "bins_y", "bins_z"].into_iter().zip(bins) {
        x.publish(port, n as f32);
    }
    Ok(())
}

/// The bins a searching atom reads, as `read_searched_bins`, checked against
/// the ranges it indexes.
fn searched(x: &AtomExtent<'_>) -> Result<[u32; 3], Verdict> {
    let ports = ["bins_x", "bins_y", "bins_z"];
    let bins = ports.map(|port| x.scalar(port, 0.0));
    let ranges = x.bytes("cell_ranges").ok_or_else(|| x.uncovered("cell_ranges is unbound".into()))?;
    let bins = if bins == [0.0; 3] && ports.iter().all(|port| !x.wired(port)) {
        let size = ["size_x", "size_y", "size_z"].map(|name| x.scalar(name, 4.0));
        bin_counts(size, x.scalar("cell_size", 0.0625)).map(|n| n as f32)
    } else {
        bins
    };
    searched_bins(bins, ranges, "search").map_err(|error| x.uncovered(error))
}

fn shape_particle_blobs(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    searched(x)?;
    // One blob per sorted slot.
    let slots = x.items("sorted").unwrap_or(0);
    x.covers("blobs", slots * size_of::<FluidBlob>() as u64)
}

fn particle_volume(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    let refined = refined_nodes(nodes, volume_scale(x.params()));
    for (port, n) in ["volume_nodes_x", "volume_nodes_y", "volume_nodes_z"].into_iter().zip(refined) {
        x.publish(port, n as f32);
    }
    searched(x)?;
    x.covers("solid", nodes_total(nodes) * 4)?;
    x.covers("levelset", lattice_total(refined) * 4)
}

fn smooth_lattice(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("smoothed", nodes_total(nodes) * 4)
}

fn clamp_liquid_to_solids(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    let solid = x.nodes(["solid_nodes_x", "solid_nodes_y", "solid_nodes_z"]);
    if nodes.iter().chain(&solid).any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}, solid {solid:?}")));
    }
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("clamped", nodes_total(nodes) * 4)?;
    x.covers("solid", nodes_total(solid) * 4)
}

fn count_surface_triangles(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("counts", nodes_total(nodes.map(|n| n - 1.0)) * 4)
}

fn running_total(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // The scan runs over min(count, in, out).
    x.covers("out", x.bytes("in").unwrap_or(0))
}

fn volume_surface_mesh(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("scan", nodes_total(nodes.map(|n| n - 1.0)) * 4)?;
    // The kernel places triangles up to Mesh Capacity, not the buffer.
    x.covers("vertices", u64::from(mesh_capacity(x.params())) * size_of::<MeshVertex>() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::bundled_presets::{bundled_preset_def, bundled_preset_type_ids};
    use crate::node_graph::fluid::domain_layout;
    use crate::node_graph::matter::block_sort_box;
    use crate::node_graph::primitives::matter_domain::admit_lattice;
    use manifold_core::preset_def::PresetKind;

    /// Every bundled generator preset holding a liquid domain.
    fn liquid_presets() -> Vec<(String, &'static EffectGraphDef)> {
        let holds_liquid = |def: &EffectGraphDef| {
            let flat = manifold_core::flatten::flatten_groups(def).expect("flattens");
            flat.nodes.iter().any(|node| is_liquid_domain(&node.type_id))
        };
        bundled_preset_type_ids(PresetKind::Generator)
            .filter_map(|id| bundled_preset_def(&id).filter(|def| holds_liquid(def)).map(|def| (id.to_string(), def)))
            .collect()
    }

    /// Section 3.7 (Safety rails) rule 1, I9: every liquid preset at every
    /// resolution its domain admits either refuses by name before any GPU
    /// work, or every buffer covers every dispatch.
    #[test]
    fn liquid_presets_all_extent_checked() {
        let presets = liquid_presets();
        assert!(presets.len() >= 7, "liquid presets: {:?}", presets.iter().map(|(id, _)| id).collect::<Vec<_>>());
        let mut refusals: AHashMap<String, (u32, String)> = AHashMap::default();
        for (id, def) in &presets {
            let mut preset = LiquidPreset::build(def).unwrap_or_else(|error| panic!("{id}: {error}"));
            let resolutions = preset.resolutions();
            assert_eq!(resolutions, 8..=512, "{id}");
            let mut largest = None;
            for resolution in resolutions {
                match preset.check(resolution) {
                    Ok(report) => {
                        assert!(report.checked > 0, "{id} at {resolution}");
                        largest = Some((resolution, report));
                    }
                    Err(ExtentError::Refused { node, reason }) => {
                        refusals.entry(id.clone()).or_insert((resolution, format!("{node}: {reason}")));
                    }
                    Err(error) => panic!("{id} at resolution {resolution}: {error}"),
                }
            }
            let (resolution, report) = largest.unwrap_or_else(|| panic!("{id}: no resolution runs"));
            println!(
                "{id}: runs to resolution {resolution} ({} nodes checked, {:.2} GB at the top)",
                report.checked,
                report.scene_bytes as f64 / 1e9
            );
        }
        let mut refusals: Vec<_> = refusals.into_iter().collect();
        refusals.sort();
        for (id, (resolution, reason)) in &refusals {
            println!("{id}: first refused at resolution {resolution}: {reason}");
            assert!(reason.contains("Resolution") || reason.contains("Grid Budget"), "{id}: {reason}");
        }
    }

    /// A graph with a GPU atom no rule knows fails by name.
    #[test]
    fn an_atom_without_a_rule_fails_by_name() {
        let (_, def) = liquid_presets().into_iter().find(|(id, _)| id == "WaterDamBreakMatter").expect("preset");
        let mut preset = LiquidPreset::build(def).expect("builds");
        let rules: Vec<ExtentRule> =
            LIQUID_EXTENT_RULES.iter().filter(|rule| rule.type_id != "node.matter_to_grid").copied().collect();
        match check_graph(&mut preset.graph, &preset.plan, &rules) {
            Err(ExtentError::NoRule { type_id, .. }) => assert_eq!(type_id, "node.matter_to_grid"),
            other => panic!("expected a missing rule, got {other:?}"),
        }
    }

    /// A mesher told its lattice is larger than the level set it reads is
    /// caught before the GPU: Dam Break Matter with count_surface_triangles
    /// unwired from the volume's lattice and set to 4096 nodes per axis.
    #[test]
    fn a_lattice_past_its_storage_is_caught() {
        use manifold_core::effect_graph_def::SerializedParamValue;
        let (_, def) = liquid_presets().into_iter().find(|(id, _)| id == "WaterDamBreakMatter").expect("preset");
        let mut flat = manifold_core::flatten::flatten_groups(def).expect("flattens");
        let counter = flat.nodes.iter().find(|node| node.type_id == "node.count_surface_triangles").map(|node| node.id).expect("a counter");
        let lattice = ["nodes_x", "nodes_y", "nodes_z"];
        flat.wires.retain(|wire| !(wire.to_node == counter && lattice.contains(&wire.to_port.as_str())));
        let node = flat.nodes.iter_mut().find(|node| node.id == counter).expect("counter");
        for port in lattice {
            node.params.insert(port.into(), SerializedParamValue::Float { value: 4096.0 });
        }
        match check_preset_extents(&flat, 64) {
            Err(ExtentError::Uncovered { node, detail }) => {
                assert!(node.contains("count_surface_triangles") && detail.starts_with("levelset holds"), "{node}: {detail}");
            }
            other => panic!("expected the level set to be short, got {other:?}"),
        }
    }

    /// MPM's lattice arithmetic at every resolution the domain allows: Grid
    /// Budget admits or names the refusal, the block sort's bins are P2G's
    /// blocks, and the default budget stops at 193 (200³ nodes).
    #[test]
    fn matter_grid_budget_gates_every_resolution() {
        let mut largest_default = 0;
        for resolution in 8..=512 {
            let layout = domain_layout(None, 4.0, resolution).expect("layout");
            let lattice = LiquidLattice::from_layout(&layout);
            let nodes = lattice_nodes(lattice.nodes());
            for budget in [8.0f32, 512.0] {
                let fits = nodes as f64 <= f64::from(budget) * 1e6;
                match admit_lattice(&lattice, budget) {
                    Ok(()) => assert!(fits, "res {resolution}"),
                    Err(error) => assert!(!fits && error.contains("Grid Budget"), "res {resolution}: {error}"),
                }
            }
            if admit_lattice(&lattice, 8.0).is_ok() {
                largest_default = resolution;
            }
            let (_, size, bin) = block_sort_box(&lattice);
            assert_eq!(bin_counts(size, bin), lattice_blocks(&lattice), "res {resolution}");
        }
        assert_eq!(largest_default, 193);
    }
}
