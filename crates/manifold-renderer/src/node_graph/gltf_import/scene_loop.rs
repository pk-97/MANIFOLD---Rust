//! SCENE_LOOP_DESIGN D5: the renderer-side Scene Loop plan builder.
//!
//! The loop's apply plan (nodes, wires, per-group instance splices) is built
//! HERE, against the layer's CURRENT `EffectGraphDef` — the same split the
//! import merge uses (`assemble_merge_plan` builds plain `manifold_core`
//! fields; the editing crate's `ImportModelIntoSceneCommand` /
//! `ApplySceneLoopCommand` apply them). `manifold-renderer` can read the
//! primitive manifests (`metadata_for_node_type`) that the exposure stamping
//! needs, which `manifold-editing` cannot depend on.

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::scene_loop::{InstanceWiring, SceneLoopPlan};

use crate::node_graph::scene_exposure::metadata_for_node_type;

/// Build the Scene Loop apply plan for `def`'s scene rooted at
/// `render_scene_node_id`. Returns `None` when the graph isn't a single-scene
/// import the loop can splice (no render_scene, no lens/camera fallback target,
/// no object groups, or failed INV-1 multi-scene check — the command re-checks
/// INV-1 at execute time too).
///
/// Cell size (D4): TWO× the extent along +Z of `PresetMetadata.scene_bounds`
/// (one object-depth of solid + one of air — the BUG-70wo gap rule), or a
/// 10.0 fallback when the import stamped no bounds. Same value feeds
/// scene_array and loop_camera — camera travel per loop equals instance
/// spacing by construction.
pub fn assemble_scene_loop_plan(
    def: &EffectGraphDef,
    render_scene_node_id: u32,
) -> Option<SceneLoopPlan> {
    // Confirm the layer actually carries a scene.
    def.nodes.iter().find(|n| n.id == render_scene_node_id)?;

    // D4: cell_size from scene_bounds (Z extent), 10.0 fallback.
    //
    // Gap rule (BUG-70wo): the cell is TWO object-depths — one depth of solid,
    // one depth of open air. cell == extent packs copies face-to-face, so the
    // camera path never leaves the bounding box: for any solid asset (a tree,
    // a rock — anything but a hollow set) every frame renders from inside the
    // mesh and the loop is uniformly black. With a gap the loop reads as
    // approach → through → emerge → next copy ahead, and phase 0 sits
    // mid-gap looking at the next copy (still wrap-pure: travel per loop is
    // one cell by construction).
    let bounds = def.preset_metadata.as_ref().and_then(|m| m.scene_bounds);
    let axis_extent = bounds.map(|(min, max)| (max[2] - min[2]).abs()).unwrap_or(0.0);
    let cell_size = if axis_extent > 0.0 { axis_extent * 2.0 } else { 10.0 };

    // The lens (import spine: camera → lens → render + ao/dof/mb). The loop
    // camera re-points INTO lens.camera so every downstream consumer follows
    // (D5). Falls back to render_scene.camera when no lens exists (the
    // minimal hand-built scene shape).
    let lens_node = def.nodes.iter().find(|n| n.type_id == "node.camera_lens");
    let camera_repoint_to = lens_node.map(|n| n.id).unwrap_or(render_scene_node_id);

    // Object groups = producers wired into render_scene's object_k / mesh_k.
    // The `objects` param is a stale hint in sync with the wires; the WIRES are
    // the truth (a merge adds groups before the command re-syncs the param).
    // Each group's `node.scene_object` (object_k_bind) is found INSIDE its body.
    let mut instance_wirings = Vec::new();
    for w in &def.wires {
        if w.to_node != render_scene_node_id {
            continue;
        }
        if !(w.to_port.starts_with("object_") || w.to_port.starts_with("mesh_")) {
            continue;
        }
        let Some(group) = def.nodes.iter().find(|n| n.id == w.from_node && n.group.is_some()) else {
            continue;
        };
        let Some(body) = group.group.as_ref() else { continue };
        let scene_object_id = body
            .nodes
            .iter()
            .find(|n| n.type_id == "node.scene_object")
            .map(|n| n.id)
            .unwrap_or(0);
        instance_wirings.push(InstanceWiring {
            group_node_id: group.id,
            scene_object_node_id: scene_object_id,
        });
    }

    // Fresh top-level doc ids. (Group-body ids for a group_input the apply
    // mints live in a high spare range the command owns.)
    let max_id = def.nodes.iter().map(|n| n.id).max().unwrap_or(0);
    let beat_ramp_id = max_id + 1;
    let scene_array_id = max_id + 2;
    let loop_camera_id = max_id + 3;

    // D7 (P4): apply mints EXACTLY three nodes — loop_phase, scene_array,
    // loop_camera. Fog is never minted: the D4 gap rule makes every copy
    // self-contained, so there is no seam to hide, and the auto-minted
    // atmosphere + driver was unrequested complexity. A scene's own
    // atmosphere, if it has one, is left alone.

    let mut new_nodes = Vec::new();
    let mut new_wires = Vec::new();

    // beat_ramp (loop_phase): bars = 8 (D10 default) — with bars > 0 the ramp
    // runs at 1/bars cycles/beat, so the panel's Bars row reads and writes
    // bars directly (rate = 1/bars by construction, D6). attack = 1.0 makes
    // the output exactly the 0..1 loop phase (D3). rate stays 0.0: it is the
    // disabled fallback, so bars = 0 (the wrap-debug park) freezes the phase
    // at 0 instead of resurrecting a stale rate.
    let bars = 8.0_f32; // D10 default
    new_nodes.push(EffectGraphNode {
        id: beat_ramp_id,
        node_id: manifold_core::NodeId::new("loop_phase"),
        type_id: "node.beat_ramp".to_string(),
        handle: Some("loop_phase".to_string()),
        params: {
            let mut p = std::collections::BTreeMap::new();
            p.insert("bars".to_string(), SerializedParamValue::Float { value: bars });
            p.insert("rate".to_string(), SerializedParamValue::Float { value: 0.0 });
            p.insert("attack".to_string(), SerializedParamValue::Float { value: 1.0 });
            p
        },
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: Default::default(),
        output_canvas_scales: Default::default(),
        group: None,
    });

    // scene_array: the shared copy array (D2/D10) — count 3, axis +Z default.
    new_nodes.push(EffectGraphNode {
        id: scene_array_id,
        node_id: manifold_core::NodeId::new("scene_array"),
        type_id: "node.scene_array".to_string(),
        handle: Some("scene_array".to_string()),
        params: {
            let mut p = std::collections::BTreeMap::new();
            p.insert("count".to_string(), SerializedParamValue::Float { value: 3.0 });
            p.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 }); // +Z
            p.insert("cell_size".to_string(), SerializedParamValue::Float { value: cell_size });
            p
        },
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: Default::default(),
        output_canvas_scales: Default::default(),
        group: None,
    });

    // loop_camera: flies one cell per loop. home = -cell/2 = mid-gap before
    // copy 0 (with the D4 gap rule, -cell/2 is one half-depth of air in front
    // of the copy's near face, not on it): the phase-0 frame is an approach
    // shot of copy 0, period-identical to the phase-1 frame of copy 1.
    // Phase 0 == phase 1 by construction (D4 wrap purity). fov_y (not fov)
    // matches the manifest.
    new_nodes.push(EffectGraphNode {
        id: loop_camera_id,
        node_id: manifold_core::NodeId::new("loop_camera"),
        type_id: "node.loop_camera".to_string(),
        handle: Some("loop_camera".to_string()),
        params: {
            let mut p = std::collections::BTreeMap::new();
            p.insert("cell_size".to_string(), SerializedParamValue::Float { value: cell_size });
            p.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 }); // +Z — must match scene_array
            p.insert("home".to_string(), SerializedParamValue::Float { value: -cell_size * 0.5 });
            p.insert("lateral".to_string(), SerializedParamValue::Float { value: 0.0 });
            // Scale-aware framing (BUG-j65u (camera defaults fly over
            // sub-meter scenes)): height/near/far derive from the cell, never
            // room-scale constants — the primitive's 1.5/0.05/200 defaults put
            // the camera ten tree-heights above a sub-meter photoscan (black
            // at every phase) and near-clip a third of the way into the cell.
            p.insert("height".to_string(), SerializedParamValue::Float { value: 0.0 });
            p.insert("near".to_string(), SerializedParamValue::Float { value: (cell_size * 0.002).max(1e-4) });
            p.insert("far".to_string(), SerializedParamValue::Float { value: cell_size * 4.0 });
            p.insert("fov_y".to_string(), SerializedParamValue::Float { value: 0.9 });
            p
        },
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: Default::default(),
        output_canvas_scales: Default::default(),
        group: None,
    });

    // Wires: beat_ramp.out → loop_camera.phase ; loop_camera.out → lens.camera
    // (the D5 re-point).
    new_wires.push(EffectGraphWire {
        from_node: beat_ramp_id,
        from_port: "out".to_string(),
        to_node: loop_camera_id,
        to_port: "phase".to_string(),
    });
    new_wires.push(EffectGraphWire {
        from_node: loop_camera_id,
        from_port: "out".to_string(),
        to_node: camera_repoint_to,
        to_port: "camera".to_string(),
    });

    // Per-node exposure metadata — each node's REAL primitive manifest
    // (INV-6: never a shared union across nodes), curated to the D6 P4
    // performer whitelist. Stamping every param shipped the atoms' internals
    // (duplicate Axis/Cell Size rows, Home, Near/Far) and desynced the panel
    // from the loop — the whitelist is exactly Bars, Copies, Height, Lateral.
    let mut node_metadata = Vec::new();
    for node in &new_nodes {
        let manifest: Vec<_> = metadata_for_node_type(&node.type_id)
            .into_iter()
            .filter_map(|m| loop_row_label(&node.node_id, &m.name).map(|label| {
                let mut m = m;
                m.label = label.to_string();
                m
            }))
            .collect();
        if !manifest.is_empty() {
            node_metadata.push((node.node_id.clone(), manifest));
        }
    }

    Some(SceneLoopPlan {
        new_nodes,
        new_wires,
        instance_wirings,
        render_scene_node_id,
        node_metadata,
        loop_camera_node_id: manifold_core::NodeId::new("loop_camera"),
        scene_array_node_id: manifold_core::NodeId::new("scene_array"),
    })
}

/// D6 P4 whitelist: the ONLY params stamped as "Scene Loop" panel rows, as
/// `(stable node_id, param) → row label`. Everything else on the loop nodes —
/// cell_size, axis, home, near, far, fov_y, attack — is internal: the plan
/// builder computes it once and a panel row for it would desync the loop
/// (a Spacing row that edits only the array's cell breaks INV-4). Returns
/// `None` for non-whitelisted params so the caller filters them out.
fn loop_row_label(node_id: &manifold_core::NodeId, param: &str) -> Option<&'static str> {
    match (node_id.as_str(), param) {
        ("loop_phase", "bars") => Some("Bars"),
        ("scene_array", "count") => Some("Copies"),
        ("loop_camera", "height") => Some("Height"),
        ("loop_camera", "lateral") => Some("Lateral"),
        _ => None,
    }
}