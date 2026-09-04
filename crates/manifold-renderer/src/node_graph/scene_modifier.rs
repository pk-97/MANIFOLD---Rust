//! Scene modifier framework (SCENE_MODIFIER_FRAMEWORK_DESIGN.md D1–D3, D6).
//!
//! A scene modifier is to a 3D scene what an effect is to a 2D layer: a
//! named, carded, triggerable behavior applied as a *delta* on a live graph.
//! A modifier KIND is a renderer-side [`SceneModifierDescriptor`] — plan
//! builder + declarative trace signature + row whitelist + enable wiring —
//! registered once via `inventory::submit!` (one file per kind; the loop is
//! kind #1, D6). The editing crate's generic `ApplySceneModifierCommand` /
//! `RemoveSceneModifierCommand` consume the descriptor-produced
//! [`SceneModifierPlan`]; nothing here is per-kind beyond the descriptor.
//!
//! The modifier list is never stored (D2): presence, order, and identity
//! all derive from [`trace_modifier`] at VM-build time.

use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_core::scene_modifier::{
    EnablePlan, GroupSplice, NodeExposure, PlanTraceNode, PortRepoint, SceneModifierPlan,
    ToggleDecl,
};
use manifold_core::NodeId;

use crate::node_graph::scene_exposure::metadata_for_node_type;

// ---------------------------------------------------------------------------
// Descriptor
// ---------------------------------------------------------------------------

/// One scene modifier kind. Renderer-side (reads primitive manifests and
/// scene_bounds; builds plans against a live EffectGraphDef).
pub struct SceneModifierDescriptor {
    /// Stable kind id — public API forever (D6 makes "scene_loop" the first).
    pub kind_id: &'static str,
    /// Card title, exposure section string, picker label.
    pub display_name: &'static str,
    /// Fixed-slot group; card order = SLOT_GROUP_ORDER, then registry order.
    pub slot_group: SlotGroup,
    /// Build the apply plan against the CURRENT graph. None = not applicable
    /// (the picker greys the kind; apply refuses). Also how the remove
    /// command re-derives the plan it inverts — must succeed on an
    /// already-modified graph.
    pub plan_builder: fn(&EffectGraphDef, render_scene_node_id: u32) -> Option<SceneModifierPlan>,
    /// Applicability pre-check for the picker (cheaper than plan_builder;
    /// may be plan_builder itself for cheap kinds).
    pub applicable: fn(&EffectGraphDef, render_scene_node_id: u32) -> bool,
    /// Identity: required/optional (type_id, nodeId) pairs, top level (D3).
    pub trace: &'static [TraceNode],
    /// Which stamped params become card rows (None = full manifest).
    pub row_whitelist: Option<&'static [(&'static str, &'static str, &'static str)]>,
    /// How the enable toggle wires (D5).
    pub enable: EnableDecl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotGroup {
    Camera,
    Atmosphere,
    Objects,
    Lights,
    Environment,
}

/// Canonical card/picker order; v1 uses Camera, Atmosphere.
pub const SLOT_GROUP_ORDER: &[SlotGroup] = &[
    SlotGroup::Camera,
    SlotGroup::Atmosphere,
    SlotGroup::Objects,
    SlotGroup::Lights,
    SlotGroup::Environment,
];

impl SlotGroup {
    fn order_key(self) -> usize {
        SLOT_GROUP_ORDER.iter().position(|g| *g == self).unwrap_or(usize::MAX)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TraceNode {
    pub type_id: &'static str,
    pub node_id: &'static str,
    pub required: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum EnableDecl {
    /// Camera-path kinds: apply mints node.camera_switch between the previous
    /// producer of the repointed port and the modifier's camera. The toggle
    /// row targets `select` on the named node_id.
    Switch { node_id: &'static str },
    /// Value kinds: toggle + amount node.value atoms multiplied into
    /// `target_param` on `target_node_id`. Toggle row → `enabled_node`,
    /// amount row(s) → `amount_node`.
    Gate {
        enabled_node: &'static str,
        amount_node: &'static str,
        target_node: &'static str,
        target_param: &'static str,
    },
}

// ---------------------------------------------------------------------------
// Registry (inventory — one submit per kind, no central edit)
// ---------------------------------------------------------------------------

pub struct SceneModifierDescriptorEntry {
    pub descriptor: fn() -> &'static SceneModifierDescriptor,
}

inventory::collect!(SceneModifierDescriptorEntry);

::inventory::submit! {
    SceneModifierDescriptorEntry { descriptor: || &SCENE_LOOP_DESCRIPTOR }
}

/// All registered kinds in canonical slot order (SLOT_GROUP_ORDER, then
/// link order within a group).
pub fn descriptors() -> Vec<&'static SceneModifierDescriptor> {
    let mut all: Vec<&'static SceneModifierDescriptor> = inventory::iter::<SceneModifierDescriptorEntry>
        .into_iter()
        .map(|e| (e.descriptor)())
        .collect();
    all.sort_by_key(|d| d.slot_group.order_key());
    all
}

pub fn descriptor_for(kind_id: &str) -> Option<&'static SceneModifierDescriptor> {
    descriptors().into_iter().find(|d| d.kind_id == kind_id)
}

/// Build the apply plan for `kind_id` against `def` (the registry dispatch
/// the UI's modifier actions call — the same call the remove arm uses to
/// re-derive the plan it inverts).
pub fn build_plan(
    kind_id: &str,
    def: &EffectGraphDef,
    render_scene_node_id: u32,
) -> Option<SceneModifierPlan> {
    let d = descriptor_for(kind_id)?;
    (d.plan_builder)(def, render_scene_node_id)
}

// ---------------------------------------------------------------------------
// Generic trace (D3)
// ---------------------------------------------------------------------------

/// Outcome of [`trace_modifier`]: which trace nodes resolved, at which doc
/// ids, keyed by the trace's stable node_id string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceResult {
    /// doc id per trace node_id (includes optional nodes when present).
    pub doc_ids: ahash::AHashMap<&'static str, u32>,
}

impl TraceResult {
    /// All REQUIRED trace nodes resolved — the kind is applied (D3
    /// all-or-nothing). Optional nodes may be absent (e.g. a hand-deleted
    /// camera switch leaves the kind applied but un-toggleable).
    pub fn applied(&self, descriptor: &SceneModifierDescriptor) -> bool {
        descriptor
            .trace
            .iter()
            .filter(|t| t.required)
            .all(|t| self.doc_ids.contains_key(t.node_id))
    }

    /// Some but not all REQUIRED nodes present — hand-edit debris (INV-M9).
    pub fn partial(&self, descriptor: &SceneModifierDescriptor) -> bool {
        let required = descriptor.trace.iter().filter(|t| t.required);
        let present = required.clone().filter(|t| self.doc_ids.contains_key(t.node_id)).count();
        present > 0 && present < required.count()
    }
}

/// Structural trace: resolve a kind's (type_id, nodeId) signature against a
/// graph level's top-level nodes. Generalizes `trace_scene_loop`
/// (SCENE_LOOP P2, scene_vm.rs pre-D6).
pub fn trace_modifier(
    descriptor: &SceneModifierDescriptor,
    nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
) -> TraceResult {
    let mut doc_ids = ahash::AHashMap::new();
    for t in descriptor.trace {
        if let Some(node) = nodes
            .iter()
            .find(|n| n.type_id == t.type_id && n.node_id.as_str() == t.node_id)
        {
            doc_ids.insert(t.node_id, node.id);
        }
    }
    TraceResult { doc_ids }
}

// ---------------------------------------------------------------------------
// Builder helpers (so kind fns stay small)
// ---------------------------------------------------------------------------

/// Curate one minted node's exposure metadata to the kind's whitelist,
/// labelling rows (D6 P4 whitelist — the loop's Bars/Copies/Height/Lateral).
/// `None` whitelist stamps the full manifest. Returns None when nothing is
/// curated (the node stamps no rows).
pub fn curated_exposure(
    whitelist: Option<&'static [(&'static str, &'static str, &'static str)]>,
    node: &manifold_core::effect_graph_def::EffectGraphNode,
) -> Option<NodeExposure> {
    let manifest: Vec<_> = metadata_for_node_type(&node.type_id)
        .into_iter()
        .filter_map(|mut m| {
            let label = match whitelist {
                Some(w) => w
                    .iter()
                    .find(|(nid, param, _)| nid == &node.node_id.as_str() && param == &m.name)
                    .map(|(_, _, label)| *label),
                None => Some(m.label.as_str()),
            }?;
            m.label = label.to_string();
            Some(m)
        })
        .collect();
    if manifest.is_empty() {
        return None;
    }
    Some(NodeExposure {
        node_doc_id: node.id,
        node_id: node.node_id.clone(),
        type_id: node.type_id.clone(),
        params: node.params.clone(),
        metadata: manifest,
    })
}

/// The camera re-point every camera-path kind shares: the lens
/// (`node.camera_lens`, falling back to the render_scene node) the modifier
/// camera re-points into, and its `camera` port.
pub fn camera_repoint_target(def: &EffectGraphDef, render_scene_node_id: u32) -> (u32, String) {
    let lens_node = def.nodes.iter().find(|n| n.type_id == "node.camera_lens");
    let target = lens_node.map(|n| n.id).unwrap_or(render_scene_node_id);
    (target, "camera".to_string())
}

/// The current producer of `camera` on the repoint target — the source the
/// modifier's camera switch takes over (`a` side). None when unwired.
pub fn camera_port_producer(def: &EffectGraphDef, target_node_id: u32) -> Option<u32> {
    def.wires
        .iter()
        .find(|w| w.to_node == target_node_id && w.to_port == "camera")
        .map(|w| w.from_node)
}

/// Assemble the generic plan skeleton shared by kinds: trace + repoint +
/// exposures for the minted atoms. The kind fn fills nodes/wires/splices/
/// enable around it.
pub struct PlanSkeleton {
    pub trace: Vec<PlanTraceNode>,
    pub repoints: Vec<PortRepoint>,
    pub exposures: Vec<NodeExposure>,
}

pub fn plan_skeleton(
    descriptor: &SceneModifierDescriptor,
    new_nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
    repoints: Vec<PortRepoint>,
) -> PlanSkeleton {
    let trace = descriptor
        .trace
        .iter()
        .map(|t| PlanTraceNode {
            type_id: t.type_id.to_string(),
            node_id: t.node_id.to_string(),
            required: t.required,
        })
        .collect();
    let exposures = new_nodes
        .iter()
        .filter_map(|n| curated_exposure(descriptor.row_whitelist, n))
        .collect();
    PlanSkeleton { trace, repoints, exposures }
}

/// Re-export the loop kind's builder so `gltf_import`'s historical public
/// path keeps working for tests and tooling during the D6 migration.
pub use scene_modifier_loop::{LOOP_KIND_ID, migrate_pre_switch_scene_loops, SCENE_LOOP_DESCRIPTOR};

pub mod scene_modifier_loop {
    //! Kind `scene_loop` — the Scene Loop as modifier kind #1 (D6/D8).
    //!
    //! Same three atoms, same params, same `"Scene Loop"` section string,
    //! same whitelist (Bars/Copies/Height/Lateral), same cell_size D4 gap
    //! rule — byte-identical behavior through the generic plan/command pair.
    //! What changes is the camera path: the apply additionally mints a
    //! `node.camera_switch` (`loop_cam_switch`: previous camera producer →
    //! `a`, loop_camera → `b`, `out` → lens.camera; the old direct
    //! loop_camera → lens.camera wire shape is replaced). Old projects that
    //! predate the switch trace unchanged (the switch is an OPTIONAL trace
    //! node) and migrate automatically at load
    //! ([`migrate_pre_switch_scene_loops`]).

    use super::*;
    use manifold_core::effect_graph_def::{EffectGraphNode, EffectGraphWire};

    pub const LOOP_KIND_ID: &str = "scene_loop";

    const CAMERA_RESTORE_TYPES: &[&str] = &[
        "node.orbit_camera",
        "node.free_camera",
        "node.look_at_camera",
    ];

    /// D6 P4 whitelist: the ONLY params stamped as "Scene Loop" rows, as
    /// `(stable node_id, param) → row label`. Everything else on the loop
    /// nodes — cell_size, axis, home, near, far, fov_y, attack — is internal:
    /// the plan builder computes it once and a panel row for it would
    /// desync the loop.
    const LOOP_ROW_WHITELIST: &[(&str, &str, &str)] = &[
        ("loop_phase", "bars", "Bars"),
        ("scene_array", "count", "Copies"),
        ("loop_camera", "height", "Height"),
        ("loop_camera", "lateral", "Lateral"),
    ];

    pub static SCENE_LOOP_DESCRIPTOR: SceneModifierDescriptor = SceneModifierDescriptor {
        kind_id: LOOP_KIND_ID,
        display_name: "Scene Loop",
        slot_group: SlotGroup::Camera,
        plan_builder: build_scene_loop_plan,
        applicable: |def, render_scene_node_id| {
            def.nodes.iter().any(|n| n.id == render_scene_node_id)
        },
        // The three atoms are required (D3 all-or-nothing); the camera
        // switch is optional — pre-migration projects and hand-deleted
        // switches still trace as applied (D8).
        trace: &[
            TraceNode { type_id: "node.beat_ramp", node_id: "loop_phase", required: true },
            TraceNode { type_id: "node.scene_array", node_id: "scene_array", required: true },
            TraceNode { type_id: "node.loop_camera", node_id: "loop_camera", required: true },
            TraceNode { type_id: "node.camera_switch", node_id: "loop_cam_switch", required: false },
        ],
        row_whitelist: Some(LOOP_ROW_WHITELIST),
        enable: EnableDecl::Switch { node_id: "loop_cam_switch" },
    };

    fn f32_param(map: &mut std::collections::BTreeMap<String, SerializedParamValue>, name: &str, value: f32) {
        map.insert(name.to_string(), SerializedParamValue::Float { value });
    }

    fn mint_node(
        id: u32,
        node_id: &str,
        type_id: &str,
        params: std::collections::BTreeMap<String, SerializedParamValue>,
    ) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: NodeId::new(node_id),
            type_id: type_id.to_string(),
            handle: Some(node_id.to_string()),
            params,
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        }
    }

    fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire {
            from_node,
            from_port: from_port.to_string(),
            to_node,
            to_port: to_port.to_string(),
        }
    }

    /// Build the Scene Loop apply plan for `def`'s scene rooted at
    /// `render_scene_node_id`. Returns None when the graph isn't a
    /// single-scene import the loop can splice (the command re-checks INV-1
    /// at execute time too). Succeeds on an already-looped graph so the
    /// remove arm can re-derive the plan it inverts.
    pub fn build_scene_loop_plan(
        def: &EffectGraphDef,
        render_scene_node_id: u32,
    ) -> Option<SceneModifierPlan> {
        // Confirm the layer actually carries a scene.
        def.nodes.iter().find(|n| n.id == render_scene_node_id)?;

        // D4: cell_size from scene_bounds (Z extent), 10.0 fallback.
        //
        // Gap rule (BUG-70wo): the cell is TWO object-depths — one depth of
        // solid, one depth of open air. cell == extent packs copies
        // face-to-face, so the camera path never leaves the bounding box:
        // for any solid asset every frame renders from inside the mesh and
        // the loop is uniformly black. With a gap the loop reads as
        // approach → through → emerge → next copy ahead, and phase 0 sits
        // mid-gap looking at the next copy (still wrap-pure: travel per
        // loop is one cell by construction).
        let bounds = def.preset_metadata.as_ref().and_then(|m| m.scene_bounds);
        let axis_extent = bounds.map(|(min, max)| (max[2] - min[2]).abs()).unwrap_or(0.0);
        let cell_size = if axis_extent > 0.0 { axis_extent * 2.0 } else { 10.0 };

        // The lens (import spine: camera → lens → render + ao/dof/mb). The
        // loop camera re-points INTO lens.camera so every downstream
        // consumer follows (D5). Falls back to render_scene.camera when no
        // lens exists (the minimal hand-built scene shape).
        let (camera_target, camera_port) = camera_repoint_target(def, render_scene_node_id);
        // The camera producer the switch takes over (`a` side). None when
        // the port is unwired — the switch's a input stays unwired and
        // select=B keeps the loop camera (the mux falls back across).
        let previous_camera = camera_port_producer(def, camera_target);

        // Object groups = producers wired into render_scene's object_k /
        // mesh_k. The `objects` param is a stale hint in sync with the
        // wires; the WIRES are the truth. Each group's `node.scene_object`
        // (object_k_bind) is found INSIDE its body.
        let mut group_splices = Vec::new();
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
            if group.group.is_none() {
                continue;
            }
            group_splices.push(GroupSplice {
                group_node_id: group.id,
                inner_node_type: "node.scene_object",
                inner_port: "instances",
                source_doc_id: 0, // filled once scene_array's id is known
                source_port: "out".to_string(),
            });
        }

        // Fresh top-level doc ids. (Group-body ids for a group_input the
        // apply mints live in a high spare range the command owns.)
        let max_id = def.nodes.iter().map(|n| n.id).max().unwrap_or(0);
        let beat_ramp_id = max_id + 1;
        let scene_array_id = max_id + 2;
        let loop_camera_id = max_id + 3;
        let switch_id = max_id + 4;

        // D7 (SCENE_LOOP P4): exactly three atoms — loop_phase, scene_array,
        // loop_camera. Fog is never minted: the D4 gap rule makes every
        // copy self-contained.
        let mut new_nodes = Vec::new();
        let mut new_wires = Vec::new();

        // beat_ramp (loop_phase): bars = 8 (D10 default) — with bars > 0 the
        // ramp runs at 1/bars cycles/beat, so the Bars row reads/writes bars
        // directly. attack = 1.0 makes the output exactly the 0..1 loop
        // phase. rate stays 0.0: the disabled fallback, so bars = 0 (the
        // wrap-debug park) freezes the phase at 0.
        let mut params = std::collections::BTreeMap::new();
        f32_param(&mut params, "bars", 8.0);
        f32_param(&mut params, "rate", 0.0);
        f32_param(&mut params, "attack", 1.0);
        new_nodes.push(mint_node(beat_ramp_id, "loop_phase", "node.beat_ramp", params));

        // scene_array: the shared copy array — count 3, axis +Z default.
        let mut params = std::collections::BTreeMap::new();
        f32_param(&mut params, "count", 3.0);
        params.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 }); // +Z
        f32_param(&mut params, "cell_size", cell_size);
        new_nodes.push(mint_node(scene_array_id, "scene_array", "node.scene_array", params));

        // loop_camera: flies one cell per loop. home = -cell/2 = mid-gap
        // before copy 0. Scale-aware framing (BUG-j65u): height/near/far
        // derive from the cell, never room-scale constants.
        let mut params = std::collections::BTreeMap::new();
        f32_param(&mut params, "cell_size", cell_size);
        params.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 }); // +Z — must match scene_array
        f32_param(&mut params, "home", -cell_size * 0.5);
        f32_param(&mut params, "lateral", 0.0);
        f32_param(&mut params, "height", 0.0);
        f32_param(&mut params, "near", (cell_size * 0.002).max(1e-4));
        f32_param(&mut params, "far", cell_size * 4.0);
        f32_param(&mut params, "fov_y", 0.9);
        new_nodes.push(mint_node(loop_camera_id, "loop_camera", "node.loop_camera", params));

        for s in &mut group_splices {
            s.source_doc_id = scene_array_id;
        }

        // Wires: beat_ramp.out → loop_camera.phase (D5).
        new_wires.push(wire(beat_ramp_id, "out", loop_camera_id, "phase"));

        // D5 Switch enable: the camera path runs through loop_cam_switch —
        // previous camera producer → a, loop_camera → b, out → lens.camera.
        // Toggling the loop is a param write on `select` (INV-M7), never a
        // graph rebuild. Applied enabled: select = B.
        let mut switch_params = std::collections::BTreeMap::new();
        switch_params.insert("select".to_string(), SerializedParamValue::Enum { value: 1 }); // B
        let switch_node =
            mint_node(switch_id, "loop_cam_switch", "node.camera_switch", switch_params);

        let extra_nodes = vec![switch_node];
        let mut extra_wires = Vec::new();
        if let Some(prev) = previous_camera {
            extra_wires.push(wire(prev, "out", switch_id, "a"));
        }
        extra_wires.push(wire(loop_camera_id, "out", switch_id, "b"));
        extra_wires.push(wire(switch_id, "out", camera_target, &camera_port));

        let repoints = vec![PortRepoint {
            target_node_id: camera_target,
            target_port: camera_port,
            new_producer_doc_id: switch_id,
            restore_types: CAMERA_RESTORE_TYPES,
        }];

        let skeleton = plan_skeleton(&SCENE_LOOP_DESCRIPTOR, &new_nodes, repoints);

        Some(SceneModifierPlan {
            kind_id: LOOP_KIND_ID.to_string(),
            display_name: "Scene Loop".to_string(),
            trace: skeleton.trace,
            new_nodes,
            new_wires,
            group_splices,
            repoints: skeleton.repoints,
            exposures: skeleton.exposures,
            enable: EnablePlan {
                toggle: ToggleDecl::NodeParam {
                    node_doc_hint: NodeId::new("loop_cam_switch"),
                    param: "select".to_string(),
                    on: 1.0,  // B = loop camera
                    off: 0.0, // A = original camera
                },
                extra_nodes,
                extra_wires,
            },
        })
    }

    /// Automatic-at-load migration for pre-switch loop graphs (D8, INV-M8):
    /// when the trace finds the loop WITHOUT a `loop_cam_switch`, mint the
    /// switch and re-point the camera path through it — previous camera
    /// producer → `a`, loop_camera → `b`, switch.out → lens.camera,
    /// replacing the direct loop_camera → lens.camera wire. Idempotent: a
    /// graph with the switch already in place is untouched. Runs in the
    /// same per-layer load loop as `migrate_scene_exposures`
    /// (manifold-app's project_io). Returns true when the def changed.
    pub fn migrate_pre_switch_scene_loops(def: &mut EffectGraphDef) -> bool {
        let result = trace_modifier(&SCENE_LOOP_DESCRIPTOR, &def.nodes);
        if !result.applied(&SCENE_LOOP_DESCRIPTOR) || result.doc_ids.contains_key("loop_cam_switch") {
            return false;
        }
        let Some(loop_camera_doc) = result.doc_ids.get("loop_camera").copied() else {
            return false;
        };

        // The camera port the loop re-pointed: lens.camera, falling back to
        // render_scene.camera (the plan builder's fallback shape).
        let render_scene = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.render_scene")
            .map(|n| n.id);
        let (target, _) = camera_repoint_target(def, render_scene.unwrap_or(0));

        // The direct producer of that port must be the loop_camera itself
        // (the pre-switch shape). Anything else is a hand-edited graph the
        // migration doesn't pretend to understand.
        let Some(producer) = camera_port_producer(def, target) else {
            return false;
        };
        if producer != loop_camera_doc {
            return false;
        }

        // The original camera the remove step would restore — wired to `a`
        // so disabling the loop (select=A) lands back on it seamlessly.
        let previous_camera = def.nodes.iter().find(|n| {
            matches!(
                n.type_id.as_str(),
                "node.orbit_camera" | "node.free_camera" | "node.look_at_camera"
            )
        }).map(|n| n.id);

        let switch_id = def.nodes.iter().map(|n| n.id).max().unwrap_or(0) + 1;
        let mut switch_params = std::collections::BTreeMap::new();
        switch_params.insert("select".to_string(), SerializedParamValue::Enum { value: 1 }); // B = enabled
        def.nodes.push(mint_node(switch_id, "loop_cam_switch", "node.camera_switch", switch_params));

        // Drop the direct loop_camera → target wire; re-point through the
        // switch.
        def.wires.retain(|w| !(w.from_node == loop_camera_doc && w.to_node == target && w.to_port == "camera"));
        if let Some(prev) = previous_camera {
            def.wires.push(wire(prev, "out", switch_id, "a"));
        }
        def.wires.push(wire(loop_camera_doc, "out", switch_id, "b"));
        def.wires.push(wire(switch_id, "out", target, "camera"));
        true
    }
}
