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

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
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
    /// P4 coupled rows: a card row whose write also writes second params in
    /// the SAME undo unit (Stride → scene_array.count, Spacing → the other
    /// cell_size + home). Resolved app-side at write time; the UI never
    /// learns about it (rows stay pure data).
    pub coupled_writes: &'static [CoupledWrite],
    /// How the enable toggle wires (D5).
    pub enable: EnableDecl,
}

/// One coupled row write: `primary` (node_id, param) is the whitelisted row
/// the performer drags; each secondary derives its value from the primary's
/// new value and lands in the same undoable command.
pub struct CoupledWrite {
    pub primary: (&'static str, &'static str),
    pub secondaries: &'static [CoupledSecondary],
}

pub struct CoupledSecondary {
    pub node_id: &'static str,
    pub param: &'static str,
    /// Secondary value from the primary's new value.
    pub value: fn(f32) -> f32,
}

/// One resolved coupled secondary: `(kind_id, node_id, param, value_fn)` —
/// the app evaluates `value_fn` at the write's value (it changes per scrub
/// Move).
pub type CoupledWriteTarget = (&'static str, &'static str, &'static str, fn(f32) -> f32);

/// Resolve the coupled secondaries for a modifier row write: given the
/// written `(node_id, param)` on a graph carrying applied kinds, return one
/// per secondary that must land in the same undo unit. Empty when the row
/// isn't coupled.
pub fn coupled_writes_for(
    def: &EffectGraphDef,
    node_id: &str,
    param: &str,
) -> Vec<CoupledWriteTarget> {
    let mut out = Vec::new();
    for d in descriptors() {
        if d.coupled_writes.is_empty() {
            continue;
        }
        // Coupling applies only while the kind is applied — a hand-removed
        // modifier's rows are gone, and a stray write must not couple.
        if !trace_modifier(d, &def.nodes).applied(d) {
            continue;
        }
        for cw in d.coupled_writes {
            if cw.primary.0 == node_id && cw.primary.1 == param {
                for s in cw.secondaries {
                    out.push((d.kind_id, s.node_id, s.param, s.value));
                }
            }
        }
    }
    out
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

::inventory::submit! {
    SceneModifierDescriptorEntry { descriptor: || &scene_modifier_fog::SCENE_FOG_DESCRIPTOR }
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

// ---------------------------------------------------------------------------
// Plan-node helpers shared by the kind modules (minting + wiring)
// ---------------------------------------------------------------------------

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

/// Re-export the loop kind's builder so `gltf_import`'s historical public
/// path keeps working for tests and tooling during the D6 migration.
pub use scene_modifier_loop::{LOOP_KIND_ID, migrate_loop_exposure_rows, migrate_pre_switch_scene_loops, SCENE_LOOP_DESCRIPTOR};

pub use scene_modifier_fog::{FOG_KIND_ID, SCENE_FOG_DESCRIPTOR};

pub mod scene_modifier_loop {
    //! Kind `scene_loop` — the Scene Loop as modifier kind #1 (D6/D8).
    //!
    //! Same three atoms, same params, same `"Scene Loop"` section string,
    //! same row whitelist, same cell_size D4 gap
    //! rule — byte-identical behavior through the generic plan/command pair.
    //! What changes is the camera path: the apply additionally mints a
    //! `node.camera_switch` (`loop_cam_switch`: previous camera producer →
    //! `a`, loop_camera → `b`, `out` → lens.camera; the old direct
    //! loop_camera → lens.camera wire shape is replaced). Old projects that
    //! predate the switch trace unchanged (the switch is an OPTIONAL trace
    //! node) and migrate automatically at load
    //! ([`migrate_pre_switch_scene_loops`]).

    use super::*;

    pub const LOOP_KIND_ID: &str = "scene_loop";

    const CAMERA_RESTORE_TYPES: &[&str] = &[
        "node.orbit_camera",
        "node.free_camera",
        "node.look_at_camera",
    ];

    /// D6 P4 whitelist + SCENE_MODIFIER_FRAMEWORK P4 enrichment + BUG-gsql
    /// framing rows: the ONLY params stamped as "Scene Loop" rows, as
    /// `(stable node_id, param) → row label`. Everything else on the loop
    /// nodes — axis, attack, jitter_seed, look_sweep_cycles — is internal:
    /// the plan builder computes it once and a panel row for it would
    /// desync the loop. Spacing rides loop_camera.cell_size (its stamped
    /// range is curated to auto×0.25..4.0 at apply time); Near/Far/Home
    /// ranges are curated to the cell too (the manifest bands are
    /// room-scale generics, unusable on a minted flythrough camera — same
    /// defect Spacing curation fixed); look_sweep_cycles stays internal at
    /// 1 (integer cycles are a wrap-safety requirement, INV-3).
    const LOOP_ROW_WHITELIST: &[(&str, &str, &str)] = &[
        ("loop_phase", "bars", "Bars"),
        ("scene_array", "count", "Copies"),
        ("loop_camera", "height", "Height"),
        ("loop_camera", "lateral", "Lateral"),
        ("loop_camera", "near", "Near"),
        ("loop_camera", "far", "Far"),
        ("loop_camera", "fov_y", "FOV"),
        ("loop_camera", "home", "Home"),
        ("loop_camera", "roll", "Roll"),
        ("loop_camera", "pitch", "Pitch"),
        ("loop_camera", "yaw", "Yaw"),
        ("loop_camera", "flow", "Flow"),
        ("loop_camera", "stride", "Stride"),
        ("loop_camera", "sway_amp", "Sway"),
        ("loop_camera", "sway_cycles", "Sway Rate"),
        ("loop_camera", "look_sweep_amp", "Look Sway"),
        ("loop_camera", "zoom_pulse_amp", "Zoom Pulse"),
        ("loop_camera", "cell_size", "Spacing"),
        ("scene_array", "jitter_amount", "Jitter"),
    ];

    /// Coupled row writes (P4). Stride travels K cells per loop, so the
    /// instance array must scale with it: count = K+2 (behind + current +
    /// ahead), clamped at count's own ceiling of 8 — K ≥ 7 outruns the
    /// array by one cell (reported, not hidden). Spacing writes BOTH
    /// cell_size params in one undo unit (INV-4: camera travel ==
    /// instance spacing by construction) and home = −cell/2 must track the
    /// cell or the mid-gap phase-0 framing desyncs.
    const LOOP_COULED_WRITES: &[CoupledWrite] = &[
        CoupledWrite {
            primary: ("loop_camera", "stride"),
            secondaries: &[CoupledSecondary {
                node_id: "scene_array",
                param: "count",
                value: stride_to_count,
            }],
        },
        CoupledWrite {
            primary: ("loop_camera", "cell_size"),
            secondaries: &[
                CoupledSecondary {
                    node_id: "scene_array",
                    param: "cell_size",
                    value: identity_value,
                },
                CoupledSecondary {
                    node_id: "loop_camera",
                    param: "home",
                    value: cell_to_home,
                },
            ],
        },
    ];

    fn stride_to_count(k: f32) -> f32 {
        // K cells of travel need K+2 copies (behind + current + ahead);
        // count's own range caps at 8, so K ≥ 7 runs one copy short.
        (k + 2.0).min(8.0)
    }

    fn identity_value(v: f32) -> f32 {
        v
    }

    fn cell_to_home(cell: f32) -> f32 {
        // The D10/P2 mid-gap phase-0 start tracks the cell (home = −cell/2).
        -cell * 0.5
    }

    /// Curate the loop_camera rows whose manifest ranges are room-scale
    /// generics, unusable on a minted flythrough camera:
    /// - Spacing (cell_size): 0.01..1000 → auto×0.25..4.0 (the P4-pinned
    ///   band).
    /// - Near/Far/Home: scale with the cell, mirroring the orbit_camera
    ///   radius rules in `scene_scaled_range` (the card never flows through
    ///   that table — it serves the import stamping path — so the modifier
    ///   curates here, at the same two call sites as Spacing).
    ///   Every edit keeps the stamped default inside the band (the stamper's
    ///   own widen rule). Applied by BOTH the plan builder and the load
    ///   migration — one fn, two call sites, no drift.
    fn curate_loop_camera_rows(exposure: &mut NodeExposure, cell: f32) {
        for m in &mut exposure.metadata {
            let default = match &m.default_value {
                SerializedParamValue::Float { value } => *value,
                _ => continue,
            };
            match m.name.as_str() {
                "cell_size" => {
                    m.min = cell * 0.25;
                    m.max = cell * 4.0;
                }
                "home" => {
                    m.min = (-2.0 * cell).min(default);
                    m.max = (2.0 * cell).max(default);
                }
                "near" => {
                    m.min = 0.001_f32.min(default);
                    m.max = (2.0 * cell).max(default);
                }
                "far" => {
                    m.min = 1.0_f32.min(default);
                    m.max = (20.0 * cell).min(10_000.0).max(default);
                }
                _ => {}
            }
        }
    }

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
        coupled_writes: LOOP_COULED_WRITES,
        enable: EnableDecl::Switch { node_id: "loop_cam_switch" },
    };

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
                // D6: the loop's scene_array takes over (group, instances)
                // from whatever fed it before (the camera-switch precedent).
                replace_existing: true,
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
        // jitter_seed/jitter_amount pinned off: the loop's copies repeat
        // exactly until the performer dials the Jitter row.
        let mut params = std::collections::BTreeMap::new();
        f32_param(&mut params, "count", 3.0);
        params.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 }); // +Z
        f32_param(&mut params, "cell_size", cell_size);
        f32_param(&mut params, "jitter_seed", 0.0);
        f32_param(&mut params, "jitter_amount", 0.0);
        new_nodes.push(mint_node(scene_array_id, "scene_array", "node.scene_array", params));

        // loop_camera: flies one cell per loop. home = -cell/2 = mid-gap
        // before copy 0. Scale-aware framing (BUG-j65u): height/near/far
        // derive from the cell, never room-scale constants. P4 movement
        // controls pinned at "off" defaults (flow 0 = linear travel, sway /
        // look / zoom 0 = no effect, stride 1 = one cell) — the performer
        // dials them from the card rows.
        let mut params = std::collections::BTreeMap::new();
        f32_param(&mut params, "cell_size", cell_size);
        params.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 }); // +Z — must match scene_array
        f32_param(&mut params, "home", -cell_size * 0.5);
        f32_param(&mut params, "lateral", 0.0);
        f32_param(&mut params, "height", 0.0);
        f32_param(&mut params, "near", (cell_size * 0.002).max(1e-4));
        f32_param(&mut params, "far", cell_size * 4.0);
        f32_param(&mut params, "fov_y", 0.9);
        f32_param(&mut params, "flow", 0.0);
        f32_param(&mut params, "stride", 1.0);
        f32_param(&mut params, "sway_amp", 0.0);
        f32_param(&mut params, "sway_cycles", 1.0);
        f32_param(&mut params, "look_sweep_amp", 0.0);
        f32_param(&mut params, "look_sweep_cycles", 1.0);
        f32_param(&mut params, "zoom_pulse_amp", 0.0);
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

        let mut skeleton = plan_skeleton(&SCENE_LOOP_DESCRIPTOR, &new_nodes, repoints);
        // Framing row range curation: the stamped specs span the manifests'
        // generic room-scale bands — unusable. Narrow them to the cell-scaled
        // bands (the plan's cell IS the auto cell).
        for exposure in &mut skeleton.exposures {
            if exposure.node_id.as_str() == "loop_camera" {
                curate_loop_camera_rows(exposure, cell_size);
            }
        }

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

    /// P4 load migration: an applied loop stamped before the enrichment
    /// carries only the four D6 rows. Re-stamp the loop nodes' curated
    /// exposures through the CURRENT whitelist — the stamper is idempotent
    /// by (node_id, param), so existing rows are untouched and only the new
    /// rows (P4 controls, Spacing/Jitter, and the BUG-gsql framing rows)
    /// land. The framing rows' ranges are curated against the node's
    /// CURRENT cell_size (the auto cell for every pre-P4 project — none
    /// have Spacing-written cells yet).
    /// Runs in the same per-layer load loop as `migrate_scene_exposures`.
    /// Returns true when any new row was stamped.
    pub fn migrate_loop_exposure_rows(def: &mut EffectGraphDef) -> bool {
        let result = trace_modifier(&SCENE_LOOP_DESCRIPTOR, &def.nodes);
        if !result.applied(&SCENE_LOOP_DESCRIPTOR) {
            return false;
        }
        let mut changed = false;
        for t in SCENE_LOOP_DESCRIPTOR.trace {
            let Some(&doc_id) = result.doc_ids.get(t.node_id) else { continue };
            let Some(node) = def.nodes.iter().find(|n| n.id == doc_id) else { continue };
            let Some(mut exposure) = curated_exposure(SCENE_LOOP_DESCRIPTOR.row_whitelist, node) else {
                continue;
            };
            if t.node_id == "loop_camera" {
                let cell = exposure
                    .params
                    .get("cell_size")
                    .and_then(|v| match v {
                        SerializedParamValue::Float { value } => Some(*value),
                        _ => None,
                    })
                    .unwrap_or(10.0);
                curate_loop_camera_rows(&mut exposure, cell);
            }
            if manifold_core::scene_exposure::stamp_scene_node_exposures(
                def,
                doc_id,
                "Scene Loop",
                &exposure.metadata,
            ) {
                changed = true;
            }
        }
        changed
    }
}

pub mod scene_modifier_fog {
    //! Kind `scene_fog` — scene-wide fog as modifier kind #2 (section 3.6,
    //! the generality proof: a VALUE kind through the same descriptor +
    //! generic command pair as the loop, zero framework change).
    //!
    //! Mints `node.atmosphere` ("fog_atm") into render_scene's `atmosphere`
    //! input, two `node.value` atoms (the Enabled toggle and the Density
    //! amount), and a `node.math` Mul multiplying them into
    //! `fog_atm.fog_density` — the D5 gate bypass: enable = 0 drives the
    //! density to exactly 0 with no graph churn (INV-M7), applied enabled
    //! with a light-haze default the performer dials from.

    use super::*;
    use crate::node_graph::scene_vm::RENDER_SCENE_TYPE_ID;

    pub const FOG_KIND_ID: &str = "scene_fog";

    /// The applied Density default — a light haze, well inside the
    /// atmosphere's 0..1 density band.
    const FOG_AMOUNT_DEFAULT: f32 = 0.1;

    /// The card rows (the K3 P2 amendment): ONLY the two value atoms
    /// surface. Everything on fog_atm (colors, falloff, shafts) is an
    /// authored default the plan pins — a panel row for it would desync.
    const FOG_ROW_WHITELIST: &[(&str, &str, &str)] = &[
        ("fog_enabled", "value", "Enabled"),
        ("fog_amount", "value", "Density"),
    ];

    pub static SCENE_FOG_DESCRIPTOR: SceneModifierDescriptor = SceneModifierDescriptor {
        kind_id: FOG_KIND_ID,
        display_name: "Scene Fog",
        slot_group: SlotGroup::Atmosphere,
        plan_builder: build_scene_fog_plan,
        applicable: scene_fog_applicable,
        // All FOUR minted nodes are required. The apply command refuses a
        // re-apply only on the plan's trace (INV-M9) — a minted node left
        // out of the trace would let a re-apply re-mint its stable nodeId
        // onto surviving debris (the section 3.6 "three nodes" reading
        // breaks here; the math Mul is minted, so it is traced).
        trace: &[
            TraceNode { type_id: "node.atmosphere", node_id: "fog_atm", required: true },
            TraceNode { type_id: "node.value", node_id: "fog_enabled", required: true },
            TraceNode { type_id: "node.value", node_id: "fog_amount", required: true },
            TraceNode { type_id: "node.math", node_id: "fog_mul", required: true },
        ],
        row_whitelist: Some(FOG_ROW_WHITELIST),
        coupled_writes: &[],
        enable: EnableDecl::Gate {
            enabled_node: "fog_enabled",
            amount_node: "fog_amount",
            target_node: "fog_atm",
            target_param: "fog_density",
        },
    };

    /// Picker/dispatch gate (section 3.6 + the K3 amendments): exactly one
    /// render_scene (INV-M6), the render_scene's `atmosphere` input free,
    /// and no applied kind in the Atmosphere slot group (D2 exclusivity —
    /// today that IS this kind, so the last check also covers
    /// "already applied"). The applied-state is derived from the def via
    /// the generic trace — the descriptor signature carries no map, and
    /// none is needed.
    fn scene_fog_applicable(def: &EffectGraphDef, render_scene_node_id: u32) -> bool {
        if def.nodes.iter().filter(|n| n.type_id == RENDER_SCENE_TYPE_ID).count() != 1 {
            return false;
        }
        if !def.nodes.iter().any(|n| n.id == render_scene_node_id) {
            return false;
        }
        if def
            .wires
            .iter()
            .any(|w| w.to_node == render_scene_node_id && w.to_port == "atmosphere")
        {
            return false;
        }
        descriptors()
            .into_iter()
            .filter(|d| d.slot_group == SlotGroup::Atmosphere)
            .all(|d| !trace_modifier(d, &def.nodes).applied(d))
    }

    /// Build the Scene Fog apply plan. Succeeds on any graph carrying the
    /// render_scene node so the remove arm can re-derive the plan it
    /// inverts (the P1 contract) — all gating lives in
    /// [`scene_fog_applicable`].
    pub fn build_scene_fog_plan(
        def: &EffectGraphDef,
        render_scene_node_id: u32,
    ) -> Option<SceneModifierPlan> {
        def.nodes.iter().find(|n| n.id == render_scene_node_id)?;

        let max_id = def.nodes.iter().map(|n| n.id).max().unwrap_or(0);
        let fog_atm_id = max_id + 1;
        let fog_enabled_id = max_id + 2;
        let fog_amount_id = max_id + 3;
        let fog_mul_id = max_id + 4;

        // fog_atm: the full param set, explicit. ambient_tint is stamped at
        // the neutral (1,1,1) even though that equals today's manifest
        // default — pinned so a FUTURE atmosphere default change can't tint
        // the bypass path (the K3 P2 amendment).
        let mut atm_params = std::collections::BTreeMap::new();
        f32_param(&mut atm_params, "fog_color_r", 0.5);
        f32_param(&mut atm_params, "fog_color_g", 0.55);
        f32_param(&mut atm_params, "fog_color_b", 0.65);
        f32_param(&mut atm_params, "fog_density", 0.0);
        f32_param(&mut atm_params, "height_falloff", 0.0);
        f32_param(&mut atm_params, "ambient_tint_r", 1.0);
        f32_param(&mut atm_params, "ambient_tint_g", 1.0);
        f32_param(&mut atm_params, "ambient_tint_b", 1.0);
        f32_param(&mut atm_params, "shaft_intensity", 0.0);
        f32_param(&mut atm_params, "shaft_anisotropy", 0.6);
        atm_params.insert("shaft_quality".to_string(), SerializedParamValue::Enum { value: 1 });
        let fog_atm = mint_node(fog_atm_id, "fog_atm", "node.atmosphere", atm_params);

        // The D5 gate atoms: the Enabled toggle (applied on) and the
        // Density amount, multiplied into fog_atm.fog_density.
        let fog_enabled =
            mint_node(fog_enabled_id, "fog_enabled", "node.value", value_params(1.0));
        let fog_amount = mint_node(
            fog_amount_id,
            "fog_amount",
            "node.value",
            value_params(FOG_AMOUNT_DEFAULT),
        );

        let mut mul_params = std::collections::BTreeMap::new();
        f32_param(&mut mul_params, "a", 0.0);
        f32_param(&mut mul_params, "b", 0.0);
        mul_params.insert("op".to_string(), SerializedParamValue::Enum { value: 2 }); // Multiply
        let fog_mul = mint_node(fog_mul_id, "fog_mul", "node.math", mul_params);

        let new_nodes = vec![fog_atm, fog_enabled, fog_amount, fog_mul];
        let new_wires = vec![
            wire(fog_enabled_id, "out", fog_mul_id, "a"),
            wire(fog_amount_id, "out", fog_mul_id, "b"),
            wire(fog_mul_id, "out", fog_atm_id, "fog_density"),
            wire(fog_atm_id, "atmosphere", render_scene_node_id, "atmosphere"),
        ];

        let mut skeleton = plan_skeleton(&SCENE_FOG_DESCRIPTOR, &new_nodes, Vec::new());

        // The Enabled row must render as a toggle (D5), but node.value's
        // `value` param is Float — the manifest-derived is_toggle is false.
        // The kind's curation flips the stamped metadata: the scene write
        // path is f32 end-to-end (a toggle row writes 0.0/1.0), so the
        // flag is display curation, not a type change. (Escalated to the
        // lead as the P2 fork; option (a) of the three on record.)
        if let Some(exposure) = skeleton
            .exposures
            .iter_mut()
            .find(|e| e.node_id.as_str() == "fog_enabled")
        {
            for m in &mut exposure.metadata {
                if m.name == "value" {
                    m.is_toggle = true;
                }
            }
        }

        Some(SceneModifierPlan {
            kind_id: FOG_KIND_ID.to_string(),
            display_name: "Scene Fog".to_string(),
            trace: skeleton.trace,
            new_nodes,
            new_wires,
            group_splices: Vec::new(),
            repoints: skeleton.repoints,
            exposures: skeleton.exposures,
            // Gate kinds (D5): the toggle row writes the enabled value
            // atom's `value` param; the multiply IS the bypass wiring, so
            // there are no enable extras beyond the atoms themselves.
            enable: EnablePlan {
                toggle: ToggleDecl::ValueAtom { node_id: NodeId::new("fog_enabled") },
                extra_nodes: Vec::new(),
                extra_wires: Vec::new(),
            },
        })
    }

    fn value_params(value: f32) -> std::collections::BTreeMap<String, SerializedParamValue> {
        let mut params = std::collections::BTreeMap::new();
        f32_param(&mut params, "value", value);
        params
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::node_graph::primitives::{Math, Value};
        use crate::node_graph::{Executor, FrameTime, Graph, ParamValue, compile};
        use manifold_core::{Beats, Seconds};

        fn frame_time() -> FrameTime {
            FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(0.0),
                delta: Seconds(1.0 / 60.0),
                frame_count: 0,
            }
        }

        /// A bare scene: just the render_scene node — fog splices nothing
        /// and re-points nothing, so the plan's only graph touch is the
        /// atmosphere wire.
        fn minimal_scene_def() -> EffectGraphDef {
            EffectGraphDef {
                version: 1,
                name: None,
                description: None,
                preset_metadata: None,
                nodes: vec![mint_node(1, "render", RENDER_SCENE_TYPE_ID, Default::default())],
                wires: vec![],
            }
        }

        /// Records the scalar it sees on its `in` port (the value.rs test
        /// sink pattern — a real wiring proof without a render).
        struct ScalarSink {
            type_id: crate::node_graph::effect_node::EffectNodeType,
            seen: std::sync::Arc<std::sync::Mutex<Option<ParamValue>>>,
        }
        impl crate::node_graph::effect_node::EffectNode for ScalarSink {
            fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
                crate::node_graph::depth_rule::DepthRule::Terminal
            }
            fn type_id(&self) -> &crate::node_graph::effect_node::EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[crate::node_graph::ports::NodeInput] {
                static INPUTS: [crate::node_graph::ports::NodePort; 1] =
                    [crate::node_graph::ports::NodePort {
                        name: std::borrow::Cow::Borrowed("in"),
                        ty: crate::node_graph::ports::PortType::Scalar(
                            crate::node_graph::ports::ScalarType::F32,
                        ),
                        kind: crate::node_graph::ports::PortKind::Input,
                        required: true,
                    }];
                &INPUTS
            }
            fn outputs(&self) -> &[crate::node_graph::ports::NodeOutput] {
                &[]
            }
            fn parameters(&self) -> &[crate::node_graph::parameters::ParamDef] {
                &[]
            }
            fn evaluate(
                &mut self,
                ctx: &mut crate::node_graph::effect_node::EffectNodeContext<'_, '_>,
            ) {
                *self.seen.lock().unwrap() = ctx.inputs.scalar("in");
            }
        }

        /// The D5 gate, value-level: instantiate the minted control chain
        /// (fog_enabled, fog_amount, fog_mul) on a runtime graph wired
        /// EXACTLY as the plan wires it, seeded with the plan's own param
        /// values, and read fog_mul's output through the sink. Applied
        /// (enabled = 1) → the amount passes through; the INV-M7 toggle
        /// write (enabled = 0) → exactly 0, the fog off.
        #[test]
        fn gate_bypass_multiplies_enabled_by_amount() {
            let def = minimal_scene_def();
            let plan = build_scene_fog_plan(&def, 1).expect("plan builds");

            let mut g = Graph::new();
            let mut handles: std::collections::BTreeMap<u32, _> = Default::default();
            for n in &plan.new_nodes {
                if n.type_id == "node.value" {
                    handles.insert(n.id, g.add_node(Box::new(Value::new())));
                } else if n.type_id == "node.math" {
                    handles.insert(n.id, g.add_node(Box::new(Math::new())));
                }
            }
            for n in &plan.new_nodes {
                if let Some(&h) = handles.get(&n.id) {
                    for (k, v) in &n.params {
                        let pv = match v {
                            SerializedParamValue::Float { value } => ParamValue::Float(*value),
                            SerializedParamValue::Enum { value } => ParamValue::Enum(*value),
                            other => panic!("unexpected seeded param {k}: {other:?}"),
                        };
                        g.set_param(h, k, pv).unwrap();
                    }
                }
            }
            for w in &plan.new_wires {
                if let (Some(&f), Some(&t)) =
                    (handles.get(&w.from_node), handles.get(&w.to_node))
                {
                    // Graph::connect stores the port names ('static) — the
                    // plan's strings outlive the test via the leak.
                    let from_port: &'static str = Box::leak(w.from_port.clone().into_boxed_str());
                    let to_port: &'static str = Box::leak(w.to_port.clone().into_boxed_str());
                    g.connect((f, from_port), (t, to_port)).unwrap();
                }
            }
            let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
            let sink = g.add_node(Box::new(ScalarSink {
                type_id: crate::node_graph::effect_node::EffectNodeType::new("test.scalar_sink"),
                seen: seen.clone(),
            }));
            let mul_doc = plan
                .new_nodes
                .iter()
                .find(|n| n.node_id.as_str() == "fog_mul")
                .map(|n| n.id)
                .unwrap();
            g.connect((handles[&mul_doc], "out"), (sink, "in")).unwrap();

            // Applied enabled: fog_mul.out == 1.0 × FOG_AMOUNT_DEFAULT.
            {
                let plan = compile(&g).unwrap();
                let mut exec = Executor::with_mock();
                exec.execute_frame(&mut g, &plan, frame_time());
            }
            let observed = seen.lock().unwrap().clone();
            assert!(
                matches!(observed, Some(ParamValue::Float(f)) if (f - FOG_AMOUNT_DEFAULT).abs() < 1e-6),
                "an enabled gate must pass the amount through, got {observed:?}"
            );

            // Toggle OFF (the INV-M7 write): 0 × amount == 0 exactly.
            let enabled_doc = plan
                .new_nodes
                .iter()
                .find(|n| n.node_id.as_str() == "fog_enabled")
                .map(|n| n.id)
                .unwrap();
            g.set_param(handles[&enabled_doc], "value", ParamValue::Float(0.0))
                .unwrap();
            {
                let plan = compile(&g).unwrap();
                let mut exec = Executor::with_mock();
                exec.execute_frame(&mut g, &plan, frame_time());
            }
            let observed = seen.lock().unwrap().clone();
            assert!(
                matches!(observed, Some(ParamValue::Float(f)) if f == 0.0),
                "a disabled gate must drive fog_density to exactly 0, got {observed:?}"
            );
        }

        /// Row curation: the Enabled row's stamped metadata carries
        /// is_toggle (D5 — node.value's manifest param is Float, so the kind
        /// flips it), the Density row stays a plain slider, and fog_atm
        /// stamps no rows at all (INV-M3 whitelist-exactness).
        #[test]
        fn enabled_row_stamps_toggle_metadata() {
            let def = minimal_scene_def();
            let plan = build_scene_fog_plan(&def, 1).expect("plan builds");

            let enabled = plan
                .exposures
                .iter()
                .find(|e| e.node_id.as_str() == "fog_enabled")
                .expect("fog_enabled exposure");
            let enabled_meta = enabled
                .metadata
                .iter()
                .find(|m| m.name == "value")
                .expect("value metadata");
            assert!(enabled_meta.is_toggle, "the Enabled row must render as a toggle");
            assert_eq!(enabled_meta.label, "Enabled");

            let amount = plan
                .exposures
                .iter()
                .find(|e| e.node_id.as_str() == "fog_amount")
                .expect("fog_amount exposure");
            let amount_meta = amount
                .metadata
                .iter()
                .find(|m| m.name == "value")
                .expect("value metadata");
            assert!(!amount_meta.is_toggle, "the Density row is a slider");
            assert_eq!(amount_meta.label, "Density");

            assert!(
                !plan.exposures.iter().any(|e| e.node_id.as_str() == "fog_atm"),
                "fog_atm is not in the whitelist — it stamps no rows"
            );
        }

        /// The K3 P2 amendment: ambient_tint is stamped explicitly at the
        /// neutral (1,1,1) so a future atmosphere default change can't tint
        /// the bypass path.
        #[test]
        fn ambient_tint_pinned_to_neutral_default() {
            let def = minimal_scene_def();
            let plan = build_scene_fog_plan(&def, 1).expect("plan builds");
            let fog_atm = plan
                .new_nodes
                .iter()
                .find(|n| n.node_id.as_str() == "fog_atm")
                .expect("fog_atm minted");
            for channel in ["ambient_tint_r", "ambient_tint_g", "ambient_tint_b"] {
                assert_eq!(
                    fog_atm.params.get(channel),
                    Some(&SerializedParamValue::Float { value: 1.0 }),
                    "{channel} must be pinned at the neutral default"
                );
            }
        }
    }
}
