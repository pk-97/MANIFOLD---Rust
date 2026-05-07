//! The [`Graph`] data model — owns [`EffectNode`]s and [`NodeWire`]s, and
//! exposes mutation operations for adding nodes, wiring outputs to inputs,
//! and updating per-instance parameter values.
//!
//! No execution happens here. The runtime (execution plan, resource bindings,
//! per-frame evaluation) lands in subsequent steps.

use ahash::AHashMap;

use crate::node_graph::effect_node::{EffectNode, NodeInstanceId, NodeWire, ParamValues};
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::validation::{validate_connection, GraphError};

/// One instance of an [`EffectNode`] within a [`Graph`].
///
/// Owns the boxed node implementation and its current parameter values.
/// Per-frame state (previous-frame textures, density grids, etc.) lives
/// inside the boxed node, keyed implicitly by the node owning it.
pub struct NodeInstance {
    pub id: NodeInstanceId,
    pub node: Box<dyn EffectNode>,
    /// Current values for every parameter the node defines.
    /// Initialised to defaults when the instance is added to the graph.
    pub params: ParamValues,
}

impl NodeInstance {
    fn new(id: NodeInstanceId, node: Box<dyn EffectNode>) -> Self {
        let mut params = AHashMap::default();
        for def in node.parameters() {
            params.insert(def.name, def.default);
        }
        Self { id, node, params }
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
    pub fn set_param(
        &mut self,
        id: NodeInstanceId,
        name: &'static str,
        value: ParamValue,
    ) -> Result<(), GraphError> {
        let inst = self
            .nodes
            .get_mut(&id)
            .ok_or(GraphError::NodeNotFound(id))?;
        if !inst.node.parameters().iter().any(|p| p.name == name) {
            return Err(GraphError::ParamNotFound {
                node: id,
                param: name.to_string(),
            });
        }
        inst.params.insert(name, value);
        Ok(())
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
            name,
            ty,
            kind: PortKind::Input,
            required,
        }
    }

    pub(super) fn output(name: &'static str, ty: PortType) -> NodeOutput {
        NodePort {
            name,
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
