//! Read-only graph snapshot for the editor UI.
//!
//! [`GraphSnapshot`] is an owned, `Send`able view of a [`Graph`] that the
//! content thread can build once per frame and hand to the UI thread. It
//! deliberately holds no references back into the live graph, no GPU
//! resources, and no trait objects — just plain data the canvas can render.
//!
//! V1 graphs don't carry editor positions yet (the editor that would set
//! them is V2 work — see `docs/NODE_GRAPH_SYSTEM.md` §13–14), so the
//! snapshot exposes `editor_pos: Option<(f32, f32)>` and the canvas falls
//! back to auto-layout when it's `None`.
//!
//! Cost is bounded by graph size: one allocation per node + one per wire
//! plus a couple of small string clones per port. Cheap enough to rebuild
//! every frame for V1 (4-node test graph). A future optimization is to
//! gate snapshot generation on a topology version counter.

use crate::node_graph::graph::Graph;
use crate::node_graph::ports::{PortKind, PortType};

/// Owned, `Send`able view of a graph for the editor canvas.
#[derive(Debug, Clone)]
pub struct GraphSnapshot {
    pub nodes: Vec<NodeSnapshot>,
    pub wires: Vec<WireSnapshot>,
}

/// One node in the snapshot.
#[derive(Debug, Clone)]
pub struct NodeSnapshot {
    /// Stable instance id within the graph. Matches `NodeInstanceId.0`.
    pub id: u32,
    /// `EffectNodeType` string — `primitive.mix`, `effect.bloom`, etc.
    pub type_id: String,
    /// Display title derived from `type_id` (e.g. `primitive.mix` → "Mix").
    pub title: String,
    pub inputs: Vec<PortSnapshot>,
    pub outputs: Vec<PortSnapshot>,
    /// Editor-saved position in graph-space, or `None` when the graph
    /// has never been opened in an editor (V1).
    pub editor_pos: Option<(f32, f32)>,
}

/// One port (input or output) on a node snapshot.
#[derive(Debug, Clone)]
pub struct PortSnapshot {
    pub name: String,
    pub kind: PortKindSnapshot,
}

/// Simplified port type for snapshot — collapses scalar sub-types into
/// one bucket since the canvas colours by category, not by float vs vec3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKindSnapshot {
    Texture2D,
    Texture3D,
    Scalar,
}

impl From<PortType> for PortKindSnapshot {
    fn from(t: PortType) -> Self {
        match t {
            PortType::Texture2D => Self::Texture2D,
            PortType::Texture3D => Self::Texture3D,
            PortType::Scalar(_) => Self::Scalar,
        }
    }
}

/// One wire in the snapshot.
#[derive(Debug, Clone)]
pub struct WireSnapshot {
    pub from_node: u32,
    pub from_port: String,
    pub to_node: u32,
    pub to_port: String,
}

impl GraphSnapshot {
    /// Build a snapshot from a live graph. Walks every node and wire,
    /// allocates owned strings so the result is fully detached from the
    /// graph's `'static` port-name references.
    pub fn from_graph(graph: &Graph) -> Self {
        let mut nodes: Vec<NodeSnapshot> = graph
            .nodes()
            .map(|inst| {
                let type_id = inst.node.type_id().as_str().to_string();
                let title = title_from_type_id(&type_id);
                let inputs = inst
                    .node
                    .inputs()
                    .iter()
                    .filter(|p| matches!(p.kind, PortKind::Input))
                    .map(|p| PortSnapshot {
                        name: p.name.to_string(),
                        kind: PortKindSnapshot::from(p.ty),
                    })
                    .collect();
                let outputs = inst
                    .node
                    .outputs()
                    .iter()
                    .filter(|p| matches!(p.kind, PortKind::Output))
                    .map(|p| PortSnapshot {
                        name: p.name.to_string(),
                        kind: PortKindSnapshot::from(p.ty),
                    })
                    .collect();
                NodeSnapshot {
                    id: inst.id.0,
                    type_id,
                    title,
                    inputs,
                    outputs,
                    editor_pos: None,
                }
            })
            .collect();
        // Stable order so the canvas's auto-layout is deterministic across
        // snapshots (graph.nodes() iterates the underlying AHashMap in
        // arbitrary order).
        nodes.sort_by_key(|n| n.id);

        let wires = graph
            .wires()
            .iter()
            .map(|w| WireSnapshot {
                from_node: w.from.0.0,
                from_port: w.from.1.to_string(),
                to_node: w.to.0.0,
                to_port: w.to.1.to_string(),
            })
            .collect();

        Self { nodes, wires }
    }
}

/// Convert a stable type id like `primitive.mix` or `composite.bloom` into
/// a short title suitable for display: "Mix", "Bloom". Falls back to the
/// raw id when there's no dot separator.
fn title_from_type_id(type_id: &str) -> String {
    let tail = type_id.rsplit_once('.').map(|(_, t)| t).unwrap_or(type_id);
    let mut chars = tail.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort};

    struct StubNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl EffectNode for StubNode {
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

    fn input(name: &'static str) -> NodeInput {
        NodePort {
            name,
            ty: PortType::Texture2D,
            kind: PortKind::Input,
            required: true,
        }
    }
    fn output(name: &'static str) -> NodeOutput {
        NodePort {
            name,
            ty: PortType::Texture2D,
            kind: PortKind::Output,
            required: false,
        }
    }

    #[test]
    fn snapshot_captures_nodes_and_wires() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(StubNode {
            type_id: EffectNodeType::new("primitive.source"),
            inputs: vec![],
            outputs: vec![output("out")],
        }));
        let b = g.add_node(Box::new(StubNode {
            type_id: EffectNodeType::new("primitive.mix"),
            inputs: vec![input("a"), input("b")],
            outputs: vec![output("out")],
        }));
        g.connect((a, "out"), (b, "a")).unwrap();

        let snap = GraphSnapshot::from_graph(&g);
        assert_eq!(snap.nodes.len(), 2);
        assert_eq!(snap.wires.len(), 1);
        assert!(snap.nodes.iter().any(|n| n.title == "Source"));
        assert!(snap.nodes.iter().any(|n| n.title == "Mix"));
        let mix = snap.nodes.iter().find(|n| n.title == "Mix").unwrap();
        assert_eq!(mix.inputs.len(), 2);
        assert_eq!(mix.outputs.len(), 1);
    }

    #[test]
    fn title_lowercase_id_capitalizes() {
        assert_eq!(title_from_type_id("primitive.blur"), "Blur");
        assert_eq!(title_from_type_id("composite.bloom"), "Bloom");
        assert_eq!(title_from_type_id("oddball"), "Oddball");
    }
}
