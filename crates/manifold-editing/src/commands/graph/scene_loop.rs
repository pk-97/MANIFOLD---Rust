//! Scene Loop apply/remove commands (SCENE_LOOP_DESIGN.md D5).
//!
//! Composite commands that splice loop infrastructure into an imported scene
//! graph. The plan (nodes, wires, group wiring) is built by the renderer-side
//! plan builder in `manifold-app` (which depends on both crates), then passed
//! as plain `manifold_core` fields to these commands.

use manifold_core::GraphTarget;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, ParamSpecDef, PresetMetadata,
};
use manifold_core::project::Project;
use manifold_core::scene_exposure::{stamp_scene_node_exposures_into, SceneParamMetadata};

use crate::command::Command;

use super::{
    descend_level, refresh_target_manifest, with_existing_target_graph_mut,
    with_target_graph_mut,
};

/// Data needed to wire scene_array into one object group's instances port.
#[derive(Debug, Clone)]
pub struct InstanceWiring {
    /// The group node id (scope_path for descend_level).
    pub group_node_id: u32,
}

/// Plan data for applying a scene loop. Built by the renderer-side plan builder
/// (`manifold-app`), handed to [`ApplySceneLoopCommand::new`].
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
    /// Exposure metadata for the new loop nodes' params.
    pub loop_metadata: Vec<SceneParamMetadata>,
    /// Card-level param specs for the loop nodes' exposed params.
    pub card_params: Vec<ParamSpecDef>,
    /// The stable node_id of the loop_camera (for the camera rewiring wire).
    pub loop_camera_node_id: manifold_core::NodeId,
    /// The stable node_id of the scene_array (for instance wiring).
    pub scene_array_node_id: manifold_core::NodeId,
}

/// "Apply Scene Loop" — splice loop nodes into the scene graph.
///
/// One undo unit. Refuses with a logged error when the graph has != 1
/// `node.render_scene` (INV-1).
#[derive(Debug)]
pub struct ApplySceneLoopCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    plan: SceneLoopPlan,
    catalog_default: EffectGraphDef,
    /// Pre-edit `(nodes, wires)` at `scope_path`, plus pre-edit
    /// `preset_metadata`. Set on execute.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>)>,
}

impl ApplySceneLoopCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        plan: SceneLoopPlan,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            plan,
            catalog_default,
            prev: None,
        }
    }
}

impl Command for ApplySceneLoopCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let plan = self.plan.clone();
        let result = with_target_graph_mut(
            project,
            &self.target,
            &self.catalog_default,
            true,
            |def| {
                let prev_metadata = def.preset_metadata.clone();

                // Card-spec additions land on the WHOLE def's preset_metadata
                // (same pattern as ImportModelIntoSceneCommand).
                if !plan.card_params.is_empty() {
                    let meta = def.preset_metadata.get_or_insert_with(|| PresetMetadata {
                        id: manifold_core::PresetTypeId::from_string("UnnamedScene".to_string()),
                        display_name: "Scene".to_string(),
                        category: "Geometry".to_string(),
                        osc_prefix: "scene".to_string(),
                        legacy_discriminant: None,
                        available: true,
                        is_line_based: false,
                        params: Vec::new(),
                        bindings: Vec::new(),
                        param_aliases: Vec::new(),
                        value_aliases: Vec::new(),
                        string_params: Vec::new(),
                        string_bindings: Vec::new(),
                        scene_bounds: None,
                    });
                    meta.params.extend(plan.card_params);
                }

                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let prev_nodes_wires = (nodes.clone(), wires.clone());

                // INV-1: exactly one render_scene.
                let scene_count = nodes
                    .iter()
                    .filter(|n| n.type_id == "node.render_scene")
                    .count();
                if scene_count != 1 {
                    eprintln!(
                        "ApplySceneLoopCommand: INV-1 violation — expected 1 render_scene, found {scene_count}"
                    );
                    return None;
                }

                // Add all loop nodes.
                nodes.extend(plan.new_nodes.iter().cloned());
                wires.extend(plan.new_wires.iter().cloned());

                // Wire scene_array.out into each object group's instances port.
                for wiring in &plan.instance_wirings {
                    // Find the scene_object node inside the group and wire
                    // scene_array.out → object_bind.instances.
                    // The wiring target is scope_path=[group_node_id] and the port
                    // is "instances" on the scene_object node.
                    wires.push(EffectGraphWire {
                        from_node: plan.new_nodes.iter().find(|n| n.node_id == plan.scene_array_node_id).map(|n| n.id).unwrap_or(0),
                        from_port: "out".to_string(),
                        to_node: wiring.group_node_id,
                        to_port: "instances".to_string(),
                    });
                }

                Some((prev_nodes_wires, prev_metadata))
            },
        );
        if let Some((pnw, pmeta)) = result.flatten() {
            self.prev = Some((pnw.0, pnw.1, pmeta));
        }

        // Stamp exposures for the loop nodes.
        let plan_ref = &self.plan;
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some(ref mut meta) = def.preset_metadata {
                for node in &plan_ref.new_nodes {
                    stamp_scene_node_exposures_into(
                        &mut meta.params,
                        &mut meta.bindings,
                        node.id,
                        &node.node_id,
                        &node.type_id,
                        "Scene Loop",
                        &plan_ref.loop_metadata,
                        &node.params,
                    );
                }
            }
        });

        refresh_target_manifest(project, &self.target);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw, pmeta)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.preset_metadata = pmeta;
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
        refresh_target_manifest(project, &self.target);
    }

    fn description(&self) -> &str {
        "Apply Scene Loop"
    }
}

/// "Remove Scene Loop" — symmetric removal (not "undo and hope").
///
/// Restores the graph to its pre-loop state, same as undo but callable as a
/// standalone command.
#[derive(Debug)]
pub struct RemoveSceneLoopCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    /// The pre-loop state (nodes, wires, metadata). Set by the caller who
    /// captures it before applying.
    pre_loop_state: (Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>),
    /// Post-remove state for undo.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>)>,
}

impl RemoveSceneLoopCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        pre_loop_state: (Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>),
    ) -> Self {
        Self {
            target,
            scope_path,
            pre_loop_state,
            prev: None,
        }
    }
}

impl Command for RemoveSceneLoopCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let pre = self.pre_loop_state.clone();
        let result = with_existing_target_graph_mut(project, &self.target, true, |def| {
            let prev_metadata = def.preset_metadata.clone();
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());

            *nodes = pre.0;
            *wires = pre.1;
            def.preset_metadata = pre.2;

            Some((prev, prev_metadata))
        });
        if let Some(((pn, pw), pmeta)) = result.flatten() {
            self.prev = Some((pn, pw, pmeta));
        }
        refresh_target_manifest(project, &self.target);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw, pmeta)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.preset_metadata = pmeta;
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
        refresh_target_manifest(project, &self.target);
    }

    fn description(&self) -> &str {
        "Remove Scene Loop"
    }
}
