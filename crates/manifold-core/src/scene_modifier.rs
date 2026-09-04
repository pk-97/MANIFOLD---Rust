//! Scene modifier plan data (SCENE_MODIFIER_FRAMEWORK_DESIGN.md D1/D2 section 3.2).
//!
//! A scene modifier is to a 3D scene what an effect is to a 2D layer. This
//! module carries the crate-neutral plan a modifier kind's renderer-side
//! builder produces and the editing crate's generic `ApplySceneModifierCommand`
//! / `RemoveSceneModifierCommand` consume — the same renderer-builds /
//! editing-applies split `scene_loop.rs` used (the loop is kind #1, D6).
//!
//! No GPU, no editing dependency: the shared data contract, nothing more.

use std::collections::BTreeMap;

use crate::NodeId;
use crate::effect_graph_def::{EffectGraphNode, EffectGraphWire};
use crate::scene_exposure::SceneParamMetadata;

/// One entry of a kind's declarative trace signature (D3). The plan carries
/// it so the editing-crate apply command can refuse a PARTIAL trace (INV-M9)
/// without depending on the renderer-side registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTraceNode {
    pub type_id: String,
    pub node_id: String,
    pub required: bool,
}

/// Per-object-group interface splice: drive one port of every inner node of
/// a type inside a group through the group's interface (generalizes the
/// scene loop's per-group `instances` wiring).
#[derive(Debug, Clone)]
pub struct GroupSplice {
    /// The group node id (scope_path for descend_level).
    pub group_node_id: u32,
    /// The inner node type the splice targets (e.g. `node.scene_object`).
    pub inner_node_type: &'static str,
    /// The inner node's port the splice wires (e.g. `instances`) — also the
    /// interface input name the apply mints.
    pub inner_port: &'static str,
    /// The new top-level producer's doc id (e.g. the scene_array).
    pub source_doc_id: u32,
    /// The producer's output port (e.g. `out`).
    pub source_port: String,
}

/// Port take-over with declarative restore (generalizes the loop's
/// lens.camera re-point): apply drops other producers of
/// (target_node_id, target_port); remove re-wires the first non-mine node
/// whose type is in `restore_types` when the port is left unwired.
#[derive(Debug, Clone)]
pub struct PortRepoint {
    pub target_node_id: u32,
    pub target_port: String,
    /// The modifier's own producer doc id — its wire is kept.
    pub new_producer_doc_id: u32,
    /// Producer type ids the remove step may wire back in (camera sources
    /// for a camera-path repoint).
    pub restore_types: &'static [&'static str],
}

/// Per-node exposure curation (INV-6: each node its own manifest only).
/// `params` are the node's stamped param values at apply time — the exposure
/// defaults seed from them, never from the generic manifest default.
#[derive(Debug, Clone)]
pub struct NodeExposure {
    pub node_doc_id: u32,
    pub node_id: NodeId,
    pub type_id: String,
    pub params: BTreeMap<String, crate::effect_graph_def::SerializedParamValue>,
    pub metadata: Vec<SceneParamMetadata>,
}

/// Enable wiring the apply stamps (D5). `extra_nodes`/`extra_wires` ride the
/// plan so the loop's `loop_cam_switch` lands in the same undo unit as the
/// atoms themselves.
#[derive(Debug, Clone)]
pub struct EnablePlan {
    /// What the enable toggle writes (the row target P3 cards will surface).
    pub toggle: ToggleDecl,
    pub extra_nodes: Vec<EffectGraphNode>,
    pub extra_wires: Vec<EffectGraphWire>,
}

#[derive(Debug, Clone)]
pub enum ToggleDecl {
    /// Switch kinds: the row targets this node's `param` directly; `on`/`off`
    /// are the written values (the loop's camera_switch select).
    NodeParam { node_doc_hint: NodeId, param: String, on: f32, off: f32 },
    /// Gate kinds: the row targets the enabled value atom's `value` param.
    ValueAtom { node_id: NodeId },
}

/// Plan data for applying one modifier kind. Built renderer-side by the
/// kind's descriptor `plan_builder`; handed to
/// [`ApplySceneModifierCommand`][editing] and (as the inverse-it-knows)
/// [`RemoveSceneModifierCommand`][editing]. Semantics byte-identical to the
/// scene-loop pair this generalizes (D1).
///
/// [editing]: ../../../manifold-editing/src/commands/graph/scene_modifier.rs
#[derive(Debug, Clone)]
pub struct SceneModifierPlan {
    /// Stable kind id — public API forever (D6 makes "scene_loop" the first).
    pub kind_id: String,
    /// Card title == exposure section string (stamped into
    /// `ParamSpecDef::section`; the remove strips by it).
    pub display_name: String,
    /// The kind's trace signature — carried so apply can refuse a partial
    /// trace (INV-M9) with no registry dependency.
    pub trace: Vec<PlanTraceNode>,
    /// New nodes the apply adds at the scene graph level.
    pub new_nodes: Vec<EffectGraphNode>,
    /// New wires connecting the modifier's nodes to each other and to
    /// existing nodes.
    pub new_wires: Vec<EffectGraphWire>,
    /// Per-object-group interface splices.
    pub group_splices: Vec<GroupSplice>,
    /// Port take-overs with declarative restore.
    pub repoints: Vec<PortRepoint>,
    /// Per-node exposure curation (INV-6: each node its own manifest only).
    pub exposures: Vec<NodeExposure>,
    pub enable: EnablePlan,
}

impl SceneModifierPlan {
    /// Every stable node_id the plan mints — the identity the remove command
    /// drops by (new nodes plus the enable wiring's extras, e.g. the loop's
    /// camera switch). Never matched by numeric doc id (the flattener
    /// renumbers).
    pub fn minted_node_ids(&self) -> Vec<NodeId> {
        self.new_nodes
            .iter()
            .chain(self.enable.extra_nodes.iter())
            .map(|n| n.node_id.clone())
            .collect()
    }
}
