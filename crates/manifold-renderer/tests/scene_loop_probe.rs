//! SCENE_LOOP_P2 copies proof (hand-built graphs): the fullest 4-way fan-out
//! splice — ONE `node.scene_array` feeding FOUR object groups, each a group
//! whose `node.scene_object` receives mesh/material through its own group body
//! and `instances` through the group-interface input — renders count=1 vs
//! count=3 DIFFERENTLY (diff > 40). This is the pixel-level proof that the
//! apply splice reaches the renderer on a REALISTIC topology (the import-shape
//! group + interface fan-out). The real-import gate (`scene_loop_e2e_import.rs`)
//! asserts the pipeline facts because that GLB renders near-black in the
//! throwaway headless runtime.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, PresetMetadata, SerializedParamValue,
};
use manifold_core::preset_type_id::PresetTypeId;
use manifold_renderer::node_graph::{PrimitiveRegistry, render_viewport_frame};
use manifold_renderer::preset_context::PresetContext;

fn node(
    id: u32,
    node_id: &str,
    type_id: &str,
    params: BTreeMap<String, SerializedParamValue>,
) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: manifold_core::NodeId::new(node_id),
        type_id: type_id.to_string(),
        handle: Some(node_id.to_string()),
        params,
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
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

fn render(def: &EffectGraphDef) -> Vec<u8> {
    let device = manifold_gpu::GpuDevice::new();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (64u32, 64u32);
    let ctx = PresetContext {
        time: 2.0,
        beat: 4.0,
        dt: 0.016,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: w as f32 / h as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let (rgba, _, _) = render_viewport_frame(
        def.clone(),
        &registry,
        std::sync::Arc::new(device),
        w,
        h,
        &ctx,
    )
    .expect("render_viewport_frame");
    rgba
}

fn max_pixel_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

/// ONE scene_array feeding FOUR import-style groups (mesh/material wired in
/// the group body, instances through the interface input) — the apply splice's
/// real fan-out shape.
fn fanout_loop_def(count: f32) -> EffectGraphDef {
    use manifold_core::effect_graph_def::{GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef};
    let groups: Vec<EffectGraphNode> = vec![50, 60, 70, 80]
        .into_iter()
        .map(|gid| {
            let bind = 900 + gid;
            let mesh = 910 + gid;
            let mat = 920 + gid;
            let gi = 930 + gid;
            let go = 940 + gid;
            EffectGraphNode {
                id: gid,
                node_id: manifold_core::NodeId::new(format!("object_group_{gid}")),
                type_id: GROUP_TYPE_ID.to_string(),
                handle: Some(format!("object_group_{gid}")),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: Some(Box::new(GroupDef {
                    interface: GroupInterface {
                        inputs: vec![InterfacePortDef {
                            name: "instances".to_string(),
                            port_type: "Array(InstanceTransform)".to_string(),
                        }],
                        outputs: vec![InterfacePortDef {
                            name: "object".to_string(),
                            port_type: "Object".to_string(),
                        }],
                        params: Vec::new(),
                    },
                    nodes: vec![
                        node(bind, &format!("obj_{gid}_bind"), "node.scene_object", BTreeMap::new()),
                        node(mesh, &format!("mesh_{gid}"), "node.cube_mesh", BTreeMap::new()),
                        node(mat, &format!("mat_{gid}"), "node.unlit_material", {
                            let mut p = BTreeMap::new();
                            p.insert(
                                "color_r".to_string(),
                                SerializedParamValue::Float { value: 0.5 + (gid % 3) as f32 * 0.2 },
                            );
                            p
                        }),
                        node(gi, &format!("gi_{gid}"), "system.group_input", BTreeMap::new()),
                        node(go, &format!("go_{gid}"), "system.group_output", BTreeMap::new()),
                    ],
                    wires: vec![
                        wire(mesh, "vertices", bind, "vertices"),
                        wire(mat, "out", bind, "material"),
                        wire(gi, "instances", bind, "instances"),
                        wire(bind, "object", go, "object"),
                    ],
                    tint: None,
                })),
            }
        })
        .collect();

    let mut nodes = vec![
        node(0, "input", "system.generator_input", BTreeMap::new()),
        node(1, "loop_phase", "node.beat_ramp", {
            let mut p = BTreeMap::new();
            p.insert("rate".to_string(), SerializedParamValue::Float { value: 0.125 });
            p.insert("attack".to_string(), SerializedParamValue::Float { value: 1.0 });
            p
        }),
        node(2, "scene_array", "node.scene_array", {
            let mut p = BTreeMap::new();
            p.insert("count".to_string(), SerializedParamValue::Float { value: count });
            p.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });
            p.insert("cell_size".to_string(), SerializedParamValue::Float { value: 10.0 });
            p
        }),
        node(3, "loop_camera", "node.loop_camera", {
            let mut p = BTreeMap::new();
            p.insert("cell_size".to_string(), SerializedParamValue::Float { value: 10.0 });
            p.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });
            p.insert("fov_y".to_string(), SerializedParamValue::Float { value: 0.9 });
            p
        }),
        node(7, "scene", "node.render_scene", {
            let mut p = BTreeMap::new();
            p.insert("objects".to_string(), SerializedParamValue::Float { value: 4.0 });
            p.insert("lights".to_string(), SerializedParamValue::Float { value: 0.0 });
            p
        }),
        node(8, "out", "system.final_output", BTreeMap::new()),
    ];
    nodes.extend(groups);
    let wires = vec![
        wire(1, "out", 3, "phase"),
        wire(3, "out", 7, "camera"),
        wire(2, "out", 50, "instances"),
        wire(2, "out", 60, "instances"),
        wire(2, "out", 70, "instances"),
        wire(2, "out", 80, "instances"),
        wire(50, "object", 7, "object_0"),
        wire(60, "object", 7, "object_1"),
        wire(70, "object", 7, "object_2"),
        wire(80, "object", 7, "object_3"),
        wire(7, "color", 8, "in"),
    ];

    EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: Some(PresetMetadata {
            id: PresetTypeId::from_string("ProbeFanout".to_string()),
            display_name: "ProbeFanout".to_string(),
            category: "Test".to_string(),
            osc_prefix: "probe".to_string(),
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
        }),
        nodes,
        wires,
    }
}

#[test]
fn fanout_scene_array_renders_copies() {
    let a = render(&fanout_loop_def(1.0));
    let b = render(&fanout_loop_def(3.0));
    let diff = max_pixel_diff(&a, &b);
    assert!(
        diff > 40,
        "fan-out loop: count=1 vs count=3 differ by only {diff} — the instance splice \
         does not reach the renderer on this topology"
    );
}