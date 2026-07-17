//! The [`Graph`] data model — owns [`EffectNode`]s and [`NodeWire`]s, and
//! exposes mutation operations for adding nodes, wiring outputs to inputs,
//! and updating per-instance parameter values.
//!
//! No execution happens here. The runtime (execution plan, resource bindings,
//! per-frame evaluation) lands in subsequent steps.

use ahash::{AHashMap, AHashSet};

use crate::node_graph::effect_node::{EffectNode, NodeInstanceId, NodeWire, ParamValues};
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::validation::{GraphError, validate_connection};

/// One instance of an [`EffectNode`] within a [`Graph`].
///
/// Owns the boxed node implementation and its current parameter values.
/// Per-frame state (previous-frame textures, density grids, etc.) lives
/// inside the boxed node, keyed implicitly by the node owning it.
pub struct NodeInstance {
    pub id: NodeInstanceId,
    /// Stable document identity (`EffectGraphNode::node_id`), copied here when
    /// the node is instantiated from a def. This is what param bindings
    /// resolve against — globally unique, so a lookup is unambiguous even in a
    /// shared chain graph holding many effects' nodes. Empty for nodes built
    /// directly in Rust (test composites) that never carried a doc identity.
    pub node_id: manifold_core::NodeId,
    pub node: Box<dyn EffectNode>,
    /// Current values for every parameter the node defines.
    /// Initialised to defaults when the instance is added to the graph.
    pub params: ParamValues,
    /// Names of params currently exposed on the outer card. Mirrors
    /// the serialised `exposed_params` set in `EffectGraphNode`.
    /// `Cow<'static, str>` matches the parameter name keys used throughout
    /// `EffectNode::parameters()` (borrowed for fixed params, owned for a
    /// variadic node's formatted names). Mutated by the unified
    /// `ToggleNodeParamExposeCommand` via `Graph::set_param_exposed`.
    pub exposed_params: AHashSet<std::borrow::Cow<'static, str>>,
    /// Author-supplied display title shown in the node header, copied from
    /// `EffectGraphNode::title` at load. `None` falls back to the friendly
    /// palette label (or a prettified type id). Honored for every node type;
    /// the snapshot builder appends a `(WGSL)` marker for `wgsl_compute` nodes
    /// so a hand-written shader reads as custom rather than native. This is the
    /// single home for the title — it round-trips back to the def via
    /// persistence's `from_graph`.
    pub title: Option<String>,
    /// Bumped whenever a param write actually CHANGES a value (compare-on-
    /// write in [`Graph::set_param`] / [`Graph::set_param_unchecked`]). The
    /// executor's memoized-dataflow skip reads this: a pure step whose
    /// `param_epoch` and input resources are unchanged since its last execute
    /// is skipped and its held output slot serves consumers. Per-frame hosts
    /// re-writing the SAME value (binding applies, constant `aspect`) don't
    /// bump; `time`/`beat` writes bump every frame, which is exactly what
    /// keeps time-driven nodes re-executing.
    pub param_epoch: u64,
}

impl NodeInstance {
    fn new(id: NodeInstanceId, mut node: Box<dyn EffectNode>) -> Self {
        let mut params = AHashMap::default();
        for def in node.parameters() {
            params.insert(def.name.clone(), def.default.clone());
        }
        // Let variadic nodes build their param-derived port lists from the
        // default param values before the instance is queried by compile /
        // snapshot. No-op for the fixed-port-shape majority.
        node.reconfigure(&params);
        Self {
            id,
            node_id: manifold_core::NodeId::default(),
            node,
            params,
            exposed_params: AHashSet::default(),
            title: None,
            param_epoch: 0,
        }
    }
}

/// A directed graph of [`EffectNode`]s connected by [`NodeWire`]s.
///
/// All mutation goes through this type's methods. Connection legality is
/// checked at call time (see `connect`); whole-graph invariants (required
/// inputs wired, no cycles) are checked separately via
/// [`crate::node_graph::validation::validate`].
pub struct Graph {
    nodes: AHashMap<NodeInstanceId, NodeInstance>,
    wires: Vec<NodeWire>,
    next_id: u32,
    /// Stable handle → node id map for V2 user-exposed parameters.
    /// Populated only by [`Graph::add_node_named`] — anonymous nodes
    /// added via [`Graph::add_node`] don't appear here. Handles are
    /// `&'static str` (set at effect construction); user bindings on
    /// disk store the same string and look up the live id at apply
    /// time. See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7.
    handles: AHashMap<&'static str, NodeInstanceId>,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            nodes: AHashMap::default(),
            wires: Vec::new(),
            next_id: 0,
            handles: AHashMap::default(),
        }
    }

    /// Add a node, return its assigned [`NodeInstanceId`].
    /// Parameters are initialised to their declared defaults.
    pub fn add_node(&mut self, node: Box<dyn EffectNode>) -> NodeInstanceId {
        let id = NodeInstanceId(self.next_id);
        self.next_id += 1;
        self.nodes.insert(id, NodeInstance::new(id, node));
        id
    }

    /// Add a node with a stable string handle and return its assigned
    /// [`NodeInstanceId`]. The handle is recorded in a per-graph map
    /// so user-exposed parameter bindings can address the inner node
    /// by name across renderer refactors that reorder construction.
    ///
    /// Naming convention is up to the effect author: `"uv_transform"`,
    /// `"feedback"`, `"mix"`, etc. Handles must be unique within the
    /// graph — passing a duplicate handle panics. This is a programming
    /// error (the developer wrote the same literal twice), not user
    /// error, so it's loud at construction time.
    ///
    /// Renames go through `EffectNodeAliasMetadata` in `manifold-core`;
    /// the resolver translates saved bindings on load.
    pub fn add_node_named(
        &mut self,
        handle: &'static str,
        node: Box<dyn EffectNode>,
    ) -> NodeInstanceId {
        let id = self.add_node(node);
        if let Some(prev) = self.handles.insert(handle, id) {
            panic!(
                "Graph::add_node_named: duplicate handle '{handle}' \
                 (already mapped to {prev:?}, just tried to remap to {id:?}). \
                 Handles must be unique within a graph."
            );
        }
        id
    }

    /// Look up a node id by its stable handle. Returns `None` if no
    /// node was added with that handle (or if the handle has been
    /// retired — handles are not removed when their node is, since
    /// `remove_node` is rare and keeping the old mapping doesn't
    /// break anything).
    pub fn node_id_by_handle(&self, handle: &str) -> Option<NodeInstanceId> {
        self.handles.get(handle).copied()
    }

    /// Set the stable document identity on an instance, copied from the def
    /// node at instantiation. No-op if the instance isn't present.
    pub fn set_node_id(&mut self, id: NodeInstanceId, node_id: manifold_core::NodeId) {
        if let Some(inst) = self.nodes.get_mut(&id) {
            inst.node_id = node_id;
        }
    }

    /// Resolve a stable [`manifold_core::NodeId`] to its live instance id.
    /// Node ids are globally unique, so this is unambiguous even when the
    /// graph holds many spliced effects. `None` for an unknown / empty id.
    /// This is the binding resolver's lookup — the node-id successor to
    /// `node_id_by_handle`.
    pub fn instance_by_node_id(&self, node_id: &manifold_core::NodeId) -> Option<NodeInstanceId> {
        if node_id.is_empty() {
            return None;
        }
        self.nodes
            .values()
            .find(|inst| &inst.node_id == node_id)
            .map(|inst| inst.id)
    }

    /// Register a handle for a node that was added via plain
    /// [`add_node`]. Used by ChainSpec snapshot construction where the
    /// splice function adds nodes anonymously and the handle map
    /// (effect-local, returned in `SpliceResult`) needs to be projected
    /// onto the snapshot graph so the editor inspector can match
    /// outer-routing handle names against `NodeSnapshot.node_handle`.
    ///
    /// Panics on duplicate handle, same as [`add_node_named`].
    pub fn register_handle(&mut self, handle: &'static str, node: NodeInstanceId) {
        if let Some(prev) = self.handles.insert(handle, node) {
            panic!(
                "Graph::register_handle: duplicate handle '{handle}' \
                 (already mapped to {prev:?}, just tried to remap to {node:?})."
            );
        }
    }

    /// Iterate the (handle, node id) pairs registered on this graph.
    pub fn handles(&self) -> impl Iterator<Item = (&'static str, NodeInstanceId)> + '_ {
        self.handles.iter().map(|(k, v)| (*k, *v))
    }

    /// Remove a node and any wires that touch it. Returns the removed
    /// [`NodeInstance`], or `None` if the id wasn't in the graph.
    pub fn remove_node(&mut self, id: NodeInstanceId) -> Option<NodeInstance> {
        let removed = self.nodes.remove(&id)?;
        self.wires.retain(|w| w.from.0 != id && w.to.0 != id);
        // Drop any handle that pointed at this node — keeping it would
        // strand a stale handle->dead-id mapping that future
        // node_id_by_handle lookups would honor.
        self.handles.retain(|_, v| *v != id);
        Some(removed)
    }

    /// Connect an output port of one node to an input port of another.
    ///
    /// Validates that:
    /// - both nodes exist
    /// - both port names exist on their respective nodes
    /// - the source port is an Output and the target port is an Input
    /// - the port types match
    /// - adding this wire wouldn't create a cycle
    ///
    /// If the target input was already wired, the previous wire is replaced
    /// (inputs accept exactly one source; outputs may fan out to many).
    pub fn connect(
        &mut self,
        from: (NodeInstanceId, &'static str),
        to: (NodeInstanceId, &'static str),
    ) -> Result<(), GraphError> {
        validate_connection(self, from, to)?;
        self.wires.retain(|w| w.to != to);
        self.wires.push(NodeWire { from, to });
        Ok(())
    }

    /// Remove the wire feeding `to`, if any. Returns the removed wire.
    pub fn disconnect(&mut self, to: (NodeInstanceId, &'static str)) -> Option<NodeWire> {
        let pos = self.wires.iter().position(|w| w.to == to)?;
        Some(self.wires.remove(pos))
    }

    /// Set a parameter value on a node instance. Errors if the node or
    /// parameter doesn't exist. No type coercion — the caller is expected
    /// to supply a [`ParamValue`] of the parameter's declared kind.
    ///
    /// Accepts `&str` for `name`: the actual storage key in
    /// `ParamValues` is the `&'static str` discovered on the primitive's
    /// own `parameters()` list. Callers don't need to materialize a
    /// `&'static str` themselves — the lookup that validates the
    /// param name also yields the canonical static reference. This is
    /// how JSON-driven bindings avoid `Box::leak`-ing every param key
    /// they want to set.
    pub fn set_param(
        &mut self,
        id: NodeInstanceId,
        name: &str,
        value: ParamValue,
    ) -> Result<(), GraphError> {
        let inst = self
            .nodes
            .get_mut(&id)
            .ok_or(GraphError::NodeNotFound(id))?;
        // Clone the ParamDef's canonical name (a `Cow` — `Borrowed` for
        // fixed params, `Owned` for a variadic node's formatted name) as the
        // storage key. Cloning also releases the immutable borrow of
        // `inst.node` before the mutable `inst.params` write below.
        let key = inst
            .node
            .parameters()
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.name.clone())
            .ok_or_else(|| GraphError::ParamNotFound {
                node: id,
                param: name.to_string(),
            })?;
        // Compare-on-write: a write that doesn't change the value is a no-op
        // — no epoch bump (the executor's memo skip keys on `param_epoch`),
        // and no reconfigure (same params ⇒ same port shape). Hosts re-apply
        // bindings and constants every frame; only real changes count.
        if inst.params.get(key.as_ref()) == Some(&value) {
            return Ok(());
        }
        inst.params.insert(key, value);
        inst.param_epoch += 1;
        // Variadic nodes rebuild their port lists when a count-style param
        // changes. Disjoint field borrows (`node` mut, `params` shared).
        inst.node.reconfigure(&inst.params);
        Ok(())
    }

    /// Install a WGSL kernel source on a node. Used by persistence
    /// after a `node.wgsl_compute_*` node is constructed so the kernel
    /// is in place before the first `evaluate`. No-op on nodes whose
    /// shader is fixed at compile time via `include_str!`.
    pub fn set_wgsl_source(
        &mut self,
        id: NodeInstanceId,
        source: &str,
    ) -> Result<(), GraphError> {
        let inst = self
            .nodes
            .get_mut(&id)
            .ok_or(GraphError::NodeNotFound(id))?;
        inst.node.set_wgsl_source(source);
        Ok(())
    }

    /// Install a per-output-port format override on a node. Used by
    /// persistence to apply JSON-declared `outputFormats` entries after
    /// a node is constructed but before `compile()` walks outputs.
    /// No-op on nodes whose format is fixed at compile time (the
    /// default for nearly every primitive).
    pub fn set_output_format(
        &mut self,
        id: NodeInstanceId,
        port: &str,
        format: manifold_gpu::GpuTextureFormat,
    ) -> Result<(), GraphError> {
        let inst = self
            .nodes
            .get_mut(&id)
            .ok_or(GraphError::NodeNotFound(id))?;
        inst.node.set_output_format(port, format);
        Ok(())
    }

    /// Install a per-output-port canvas-relative scale override on a
    /// node. Mirrors `set_output_format` — used by persistence to
    /// apply JSON-declared `outputCanvasScales` entries. No-op on
    /// nodes whose canvas scale is fixed at compile time.
    pub fn set_output_canvas_scale(
        &mut self,
        id: NodeInstanceId,
        port: &str,
        scale: (u32, u32),
    ) -> Result<(), GraphError> {
        let inst = self
            .nodes
            .get_mut(&id)
            .ok_or(GraphError::NodeNotFound(id))?;
        inst.node.set_output_canvas_scale(port, scale);
        Ok(())
    }

    /// Mark `name` as exposed (`true`) or unexposed (`false`) on the
    /// outer card. The graph is the single source of truth for this —
    /// the unified `ToggleNodeParamExposeCommand` flips entries via
    /// this call, regardless of whether the host is an Effect or a
    /// Generator. Validates that `name` is one of the node's declared
    /// params so a typo from a persisted JSON document doesn't silently
    /// add a phantom exposure.
    pub fn set_param_exposed(
        &mut self,
        id: NodeInstanceId,
        name: &str,
        exposed: bool,
    ) -> Result<(), GraphError> {
        let inst = self
            .nodes
            .get_mut(&id)
            .ok_or(GraphError::NodeNotFound(id))?;
        let key = inst
            .node
            .parameters()
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.name.clone())
            .ok_or_else(|| GraphError::ParamNotFound {
                node: id,
                param: name.to_string(),
            })?;
        if exposed {
            inst.exposed_params.insert(key);
        } else {
            inst.exposed_params.remove(key.as_ref());
        }
        Ok(())
    }

    /// Read-only access to whether a node's param is exposed.
    pub fn is_param_exposed(&self, id: NodeInstanceId, name: &str) -> bool {
        self.nodes
            .get(&id)
            .map(|inst| inst.exposed_params.contains(name))
            .unwrap_or(false)
    }

    /// Fast-path variant of [`Self::set_param`] for callers that
    /// constructed the node themselves and know `name` is valid.
    /// Skips the linear scan over `parameters()`. Silently no-ops on
    /// unknown `id` — caller is expected to be looping over node ids
    /// that came out of `add_node`, so the lookup is just a hash hit
    /// in the steady state.
    ///
    /// Used on the per-frame chain-runtime hot path (Mix amount
    /// refresh, generator-side param injection) — saves O(P) per
    /// param-set call across every node in every chain every frame.
    pub fn set_param_unchecked(
        &mut self,
        id: NodeInstanceId,
        name: &'static str,
        value: ParamValue,
    ) {
        if let Some(inst) = self.nodes.get_mut(&id) {
            // Same compare-on-write contract as `set_param` — see there.
            // Hot path: `name` is a `&'static str`, wrapped as a borrowed
            // `Cow` key with no allocation.
            if inst.params.get(name) == Some(&value) {
                return;
            }
            inst.params.insert(std::borrow::Cow::Borrowed(name), value);
            inst.param_epoch += 1;
        }
    }

    pub fn get_node(&self, id: NodeInstanceId) -> Option<&NodeInstance> {
        self.nodes.get(&id)
    }

    pub fn get_node_mut(&mut self, id: NodeInstanceId) -> Option<&mut NodeInstance> {
        self.nodes.get_mut(&id)
    }

    /// Iterate every node in the graph. Iteration order is unspecified.
    pub fn nodes(&self) -> impl Iterator<Item = &NodeInstance> {
        self.nodes.values()
    }

    /// Mutable iterator over every node in the graph. Order is
    /// unspecified. Used by lifecycle paths like
    /// [`crate::generators::json_graph_generator::JsonGraphGenerator::reset_state`]
    /// that need to call `clear_state` on every node.
    pub fn nodes_mut(&mut self) -> impl Iterator<Item = &mut NodeInstance> {
        self.nodes.values_mut()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn wires(&self) -> &[NodeWire] {
        &self.wires
    }

    /// All wires terminating at the given node.
    pub fn wires_into(&self, id: NodeInstanceId) -> impl Iterator<Item = &NodeWire> {
        self.wires.iter().filter(move |w| w.to.0 == id)
    }

    /// All wires originating from the given node.
    pub fn wires_from(&self, id: NodeInstanceId) -> impl Iterator<Item = &NodeWire> {
        self.wires.iter().filter(move |w| w.from.0 == id)
    }

    /// Filtered wire-walking API. The bare `wires()` / `wires_from()`
    /// / `wires_into()` accessors above return every wire regardless
    /// of state-capture status — correct for live-set walks, format
    /// audits, and serialization. Code that follows *this-frame
    /// causality* (topological sort, cycle detection, dependency
    /// propagation) must skip state-capture back-edges; that decision
    /// used to live as duplicated `is_state_capture_wire` closures
    /// scattered across `validation.rs` and `execution_plan.rs`. Two
    /// instances of "pass X forgot to skip state-capture" landed as
    /// silent bugs (the cycle detector false-positive, the array
    /// resource sizing failure); this API forces every caller to
    /// declare its intent, so the next pass we add can't drift the
    /// same way.
    ///
    /// Use [`walk_wires`](Self::walk_wires) for whole-graph walks,
    /// [`walk_wires_from`](Self::walk_wires_from) for outgoing walks,
    /// [`walk_wires_into`](Self::walk_wires_into) for incoming walks.
    pub fn walk_wires(&self, mode: WireWalkMode) -> impl Iterator<Item = &NodeWire> {
        self.wires.iter().filter(move |w| self.wire_matches(w, mode))
    }

    /// Outgoing-wire walk filtered by `mode`. See [`walk_wires`](Self::walk_wires).
    pub fn walk_wires_from(
        &self,
        id: NodeInstanceId,
        mode: WireWalkMode,
    ) -> impl Iterator<Item = &NodeWire> {
        self.wires
            .iter()
            .filter(move |w| w.from.0 == id && self.wire_matches(w, mode))
    }

    /// Incoming-wire walk filtered by `mode`. See [`walk_wires`](Self::walk_wires).
    pub fn walk_wires_into(
        &self,
        id: NodeInstanceId,
        mode: WireWalkMode,
    ) -> impl Iterator<Item = &NodeWire> {
        self.wires
            .iter()
            .filter(move |w| w.to.0 == id && self.wire_matches(w, mode))
    }

    /// Single source of truth for the state-capture predicate. All
    /// `walk_wires*` methods delegate to this so the rule lives in
    /// one place — if the runtime ever adds another kind of back-edge
    /// (e.g. a `state_capture_output_ports` for producer-side
    /// captures) the check extends here, not at every caller.
    pub fn is_state_capture_wire(&self, w: &NodeWire) -> bool {
        self.get_node(w.to.0)
            .map(|inst| inst.node.state_capture_input_ports().contains(&w.to.1))
            .unwrap_or(false)
    }

    fn wire_matches(&self, w: &NodeWire, mode: WireWalkMode) -> bool {
        match mode {
            WireWalkMode::All => true,
            WireWalkMode::ForwardOnly => !self.is_state_capture_wire(w),
            WireWalkMode::CaptureOnly => self.is_state_capture_wire(w),
        }
    }
}

/// Filter mode for [`Graph::walk_wires`] and friends. Every wire-
/// walking call site must declare its intent — the API forces the
/// choice rather than letting a forgotten state-capture check drift
/// into a silent bug (as happened twice before this API existed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireWalkMode {
    /// Every wire, regardless of whether it terminates on a
    /// state-capture input port. Use for live-set reachability,
    /// format compatibility audits, JSON serialization — anywhere
    /// the question is "what wires exist?" not "what depends on
    /// what this frame?"
    All,
    /// Skip wires terminating on a state-capture input port — the
    /// "forward dependencies only" view. Use for topological sort,
    /// cycle detection, in-degree counting, dependency propagation —
    /// anywhere the question is "what runs before what THIS frame?"
    ///
    /// State-capture wires close per-frame loops through the
    /// StateStore rather than this-frame's dependency graph; they
    /// don't contribute to in-degree and don't form cycles in the
    /// causality sense.
    ForwardOnly,
    /// Only wires terminating on a state-capture input port. Use
    /// for the late-capture phase that snapshots producer outputs
    /// at end of frame.
    CaptureOnly,
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::effect_node::{EffectNodeContext, EffectNodeType};
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};

    /// Minimal test [`EffectNode`] — declares port shape, evaluate is a no-op.
    pub(super) struct TestNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl TestNode {
        pub(super) fn new(
            type_name: &'static str,
            inputs: Vec<NodeInput>,
            outputs: Vec<NodeOutput>,
        ) -> Self {
            Self {
                type_id: EffectNodeType::new(type_name),
                inputs,
                outputs,
            }
        }
    }

    impl EffectNode for TestNode {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &self.inputs
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
    }

    pub(super) fn input(name: &'static str, ty: PortType, required: bool) -> NodeInput {
        NodePort {
            name: std::borrow::Cow::Borrowed(name),
            ty,
            kind: PortKind::Input,
            required,
        }
    }

    pub(super) fn output(name: &'static str, ty: PortType) -> NodeOutput {
        NodePort {
            name: std::borrow::Cow::Borrowed(name),
            ty,
            kind: PortKind::Output,
            required: false,
        }
    }

    #[test]
    fn add_then_remove_node_clears_wires_touching_it() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        assert_eq!(g.wires().len(), 1);

        g.remove_node(a);
        assert!(g.get_node(a).is_none());
        assert_eq!(g.wires().len(), 0); // wire was cleaned up
    }

    #[test]
    fn connect_replaces_previous_wire_into_same_input() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "b",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let c = g.add_node(Box::new(TestNode::new(
            "c",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        g.connect((a, "out"), (c, "in")).unwrap();
        g.connect((b, "out"), (c, "in")).unwrap();
        assert_eq!(g.wires().len(), 1);
        assert_eq!(g.wires()[0].from.0, b); // newer wire wins
    }

    #[test]
    fn disconnect_removes_wire() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(TestNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
        )));
        let b = g.add_node(Box::new(TestNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        assert!(g.disconnect((b, "in")).is_some());
        assert_eq!(g.wires().len(), 0);
    }

    #[test]
    fn set_param_rejects_unknown_node_and_param() {
        let mut g = Graph::new();
        let result = g.set_param(NodeInstanceId(99), "x", ParamValue::Float(0.5));
        assert!(matches!(result, Err(GraphError::NodeNotFound(_))));

        let id = g.add_node(Box::new(TestNode::new("a", vec![], vec![])));
        let result = g.set_param(id, "missing", ParamValue::Float(0.5));
        assert!(matches!(result, Err(GraphError::ParamNotFound { .. })));
    }
}
