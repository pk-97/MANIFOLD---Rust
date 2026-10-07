use ahash::AHashMap;
use crate::node_graph::effect_node::{EffectNodeType, NodeInstanceId};
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::validation::GraphError;

/// Returned by every composite-builder function. Tracks the wire endpoint
/// to use as the composite's output, the routing from outer parameter
/// names to the inner `(node, param)` they drive, and the full set of
/// inner node ids (so a future editor can draw the composite as a
/// collapsible group and so save/load can identify the cluster).
pub struct CompositeHandle {
    type_id: EffectNodeType,
    output: (NodeInstanceId, &'static str),
    param_routing: AHashMap<&'static str, (NodeInstanceId, &'static str)>,
    inner_nodes: Vec<NodeInstanceId>,
}

impl CompositeHandle {
    /// Construct a handle for a composite whose output port is `(node, port_name)`.
    pub fn new(type_id: &'static str, output: (NodeInstanceId, &'static str)) -> Self {
        Self {
            type_id: EffectNodeType::new(type_id),
            output,
            param_routing: AHashMap::default(),
            inner_nodes: Vec::new(),
        }
    }

    /// Record an inner node as belonging to this composite (for editor /
    /// save-load purposes; doesn't affect runtime).
    pub fn add_inner(&mut self, node: NodeInstanceId) -> &mut Self {
        self.inner_nodes.push(node);
        self
    }

    /// Expose an inner node's parameter under an outer name.
    /// Outer names are the slots that will appear on the effect card.
    pub fn expose_param(
        &mut self,
        outer_name: &'static str,
        inner_node: NodeInstanceId,
        inner_param: &'static str,
    ) -> &mut Self {
        self.param_routing
            .insert(outer_name, (inner_node, inner_param));
        self
    }

    pub fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }

    /// The wire endpoint downstream nodes connect to.
    pub fn output(&self) -> (NodeInstanceId, &'static str) {
        self.output
    }

    /// Inner node ids, in insertion order. The composite can be identified
    /// by this set; deleting them from the graph removes the composite.
    pub fn inner_nodes(&self) -> &[NodeInstanceId] {
        &self.inner_nodes
    }

    /// Outer parameter names this composite exposes.
    pub fn exposed_params(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.param_routing.keys().copied()
    }

    /// Resolve an outer parameter name to its inner (node, param)
    /// destination, or `None` if no such routing exists. Used by the
    /// editor inspector to flag inner params that an outer effect-
    /// card slider drives every frame.
    pub fn inner_routing_for(
        &self,
        outer_name: &str,
    ) -> Option<(NodeInstanceId, &'static str)> {
        self.param_routing.get(outer_name).copied()
    }

    /// Set an exposed parameter by its outer name. Routes through to the
    /// underlying inner node's parameter.
    pub fn set_param(
        &self,
        graph: &mut Graph,
        outer_name: &str,
        value: ParamValue,
    ) -> Result<(), GraphError> {
        let (node, inner_name) = self.param_routing.get(outer_name).copied().ok_or_else(|| {
            GraphError::ParamNotFound {
                // sentinel: this is a composite-level lookup, not a node-level one.
                node: NodeInstanceId(u32::MAX),
                param: outer_name.to_string(),
            }
        })?;
        graph.set_param(node, inner_name, value)
    }
}

