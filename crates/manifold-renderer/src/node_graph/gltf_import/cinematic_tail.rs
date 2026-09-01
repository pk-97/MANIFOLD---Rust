//! Cinematic tail for imported scenes (CINEMATIC_SCENE_TAIL_DESIGN.md D1 /
//! section 3 (chain topology)): the polished DoF chain (`coc_from_depth →
//! `coc_dilate` → `bokeh_gather`) plus the velocity-directed `motion_blur`,
//! templated node-for-node on the CinematicScene reference preset.
//! Reinstated after the 2026-07-12 SSAO-only carve-out once BUG-136 (motion
//! blur no visible effect) was root-caused in P0 of that design: never a
//! code defect — the playing layers simply lacked the chain. Extracted from
//! `scene.rs` (god-file ceiling, `godfile_regrowth.rs`).

use manifold_core::effect_graph_def::{
    BindingDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID,
    GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef, ParamSpecDef, SerializedParamValue,
};
use manifold_core::NodeId;
use manifold_core::scene_exposure::stamp_scene_node_exposures_into;

use super::assembly::{float, plain_node, wire};
use crate::node_graph::scene_exposure::metadata_for_node_type;

/// The tail's products: the assembled `dof` group node and the top-level
/// `motion_blur` node (in `nodes`, push-order preserved), plus their ids
/// for the caller's spine wiring (`ao → dof → motion_blur → final`; the
/// shared lens feeds `dof.camera` and `motion_blur.camera`). `bokeh_id`
/// and `motion_blur_params` feed the caller's Camera-section card stamps
/// (P4): the bokeh stamp targets the group-internal `dof/bokeh` nodeId,
/// and the motion_blur stamp seeds its slider defaults from the node's
/// stamped params.
pub(super) struct CinematicTail {
    pub nodes: Vec<EffectGraphNode>,
    pub dof_group_id: u32,
    pub motion_blur_id: u32,
    pub bokeh_id: u32,
    pub motion_blur_params: std::collections::BTreeMap<String, manifold_core::effect_graph_def::SerializedParamValue>,
    pub bokeh_params: std::collections::BTreeMap<String, manifold_core::effect_graph_def::SerializedParamValue>,
}

/// Build the DoF group + motion_blur node with neutral lens-era params
/// (CoC/bokeh `max_radius` = 24, `max_blur_px` = 32 — the CinematicScene
/// values), bokeh `enabled = false` (2026-08-27: "DoF off" is the labeled
/// toggle, OFF by default — no magic big f-stop. The old f/1000-then-f/32
/// neutral seeds failed two ways: 1000 sat outside the slider band and the
/// stamper's widen stretched every f-stop slider to fit; 32 blurs visibly
/// on close-up scenes. Off-by-default also preserves every pre-tail
/// project's look). The caller wires the shared lens in, so depth-of-field
/// and shutter read the SAME lens the exposure and FOV card knob surface.
///
/// `scene_radius` is the imported bbox bounding-sphere radius (BUG-bdwd):
/// the CoC node gets `world_to_mm = 1000/radius` so the scene's model units
/// read as real meter-scale distances in the lens physics (a unit-scale
/// scene reads 1 unit = 1 meter, the old constant — a 0.01-unit scene reads
/// 1 unit = 100m; see `docs/CINEMATIC_POST_DESIGN.md` D1's `WORLD_TO_MM`).
pub(super) fn build_cinematic_tail(
    fresh_id: &mut impl FnMut() -> u32,
    scene_radius: f32,
) -> CinematicTail {
    let mut dof_nodes: Vec<EffectGraphNode> = Vec::new();
    let mut dof_wires: Vec<EffectGraphWire> = Vec::new();
    let dof_in_id = fresh_id();
    dof_nodes.push(plain_node(dof_in_id, "dof_in", GROUP_INPUT_TYPE_ID, "input"));
    let coc_id = fresh_id();
    let mut coc_node = plain_node(coc_id, "coc", "node.coc_from_depth", "coc");
    coc_node.params.insert("max_radius".to_string(), float(24.0));
    // world_to_mm = 1000 / scene_radius, floored so a tiny/degenerate bbox
    // can't produce an absurd calibration (>100,000 mm/unit). This is the
    // plumbing that makes musical f-stops work at any scene scale.
    coc_node
        .params
        .insert("world_to_mm".to_string(), float((1000.0 / scene_radius).min(100_000.0)));
    dof_nodes.push(coc_node);
    let coc_dilate_id = fresh_id();
    dof_nodes.push(plain_node(
        coc_dilate_id,
        "coc_dilate",
        "node.coc_dilate",
        "coc_dilate",
    ));
    let bokeh_id = fresh_id();
    let mut bokeh_node = plain_node(bokeh_id, "bokeh", "node.bokeh_gather", "bokeh");
    bokeh_node.params.insert("max_radius".to_string(), float(24.0));
    bokeh_node.params.insert("enabled".to_string(), super::assembly::bool_val(false));
    let bokeh_params = bokeh_node.params.clone();
    dof_nodes.push(bokeh_node);
    let dof_out_id = fresh_id();
    dof_nodes.push(plain_node(dof_out_id, "dof_out", GROUP_OUTPUT_TYPE_ID, "output"));
    dof_wires.push(wire(dof_in_id, "depth", coc_id, "depth"));
    dof_wires.push(wire(dof_in_id, "camera", coc_id, "camera"));
    dof_wires.push(wire(coc_id, "out", coc_dilate_id, "in"));
    dof_wires.push(wire(coc_dilate_id, "out", bokeh_id, "width"));
    dof_wires.push(wire(dof_in_id, "color", bokeh_id, "in"));
    dof_wires.push(wire(bokeh_id, "out", dof_out_id, "out"));

    let dof_group_id = fresh_id();
    let mut dof_group_node = plain_node(dof_group_id, "dof", GROUP_TYPE_ID, "dof");
    dof_group_node.title = Some("Depth of Field".to_string());
    dof_group_node.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: vec![
                InterfacePortDef { name: "depth".to_string(), port_type: "Texture2D".to_string() },
                InterfacePortDef { name: "camera".to_string(), port_type: "Camera".to_string() },
                InterfacePortDef { name: "color".to_string(), port_type: "Texture2D".to_string() },
            ],
            outputs: vec![InterfacePortDef { name: "out".to_string(), port_type: "Texture2D".to_string() }],
            params: Vec::new(),
        },
        nodes: dof_nodes,
        wires: dof_wires,
        tint: None,
    }));

    // One full-res `node.motion_blur` dispatch at the end of the chain,
    // exactly as CinematicScene ships it.
    let motion_blur_id = fresh_id();
    let mut motion_blur_node =
        plain_node(motion_blur_id, "motion_blur", "node.motion_blur", "motion_blur");
    motion_blur_node.params.insert("max_blur_px".to_string(), float(32.0));
    let motion_blur_params = motion_blur_node.params.clone();

    CinematicTail {
        nodes: vec![dof_group_node, motion_blur_node],
        dof_group_id,
        motion_blur_id,
        bokeh_id,
        motion_blur_params,
        bokeh_params,
    }
}

/// P4 (Peter): the tail's performance/character knobs surface on the
/// Camera card next to the lens rows — motion_blur's `max_blur_px` +
/// `enabled`, and bokeh's `enabled` (the DoF on/off). Bokeh lives inside
/// the `dof` group, but its `node_id` ("bokeh") survives flattening
/// verbatim (flatten.rs prefixes HANDLES only — "dof/bokeh" is the
/// handle; the nodeId safety invariant keeps the id), and both binding
/// resolution paths (build.rs's instance_by_node_id, bindings.rs's
/// identity match) key on the id. Only `enabled` is stamped for bokeh:
/// the radius slider stays deferred (f-stop is the photographic DoF
/// control).
pub(super) fn stamp_tail_camera_sections(
    card_params: &mut Vec<ParamSpecDef>,
    card_bindings: &mut Vec<BindingDef>,
    motion_blur_id: u32,
    bokeh_id: u32,
    motion_blur_params: &std::collections::BTreeMap<String, SerializedParamValue>,
    bokeh_params: &std::collections::BTreeMap<String, SerializedParamValue>,
) {
    stamp_scene_node_exposures_into(
        card_params,
        card_bindings,
        motion_blur_id,
        &NodeId::new("motion_blur"),
        "node.motion_blur",
        "Camera",
        &metadata_for_node_type("node.motion_blur"),
        motion_blur_params,
    );
    let bokeh_enabled_meta: Vec<_> = metadata_for_node_type("node.bokeh_gather")
        .into_iter()
        .filter(|m| m.name == "enabled")
        .collect();
    stamp_scene_node_exposures_into(
        card_params,
        card_bindings,
        bokeh_id,
        &NodeId::new("bokeh"),
        "node.bokeh_gather",
        "Camera",
        &bokeh_enabled_meta,
        bokeh_params,
    );
}
