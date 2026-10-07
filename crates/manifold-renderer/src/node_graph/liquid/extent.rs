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
use manifold_core::liquid_domain::is_liquid_domain;
use manifold_core::Beats;
use manifold_core::Seconds;
use crate::node_graph::fluid_particles::CellRange;
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::fluid_particles::bin_counts;
use crate::node_graph::fluid_particles::searched_bins;
use crate::node_graph::freeze::classify::fusion_kind_str;
use crate::node_graph::liquid::EXACT_F32_COUNT;
use crate::node_graph::liquid::grid::FACE_GRID_PORTS;
use crate::node_graph::liquid::grid::FACE_INPUT_PORTS;
use crate::node_graph::liquid::grid::face_len;
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::matter::ACCUM_WORDS_PER_NODE;
use crate::node_graph::matter::lattice_nodes;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::ports::PortType;
use crate::node_graph::resource_allocation::plan_array_allocations;
use crate::node_graph::transform::Transform;
use crate::node_graph::whitewater::KnownValue;
use crate::node_graph::whitewater::cell_total;
use crate::node_graph::whitewater::face_offset;
use crate::node_graph::whitewater::grid_cells;
use crate::node_graph::Backend;
use crate::node_graph::EffectGraphDefExt;
use crate::node_graph::EffectNodeContext;
use crate::node_graph::ExecutionPlan;
use crate::node_graph::ExecutionStep;
use crate::node_graph::FrameTime;
use crate::node_graph::Graph;
use crate::node_graph::MockBackend;
use crate::node_graph::NodeInputs;
use crate::node_graph::NodeInstance;
use crate::node_graph::NodeOutputs;
use crate::node_graph::ParamValues;
use crate::node_graph::PrimitiveRegistry;
use crate::node_graph::ResourceId;
use crate::node_graph::Slot;
use crate::node_graph::compile;

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

inventory::collect!(ExtentRule);

#[derive(Debug)]
pub enum ExtentError {
    Build(String),
    DuplicateRule { type_id: String },
    Refused { node: String, reason: String },
    Uncovered { node: String, detail: String },
    NoRule { node: String, type_id: String },
    Unresolved { node: String, port: String },
}

impl std::fmt::Display for ExtentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateRule { type_id } => write!(f, "duplicate extent rule for {type_id}"),
            Self::Build(error) => write!(f, "build: {error}"),
            Self::Refused { node, reason } => write!(f, "{node} refuses: {reason}"),
            Self::Uncovered { node, detail } => write!(f, "{node}: {detail}"),
            Self::NoRule { node, type_id } => write!(f, "{node}: no extent rule for {type_id}"),
            Self::Unresolved { node, port } => write!(f, "{node}.{port}: the wire's value is not resolved on the CPU"),
        }
    }
}

/// A checked graph: the nodes ruled on, and array storage plus rule-declared
/// private device storage once every array has grown to what the walk reached.
/// Texture memory and driver-owned allocations are not included.
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

    pub(crate) fn input(&self, port: &str) -> Option<ResourceId> {
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
    for (index, rule) in rules.iter().enumerate() {
        if rules[..index].iter().any(|prior| prior.type_id == rule.type_id) {
            return Err(ExtentError::DuplicateRule { type_id: rule.type_id.to_string() });
        }
    }
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
        Self::build_with_registry(def, &registry)
    }

    /// Build with an explicitly selected primitive registry. Product callers
    /// use [`Self::build`], while reference proofs opt into the retired CPU
    /// FLIP node through `PrimitiveRegistry::with_cpu_flip_reference`.
    pub(crate) fn build_with_registry(def: &EffectGraphDef, registry: &PrimitiveRegistry) -> Result<Self, ExtentError> {
        let build = |error: String| ExtentError::Build(error);
        let expanded = crate::node_graph::scene_modifier_expand::expand_scene_modifiers(def, registry)
            .map_err(|error| build(error.to_string()))?;
        let flat = manifold_core::flatten::flatten_groups(&expanded).map_err(|error| build(error.to_string()))?;
        let graph = flat.into_graph(registry, &Default::default()).map_err(|error| build(format!("{error:?}")))?;
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
        check_graph(&mut self.graph, &self.plan, &LIQUID_EXTENT_RULES)
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

/// Registered rules, ordered by node type independently of linker order.
pub static LIQUID_EXTENT_RULES: std::sync::LazyLock<Vec<ExtentRule>> = std::sync::LazyLock::new(|| {
    let mut rules: Vec<_> = inventory::iter::<ExtentRule>.into_iter().copied().collect();
    rules.sort_by_key(|rule| rule.type_id);
    rules
});

pub(crate) fn nodes_total(nodes: [f32; 3]) -> u64 {
    nodes.iter().map(|&n| n.max(0.0) as u64).product()
}

pub(crate) fn lattice_total(nodes: [u32; 3]) -> u64 {
    lattice_nodes(nodes)
}

pub(crate) fn whole(x: &AtomExtent<'_>, name: &str, default: f32) -> u32 {
    x.scalar(name, default).round().max(0.0) as u32
}

/// Texture-only GPU work: no array extent to prove. A node of these types
/// that grows an array port needs a real rule.
/// Asset sources size their source textures from decoded images and bound
/// writes by the destination texture; this does not account texture memory.
pub(crate) fn texture_only(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
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
pub(crate) fn size_bounded(_: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    Ok(())
}

/// Dispatches over its own input, in place.
pub(crate) fn in_place(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.bytes("in").unwrap_or(0))
}

/// Field reads clamp to the field lattices the scalars name, so the wired
/// buffers must hold them.
pub(crate) fn field_reads(x: &AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = ["field_nodes_x", "field_nodes_y", "field_nodes_z"].map(|name| whole(x, name, 2.0).max(2));
    let bytes = nodes.iter().map(|&n| u64::from(n)).product::<u64>() * 16;
    x.covers_if_bound("forces", u64::from(whole(x, "force_lattices", 0.0)) * bytes)?;
    x.covers_if_bound("impulses", bytes)
}

/// Lattice-wide node kernels read the accumulator and grid through the node
/// count; P2G's word index is an i32 node index times four.
pub(crate) fn node_extent(x: &AtomExtent<'_>, lattice: &LiquidLattice) -> Result<u64, Verdict> {
    let nodes = lattice_total(lattice.nodes());
    if nodes > i32::MAX as u64 || nodes * u64::from(ACCUM_WORDS_PER_NODE) > u64::from(u32::MAX) {
        return Err(x.uncovered(format!("{nodes} nodes overflow the accumulator's 32-bit word index")));
    }
    Ok(nodes)
}

/// A frame's face grid storage: one array per wired axis over the domain's
/// cells, the one-record hint otherwise. Provided before any check can stop
/// the rule, since consumers size from it.
pub(crate) fn provide_frame_faces(x: &mut AtomExtent<'_>, cells: [u32; 3], valid_layers: f32) {
    let published = FACE_INPUT_PORTS.iter().all(|port| x.wired(port));
    x.publish(FACE_GRID_PORTS[6], if published { valid_layers } else { 0.0 });
    for axis in 0..3 {
        let bytes = face_len(cells, axis) * 4;
        if x.wired(FACE_INPUT_PORTS[axis]) {
            x.provide(FACE_GRID_PORTS[axis], bytes);
            x.hold(bytes);
        } else {
            x.provide(FACE_GRID_PORTS[axis], 4);
        }
    }
    for (&port, n) in FACE_GRID_PORTS[3..6].iter().zip(cells) {
        x.publish(port, n as f32);
    }
}

/// Each wired face input holds its whole axis: the copy never publishes a
/// partial grid.
pub(crate) fn cover_frame_faces(x: &AtomExtent<'_>, cells: [u32; 3]) -> Result<(), Verdict> {
    for (axis, port) in FACE_INPUT_PORTS.into_iter().enumerate() {
        if x.wired(port) {
            x.covers(port, face_len(cells, axis) * 4)?;
        }
    }
    Ok(())
}

/// The sort's bin grid is searched with exactly the ranges it allocates, and
/// its last bin index stays inside them in i32.
pub(crate) fn search_fits(x: &AtomExtent<'_>, bins: [u32; 3], range_bytes: u64) -> Result<(), Verdict> {
    let searched = searched_bins(bins.map(|n| n as f32), range_bytes, "search").map_err(|error| x.uncovered(error))?;
    let [bx, by, bz] = bins.map(u64::from);
    let last = (bx - 1) + bx * ((by - 1) + by * (bz - 1));
    if searched != bins || last >= range_bytes / size_of::<CellRange>() as u64 || last > i32::MAX as u64 {
        return Err(x.uncovered(format!("bin {last} of {bins:?} lies past the ranges")));
    }
    Ok(())
}

/// The bins a searching atom reads, as `read_searched_bins`, checked against
/// the ranges it indexes.
pub(crate) fn searched(x: &AtomExtent<'_>) -> Result<[u32; 3], Verdict> {
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

pub(crate) fn required_blob_bounds(x: &AtomExtent<'_>) -> Result<(), Verdict> {
    if x.bytes("bounds") != Some(8) {
        return Err(x.uncovered("bounds must contain exactly two f32 words from Blob Bounds".into()));
    }
    Ok(())
}

pub(crate) fn brick_schedule(x: &AtomExtent<'_>, nodes: [u32; 3]) -> Result<(), Verdict> {
    if !x.wired("bricks") { return Ok(()); }
    let words = crate::node_graph::primitives::liquid_bricks::schedule_words(nodes)
        .ok_or_else(|| x.uncovered("brick schedule size overflow".into()))?;
    x.covers("bricks", words * 4)
}

pub(crate) fn surface_mesh_pass(x: &mut AtomExtent<'_>, output: &str) -> Result<(), Verdict> {
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

pub(crate) const PARTICLE: u64 = size_of::<FluidParticle>() as u64;

/// A whole-number param, as the vector atoms round it.
pub(crate) fn whole_param(x: &AtomExtent<'_>, name: &str, default: f32) -> u32 {
    x.param(name, default).round().max(0.0) as u32
}

/// The body rows the step may read, a whole count.
pub(crate) fn body_rows(x: &AtomExtent<'_>) -> Result<u64, Verdict> {
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
pub(crate) fn whitewater_lattice(x: &AtomExtent<'_>, names: [&str; 3]) -> Result<([u32; 3], [u32; 3]), Verdict> {
    let nodes = names.map(|name| whole(x, name, 71.0));
    let cells = grid_cells(nodes).ok_or_else(|| Verdict::Refused(format!("a {nodes:?} solid lattice has too few or too many nodes")))?;
    Ok((nodes, cells))
}

pub(crate) fn whitewater_grid(x: &AtomExtent<'_>) -> Result<(u64, u64), Verdict> {
    let (nodes, cells) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    Ok((cell_total(nodes), cell_total(cells)))
}

/// The face grid, placed centred in the whitewater grid, and each face
/// array covering its axis.
pub(crate) fn whitewater_faces(x: &AtomExtent<'_>, nodes: [u32; 3], names: [&str; 3]) -> Result<[u32; 3], Verdict> {
    let face_cells = names.map(|name| whole(x, name, 64.0));
    face_offset(nodes, face_cells).map_err(Verdict::Refused)?;
    for (axis, port) in ["face_u", "face_v", "face_w"].into_iter().enumerate() {
        x.covers(port, face_len(face_cells, axis) * 4)?;
    }
    Ok(face_cells)
}

pub(crate) const KNOWN_VALUE: u64 = size_of::<KnownValue>() as u64;

/// One particle record out per particle in.
pub(crate) fn particle_map(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.bytes("particles").unwrap_or(0))
}

/// One f32 out per particle in.
pub(crate) fn particle_values(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.items("particles").unwrap_or(0) * 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_extent_rule_is_an_error() {
        let mut graph = Graph::new();
        let plan = compile(&graph).expect("empty graph plan");
        let rule = ExtentRule { type_id: "test.duplicate_extent", check: size_bounded };
        let error = check_graph(&mut graph, &plan, &[rule, rule]).unwrap_err();
        assert!(matches!(error, ExtentError::DuplicateRule { type_id } if type_id == rule.type_id));
    }


    use crate::node_graph::fluid::domain_layout;
    use crate::node_graph::matter::{block_sort_box, lattice_blocks};

    use crate::node_graph::primitives::matter_domain::admit_lattice;

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

#[cfg(test)]
#[doc(hidden)]
pub(crate) mod testkit;
