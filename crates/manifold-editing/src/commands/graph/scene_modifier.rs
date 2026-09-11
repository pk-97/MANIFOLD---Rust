//! Generic scene modifier apply/remove commands (SCENE_MODIFIER_FRAMEWORK_DESIGN
//! D1, section 3.3 seam brief).
//!
//! ONE command pair serves every modifier kind: the kind's renderer-side
//! descriptor builds a [`SceneModifierPlan`]; these commands apply and invert
//! it. Semantics byte-identical to the scene-loop pair they replace
//! (level-snapshot undo; apply = extend nodes/wires + repoint + splices +
//! stamp exposures; remove = drop by stable node_id, restore repoint, strip
//! splices, strip exposures, `refresh_target_manifest`).
//!
//! The remove prunes THREE layers, not two (K3 review major 1, the
//! BUG-6vv7 (scene-loop-remove-orphan-presetinstance-params) class): the
//! graph + `preset_metadata` are stripped as today, AND the instance's
//! manifest params + every modulation vec entry targeting the stripped
//! binding ids (the `ToggleEffectParamExposeCommand` ReverseState pattern,
//! `effects.rs`).

use manifold_core::GraphTarget;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_TYPE_ID,
    InterfacePortDef, PresetMetadata,
};
use manifold_core::project::Project;
use manifold_core::scene_exposure::stamp_scene_node_exposures_into;
use manifold_core::scene_modifier::SceneModifierPlan;

use std::collections::BTreeMap;

use crate::command::Command;

use super::{
    descend_level, refresh_target_manifest, with_existing_target_graph_mut, with_target_graph_mut,
};

// The plan travels as plain manifold_core data; the editing crate re-exports
// it (and its satellite types) so call sites stay on the `commands::graph::`
// path.
pub use manifold_core::scene_modifier::{
    EnablePlan, GroupSplice, MeshStageSplice, NodeExposure, PlanTraceNode, PortRepoint, ToggleDecl,
};

const MESH_STAGE_CURRENT_PORT: &str = "current";
const MESH_STAGE_REFERENCE_PORT: &str = "reference";
const MESH_STAGE_OUTPUT_PORT: &str = "vertices";
const MESH_STAGE_PORT_TYPE: &str = "Array(MeshVertex)";

fn descend_stable_level<'a>(
    nodes: &'a mut Vec<EffectGraphNode>,
    wires: &'a mut Vec<EffectGraphWire>,
    scope: &[manifold_core::NodeId],
) -> Option<(&'a mut Vec<EffectGraphNode>, &'a mut Vec<EffectGraphWire>)> {
    match scope.split_first() {
        None => Some((nodes, wires)),
        Some((node_id, rest)) => {
            if nodes.iter().filter(|node| &node.node_id == node_id).count() != 1 {
                return None;
            }
            let group = nodes.iter_mut().find(|node| &node.node_id == node_id)?;
            let body = group.group.as_deref_mut()?;
            descend_stable_level(&mut body.nodes, &mut body.wires, rest)
        }
    }
}

fn collect_stable_ids(
    nodes: &[EffectGraphNode],
    out: &mut std::collections::HashSet<manifold_core::NodeId>,
) -> bool {
    let mut unique = true;
    for node in nodes {
        if !node.node_id.is_empty() && !out.insert(node.node_id.clone()) {
            unique = false;
        }
        if let Some(body) = node.group.as_deref()
            && !collect_stable_ids(&body.nodes, out)
        {
            unique = false;
        }
    }
    unique
}

fn stage_shape_is_valid(stage: &EffectGraphNode) -> bool {
    if stage.type_id != GROUP_TYPE_ID || stage.node_id.is_empty() {
        return false;
    }
    let Some(body) = stage.group.as_deref() else {
        return false;
    };
    let mut doc_ids = std::collections::HashSet::new();
    if body.nodes.iter().any(|node| !doc_ids.insert(node.id))
        || body
            .wires
            .iter()
            .any(|wire| !doc_ids.contains(&wire.from_node) || !doc_ids.contains(&wire.to_node))
    {
        return false;
    }
    let current_ports: Vec<_> = body
        .interface
        .inputs
        .iter()
        .filter(|port| port.name == MESH_STAGE_CURRENT_PORT)
        .collect();
    let reference_ports: Vec<_> = body
        .interface
        .inputs
        .iter()
        .filter(|port| port.name == MESH_STAGE_REFERENCE_PORT)
        .collect();
    let output_ports: Vec<_> = body
        .interface
        .outputs
        .iter()
        .filter(|port| port.name == MESH_STAGE_OUTPUT_PORT)
        .collect();
    current_ports.len() == 1
        && current_ports[0].port_type == MESH_STAGE_PORT_TYPE
        && reference_ports.len() == 1
        && reference_ports[0].port_type == MESH_STAGE_PORT_TYPE
        && output_ports.len() == 1
        && output_ports[0].port_type == MESH_STAGE_PORT_TYPE
}

fn preflight_mesh_stage_splices(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    splices: &[MeshStageSplice],
    additions: &[EffectGraphNode],
) -> Result<(), &'static str> {
    if splices.is_empty() {
        return Ok(());
    }
    let mut simulated_nodes = nodes.to_vec();
    let mut simulated_wires = wires.to_vec();
    let mut stable_ids = std::collections::HashSet::new();
    collect_stable_ids(nodes, &mut stable_ids);
    for addition in additions {
        collect_stable_ids(std::slice::from_ref(addition), &mut stable_ids);
    }

    for splice in splices {
        if splice
            .scope_path
            .iter()
            .any(manifold_core::NodeId::is_empty)
            || splice.target_node_id.is_empty()
            || !stage_shape_is_valid(&splice.stage)
        {
            return Err("invalid mesh stage identity or shape");
        }
        let mut stage_ids = std::collections::HashSet::new();
        if !collect_stable_ids(std::slice::from_ref(&splice.stage), &mut stage_ids) {
            return Err("duplicate stable node id inside mesh stage");
        }
        if stage_ids.iter().any(|id| stable_ids.contains(id)) {
            return Err("mesh stage stable node id already exists");
        }
        stable_ids.extend(stage_ids);

        let (level_nodes, level_wires) = descend_stable_level(
            &mut simulated_nodes,
            &mut simulated_wires,
            &splice.scope_path,
        )
        .ok_or("mesh stage scope does not resolve")?;
        let targets: Vec<_> = level_nodes
            .iter()
            .filter(|node| node.node_id == splice.target_node_id)
            .collect();
        if targets.len() != 1 || targets[0].type_id != "node.scene_object" {
            return Err("mesh stage target scene object is ambiguous or invalid");
        }
        let target_doc_id = targets[0].id;
        let producers: Vec<_> = level_wires
            .iter()
            .filter(|wire| wire.to_node == target_doc_id && wire.to_port == MESH_STAGE_OUTPUT_PORT)
            .collect();
        if producers.len() != 1 {
            return Err("scene object must have exactly one vertices producer");
        }
        let (current_from_node, current_from_port) =
            (producers[0].from_node, producers[0].from_port.clone());
        let (reference_node_id, reference_port) = &splice.reference_source;
        let reference_nodes: Vec<_> = level_nodes
            .iter()
            .filter(|node| &node.node_id == reference_node_id)
            .collect();
        if reference_nodes.len() != 1 {
            return Err("mesh stage reference source is ambiguous");
        }
        let reference_doc_id = reference_nodes[0].id;
        let reference_is_structural = level_wires
            .iter()
            .any(|wire| wire.from_node == reference_doc_id && wire.from_port == *reference_port)
            || (current_from_node == reference_doc_id && current_from_port == *reference_port);
        if !reference_is_structural {
            return Err("mesh stage reference source port is not structural");
        }
        if level_nodes.iter().any(|node| node.id == splice.stage.id) {
            return Err("mesh stage document id already exists");
        }

        let stage_id = splice.stage.id;
        let target_wire_idx = level_wires
            .iter()
            .position(|wire| {
                wire.to_node == target_doc_id && wire.to_port == MESH_STAGE_OUTPUT_PORT
            })
            .expect("mesh stage preflight guarantees target wire");
        level_nodes.push(splice.stage.clone());
        level_wires.splice(
            target_wire_idx..=target_wire_idx,
            [
                EffectGraphWire {
                    from_node: current_from_node,
                    from_port: current_from_port,
                    to_node: stage_id,
                    to_port: MESH_STAGE_CURRENT_PORT.to_string(),
                },
                EffectGraphWire {
                    from_node: reference_doc_id,
                    from_port: reference_port.clone(),
                    to_node: stage_id,
                    to_port: MESH_STAGE_REFERENCE_PORT.to_string(),
                },
                EffectGraphWire {
                    from_node: stage_id,
                    from_port: MESH_STAGE_OUTPUT_PORT.to_string(),
                    to_node: target_doc_id,
                    to_port: MESH_STAGE_OUTPUT_PORT.to_string(),
                },
            ],
        );
    }
    Ok(())
}

fn remove_mesh_stage_from_level(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    splice: &MeshStageSplice,
) {
    let Some(stage) = nodes
        .iter()
        .find(|node| node.node_id == splice.stage.node_id)
    else {
        return;
    };
    let stage_doc_id = stage.id;
    let Some((source_node, source_port)) = wires
        .iter()
        .find(|wire| wire.to_node == stage_doc_id && wire.to_port == MESH_STAGE_CURRENT_PORT)
        .map(|wire| (wire.from_node, wire.from_port.clone()))
    else {
        return;
    };
    let mut restored = Vec::with_capacity(wires.len());
    for wire in wires.drain(..) {
        if wire.to_node == stage_doc_id {
            continue;
        }
        if wire.from_node == stage_doc_id {
            if wire.from_port == MESH_STAGE_OUTPUT_PORT {
                let replacement = EffectGraphWire {
                    from_node: source_node,
                    from_port: source_port.clone(),
                    to_node: wire.to_node,
                    to_port: wire.to_port,
                };
                if !restored.iter().any(|existing: &EffectGraphWire| {
                    existing.from_node == replacement.from_node
                        && existing.from_port == replacement.from_port
                        && existing.to_node == replacement.to_node
                        && existing.to_port == replacement.to_port
                }) {
                    restored.push(replacement);
                }
            }
            continue;
        }
        restored.push(wire);
    }
    *wires = restored;
    nodes.retain(|node| node.id != stage_doc_id);
}

fn apply_mesh_stage_splices(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    splices: &[MeshStageSplice],
) {
    for splice in splices {
        let Some((level_nodes, level_wires)) =
            descend_stable_level(nodes, wires, &splice.scope_path)
        else {
            continue;
        };
        let Some(target_doc_id) = level_nodes
            .iter()
            .find(|node| node.node_id == splice.target_node_id)
            .map(|node| node.id)
        else {
            continue;
        };
        let Some(current) = level_wires
            .iter()
            .find(|wire| wire.to_node == target_doc_id && wire.to_port == MESH_STAGE_OUTPUT_PORT)
            .map(|wire| (wire.from_node, wire.from_port.clone()))
        else {
            continue;
        };
        let Some(reference_doc_id) = level_nodes
            .iter()
            .find(|node| node.node_id == splice.reference_source.0)
            .map(|node| node.id)
        else {
            continue;
        };
        let stage_doc_id = splice.stage.id;
        let target_wire_idx = level_wires
            .iter()
            .position(|wire| {
                wire.to_node == target_doc_id && wire.to_port == MESH_STAGE_OUTPUT_PORT
            })
            .expect("mesh stage preflight guarantees target wire");
        level_nodes.push(splice.stage.clone());
        level_wires.splice(
            target_wire_idx..=target_wire_idx,
            [
                EffectGraphWire {
                    from_node: current.0,
                    from_port: current.1,
                    to_node: stage_doc_id,
                    to_port: MESH_STAGE_CURRENT_PORT.to_string(),
                },
                EffectGraphWire {
                    from_node: reference_doc_id,
                    from_port: splice.reference_source.1.clone(),
                    to_node: stage_doc_id,
                    to_port: MESH_STAGE_REFERENCE_PORT.to_string(),
                },
                EffectGraphWire {
                    from_node: stage_doc_id,
                    from_port: MESH_STAGE_OUTPUT_PORT.to_string(),
                    to_node: target_doc_id,
                    to_port: MESH_STAGE_OUTPUT_PORT.to_string(),
                },
            ],
        );
    }
}

/// The instance layer the three-layer remove prunes: the live param
/// manifest plus every modulation vec that can target a param id. Captured
/// whole before pruning (the same level-snapshot pattern the graph layer
/// uses) so undo restores the pre-remove state verbatim.
#[derive(Debug, Clone)]
struct InstanceLayerSnapshot {
    params: manifold_core::params::ParamManifest,
    drivers: Option<Vec<manifold_core::effects::ParameterDriver>>,
    envelopes: Option<Vec<manifold_core::effects::ParamEnvelope>>,
    ableton_mappings: Option<Vec<manifold_core::ableton_mapping::AbletonParamMapping>>,
    audio_mods: Option<Vec<manifold_core::audio_mod::ParameterAudioMod>>,
    automation_lanes: Option<Vec<manifold_core::effects::AutomationLane>>,
}

impl InstanceLayerSnapshot {
    fn capture(instance: &manifold_core::effects::PresetInstance) -> Self {
        Self {
            params: instance.params.clone(),
            drivers: instance.drivers.clone(),
            envelopes: instance.envelopes.clone(),
            ableton_mappings: instance.ableton_mappings.clone(),
            audio_mods: instance.audio_mods.clone(),
            automation_lanes: instance.automation_lanes.clone(),
        }
    }

    fn restore(self, instance: &mut manifold_core::effects::PresetInstance) {
        instance.params = self.params;
        instance.drivers = self.drivers;
        instance.envelopes = self.envelopes;
        instance.ableton_mappings = self.ableton_mappings;
        instance.audio_mods = self.audio_mods;
        instance.automation_lanes = self.automation_lanes;
    }
}

/// "Apply Scene Modifier" — splice one modifier kind's plan into the scene
/// graph.
///
/// One undo unit. Refuses (logged, no mutation) when:
/// - INV-M6: the level doesn't carry exactly one `node.render_scene`;
/// - INV-M9: the plan's trace finds SOME but not all required nodes — a
///   partial/broken modifier must be removed before re-applying, never
///   re-spliced over (that would duplicate stable nodeIds);
/// - the trace finds the modifier fully applied already (a second apply
///   would duplicate every minted nodeId).
#[derive(Debug)]
pub struct ApplySceneModifierCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    plan: SceneModifierPlan,
    catalog_default: EffectGraphDef,
    /// Pre-edit `(nodes, wires)` at `scope_path`, plus pre-edit
    /// `preset_metadata`. Set on execute.
    prev: Option<(
        Vec<EffectGraphNode>,
        Vec<EffectGraphWire>,
        Option<PresetMetadata>,
    )>,
}

impl ApplySceneModifierCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        plan: SceneModifierPlan,
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

/// The interface input port type a splice mints, keyed by the inner port
/// name. v1 kinds only splice `instances`; a future kind splicing another
/// port extends this table — the plan's [`GroupSplice`] deliberately
/// carries no type string (editing cannot read primitive manifests).
fn splice_interface_port_type(inner_port: &str) -> Option<&'static str> {
    match inner_port {
        "instances" => Some("Array(InstanceTransform)"),
        other => {
            eprintln!(
                "ApplySceneModifierCommand: no interface port type known for splice port {other:?} — splice skipped"
            );
            None
        }
    }
}

/// Apply one group splice to the descended (nodes, wires). Section 3.5 of
/// SCENE_MIRROR_DESIGN splits the old conflated behaviour:
///  (a) interface input + group_input boundary wiring: added only when
///      missing (unchanged);
///  (b) the top-level wire: with `replace_existing` every other producer of
///      (group, inner_port) is dropped first, then the source wire lands;
///      without it the caller's pre-flight already refused (INV-MR8), so
///      reaching here means the port was free.
fn apply_group_splice(
    nodes: &mut [EffectGraphNode],
    wires: &mut Vec<EffectGraphWire>,
    splice: &GroupSplice,
    next_group_input_id: &mut u32,
) {
    let Some(port_type) = splice_interface_port_type(splice.inner_port) else {
        return;
    };
    let Some(group_idx) = nodes.iter().position(|n| n.id == splice.group_node_id) else {
        return;
    };
    let Some(body) = nodes[group_idx].group.as_deref_mut() else {
        return;
    };
    if !body
        .interface
        .inputs
        .iter()
        .any(|p| p.name == splice.inner_port)
    {
        body.interface.inputs.push(InterfacePortDef {
            name: splice.inner_port.to_string(),
            port_type: port_type.to_string(),
        });
        // A group_input boundary node carrying the spliced port (object
        // groups have none today; the AO group's precedent names it after
        // the group).
        let group_input_handle = "loop_in".to_string();
        let group_input_id = body
            .nodes
            .iter()
            .find(|n| n.type_id == GROUP_INPUT_TYPE_ID)
            .map(|n| n.id)
            .unwrap_or_else(|| {
                // Reserve a fresh body-local id that can't collide with the
                // top-level minted ids.
                let id = *next_group_input_id;
                *next_group_input_id += 1;
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
        // group_input.<inner_port> → <inner_node>.<inner_port> (inside body).
        let inner_target = body
            .nodes
            .iter()
            .find(|n| n.type_id == splice.inner_node_type)
            .map(|n| n.id);
        if let Some(inner) = inner_target {
            body.wires.push(EffectGraphWire {
                from_node: group_input_id,
                from_port: splice.inner_port.to_string(),
                to_node: inner,
                to_port: splice.inner_port.to_string(),
            });
        }
    }
    if splice.replace_existing {
        // Take-over (D6): drop every other producer of the port. The
        // source's own wire survives so a re-asserted splice can't
        // duplicate it.
        wires.retain(|w| {
            w.to_node != splice.group_node_id
                || w.to_port != splice.inner_port
                || w.from_node == splice.source_doc_id
        });
    }
    // <source>.<source_port> → group.<inner_port> (top level, via the
    // interface).
    if !wires.iter().any(|w| {
        w.from_node == splice.source_doc_id
            && w.from_port == splice.source_port
            && w.to_node == splice.group_node_id
            && w.to_port == splice.inner_port
    }) {
        wires.push(EffectGraphWire {
            from_node: splice.source_doc_id,
            from_port: splice.source_port.clone(),
            to_node: splice.group_node_id,
            to_port: splice.inner_port.to_string(),
        });
    }
}

impl Command for ApplySceneModifierCommand {
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

                // INV-M6: exactly one render_scene (carried from INV-1).
                let scene_count = nodes
                    .iter()
                    .filter(|n| n.type_id == "node.render_scene")
                    .count();
                if scene_count != 1 {
                    eprintln!(
                        "ApplySceneModifierCommand ({}): INV-M6 violation — expected 1 render_scene, found {scene_count}",
                        plan.kind_id
                    );
                    return None;
                }

                // INV-M9: refuse a PARTIAL trace — any required trace node
                // present but not all (hand-edit debris). Also refuse a
                // fully-applied graph; either way applying would re-mint
                // stable nodeIds the surviving nodes already carry.
                let required = plan.trace.iter().filter(|t| t.required);
                let present = required
                    .clone()
                    .filter(|t| {
                        nodes
                            .iter()
                            .any(|n| n.type_id == t.type_id && n.node_id.as_str() == t.node_id)
                    })
                    .count();
                let total = required.count();
                if present == total && total > 0 {
                    eprintln!(
                        "ApplySceneModifierCommand ({}): modifier already applied — remove it before re-applying",
                        plan.kind_id
                    );
                    return None;
                }
                if present > 0 {
                    eprintln!(
                        "ApplySceneModifierCommand ({}): partial trace ({present}/{total} required nodes) — remove the broken modifier first",
                        plan.kind_id
                    );
                    return None;
                }

                // INV-MR8 pre-flight (SCENE_MIRROR_DESIGN section 3.5): a
                // splice WITHOUT take-over onto an already-spliced port
                // refuses the WHOLE apply — the old silent skip left the
                // new kind believing it owned a port it never wired. Runs
                // before any mutation so a refusal is a true no-op.
                for splice in plan.group_splices.iter().filter(|s| !s.replace_existing) {
                    let port_taken = nodes
                        .iter()
                        .find(|n| n.id == splice.group_node_id)
                        .and_then(|n| n.group.as_ref())
                        .map(|b| {
                            b.interface
                                .inputs
                                .iter()
                                .any(|p| p.name == splice.inner_port)
                        })
                        .unwrap_or(false);
                    if port_taken {
                        eprintln!(
                            "ApplySceneModifierCommand ({}): group {} port {:?} already has a splice owner — build the plan with replace_existing to take over (INV-MR8)",
                            plan.kind_id, splice.group_node_id, splice.inner_port
                        );
                        return None;
                    }
                }

                // Mesh-stage validation runs against a clone and covers the
                // complete batch before any graph mutation. This keeps a
                // broken nested splice a true no-op, including metadata.
                let mut plan_additions = plan.new_nodes.clone();
                plan_additions.extend(plan.enable.extra_nodes.iter().cloned());
                if let Err(reason) =
                    preflight_mesh_stage_splices(nodes, wires, &plan.mesh_stages, &plan_additions)
                {
                    eprintln!(
                        "ApplySceneModifierCommand ({}): invalid mesh stage plan — {reason}",
                        plan.kind_id
                    );
                    return None;
                }

                // Add the modifier's nodes (atoms + enable wiring extras)
                // and wires.
                nodes.extend(plan.new_nodes.iter().cloned());
                nodes.extend(plan.enable.extra_nodes.iter().cloned());
                wires.extend(plan.new_wires.iter().cloned());
                wires.extend(plan.enable.extra_wires.iter().cloned());

                // Re-points: drop every OTHER producer feeding the taken-over
                // port, keeping the plan's own wire (the panel's trace walks
                // the producer and would report the first wire — the
                // dead-silent orbit-vs-loop trap).
                for repoint in &plan.repoints {
                    wires.retain(|w| {
                        w.to_node != repoint.target_node_id
                            || w.to_port != repoint.target_port
                            || w.from_node == repoint.new_producer_doc_id
                    });
                }

                // Per-group interface splices (scope_path = [group_node_id]):
                //
                //  - add an `inner_port` input to the group's interface,
                //  - add a `system.group_input` node to the body (object
                //    groups currently carry none) and wire
                //    `group_input.<inner_port> → <inner_node>.<inner_port>`
                //    inside,
                //  - at top level, wire
                //    `<source>.<source_port> → group.<inner_port>`.
                //
                // The flattener resolves the top-level wire through the
                // group's interface inputs and the body wire via
                // `group_input`, so the inner port is driven without a
                // cross-boundary wire (which the flattener rejects).
                let mut next_group_input_id = 1_000_000u32;
                for splice in &plan.group_splices {
                    apply_group_splice(nodes, wires, splice, &mut next_group_input_id);
                }
                apply_mesh_stage_splices(nodes, wires, &plan.mesh_stages);

                Some((prev_nodes_wires, prev_metadata))
            },
        );
        let Some((pnw, pmeta)) = result.flatten() else {
            return;
        };
        self.prev = Some((pnw.0, pnw.1, pmeta));

        // Stamp exposures for the curated nodes (INV-6: each node gets ONLY
        // its own params — the plan's NodeExposure entries are per-node by
        // construction). Section = the kind's display name.
        let plan_ref = &self.plan;
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some(ref mut meta) = def.preset_metadata {
                for exposure in &plan_ref.exposures {
                    stamp_scene_node_exposures_into(
                        &mut meta.params,
                        &mut meta.bindings,
                        exposure.node_doc_id,
                        &exposure.node_id,
                        &exposure.type_id,
                        &plan_ref.display_name,
                        &exposure.metadata,
                        &exposure.params,
                    );
                }
                manifold_core::scene_modifier::install_shared_param_bindings(
                    &mut meta.bindings,
                    &plan_ref.shared_params,
                );
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
        "Apply Scene Modifier"
    }
}

/// "Remove Scene Modifier" — symmetric removal (not "undo and hope").
///
/// Restores the graph by inverting the apply plan: deletes the minted nodes
/// (by their stable `node_id`), drops the wires touching them, restores the
/// port re-points the apply displaced, removes the per-group interface
/// splices, strips the kind's exposures from `preset_metadata`, and prunes
/// the instance layer — manifest params and every modulation entry
/// (drivers / envelopes / Ableton mappings / audio mods / automation lanes)
/// targeting the stripped binding ids. That third layer is the general fix
/// for the orphan-param class BUG-6vv7 (scene-loop-remove-orphan-presetinstance-params):
/// `refresh_manifest_from_graph` never prunes, so without it the removed
/// modifier's rows linger as dead manifest params. Deterministic against the
/// CURRENT graph — the inverse-of-plan is the same truth, re-derived.
#[derive(Debug)]
pub struct RemoveSceneModifierCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    plan: SceneModifierPlan,
    /// Post-remove state for undo: the graph layer snapshot plus the
    /// instance layer snapshot captured before the prune.
    prev: Option<RemovePrev>,
}

#[derive(Debug, Clone)]
struct RemovePrev {
    nodes: Vec<EffectGraphNode>,
    wires: Vec<EffectGraphWire>,
    metadata: Option<PresetMetadata>,
    instance: Option<InstanceLayerSnapshot>,
}

impl RemoveSceneModifierCommand {
    pub fn new(target: GraphTarget, scope_path: Vec<u32>, plan: SceneModifierPlan) -> Self {
        Self {
            target,
            scope_path,
            plan,
            prev: None,
        }
    }
}

impl Command for RemoveSceneModifierCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let plan = self.plan.clone();
        let mut stripped_binding_ids: Vec<String> = Vec::new();
        let result = with_existing_target_graph_mut(project, &self.target, true, |def| {
            let prev_metadata = def.preset_metadata.clone();
            let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
            let prev = (nodes.clone(), wires.clone());

            // Mesh stages are addressed by stable nested group paths. Remove
            // each stage by its current stable identity and fan its current
            // producer back out to every consumer, including a later stage.
            // This happens before the generic minted-node cleanup so the
            // reconstructed source wires remain intact.
            for splice in &plan.mesh_stages {
                if let Some((level_nodes, level_wires)) =
                    descend_stable_level(nodes, wires, &splice.scope_path)
                {
                    remove_mesh_stage_from_level(level_nodes, level_wires, splice);
                }
            }

            // The minted modifier nodes — matched by stable `node_id`, never
            // by numeric doc id (which the flattener renumbers).
            let minted: std::collections::HashSet<manifold_core::NodeId> =
                plan.minted_node_ids().into_iter().collect();
            let minted_doc_ids: std::collections::BTreeSet<u32> = nodes
                .iter()
                .filter(|n| minted.contains(&n.node_id))
                .map(|n| n.id)
                .collect();

            // Capture the binding ids about to be stripped (layer 3 prunes
            // by them) BEFORE the metadata strip removes them.
            if let Some(meta) = def.preset_metadata.as_ref() {
                stripped_binding_ids = meta
                    .bindings
                    .iter()
                    .filter(|b| match &b.target {
                        manifold_core::effect_graph_def::BindingTarget::Node {
                            node_id, ..
                        } => minted.contains(node_id),
                        _ => false,
                    })
                    .map(|b| b.id.clone())
                    .collect();
            }

            // Drop wires touching any minted node (the enable switch fans
            // to the lens; the atoms fan to each other and the groups).
            wires.retain(|w| {
                !minted_doc_ids.contains(&w.from_node) && !minted_doc_ids.contains(&w.to_node)
            });

            // Restore the port re-points: the apply dropped the displaced
            // producers of each taken-over port; re-wire the first non-mine
            // node of a restore type back in when the port is left unwired.
            for repoint in &plan.repoints {
                if !nodes.iter().any(|n| n.id == repoint.target_node_id) {
                    continue;
                }
                let still_wired = wires.iter().any(|w| {
                    w.to_node == repoint.target_node_id && w.to_port == repoint.target_port
                });
                if still_wired {
                    continue;
                }
                let restore_source = nodes
                    .iter()
                    .find(|n| {
                        !minted_doc_ids.contains(&n.id)
                            && repoint.restore_types.contains(&n.type_id.as_str())
                    })
                    .map(|n| n.id);
                if let Some(src) = restore_source {
                    wires.push(EffectGraphWire {
                        from_node: src,
                        from_port: "out".to_string(),
                        to_node: repoint.target_node_id,
                        to_port: repoint.target_port.clone(),
                    });
                }
            }

            // Drop the minted nodes.
            nodes.retain(|n| !minted_doc_ids.contains(&n.id));

            // Remove the per-group interface splices the apply added.
            for splice in &plan.group_splices {
                let Some(group_idx) = nodes.iter().position(|n| n.id == splice.group_node_id)
                else {
                    continue;
                };
                let group = &mut nodes[group_idx];
                let Some(body) = group.group.as_deref_mut() else {
                    continue;
                };
                body.interface
                    .inputs
                    .retain(|p| p.name != splice.inner_port);
                let group_input_id = body
                    .nodes
                    .iter()
                    .find(|n| n.type_id == GROUP_INPUT_TYPE_ID)
                    .map(|n| n.id);
                body.wires
                    .retain(|w| w.to_port != splice.inner_port && w.from_port != splice.inner_port);
                if let Some(gid) = group_input_id {
                    // Drop the group_input boundary node only if it carried
                    // no other interface port (a pre-existing group may use
                    // one).
                    let still_wired = body
                        .wires
                        .iter()
                        .any(|w| w.from_node == gid || w.to_node == gid);
                    if !still_wired {
                        body.nodes.retain(|n| n.id != gid);
                    }
                }
            }

            // Strip the kind's exposures from preset_metadata.
            if let Some(meta) = def.preset_metadata.as_mut() {
                meta.params
                    .retain(|p| p.section.as_deref() != Some(plan.display_name.as_str()));
                meta.bindings.retain(|b| match &b.target {
                    manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } => {
                        !minted.contains(node_id)
                    }
                    _ => true,
                });
            }

            Some((prev, prev_metadata))
        });
        let Some(((pn, pw), pmeta)) = result.flatten() else {
            return;
        };

        // Layer 3: prune the instance's manifest + modulation vecs. Captured
        // whole first so undo restores the pre-remove state verbatim. Runs
        // BEFORE refresh_target_manifest: refresh round-trips the current
        // manifest through the wire encode, so entries still present at
        // refresh time get re-seeded (that re-push is exactly how the
        // orphan class survived on main).
        let prev_instance = project.with_preset_graph_mut(&self.target, |instance| {
            let snapshot = InstanceLayerSnapshot::capture(instance);
            if !stripped_binding_ids.is_empty() {
                let ids: std::collections::BTreeSet<&str> =
                    stripped_binding_ids.iter().map(|s| s.as_str()).collect();
                for id in &ids {
                    instance.params.remove(id);
                }
                prune_by_param_id(&mut instance.drivers, &ids, |d| &*d.param_id);
                prune_by_param_id(&mut instance.envelopes, &ids, |e| &*e.param_id);
                prune_by_param_id(&mut instance.ableton_mappings, &ids, |m| &*m.param_id);
                prune_by_param_id(&mut instance.audio_mods, &ids, |a| &*a.param_id);
                prune_by_param_id(&mut instance.automation_lanes, &ids, |l| &*l.param_id);
            }
            snapshot
        });

        self.prev = Some(RemovePrev {
            nodes: pn,
            wires: pw,
            metadata: pmeta,
            instance: prev_instance,
        });
        refresh_target_manifest(project, &self.target);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.preset_metadata = prev.metadata.clone();
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = prev.nodes.clone();
                *wires = prev.wires.clone();
            }
        });
        if let Some(snapshot) = prev.instance {
            let _ = project.with_preset_graph_mut(&self.target, |instance| {
                snapshot.restore(instance);
            });
        }
        refresh_target_manifest(project, &self.target);
    }

    fn description(&self) -> &str {
        "Remove Scene Modifier"
    }
}

/// Drop every entry of `vec` whose param id is in `ids` (the
/// `ToggleEffectParamExposeCommand` prune, minus the capture — the remove
/// command snapshots the whole vec for undo, so entries are dropped, not
/// harvested). `None` stays `None`; an emptied `Some` collapses to `None`.
fn prune_by_param_id<T>(
    vec: &mut Option<Vec<T>>,
    ids: &std::collections::BTreeSet<&str>,
    param_id: impl Fn(&T) -> &str,
) {
    if let Some(entries) = vec.as_mut() {
        entries.retain(|e| !ids.contains(param_id(e)));
        if entries.is_empty() {
            *vec = None;
        }
    }
}
