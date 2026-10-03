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
use manifold_core::liquid_domain::{FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID, GPU_FLIP_DOMAIN_TYPE_ID, is_liquid_domain};
use manifold_core::{Beats, Seconds};

use crate::generators::mesh_common::{InstanceTransform, MeshVertex};
use crate::node_graph::physics::MAX_COPIES;
use crate::node_graph::fluid_particles::{
    CellRange, FluidBlob, FluidParticle, MAX_BINS, bin_counts, bin_total, searched_bins,
};
use crate::node_graph::fluid_role::MAX_FLUID_ROLES;
use crate::node_graph::freeze::classify::fusion_kind_str;
use crate::node_graph::liquid::EXACT_F32_COUNT;
use crate::node_graph::liquid::bodies::{LiquidBody, LiquidShape};
use crate::node_graph::liquid::clock::FIELD_RESERVE_INTERVALS;
use crate::node_graph::liquid::fields::{FieldFrame, FieldLattice, STAGING_SLOTS as FIELD_STAGING_SLOTS};
use crate::node_graph::liquid::frame_ring::RING;
use crate::node_graph::liquid::grid::{FACE_GRID_PORTS, FACE_INPUT_PORTS, face_len};
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::matter::{
    ACCUM_WORDS_PER_NODE, MatterGridNode, MatterPoint, REACTION_WORDS, STATS_WORDS, grid_accum_bytes, grid_bytes,
    lattice_blocks, lattice_nodes,
};
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::ports::PortType;
use crate::node_graph::primitives::dot_products::MAX_ROWS;
use crate::node_graph::liquid::coupling::REACTION_FLOATS;
use crate::node_graph::primitives::gpu_flip_bodies::held_bytes as body_pass_bytes;
use crate::node_graph::primitives::face_sample_component::axis_param;
use crate::node_graph::primitives::fluid_surface::{boundary_collisions, fluid_settings};
use crate::node_graph::primitives::liquid_fill::{fill_of, filled_sites, pool_slots};
use crate::node_graph::primitives::liquid_stats::{LIQUID_STATS_WORDS, partial_bytes};
use crate::node_graph::primitives::matter_domain::{fill_region, matter_geometry};
use crate::node_graph::primitives::matter_face_component::matter_cells;
use crate::node_graph::primitives::matter_fill::{fill_cells, fill_count};
use crate::node_graph::primitives::particle_volume::{refined_nodes, volume_scale};
use crate::node_graph::primitives::prefix_scan::storage_words;
use crate::node_graph::primitives::sort_particles_into_cells::range_storage_bytes;
use crate::node_graph::primitives::gpu_flip_domain::gpu_flip_geometry;
use crate::node_graph::primitives::gpu_flip_pressure::{lattice_refusal, scratch_bytes as pressure_scratch_bytes};
use crate::node_graph::fluid::TICK;
use crate::node_graph::primitives::gpu_flip_step::{
    ENGINE_CFL, FACE_VALID_LAYERS, band_layers, face_bytes, ring_max, scratch_bytes as step_scratch_bytes,
};
use crate::node_graph::primitives::volume_surface_mesh::start_capacity;
use crate::node_graph::resource_allocation::plan_array_allocations;
use crate::node_graph::primitives::matter_face_component::MATTER_FACE_VALID_LAYERS;
use crate::node_graph::primitives::whitewater_lifecycle::{DEFAULT_CAPACITY, MAX_CAPACITY};
use crate::node_graph::primitives::whitewater_step::{DEFAULT_CAPACITY as STEP_CAPACITY, MAX_CAPACITY as STEP_MAX_CAPACITY, StepShape};
use crate::node_graph::transform::Transform;
use crate::node_graph::whitewater::{
    KnownValue, SURFACE_CROSSING_BYTES, cell_total, face_offset, grid_box, grid_cells, refinement, require_extended_faces,
};
use crate::node_graph::whitewater_handoff::{OUTPUT_SLOTS, SNAPSHOT_SLOTS, SnapshotShape};
use manifold_fluids::{WhitewaterGrid, WhitewaterSpawn};
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

    /// An output port some later node reads.
    pub fn feeds(&self, port: &str) -> bool {
        self.output(port).is_some()
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
    pub fn lattice(&self) -> Result<LiquidLattice, Verdict> {
        LiquidLattice::from_scalars(|name, default| self.scalar(name, default)).map_err(Verdict::Refused)
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
    ExtentRule { type_id: "node.matter_face_component", check: matter_face_component },
    ExtentRule { type_id: "node.face_sample_component", check: face_sample_component },
    ExtentRule { type_id: GPU_FLIP_DOMAIN_TYPE_ID, check: gpu_flip_domain },
    ExtentRule { type_id: "node.liquid_fill", check: liquid_fill },
    ExtentRule { type_id: "node.liquid_state", check: liquid_state },
    ExtentRule { type_id: "node.liquid_stats", check: liquid_stats },
    ExtentRule { type_id: "node.liquid_frame", check: liquid_frame },
    ExtentRule { type_id: "node.gpu_flip_step", check: gpu_flip_step },
    ExtentRule { type_id: "node.dot_products", check: dot_products },
    ExtentRule { type_id: "node.divide_by_value", check: divide_by_value },
    ExtentRule { type_id: "node.sort_particles_into_cells", check: sort_particles_into_cells },
    ExtentRule { type_id: "node.shape_particle_blobs", check: shape_particle_blobs },
    ExtentRule { type_id: "node.blob_bounds", check: blob_bounds },
    ExtentRule { type_id: "node.particle_volume", check: particle_volume },
    ExtentRule { type_id: "node.offset_lattice", check: offset_lattice },
    ExtentRule { type_id: "node.redistance_lattice", check: redistance_lattice },
    ExtentRule { type_id: "node.lattice_bricks", check: lattice_bricks },
    ExtentRule { type_id: "node.smooth_lattice", check: smooth_lattice },
    ExtentRule { type_id: "node.clamp_liquid_to_solids", check: clamp_liquid_to_solids },
    ExtentRule { type_id: "node.count_surface_triangles", check: count_surface_triangles },
    ExtentRule { type_id: "node.count_surface_edges", check: count_surface_edges },
    ExtentRule { type_id: "node.running_total", check: running_total },
    ExtentRule { type_id: "node.volume_surface_mesh", check: volume_surface_mesh },
    ExtentRule { type_id: "node.relax_surface_mesh", check: relax_surface_mesh },
    ExtentRule { type_id: "node.smooth_surface_mesh", check: smooth_surface_mesh },
    ExtentRule { type_id: "node.surface_mesh_normals", check: surface_mesh_normals },
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
    ExtentRule { type_id: "node.surface_crossings", check: surface_crossings },
    ExtentRule { type_id: "node.nearest_crossing", check: nearest_crossing },
    ExtentRule { type_id: "node.crossing_distance", check: crossing_distance },
    ExtentRule { type_id: "node.liquid_cells", check: liquid_cells },
    ExtentRule { type_id: "node.lattice_curvature", check: lattice_curvature },
    ExtentRule { type_id: "node.turbulence_field", check: turbulence_field },
    ExtentRule { type_id: "node.inside_turbulence_potential", check: inside_turbulence_potential },
    ExtentRule { type_id: "node.turbulence_emission_count", check: turbulence_emission_count },
    ExtentRule { type_id: "node.whitewater_emitter_velocity", check: whitewater_emitter_velocity },
    ExtentRule { type_id: "node.whitewater_obstacle_source", check: whitewater_obstacle_source },
    ExtentRule { type_id: "node.whitewater_influence", check: whitewater_influence },
    ExtentRule { type_id: "node.dust_potential", check: dust_potential },
    ExtentRule { type_id: "node.extend_lattice", check: extend_lattice },
    ExtentRule { type_id: "node.jitter_particles", check: particle_map },
    ExtentRule { type_id: "node.sample_faces_at_particles", check: sample_faces_at_particles },
    ExtentRule { type_id: "node.energy_potential", check: particle_values },
    ExtentRule { type_id: "node.wavecrest_potential", check: wavecrest_potential },
    ExtentRule { type_id: "node.emission_count", check: emission_count },
    ExtentRule { type_id: "node.spawn_whitewater", check: spawn_whitewater },
    ExtentRule { type_id: "node.whitewater_type", check: whitewater_type },
    ExtentRule { type_id: "node.advect_whitewater", check: advect_whitewater },
    ExtentRule { type_id: "node.retype_whitewater", check: retype_whitewater },
    ExtentRule { type_id: "node.age_whitewater", check: age_whitewater },
    ExtentRule { type_id: "node.preserve_foam", check: preserve_foam },
    ExtentRule { type_id: "node.keep_whitewater", check: keep_whitewater },
    ExtentRule { type_id: "node.whitewater_lifecycle", check: whitewater_lifecycle },
    ExtentRule { type_id: "node.whitewater_step", check: whitewater_step },
    ExtentRule { type_id: "node.particles_to_copies", check: particles_to_copies },
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
        force_lattices: FIELD_RESERVE_INTERVALS,
        impulse_tick: Some(0),
    };
    let forces = u64::from(FIELD_RESERVE_INTERVALS) * field.lattice.bytes();
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
    // Tick-zero empty storage stays independent of worker-owned ring slots.
    // Include it in peak admission while the first real frame is prepared.
    x.hold(size_of::<FluidParticle>() as u64 + solid);
    // The CPU mesh grows its buffer by half again when a surface needs more,
    // through device admission; whitewater uploads stop at their capacity.
    let vertices = u64::from((x.param("max_capacity", 786_432.0).clamp(3.0, 3_145_728.0) as u32 / 3) * 3) * size_of::<MeshVertex>() as u64;
    x.provide("vertices", vertices);
    x.hold(vertices);
    Ok(())
}

fn matter_fill(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice()?;
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
    let lattice = x.lattice()?;
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
    let lattice = x.lattice()?;
    let nodes = node_extent(x, &lattice)?;
    x.covers("grid", grid_bytes(lattice.nodes()))?;
    x.covers("accum", nodes * 16)?;
    field_reads(x)
}

fn matter_body_reaction(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice()?;
    node_extent(x, &lattice)?;
    x.covers("grid", grid_bytes(lattice.nodes()))?;
    field_reads(x)
}

fn grid_to_matter(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice()?;
    node_extent(x, &lattice)?;
    // Active points clamp to the points array.
    x.covers("grid", grid_bytes(lattice.nodes()))
}

fn matter_stats(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice()?;
    let nodes = node_extent(x, &lattice)?;
    x.covers("stats", u64::from(STATS_WORDS) * 4)?;
    x.covers("grid", nodes * size_of::<MatterGridNode>() as u64)?;
    x.covers("accum", nodes * 16)
}

fn liquid_solid_distance(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // One thread per lattice node over storage sized from the same lattice.
    let solid = x.lattice()?.solid_bytes();
    x.provide("solid", solid);
    x.hold(solid);
    Ok(())
}

/// A frame's face grid storage: one array per wired axis over the domain's
/// cells, the one-record hint otherwise. Provided before any check can stop
/// the rule, since consumers size from it.
fn provide_frame_faces(x: &mut AtomExtent<'_>, lattice: &LiquidLattice, valid_layers: f32) {
    let published = FACE_INPUT_PORTS.iter().all(|port| x.wired(port));
    x.publish(FACE_GRID_PORTS[6], if published { valid_layers } else { 0.0 });
    for axis in 0..3 {
        let bytes = face_len(lattice.cells(), axis) * 4;
        if x.wired(FACE_INPUT_PORTS[axis]) {
            x.provide(FACE_GRID_PORTS[axis], bytes);
            x.hold(bytes);
        } else {
            x.provide(FACE_GRID_PORTS[axis], 4);
        }
    }
    for (&port, n) in FACE_GRID_PORTS[3..6].iter().zip(lattice.cells()) {
        x.publish(port, n as f32);
    }
}

/// Each wired face input holds its whole axis: the copy never publishes a
/// partial grid.
fn cover_frame_faces(x: &AtomExtent<'_>, lattice: &LiquidLattice) -> Result<(), Verdict> {
    for (axis, port) in FACE_INPUT_PORTS.into_iter().enumerate() {
        if x.wired(port) {
            x.covers(port, face_len(lattice.cells(), axis) * 4)?;
        }
    }
    Ok(())
}

fn matter_face_component(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| whole(x, name, 71.0));
    let (Some(cells), Some(axis)) = (matter_cells(nodes), axis_param(x.params())) else {
        return Err(Verdict::Refused(format!("a {nodes:?} node lattice has no cells, or the axis is not X, Y or Z")));
    };
    x.covers("grid", grid_bytes(nodes))?;
    x.covers("out", face_len(cells, axis) * 4)
}

fn face_sample_component(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let cells = x.lattice()?.cells();
    let Some(axis) = axis_param(x.params()) else {
        return Err(Verdict::Refused("the axis is not X, Y or Z".into()));
    };
    x.covers("faces", face_bytes(cells))?;
    x.covers("out", face_len(cells, axis) * 4)
}

fn matter_frame(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice()?;
    provide_frame_faces(x, &lattice, MATTER_FACE_VALID_LAYERS as f32);
    let count = x.count("count", 0.0)?;
    x.covers("points", u64::from(count) * size_of::<MatterPoint>() as u64)?;
    x.covers("stats", u64::from(STATS_WORDS) * 4)?;
    let particles = u64::from(count.max(1)) * size_of::<FluidParticle>() as u64;
    let solid = lattice.solid_bytes();
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
    cover_frame_faces(x, &lattice)
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

fn blob_bounds(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("bounds", 8)
}

fn optional_blob_bounds(x: &AtomExtent<'_>) -> Result<(), Verdict> {
    if x.wired("bounds") && x.bytes("bounds") != Some(8) {
        return Err(x.uncovered("bounds must contain exactly two f32 words from Blob Bounds".into()));
    }
    Ok(())
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
    optional_blob_bounds(x)?;
    searched(x)?;
    brick_schedule(x, refined)?;
    if x.wired("interior") {
        let padding = 1 + 2 * crate::node_graph::liquid::lattice::PADDING_NODES;
        if nodes.iter().any(|&n| n <= padding as f32) {
            return Err(x.uncovered("interior needs the padded liquid lattice".into()));
        }
        let bytes = nodes.iter().map(|&n| (n as u64) - u64::from(padding)).product::<u64>() * 4;
        if x.bytes("interior") != Some(bytes) {
            return Err(x.uncovered(format!("interior must hold exactly {bytes} bytes for the cell-centred lattice")));
        }
        x.covers("interior", bytes)?;
    }
    x.covers("solid", nodes_total(nodes) * 4)?;
    x.covers("levelset", lattice_total(refined) * 4)
}

fn lattice_bricks(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    use crate::node_graph::primitives::{lattice_bricks::brick_layout, prefix_scan::storage_words};
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    let layout = brick_layout(nodes.map(|n| n as u32), volume_scale(x.params()))
        .ok_or_else(|| x.uncovered("brick lattice cannot be indexed in u32".into()))?;
    searched(x)?;
    x.covers("solid", nodes_total(nodes) * 4)?;
    let bytes = u64::from(layout.words) * 4;
    x.provide("bricks", bytes);
    x.hold(bytes + storage_words(layout.count as usize) as u64 * 4);
    optional_blob_bounds(x)
}

fn brick_schedule(x: &AtomExtent<'_>, nodes: [u32; 3]) -> Result<(), Verdict> {
    if !x.wired("bricks") { return Ok(()); }
    let words = crate::node_graph::primitives::liquid_bricks::schedule_words(nodes)
        .ok_or_else(|| x.uncovered("brick schedule size overflow".into()))?;
    x.covers("bricks", words * 4)
}

fn offset_lattice(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.bytes("levelset").unwrap_or(0))
}

fn redistance_lattice(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    x.covers("levelset", nodes_total(nodes) * 4)?;
    offset_lattice(x)
}

fn smooth_lattice(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    brick_schedule(x, nodes.map(|n| n as u32))?;
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("smoothed", nodes_total(nodes) * 4)
}

fn clamp_liquid_to_solids(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    let solid = x.nodes(["solid_nodes_x", "solid_nodes_y", "solid_nodes_z"]);
    if nodes.iter().chain(&solid).any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}, solid {solid:?}")));
    }
    brick_schedule(x, nodes.map(|n| n as u32))?;
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("clamped", nodes_total(nodes) * 4)?;
    x.covers("solid", nodes_total(solid) * 4)
}

fn count_surface_triangles(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    brick_schedule(x, nodes.map(|n| n as u32))?;
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("counts", nodes_total(nodes.map(|n| n - 1.0)) * 4)
}

fn count_surface_edges(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("counts", nodes_total(nodes) * 4)
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
    if x.wired("solid") {
        let solid = x.nodes(["solid_nodes_x", "solid_nodes_y", "solid_nodes_z"]);
        if solid.iter().any(|&n| !n.is_finite() || n < 2.0) {
            return Err(x.uncovered(format!("invalid solid lattice: {solid:?}")));
        }
        x.covers("solid", nodes_total(solid) * 4)?;
    }
    brick_schedule(x, nodes.map(|n| n as u32))?;
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("scan", nodes_total(nodes.map(|n| n - 1.0)) * 4)?;
    // Provided and grown at run time; cell emission checks the live scan total
    // against the buffer's whole-triangle slot count before writing.
    let slots = start_capacity(x.params(), nodes);
    let start = slots * size_of::<MeshVertex>() as u64;
    let indices = if x.wired("edge_scan") {
        x.covers("edge_scan", nodes_total(nodes) * 4)?;
        slots * 4
    } else { 0 };
    x.provide("vertices", start);
    x.provide("indices", indices);
    x.hold(start + indices + 4); // Unindexed ABI stub.
    Ok(())
}

fn relax_surface_mesh(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    surface_mesh_pass(x, "relaxed")
}

fn smooth_surface_mesh(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    surface_mesh_pass(x, "relaxed")?;
    // The stage retains one ping-pong mesh when iterations exceeds one.
    // A scalar wire can change that count without a graph/extent rebuild.
    x.hold(x.bytes("vertices").unwrap_or(0));
    Ok(())
}

fn surface_mesh_normals(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    surface_mesh_pass(x, "out")
}

fn surface_mesh_pass(x: &mut AtomExtent<'_>, output: &str) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    brick_schedule(x, nodes.map(|n| n as u32))?;
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("scan", nodes_total(nodes.map(|n| n - 1.0)) * 4)?;
    if x.wired("edge_scan") { x.covers("edge_scan", nodes_total(nodes) * 4)?; }
    // Cell-owned intervals and neighbour reads lie below the checked live total.
    let vertices = x.bytes("vertices").ok_or_else(|| x.uncovered("vertices is unbound".into()))?;
    x.covers(output, vertices)
}

// ── GPU FLIP ─────────────────────────────────────────────────────────────────
//
// GPU FLIP's atoms read their lattice from params (P7b wires it). Several size
// their dispatch to the smallest of their arrays at run time; each rule here
// asks that no array is the smaller one, so no work is ever cut.

const PARTICLE: u64 = size_of::<FluidParticle>() as u64;

fn gpu_flip_domain(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let geometry = gpu_flip_geometry(
        |name, default| x.scalar(name, default),
        x.transform("domain"),
        x.transform("initial_volume"),
    )
    .map_err(Verdict::Refused)?;
    for (name, value) in geometry.outputs() {
        x.publish(name, value);
    }
    // Body kernels clamp rows and body counts to the bodies array, so the
    // walk takes a scene without colliders; the reaction is sized for every
    // body a liquid holds.
    x.publish("body_count", 0.0);
    x.publish("body_rows", 0.0);
    x.publish("dynamic_bodies", 0.0);
    x.publish("region_count", 0.0);
    x.publish("clock_obstacle_count", 0.0);
    x.publish("clock_source_count", 0.0);
    x.publish("live_hit_count", 0.0);
    x.publish("interval_duration", TICK as f32);
    // The walk takes a live frame's most force lattices and an impulse tick,
    // so the field reads are checked.
    let field = FieldFrame { lattice: geometry.field_lattice(), force_lattices: FIELD_RESERVE_INTERVALS, impulse_tick: Some(0) };
    let forces = u64::from(FIELD_RESERVE_INTERVALS) * field.lattice.bytes();
    for (name, value) in field.outputs() {
        x.publish(name, value);
    }
    for (port, bytes) in [
        ("bodies", size_of::<LiquidBody>() as u64),
        ("regions", size_of::<LiquidBody>() as u64),
        ("shapes", size_of::<LiquidShape>() as u64),
        ("atlas", 4),
        ("clock_obstacles", 96),
        ("clock_sources", 96),
        ("live_hits", 16),
        ("reaction", (MAX_FLUID_ROLES * REACTION_FLOATS * 4) as u64),
        ("forces", forces),
        ("impulses", field.lattice.bytes()),
    ] {
        x.provide(port, bytes);
        x.hold(bytes);
    }
    // The staging ring: the impulse lattice and the force lattices per slot.
    x.hold(FIELD_STAGING_SLOTS as u64 * (field.lattice.bytes() + forces + 16));
    Ok(())
}

/// Storage sized to exactly the particles the fill's wires place.
fn liquid_fill(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let cells = x.lattice()?.cells();
    let (pool, sites) = fill_of(|name, default| x.scalar(name, default));
    let placed = pool_slots(filled_sites(cells, pool, sites), x.scalar("particle_capacity", 0.0));
    if placed > u64::from(EXACT_F32_COUNT) {
        return Err(Verdict::Refused(format!(
            "Liquid Fill: the pool holds {placed} particles, more than the {EXACT_F32_COUNT} a particle count carries exactly"
        )));
    }
    x.publish("count", placed as f32);
    let bytes = placed.max(1) * PARTICLE;
    x.provide("particles", bytes);
    x.hold(bytes);
    Ok(())
}

fn liquid_state(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let mut whitewater_check = Ok(());
    let capacity = x.count("whitewater_capacity", STEP_CAPACITY as f32)?;
    if !(1..=STEP_MAX_CAPACITY).contains(&capacity) {
        return Err(Verdict::Refused(format!("whitewater capacity {capacity} is outside 1 to {STEP_MAX_CAPACITY}")));
    }
    let pool = u64::from(capacity) * size_of::<crate::node_graph::whitewater::WhitewaterParticle>() as u64;
    for (capture, output, bytes) in [
        ("whitewater_pool_in", "whitewater_pool", pool),
        ("whitewater_state_in", "whitewater_state", 32),
        ("whitewater_counts_in", "whitewater_counts", 36),
        ("foam_particles_in", "foam_particles", u64::from(capacity) * PARTICLE),
        ("bubble_particles_in", "bubble_particles", u64::from(capacity) * PARTICLE),
        ("spray_particles_in", "spray_particles", u64::from(capacity) * PARTICLE),
        ("dust_particles_in", "dust_particles", u64::from(capacity) * PARTICLE),
    ] {
        let active = x.input(capture).is_some();
        x.provide(output, if active { bytes } else { 0 });
        if active {
            x.hold(bytes);
            // Captures become bound on the second walk. Publish ALL sizes
            // before returning an uncovered capture from the first walk.
            whitewater_check = whitewater_check.and(x.covers(capture, bytes));
        }
    }
    if x.input("whitewater_pool_in").is_some() { x.hold(pool); }

    let mut interior_check = Ok(());
    if x.wired("interior_in") {
        let bytes = crate::node_graph::liquid::grid::interior_bytes(x.lattice()?.cells());
        x.provide("interior", bytes);
        x.hold(bytes);
        if x.bytes("interior_in") != Some(bytes) {
            interior_check = Err(x.uncovered(format!("interior_in must hold exactly {bytes} bytes for the cell-centred lattice")));
        }
    } else {
        x.provide("interior", 0);
    }
    // The faces are the lattice's face grid, sized before the region runs and
    // held only while something reads them. The tick's faces (written later
    // in the plan: the second pass sees them) must be exactly that grid.
    let mut faces_check = Ok(());
    if x.input("faces_in").is_some() {
        if ["nodes_x", "nodes_y", "nodes_z"].iter().any(|port| x.input(port).is_none()) {
            return Err(Verdict::Refused("Liquid State: faces_in needs the lattice on nodes_x, nodes_y and nodes_z".into()));
        }
        let faces = face_bytes(x.lattice()?.cells());
        let fed = x.feeds("faces");
        x.provide("faces", if fed { faces } else { 0 });
        if fed {
            x.hold(faces);
            faces_check = match x.bytes("faces_in") {
                Some(have) if have == faces => Ok(()),
                Some(have) => Err(x.uncovered(format!("faces_in holds {have} bytes; the lattice's face grid is {faces}"))),
                None => Err(x.uncovered("faces_in is unbound".into())),
            };
        }
    } else {
        x.provide("faces", 0);
    }
    let stats = u64::from(LIQUID_STATS_WORDS) * 4;
    // The zeroed stats a new epoch copies, and the readback ring.
    x.hold(4 * stats + 3 * 32);
    x.covers_if_bound("clock_status_in", 32)?;
    x.covers_if_bound("clock_status", 32)?;
    x.publish("tick_index", 0.0);
    let records = u64::from(x.count("count", 0.0)?) * PARTICLE;
    // A new epoch copies the fill into the state; each tick's capture copies
    // the tick's particles and stats back (written later in the plan: the
    // second pass sees them).
    for port in ["seed", "out", "in"] {
        x.covers(port, records)?;
    }
    x.covers("stats", stats)?;
    x.covers("stats_in", stats)?;
    faces_check.and(whitewater_check).and(interior_check)
}

fn liquid_stats(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let count = x.count("count", 0.0)?;
    x.covers("particles", u64::from(count) * PARTICLE)?;
    x.covers("stats", u64::from(LIQUID_STATS_WORDS) * 4)?;
    x.hold(partial_bytes(count));
    Ok(())
}

fn liquid_frame(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice()?;
    let mut interior_check = Ok(());
    if x.wired("interior") {
        let bytes = crate::node_graph::liquid::grid::interior_bytes(lattice.cells());
        if x.bytes("interior") != Some(bytes) {
            interior_check = Err(x.uncovered(format!("interior must hold exactly {bytes} bytes for the cell-centred lattice")));
        }
        x.provide("interior_a", bytes);
        x.provide("interior_b", bytes);
        x.hold(RING as u64 * bytes);
    } else {
        x.provide("interior_a", 0);
        x.provide("interior_b", 0);
    }
    let valid_layers = x.param("face_valid_layers", 0.0).round().clamp(0.0, 8.0);
    provide_frame_faces(x, &lattice, valid_layers);
    let count = x.count("count", 0.0)?;
    let particles = u64::from(count.max(1)) * PARTICLE;
    let solid = lattice.solid_bytes();
    let wired = x.wired("solid");
    x.hold(if wired { RING as u64 * solid } else { solid });
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
    x.covers("particles", u64::from(count) * PARTICLE)?;
    x.covers("stats", u64::from(LIQUID_STATS_WORDS) * 4)?;
    if wired {
        x.covers("solid", solid)?;
    }
    cover_frame_faces(x, &lattice).and(interior_check)
}

/// One GPU FLIP step: the face grid it provides, the sort, the solver and its
/// own scratch at the wired lattice, the field and body reads, and every
/// particle carried through.
fn gpu_flip_step(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let cells = x.lattice()?.cells();
    if let Some(reason) = lattice_refusal(cells) {
        return Err(Verdict::Refused(format!("GPU FLIP Step: {reason}. Lower Resolution.")));
    }
    let faces = face_bytes(cells);
    x.provide("faces", faces);
    x.provide("distance", cell_total(cells) * 4);
    let lattice = x.lattice()?;
    x.publish_transform("grid_bounds", lattice.bounds());
    for (port, value) in ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"].into_iter().zip(lattice.nodes())
        .chain(["face_cells_x", "face_cells_y", "face_cells_z"].into_iter().zip(cells))
        .chain([("face_valid_layers", FACE_VALID_LAYERS)]) {
        x.publish(port, value as f32);
    }
    let slots = x.items("particles").unwrap_or(0);
    x.hold(crate::node_graph::primitives::gpu_flip_clock::GpuFlipClock::held_bytes(
        slots as u32,
        whole(x, "clock_obstacle_count", 0.0),
        whole(x, "clock_source_count", 0.0),
    ));
    let cell_bytes = lattice_total(cells) * 4;
    if x.feeds("interior") { x.provide("interior", cell_bytes); x.hold(cell_bytes); }
    if x.scalar("narrow_band", 0.0) != 0.0 {
        // Four distance arrays, support mask, two face grids, lifecycle
        // particles/status and PrefixScan storage. Ferstl et al. (2016).
        x.hold(6 * cell_bytes + 3 * faces + slots * PARTICLE + 16);
        x.hold(storage_words((lattice_total(cells) * 8) as usize) as u64 * 4);
    }
    if x.wired("regions") {
        x.hold(storage_words(crate::node_graph::primitives::gpu_flip_step::emit_sites(cells) as usize) as u64 * 4);
    }
    let ranges = range_storage_bytes(cells);
    search_fits(x, cells, ranges)?;
    // The sort's ranges, cell counts, rank and slot scratch.
    x.hold(ranges + bin_total(cells) * 4 + 2 * slots.max(1) * 4);
    // Same configured CFL as the step, never particle travel.
    let ring = ring_max(band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS));
    x.hold(faces + pressure_scratch_bytes(cells) + step_scratch_bytes(cells, slots, ring));
    field_reads(x)?;
    x.covers_if_bound("clock_status", 32)?;
    for (buffer, count) in [("clock_obstacles", "clock_obstacle_count"), ("clock_sources", "clock_source_count")] {
        x.covers_if_bound(buffer, u64::from(whole(x, count, 0.0)) * 96)?;
    }
    x.covers_if_bound("live_hits", u64::from(whole(x, "live_hit_count", 0.0)) * 16)?;
    let rows = body_rows(x)?;
    x.covers_if_bound("bodies", rows * size_of::<LiquidBody>() as u64)?;
    // A wired reaction holds every body of one tick, as the step clamps
    // body_count; the body passes' sums come with it.
    if x.bytes("reaction").is_some() {
        let bodies = x.scalar("body_count", 0.0).round().clamp(0.0, MAX_FLUID_ROLES as f32) as u64;
        x.covers("reaction", bodies * REACTION_FLOATS as u64 * 4)?;
        x.hold(body_pass_bytes(cells, bodies as u32));
    }
    // It moves min(particles, out) records: every one.
    x.covers("out", slots * PARTICLE)
}

/// A whole-number param, as the vector atoms round it.
fn whole_param(x: &AtomExtent<'_>, name: &str, default: f32) -> u32 {
    x.param(name, default).round().max(0.0) as u32
}

fn dot_products(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let length = u64::from(whole_param(x, "row_length", 1024.0).max(1));
    let max_rows = whole_param(x, "max_rows", 1.0);
    if !(1..=MAX_ROWS).contains(&max_rows) {
        return Err(x.uncovered(format!("max_rows {max_rows} is outside the partials' 1 to {MAX_ROWS} rows")));
    }
    let max_rows = u64::from(max_rows);
    x.covers("matrix", max_rows * length * 4)?;
    if x.wired("vector") {
        x.covers("vector", length * 4)?;
    }
    x.covers("out", max_rows * 4)
}

fn divide_by_value(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("divisor", 4)?;
    x.covers("out", x.bytes("values").unwrap_or(0))
}

/// The body rows the step may read, a whole count.
fn body_rows(x: &AtomExtent<'_>) -> Result<u64, Verdict> {
    let rows = x.scalar("rows", 0.0);
    if !(rows >= 0.0 && rows.fract() == 0.0) {
        return Err(Verdict::Refused(format!("{rows} body rows is not a whole count")));
    }
    Ok(rows as u64)
}

// ── Whitewater ──────────────────────────────────────────────────────────────
//
// The grid atoms dispatch over the cells of the solid lattice on their
// nodes wires; the particle atoms over the smallest of their arrays, so each
// rule asks that no output is the smaller one.

/// The whitewater grid's solid nodes and cells, as `grid_nodes` reads them.
fn whitewater_lattice(x: &AtomExtent<'_>, names: [&str; 3]) -> Result<([u32; 3], [u32; 3]), Verdict> {
    let nodes = names.map(|name| whole(x, name, 71.0));
    let cells = grid_cells(nodes).ok_or_else(|| Verdict::Refused(format!("a {nodes:?} solid lattice has too few or too many nodes")))?;
    Ok((nodes, cells))
}

fn whitewater_grid(x: &AtomExtent<'_>) -> Result<(u64, u64), Verdict> {
    let (nodes, cells) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    Ok((cell_total(nodes), cell_total(cells)))
}

/// The face grid, placed centred in the whitewater grid, and each face
/// array covering its axis.
fn whitewater_faces(x: &AtomExtent<'_>, nodes: [u32; 3], names: [&str; 3]) -> Result<[u32; 3], Verdict> {
    let face_cells = names.map(|name| whole(x, name, 64.0));
    face_offset(nodes, face_cells).map_err(Verdict::Refused)?;
    for (axis, port) in ["face_u", "face_v", "face_w"].into_iter().enumerate() {
        x.covers(port, face_len(face_cells, axis) * 4)?;
    }
    Ok(face_cells)
}

fn surface_crossings(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    let levels = ["level_nodes_x", "level_nodes_y", "level_nodes_z"].map(|name| whole(x, name, 211.0));
    refinement(nodes, levels).map_err(Verdict::Refused)?;
    x.covers("out", cell_total(cells) * SURFACE_CROSSING_BYTES)?;
    x.covers("solid", cell_total(nodes) * 4)?;
    x.covers("level_set", cell_total(levels) * 4)
}

fn nearest_crossing(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    x.covers("crossings", cells * SURFACE_CROSSING_BYTES)?;
    x.covers("out", cells * SURFACE_CROSSING_BYTES)
}

fn crossing_distance(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_grid(x)?;
    x.covers("crossings", cells * SURFACE_CROSSING_BYTES)?;
    x.covers("solid", nodes * 4)?;
    x.covers("out", cells * 4)
}

fn liquid_cells(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_grid(x)?;
    x.covers("distance", cells * 4)?;
    x.covers("solid", nodes * 4)?;
    x.covers("out", cells * 4)
}

fn turbulence_field(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    whitewater_faces(x, nodes, ["face_cells_x", "face_cells_y", "face_cells_z"])?;
    x.covers("distance", cell_total(cells) * 4)?;
    x.covers("out", cell_total(cells) * 4)
}
fn inside_turbulence_potential(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    for p in ["distance", "turbulence", "cells"] { x.covers(p, cells * 4)?; }
    particle_values(x)
}
fn whitewater_emitter_velocity(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    for p in ["distance", "cells"] { x.covers(p, cells * 4)?; }
    particle_map(x)
}
fn turbulence_emission_count(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    emission_count(x)?;
    x.covers("turbulence", x.items("particles").unwrap_or(0) * 4)?;
    let (nodes, _) = whitewater_grid(x)?;
    x.covers("influence", nodes * 4)
}
fn whitewater_obstacle_source(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let bytes = x.lattice()?.solid_bytes() * 4;
    x.provide("solid", bytes); x.hold(bytes); Ok(())
}
fn whitewater_influence(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let bytes = x.bytes("values").unwrap_or(0);
    x.covers("solid", bytes)?;
    x.covers("source", bytes * 4)?;
    x.covers("out", bytes)
}
fn dust_potential(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_grid(x)?;
    x.covers("solid", nodes * 4)?;
    x.covers("source", nodes * 16)?;
    x.covers("turbulence", cells * 4)?;
    particle_values(x)
}

fn lattice_curvature(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    x.covers("distance", cells * 4)?;
    x.covers("out", cells * KNOWN_VALUE)
}

fn extend_lattice(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    x.covers("values", cells * KNOWN_VALUE)?;
    x.covers("out", cells * KNOWN_VALUE)
}

const KNOWN_VALUE: u64 = size_of::<KnownValue>() as u64;

/// One particle record out per particle in.
fn particle_map(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.bytes("particles").unwrap_or(0))
}

/// One f32 out per particle in.
fn particle_values(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.items("particles").unwrap_or(0) * 4)
}

fn sample_faces_at_particles(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, _) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    whitewater_faces(x, nodes, ["face_cells_x", "face_cells_y", "face_cells_z"])?;
    particle_map(x)
}

fn wavecrest_potential(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    x.covers("distance", cells * 4)?;
    x.covers("curvature", cells * KNOWN_VALUE)?;
    x.covers("cells", cells * 4)?;
    particle_values(x)
}

fn emission_count(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let values = x.items("particles").unwrap_or(0) * 4;
    x.covers("energy", values)?;
    x.covers("wavecrest", values)?;
    x.covers("out", values)
}

/// Every emitter's particle, energy and running total, the grid's solid and
/// faces, and a record per spawn slot.
fn spawn_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, _) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    whitewater_faces(x, nodes, ["face_cells_x", "face_cells_y", "face_cells_z"])?;
    x.covers("solid", cell_total(nodes) * 4)?;
    if x.wired("emitters") {
        let emitters = u64::from(x.count("emitters", 0.0)?);
        x.covers("particles", emitters * PARTICLE)?;
        x.covers("energy", emitters * 4)?;
        x.covers("offsets", emitters * 4)?;
    }
    let slots = u64::from(whole(x, "capacity", DEFAULT_CAPACITY as f32));
    x.covers("out", slots * size_of::<WhitewaterSpawn>() as u64)
}

fn whitewater_type(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    x.covers("distance", cells * 4)?;
    x.covers("cells", cells * 4)?;
    x.covers("out", x.bytes("spawns").unwrap_or(0))
}

/// The advect reads the face grid and the solid lattice whole, and writes a
/// record per pool slot.
fn advect_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, _) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    whitewater_faces(x, nodes, ["face_cells_x", "face_cells_y", "face_cells_z"])?;
    x.covers("solid", cell_total(nodes) * 4)?;
    x.covers("out", x.bytes("pool").unwrap_or(0))
}

/// The retype reads the distance, cells and face grid whole, and writes a
/// record per pool slot.
fn retype_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    whitewater_faces(x, nodes, ["face_cells_x", "face_cells_y", "face_cells_z"])?;
    x.covers("distance", cell_total(cells) * 4)?;
    x.covers("cells", cell_total(cells) * 4)?;
    x.covers("out", x.bytes("pool").unwrap_or(0))
}

/// The age writes a record per pool slot.
fn age_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.bytes("pool").unwrap_or(0))
}

/// The preservation searches its sort's bins and writes a record per pool
/// slot; the bins index the order and binned pool, bounded by their lengths.
fn preserve_foam(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    searched(x)?;
    x.covers("out", x.bytes("pool").unwrap_or(0))
}

/// The keep searches its sort's bins and reads the solid lattice whole; one
/// flag per pool slot. Unset bins are the sort's own rule on the grid's box
/// and cell, as the atom's run works them out.
fn keep_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    x.covers("solid", cell_total(nodes) * 4)?;
    let ports = ["bins_x", "bins_y", "bins_z"];
    if ports.map(|port| x.scalar(port, 0.0)) == [0.0; 3] && ports.iter().all(|port| !x.wired(port)) {
        let size = ["size_x", "size_y", "size_z"].map(|name| x.scalar(name, 4.375));
        let ranges = x.bytes("cell_ranges").ok_or_else(|| x.uncovered("cell_ranges is unbound".into()))?;
        let bins = bin_counts(size, size[0] / cells[0] as f32).map(|n| n as f32);
        searched_bins(bins, ranges, "search").map_err(|error| x.uncovered(error))?;
    } else {
        searched(x)?;
    }
    x.covers("out", x.items("pool").unwrap_or(0) * 4)
}

/// The lifecycle's snapshot copies read the whole face grid, level and
/// solid; it provides each population at Capacity and holds its rings.
fn whitewater_lifecycle(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let capacity = whole(x, "capacity", DEFAULT_CAPACITY as f32).clamp(1, MAX_CAPACITY);
    let population = u64::from(capacity) * PARTICLE;
    for port in ["foam_particles", "bubble_particles", "spray_particles", "dust_particles"] {
        x.provide(port, population);
    }
    x.hold(OUTPUT_SLOTS as u64 * 3 * population);
    let (nodes, cells) = whitewater_lattice(x, ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"])?;
    let face_cells = ["face_cells_x", "face_cells_y", "face_cells_z"].map(|name| whole(x, name, 0.0));
    let face_offset = face_offset(nodes, face_cells).map_err(Verdict::Refused)?;
    require_extended_faces(x.scalar("face_valid_layers", 0.0)).map_err(Verdict::Refused)?;
    let bounds = x.transform("grid_bounds").ok_or_else(|| Verdict::Refused("the grid_bounds input is not wired".into()))?;
    let (origin, cell_size) = grid_box(bounds, nodes).map_err(Verdict::Refused)?;
    let shape = SnapshotShape { grid: WhitewaterGrid { cells, cell_size, origin }, face_cells, face_offset, capacity };
    x.hold(SNAPSHOT_SLOTS as u64 * shape.slot_bytes());
    for (axis, port) in ["face_u", "face_v", "face_w"].into_iter().enumerate() {
        x.covers(port, shape.face_bytes(axis))?;
    }
    x.covers("level", shape.level_bytes())?;
    x.covers("solid", shape.solid_bytes())
}

/// The step's own shape, as its run builds it: every placement rule a
/// refusal by name, each input covering what the grid reads, each
/// population provided at Capacity, and everything else held.
fn whitewater_step(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let capacity = x.scalar("capacity", STEP_CAPACITY as f32).round();
    if !(1.0..=STEP_MAX_CAPACITY as f32).contains(&capacity) {
        return Err(Verdict::Refused(format!("capacity {capacity} is outside 1 to {STEP_MAX_CAPACITY}")));
    }
    let triple = |x: &AtomExtent<'_>, names: [&str; 3]| names.map(|name| whole(x, name, 0.0));
    let shape = StepShape::new(
        triple(x, ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"]),
        if x.input("distance").is_some() {
            triple(x, ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"])
        } else {
            triple(x, ["level_set_nodes_x", "level_set_nodes_y", "level_set_nodes_z"])
        },
        triple(x, ["face_cells_x", "face_cells_y", "face_cells_z"]),
        x.scalar("face_valid_layers", 0.0),
        x.transform("grid_bounds"),
        capacity as u32,
    )
    .map_err(Verdict::Refused)?;
    for port in ["foam_particles", "bubble_particles", "spray_particles", "dust_particles"] {
        x.provide(port, shape.population_bytes());
    }
    x.hold(shape.held_bytes(x.items("particles").unwrap_or(0)));
    for (axis, port) in ["face_u", "face_v", "face_w"].into_iter().enumerate() {
        x.covers(port, shape.face_bytes(axis))?;
    }
    x.provide("pool_out", shape.pool_bytes());
    x.provide("state_out", 32);
    x.provide("counts_out", 36);
    if x.input("obstacle_source").is_some() { x.covers("obstacle_source", shape.solid_bytes() * 4)?; }
    if x.input("distance").is_some() {
        x.covers("distance", cell_total(shape.face_cells) * 4)?;
        x.covers("pool", shape.pool_bytes())?;
        x.covers("pool_state", 32)?;
    } else {
        x.covers("level_set", shape.level_bytes())?;
    }
    x.covers("solid", shape.solid_bytes())
}

fn particles_to_copies(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("copies", x.items("particles").unwrap_or(0) * size_of::<InstanceTransform>() as u64)
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
    fn narrow_band_preset_small_extent_checked() {
        let (id, def) = liquid_presets().into_iter().find(|(id, _)| id.contains("WaterDamBreakGpuFlip")).expect("GPU FLIP preset");
        let mut preset = LiquidPreset::build(def).unwrap_or_else(|error| panic!("{id}: {error}"));
        for resolution in [8, 16] {
            let report = preset.check(resolution).unwrap_or_else(|error| panic!("{id} at {resolution}: {error}"));
            assert!(report.checked > 0);
        }
    }

    #[test]
    fn liquid_blob_bounds_reject_wrong_extent_before_gpu_work() {
        let (_, def) = liquid_presets().into_iter().find(|(id, _)| id == "WaterDamBreakGpuFlip").expect("preset");
        for consumer in ["node.particle_volume", "node.lattice_bricks"] {
            let mut flat = manifold_core::flatten::flatten_groups(def).expect("flattens");
            let id = flat.nodes.iter().find(|n| n.type_id == consumer).expect("consumer").id;
            // Both ports are Array<f32>, so the graph type check accepts this
            // deliberately wrong wire. The extent contract must reject it.
            let solid = flat.wires.iter().find(|w| w.to_node == id && w.to_port == "solid").expect("solid wire").clone();
            let bound = flat.wires.iter_mut().find(|w| w.to_node == id && w.to_port == "bounds").expect("bounds wire");
            bound.from_node = solid.from_node;
            bound.from_port = solid.from_port;
            match check_preset_extents(&flat, 8) {
                Err(ExtentError::Uncovered { detail, .. }) => assert!(detail.contains("bounds must contain exactly two"), "{consumer}: {detail}"),
                other => panic!("{consumer}: expected a malformed bounds refusal, got {other:?}"),
            }
        }
    }

    #[test]
    fn liquid_mesh_contact_rejects_short_solid_before_gpu_work() {
        let (_, def) = liquid_presets().into_iter().find(|(id, _)| id == "WaterDamBreakGpuFlip").expect("preset");
        let mut flat = manifold_core::flatten::flatten_groups(def).expect("flattens");
        let mesh = flat.nodes.iter().find(|n| n.type_id == "node.volume_surface_mesh").expect("mesh").id;
        let bounds = flat.nodes.iter().find(|n| n.type_id == "node.blob_bounds").expect("two-float source").id;
        let solid = flat.wires.iter_mut().find(|w| w.to_node == mesh && w.to_port == "solid").expect("solid wire");
        solid.from_node = bounds;
        solid.from_port = "bounds".into();
        match check_preset_extents(&flat, 8) {
            Err(ExtentError::Uncovered { node, detail }) => {
                // The early solid refusal leaves owned outputs unsized, so
                // the walk may report that before its final coverage pass.
                assert!(node.contains("volume_surface_mesh"), "{node}: {detail}");
            }
            other => panic!("expected a short solid lattice refusal, got {other:?}"),
        }
    }

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
        // Exercise the dense level-set bound specifically. With a sparse
        // schedule wired, its smaller brick bound correctly refuses first.
        flat.wires.retain(|wire| !(wire.to_node == counter
            && (lattice.contains(&wire.to_port.as_str()) || wire.to_port == "bricks")));
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
