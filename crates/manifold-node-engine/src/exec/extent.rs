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
use ahash::AHashMap;
use manifold_core::{Beats, Seconds};
use crate::bindings::{NodeInputs, NodeOutputs, Slot};
use crate::exec::backend::{Backend, MockBackend};
use crate::exec::effect_node::{EffectNodeContext, FrameTime, ParamValues};
use crate::exec::execution_plan::{ExecutionPlan, ExecutionStep, ResourceId};
use crate::exec::resource_allocation::plan_array_allocations;
use crate::freeze::classify::fusion_kind_str;
use crate::graph::{Graph, NodeInstance};
use crate::parameters::ParamValue;
use crate::ports::{EXACT_F32_COUNT, PortType};
use crate::scene::transform::Transform;
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


// Registered rules are collected here so the generic checker has no dependency on a solver.
pub static EXTENT_RULES: std::sync::LazyLock<Vec<ExtentRule>> = std::sync::LazyLock::new(|| {
    let mut rules: Vec<_> = inventory::iter::<ExtentRule>.into_iter().copied().collect();
    rules.sort_by_key(|rule| rule.type_id);
    rules
});

/// Texture-only GPU work: no array extent to prove. A node of these types
/// that grows an array port needs a real rule.
/// Asset sources size their source textures from decoded images and bound
/// writes by the destination texture; this does not account texture memory.
pub fn texture_only(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
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
pub fn size_bounded(_: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    Ok(())
}

/// Dispatches over its own input, in place.
pub fn in_place(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.bytes("in").unwrap_or(0))
}

/// A whole-number param, as the vector atoms round it.
pub fn whole_param(x: &AtomExtent<'_>, name: &str, default: f32) -> u32 {
    x.param(name, default).round().max(0.0) as u32
}


#[cfg(any(test, feature = "testkit"))]
#[doc(hidden)]
pub mod testkit;
