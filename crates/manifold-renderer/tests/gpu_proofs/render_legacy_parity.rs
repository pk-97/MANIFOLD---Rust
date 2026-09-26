//! Shared-material parity proof for the legacy single-mesh and instanced
//! adapters.  All three graphs use the same mesh, camera, light, environment,
//! and nonzero extension factors; only the owning renderer node differs.

use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

fn identity_instances(id: u32) -> String {
    format!(
        r#"{{"id":{id},"typeId":"node.arrange_copies","nodeId":"identity","params":{{
            "max_capacity":{{"type":"Int","value":1}},
            "active_count":{{"type":"Int","value":1}},
            "layout":{{"type":"Enum","value":1}},
            "seed":{{"type":"Int","value":0}},
            "extent_x":{{"type":"Float","value":0.0}},
            "extent_y":{{"type":"Float","value":0.0}},
            "extent_z":{{"type":"Float","value":0.0}},
            "base_scale":{{"type":"Float","value":1.0}},
            "rot_x":{{"type":"Float","value":0.0}},
            "rot_y":{{"type":"Float","value":0.0}},
            "rot_z":{{"type":"Float","value":0.0}}}}}}"#
    )
}

fn shared_nodes() -> String {
    r#"{"id":0,"typeId":"system.generator_input","nodeId":"input"},
    {"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{
        "max_capacity":{"type":"Int","value":1024},
        "resolution_x":{"type":"Int","value":16},
        "resolution_y":{"type":"Int","value":16},
        "size_x":{"type":"Float","value":4.0},
        "size_y":{"type":"Float","value":4.0}}},
    {"id":2,"typeId":"node.make_triangles","nodeId":"tris","params":{
        "src_cols":{"type":"Int","value":16},"src_rows":{"type":"Int","value":16}}},
    {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
        "orbit":{"type":"Float","value":0.4},"tilt":{"type":"Float","value":0.7},
        "distance":{"type":"Float","value":6.0},"fov_y":{"type":"Float","value":1.0}}},
    {"id":4,"typeId":"node.pbr_material","nodeId":"mat","params":{
        "color_r":{"type":"Float","value":0.72},"color_g":{"type":"Float","value":0.38},
        "color_b":{"type":"Float","value":0.16},"color_a":{"type":"Float","value":0.82},
        "metallic":{"type":"Float","value":0.12},"roughness":{"type":"Float","value":0.34},
        "alpha_mode":{"type":"Enum","value":2},"clearcoat":{"type":"Float","value":0.35},
        "clearcoat_roughness":{"type":"Float","value":0.22},
        "sheen_color_r":{"type":"Float","value":0.24},"sheen_color_g":{"type":"Float","value":0.11},
        "sheen_color_b":{"type":"Float","value":0.06},"sheen_roughness":{"type":"Float","value":0.38},
        "anisotropy_strength":{"type":"Float","value":0.45},"anisotropy_rotation":{"type":"Float","value":0.2},
        "transmission":{"type":"Float","value":0.18},"volume_thickness":{"type":"Float","value":0.2},
        "iridescence":{"type":"Float","value":0.3},"iridescence_ior":{"type":"Float","value":1.3},
        "iridescence_thickness_min":{"type":"Float","value":120.0},
        "iridescence_thickness_max":{"type":"Float","value":360.0}}},
    {"id":5,"typeId":"node.linear_gradient","nodeId":"env_src","params":{
        "cx":{"type":"Float","value":-5.0},"softness":{"type":"Float","value":0.0}}},
    {"id":6,"typeId":"node.gradient_map","nodeId":"env","params":{
        "color_a":{"type":"Color","value":[1.0,1.0,1.0,1.0]},
        "color_b":{"type":"Color","value":[1.0,1.0,1.0,1.0]}}},
    {"id":7,"typeId":"node.light","nodeId":"sun","params":{
        "mode":{"type":"Enum","value":0},"pos_x":{"type":"Float","value":0.0},
        "pos_y":{"type":"Float","value":30.0},"pos_z":{"type":"Float","value":0.0},
        "aim_x":{"type":"Float","value":0.0},"aim_y":{"type":"Float","value":0.0},
        "aim_z":{"type":"Float","value":0.0},"intensity":{"type":"Float","value":5.0},
        "cast_shadows":{"type":"Float","value":0.0}}}"#.to_owned()
}

fn render_graph(kind: &str) -> String {
    let mut nodes = shared_nodes();
    let mut wires = String::from(
        r#"{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":5,"fromPort":"out","toNode":6,"toPort":"source"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":6,"fromPort":"out","toNode":20,"toPort":"envmap"},
        {"fromNode":7,"fromPort":"out","toNode":20,"toPort":"light_0"},"#,
    );

    let render_node = match kind {
        "mesh" => {
            wires.push_str(r#"{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"vertices"},"#);
            r#"{"id":20,"typeId":"node.render_mesh","nodeId":"render","params":{}},"#.to_owned()
        }
        "copies" => {
            nodes.push(',');
            nodes.push_str(&identity_instances(8));
            wires.push_str(
                r#"{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"vertices"},
                {"fromNode":8,"fromPort":"instances","toNode":20,"toPort":"instances"},"#,
            );
            r#"{"id":20,"typeId":"node.render_copies","nodeId":"render","params":{"instance_count":{"type":"Int","value":1}}},"#.to_owned()
        }
        "scene" => {
            wires.push_str(r#"{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},"#);
            r#"{"id":20,"typeId":"node.render_scene","nodeId":"render","params":{"objects":{"type":"Int","value":1},"lights":{"type":"Int","value":1}}},"#.to_owned()
        }
        other => panic!("unknown parity graph {other}"),
    };
    if kind != "scene" {
        wires = wires
            .replace("material_0", "material")
            .replace("light_0", "light");
    }
    nodes.push(',');
    nodes.push_str(&render_node);
    nodes.push_str(r#"{"id":99,"typeId":"system.final_output","nodeId":"out"}"#);
    wires.push_str(r#"{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}"#);
    format!(r#"{{"version":2,"name":"LegacyMaterialParity","nodes":[{nodes}],"wires":[{wires}]}}"#)
}

fn render_readback(json: &str) -> Vec<u8> {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|e| panic!("legacy parity graph must build: {e}\n{json}"));
    let target = h.make_target("legacy-material-parity");
    for frame in 0..2 {
        let ctx = PresetContext {
            time: 0.1,
            beat: 0.2,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("legacy-material-parity");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }
    h.readback(&target.texture)
}

#[test]
fn parity_graphs_are_valid_json() {
    for kind in ["mesh", "copies", "scene"] {
        serde_json::from_str::<serde_json::Value>(&render_graph(kind))
            .unwrap_or_else(|e| panic!("{kind} parity graph JSON must parse: {e}"));
    }
}

#[test]
fn legacy_mesh_and_identity_copies_match_shared_scene_materials() {
    let mesh = render_readback(&render_graph("mesh"));
    let copies = render_readback(&render_graph("copies"));
    let scene = render_readback(&render_graph("scene"));
    assert!(scene.chunks_exact(8).any(|p| half::f16::from_le_bytes([p[2], p[3]]).to_f32() > 0.03),
        "control must shade visible geometry, not an empty or error clear");
    assert!(mesh == scene, "render_mesh differs from shared scene at byte {:?}",
        mesh.iter().zip(&scene).position(|(a,b)| a != b));
    assert!(copies == scene, "identity render_copies differs from shared scene at byte {:?}",
        copies.iter().zip(&scene).position(|(a,b)| a != b));
}
