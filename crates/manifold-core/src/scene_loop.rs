//! Scene Loop apply/remove plan data (SCENE_LOOP_DESIGN.md D5/D6).
//!
//! The plan (nodes, wires, group wiring) is built renderer-side by
//! `manifold_renderer::node_graph::gltf_import::assemble_scene_loop_plan`
//! (which can read primitive manifests) and consumed by the editing-crate
//! `ApplySceneLoopCommand` / `RemoveSceneLoopCommand`. The plan travels as
//! plain `manifold_core` fields so neither side needs to see the other's
//! crate — the same `assemble_merge_plan` → `ImportModelIntoSceneCommand`
//! split the import merge uses.
//!
//! This module carries NO GPU and NO editing dependency: it is the shared
//! data contract, nothing more.

use crate::NodeId;
use crate::effect_graph_def::{EffectGraphNode, EffectGraphWire};
use crate::scene_exposure::SceneParamMetadata;

/// Data needed to wire scene_array into one object group's instances port.
#[derive(Debug, Clone)]
pub struct InstanceWiring {
    /// The group node id (scope_path for descend_level).
    pub group_node_id: u32,
    /// The `node.scene_object` doc id INSIDE the group body whose `instances`
    /// input receives the splice (`object_k_bind`). Resolved by the plan
    /// builder (which can read the group body); the command descends into
    /// `group_node_id`'s body and adds the interface input + inner
    /// `group_input.instances → scene_object.instances` wire.
    pub scene_object_node_id: u32,
}

/// Plan data for applying a scene loop. Built renderer-side by
/// `assemble_scene_loop_plan`, handed to [`ApplySceneLoopCommand`] and
/// (as the inverse-it-knows) [`RemoveSceneLoopCommand`].
#[derive(Debug, Clone)]
pub struct SceneLoopPlan {
    /// New loop nodes to add at the scene graph level (loop_phase, scene_array,
    /// loop_camera, optionally loop_fog).
    pub new_nodes: Vec<EffectGraphNode>,
    /// New wires connecting the loop nodes to each other and to existing nodes.
    pub new_wires: Vec<EffectGraphWire>,
    /// Per-group wiring: scene_array.out → each group's scene_object instances port.
    pub instance_wirings: Vec<InstanceWiring>,
    /// The render_scene node's doc id (for INV-1 check and camera rewiring).
    pub render_scene_node_id: u32,
    /// Per-node exposure metadata: stable `node_id` → its primitive manifest.
    /// The command stamps `spec.section = Some("Scene Loop")` for each node
    /// from ITS OWN metadata — never a shared union (a shared union would
    /// stamp phantom rows for params the node doesn't have — INV-6).
    pub node_metadata: Vec<(NodeId, Vec<SceneParamMetadata>)>,
    /// The stable node_id of the loop_camera (for the camera rewiring wire).
    pub loop_camera_node_id: NodeId,
    /// The stable node_id of the scene_array (for instance wiring).
    pub scene_array_node_id: NodeId,
}