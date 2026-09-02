//! Scene Loop apply/remove commands (SCENE_LOOP_DESIGN.md D5).
//!
//! Composite commands that splice loop infrastructure into an imported scene
//! graph. The plan (nodes, wires, group wiring) is built renderer-side by
//! `manifold_renderer::node_graph::gltf_import::assemble_scene_loop_plan` and
//! travels as `manifold_core::scene_loop::SceneLoopPlan` — the same
//! `assemble_merge_plan` → `ImportModelIntoSceneCommand` split the import
//! merge uses (renderer builds plain core fields; editing applies them).

use manifold_core::GraphTarget;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, InterfacePortDef,
    PresetMetadata,
};
use manifold_core::project::Project;
use manifold_core::scene_exposure::stamp_scene_node_exposures_into;

use std::collections::BTreeMap;

use crate::command::Command;

use super::{
    descend_level, refresh_target_manifest, with_existing_target_graph_mut,
    with_target_graph_mut,
};

// The plan travels as plain manifold_core data; the editing crate re-exports
// it (and its `InstanceWiring`) so existing call sites stay on the
// `commands::graph::` path.
pub use manifold_core::scene_loop::{InstanceWiring, SceneLoopPlan};

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

                // Re-point the camera through the loop_camera (D5): the plan
                // wires `loop_camera.out → lens.camera` (or render_scene.camera
                // when no lens exists); drop the old producer of that port so
                // the two never both feed it (the panel's trace walks the
                // producer and would report the first wire — the dead-silent
                // orbit-vs-loop trap).
                let loop_camera_doc_id = plan
                    .new_nodes
                    .iter()
                    .find(|n| n.node_id == plan.loop_camera_node_id)
                    .map(|n| n.id)
                    .unwrap_or(u32::MAX);
                for w in &plan.new_wires {
                    if w.from_node == loop_camera_doc_id && w.to_port == "camera" {
                        // Drop every OTHER producer feeding this camera port —
                        // keep the loop_camera's own wire.
                        wires.retain(|old| {
                            old.to_node != w.to_node
                                || old.to_port != "camera"
                                || old.from_node == loop_camera_doc_id
                        });
                        break;
                    }
                }

                // Wire scene_array.out into each object group's instances port,
                // THROUGH the group's interface (D5/D6, `scope_path=[group_node_id]`):
                //
                //  - add an `instances` input to the group's interface,
                //  - add a `system.group_input` node to the body (object groups
                //    currently carry none) and wire `group_input.instances →
                //    scene_object.instances` inside,
                //  - at top level, wire `scene_array.out → group_node.instances`.
                //
                // The flattener resolves the top-level wire through the group's
                // interface inputs and the body wire via `group_input`, so the
                // scene_object's `instances` input is driven without a
                // cross-boundary wire (which the flattener rejects).
                let scene_array_doc_id = plan
                    .new_nodes
                    .iter()
                    .find(|n| n.node_id == plan.scene_array_node_id)
                    .map(|n| n.id)
                    .unwrap_or(0);
                let mut next_group_input_id = 1_000_000u32;
                for wiring in &plan.instance_wirings {
                    let Some(group_idx) = nodes.iter().position(|n| n.id == wiring.group_node_id) else {
                        continue;
                    };
                    let group = &mut nodes[group_idx];
                    let Some(body) = group.group.as_deref_mut() else { continue };
                    if body.interface.inputs.iter().any(|p| p.name == "instances") {
                        continue;
                    }
                    body.interface.inputs.push(InterfacePortDef {
                        name: "instances".to_string(),
                        port_type: "Array(InstanceTransform)".to_string(),
                    });
                    // A group_input boundary node carrying the `instances` port
                    // (object groups have none today; the AO group's precedent
                    // names it after the group).
                    let group_input_handle = "loop_in".to_string();
                    let group_input_id = body
                        .nodes
                        .iter()
                        .find(|n| n.type_id == GROUP_INPUT_TYPE_ID)
                        .map(|n| n.id)
                        .unwrap_or_else(|| {
                            // Reserve a fresh body-local id that can't collide
                            // with the top-level minted ids.
                            let id = next_group_input_id;
                            next_group_input_id += 1;
                            body.nodes.push(EffectGraphNode {
                                id,
                                node_id: manifold_core::NodeId::new(group_input_handle.clone()),
                                type_id: GROUP_INPUT_TYPE_ID.to_string(),
                                handle: Some(group_input_handle),
                                params: BTreeMap::new(),
                                exposed_params: Default::default(),
                                editor_pos: None,
                                wgsl_source: None,
                                title: None,
                                output_formats: BTreeMap::new(),
                                output_canvas_scales: BTreeMap::new(),
                                group: None,
                            });
                            id
                        });
                    // group_input.instances → scene_object.instances (inside body).
                    if body.nodes.iter().any(|n| n.id == wiring.scene_object_node_id) {
                        body.wires.push(EffectGraphWire {
                            from_node: group_input_id,
                            from_port: "instances".to_string(),
                            to_node: wiring.scene_object_node_id,
                            to_port: "instances".to_string(),
                        });
                    }
                    // scene_array.out → group_node.instances (top level, via interface).
                    wires.push(EffectGraphWire {
                        from_node: scene_array_doc_id,
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
                    // Per-node metadata (INV-6: each node gets ONLY its own
                    // params — a shared union would stamp phantom rows for
                    // params the node doesn't have).
                    let node_meta = plan_ref
                        .node_metadata
                        .iter()
                        .find(|(nid, _)| nid.as_str() == node.node_id.as_str())
                        .map(|(_, m)| m.clone())
                        .unwrap_or_default();
                    stamp_scene_node_exposures_into(
                        &mut meta.params,
                        &mut meta.bindings,
                        node.id,
                        &node.node_id,
                        &node.type_id,
                        "Scene Loop",
                        &node_meta,
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
/// Restores the graph to its pre-loop state by inverting the apply plan:
/// deletes the minted loop nodes (by their stable `node_id`), drops the wires
/// touching them, restores the `camera.out → lens.camera` re-point the apply
/// removed, removes the per-group `instances` interface splices, and strips
/// the `Scene Loop` exposures from `preset_metadata`. Deterministic against
/// the CURRENT graph — the panel cannot reach the content thread's undo
/// history, so a passed snapshot would have to be stashed at apply time; the
/// inverse-of-plan is the same truth, re-derived (D5, "not undo and hope").
#[derive(Debug)]
pub struct RemoveSceneLoopCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    plan: SceneLoopPlan,
    /// Post-remove state for undo.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>)>,
}

impl RemoveSceneLoopCommand {
    pub fn new(target: GraphTarget, scope_path: Vec<u32>, plan: SceneLoopPlan) -> Self {
        Self {
            target,
            scope_path,
            plan,
            prev: None,
        }
    }
}

impl Command for RemoveSceneLoopCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let plan = self.plan.clone();
        let result = with_existing_target_graph_mut(project, &self.target, true, |def| {
            let prev_metadata = def.preset_metadata.clone();
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());

            // The minted loop nodes — matched by stable `node_id`, never by
            // numeric doc id (which the flattener renumbers).
            let loop_ids: std::collections::BTreeSet<u32> = plan
                .new_nodes
                .iter()
                .filter_map(|n| nodes.iter().find(|m| m.node_id == n.node_id).map(|m| m.id))
                .collect();

            // Drop wires touching any loop node (scene_array fans to every
            // group's instances input; loop_camera feeds lens.camera; beat_ramp
            // feeds loop_camera.phase; loop_fog feeds render.atmosphere).
            wires.retain(|w| {
                !loop_ids.contains(&w.from_node) && !loop_ids.contains(&w.to_node)
            });

            // Restore the camera re-point: the apply dropped `camera.out →
            // lens.camera` and added `loop_camera.out → lens.camera`. Re-wire
            // the non-loop camera producer into lens.camera (if a lens exists
            // and nothing else now drives it).
            if let Some(lens_id) = nodes
                .iter()
                .find(|n| n.type_id == "node.camera_lens")
                .map(|n| n.id)
            {
                let camera_source = nodes
                    .iter()
                    .find(|n| {
                        !loop_ids.contains(&n.id)
                            && matches!(
                                n.type_id.as_str(),
                                "node.orbit_camera" | "node.free_camera" | "node.look_at_camera"
                            )
                    })
                    .map(|n| n.id);
                if let Some(cam_id) = camera_source
                    && !wires
                        .iter()
                        .any(|w| w.to_node == lens_id && w.to_port == "camera")
                {
                    wires.push(EffectGraphWire {
                        from_node: cam_id,
                        from_port: "out".to_string(),
                        to_node: lens_id,
                        to_port: "camera".to_string(),
                    });
                }
            }

            // Drop the loop nodes.
            nodes.retain(|n| !loop_ids.contains(&n.id));

            // Remove the per-group instances interface splices the apply added.
            for node in nodes.iter_mut() {
                let Some(body) = node.group.as_deref_mut() else { continue };
                body.interface.inputs.retain(|p| p.name != "instances");
                let group_input_id = body
                    .nodes
                    .iter()
                    .find(|n| n.type_id == GROUP_INPUT_TYPE_ID)
                    .map(|n| n.id);
                body.wires.retain(|w| {
                    w.to_port != "instances" && w.from_port != "instances"
                });
                if let Some(gid) = group_input_id {
                    // Drop the group_input boundary node only if it carried no
                    // other interface port (a pre-existing group may use one).
                    let still_wired = body
                        .wires
                        .iter()
                        .any(|w| w.from_node == gid || w.to_node == gid);
                    if !still_wired {
                        body.nodes.retain(|n| n.id != gid);
                    }
                }
            }

            // Strip the "Scene Loop" exposures from preset_metadata.
            if let Some(meta) = def.preset_metadata.as_mut() {
                meta.params.retain(|p| p.section.as_deref() != Some("Scene Loop"));
                meta.bindings.retain(|b| match &b.target {
                    manifold_core::effect_graph_def::BindingTarget::Node {
                        node_id,
                        ..
                    } => !plan
                        .new_nodes
                        .iter()
                        .any(|n| n.node_id == *node_id),
                    _ => true,
                });
            }

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
