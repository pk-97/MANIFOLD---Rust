//! Shared cinematic tail for native and imported scenes: the DoF chain (`coc_from_depth` →
//! `bokeh_gather`) plus the velocity-directed `motion_blur`,
//! templated node-for-node on the CinematicScene reference preset.
//! Reinstated after the 2026-07-12 SSAO-only carve-out once BUG-136 (motion
//! blur no visible effect) was root-caused in P0 of that design: never a
//! code defect — the playing layers simply lacked the chain. Extracted from
//! `scene.rs` (god-file ceiling, `godfile_regrowth.rs`).

use manifold_core::effect_graph_def::{
    EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID,
    GroupDef, GroupInterface, InterfacePortDef,
};

use crate::node_graph::scene_graph::{bool_val, enum_val, float, plain_node, wire};

/// The tail's products: the assembled `dof` group node and the top-level
/// `motion_blur` node (in `nodes`, push-order preserved), plus their ids
/// for the caller's spine wiring (`ao → dof → motion_blur → final`; the
/// shared lens feeds `dof.camera` and `motion_blur.camera`).
pub(crate) struct CinematicTail {
    pub nodes: Vec<EffectGraphNode>,
    pub dof_group_id: u32,
    pub motion_blur_id: u32,
    pub bokeh_id: u32,
}

/// Build the DoF group + motion_blur node with neutral lens-era params
/// (CoC/bokeh `max_radius` = 24 — the CinematicScene value; motion blur
/// keeps its node default cap). The caller chooses the initial enabled state and wires the shared
/// lens in, so depth-of-field
/// and shutter read the SAME lens the exposure and FOV card knob surface.
///
/// `scene_radius` is the imported bbox bounding-sphere radius (BUG-bdwd):
/// the CoC node gets `world_to_mm = 1000/radius` so the scene's model units
/// read as real meter-scale distances in the lens physics (a unit-scale
/// scene reads 1 unit = 1 meter, the old constant — a 0.01-unit scene reads
/// 1 unit = 100m).
pub(crate) fn build_cinematic_tail(
    fresh_id: &mut impl FnMut() -> u32,
    scene_radius: f32,
    enabled: bool,
) -> CinematicTail {
    let mut dof_nodes: Vec<EffectGraphNode> = Vec::new();
    let mut dof_wires: Vec<EffectGraphWire> = Vec::new();
    let dof_in_id = fresh_id();
    dof_nodes.push(plain_node(
        dof_in_id,
        "dof_in",
        GROUP_INPUT_TYPE_ID,
        "input",
    ));
    // Focus follows what the camera looks at unless the card switches it to
    // manual; only depth of field reads focus.
    let focus_id = fresh_id();
    dof_nodes.push(plain_node(focus_id, "focus", "node.auto_focus", "focus"));
    let coc_id = fresh_id();
    let mut coc_node = plain_node(coc_id, "coc", "node.coc_from_depth", "coc");
    coc_node
        .params
        .insert("max_radius".to_string(), float(24.0));
    // world_to_mm = 1000 / scene_radius, floored so a tiny/degenerate bbox
    // can't produce an absurd calibration (>100,000 mm/unit). This is the
    // plumbing that makes musical f-stops work at any scene scale.
    coc_node.params.insert(
        "world_to_mm".to_string(),
        float((1000.0 / scene_radius).min(100_000.0)),
    );
    dof_nodes.push(coc_node);
    let bokeh_id = fresh_id();
    let mut bokeh_node = plain_node(bokeh_id, "bokeh", "node.bokeh_gather", "bokeh");
    bokeh_node
        .params
        .insert("max_radius".to_string(), float(24.0));
    bokeh_node
        .params
        .insert("enabled".to_string(), bool_val(enabled));
    bokeh_node
        .params
        .insert("aperture".to_string(), enum_val(0));
    bokeh_node.params.insert("quality".to_string(), enum_val(1));
    bokeh_node
        .params
        .insert("blur_alpha".to_string(), bool_val(true));
    dof_nodes.push(bokeh_node);
    let dof_out_id = fresh_id();
    dof_nodes.push(plain_node(
        dof_out_id,
        "dof_out",
        GROUP_OUTPUT_TYPE_ID,
        "output",
    ));
    dof_wires.push(wire(dof_in_id, "depth", coc_id, "depth"));
    dof_wires.push(wire(dof_in_id, "camera", focus_id, "camera"));
    dof_wires.push(wire(dof_in_id, "depth", focus_id, "depth"));
    dof_wires.push(wire(focus_id, "out", coc_id, "camera"));
    dof_wires.push(wire(coc_id, "out", bokeh_id, "width"));
    dof_wires.push(wire(dof_in_id, "color", bokeh_id, "in"));
    dof_wires.push(wire(bokeh_id, "out", dof_out_id, "out"));

    let dof_group_id = fresh_id();
    let mut dof_group_node = plain_node(dof_group_id, "dof", GROUP_TYPE_ID, "dof");
    dof_group_node.title = Some("Depth of Field".to_string());
    dof_group_node.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: vec![
                InterfacePortDef {
                    name: "depth".to_string(),
                    port_type: "Texture2D".to_string(),
                },
                InterfacePortDef {
                    name: "camera".to_string(),
                    port_type: "Camera".to_string(),
                },
                InterfacePortDef {
                    name: "color".to_string(),
                    port_type: "Texture2D".to_string(),
                },
            ],
            outputs: vec![InterfacePortDef {
                name: "out".to_string(),
                port_type: "Texture2D".to_string(),
            }],
            params: Vec::new(),
        },
        nodes: dof_nodes,
        wires: dof_wires,
        tint: None,
    }));

    // One full-res `node.motion_blur` dispatch at the end of the chain,
    // exactly as CinematicScene ships it.
    let motion_blur_id = fresh_id();
    let mut motion_blur_node = plain_node(
        motion_blur_id,
        "motion_blur",
        "node.motion_blur",
        "motion_blur",
    );
    motion_blur_node
        .params
        .insert("enabled".to_string(), bool_val(enabled));

    CinematicTail {
        nodes: vec![dof_group_node, motion_blur_node],
        dof_group_id,
        motion_blur_id,
        bokeh_id,
    }
}
