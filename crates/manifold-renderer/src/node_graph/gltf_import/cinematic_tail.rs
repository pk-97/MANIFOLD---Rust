//! Cinematic tail for imported scenes (CINEMATIC_SCENE_TAIL_DESIGN.md D1 /
//! section 3 (chain topology)): the polished DoF chain (`coc_from_depth →
//! `coc_dilate` → `bokeh_gather`) plus the velocity-directed `motion_blur`,
//! templated node-for-node on the CinematicScene reference preset.
//! Reinstated after the 2026-07-12 SSAO-only carve-out once BUG-136 (motion
//! blur no visible effect) was root-caused in P0 of that design: never a
//! code defect — the playing layers simply lacked the chain. Extracted from
//! `scene.rs` (god-file ceiling, `godfile_regrowth.rs`).

use manifold_core::effect_graph_def::{
    EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID,
    GroupDef, GroupInterface, InterfacePortDef,
};

use super::assembly::{float, plain_node, wire};

/// The tail's products: the assembled `dof` group node and the top-level
/// `motion_blur` node (in `nodes`, push-order preserved), plus their ids
/// for the caller's spine wiring (`ao → dof → motion_blur → final`; the
/// shared lens feeds `dof.camera` and `motion_blur.camera`).
pub(super) struct CinematicTail {
    pub nodes: Vec<EffectGraphNode>,
    pub dof_group_id: u32,
    pub motion_blur_id: u32,
}

/// Build the DoF group + motion_blur node with neutral lens-era params
/// (CoC/bokeh `max_radius` = 24, `max_blur_px` = 32 — the CinematicScene
/// values). The caller wires the shared lens in, so depth-of-field and
/// shutter read the SAME lens the exposure and FOV card knob surface.
pub(super) fn build_cinematic_tail(fresh_id: &mut impl FnMut() -> u32) -> CinematicTail {
    let mut dof_nodes: Vec<EffectGraphNode> = Vec::new();
    let mut dof_wires: Vec<EffectGraphWire> = Vec::new();
    let dof_in_id = fresh_id();
    dof_nodes.push(plain_node(dof_in_id, "dof_in", GROUP_INPUT_TYPE_ID, "input"));
    let coc_id = fresh_id();
    let mut coc_node = plain_node(coc_id, "coc", "node.coc_from_depth", "coc");
    coc_node.params.insert("max_radius".to_string(), float(24.0));
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

    CinematicTail {
        nodes: vec![dof_group_node, motion_blur_node],
        dof_group_id,
        motion_blur_id,
    }
}
