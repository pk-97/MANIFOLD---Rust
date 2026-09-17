//! First-frame current-geometry dispatch proof for SCENE_MODIFIER_RT_DESIGN P5.
//!
//! This is a dispatch and frame-validity proof only: it does not claim
//! numerical ray-hit or image correctness. A tiny generated mesh is rendered
//! through the production `PresetRuntime` path exactly once, and the test
//! requires both a complete frame status and a real RT capture produced by
//! `render_scene`'s trace branch.

use manifold_gpu::GpuTextureFormat;
use manifold_renderer::frame_status::FrameRenderStatus;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

fn scene_json() -> &'static str {
    r#"{"version":2,"name":"RtDynamicCurrentFrame","nodes":[
        {"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{
            "max_capacity":{"type":"Int","value":16},
            "resolution_x":{"type":"Int","value":2},
            "resolution_y":{"type":"Int","value":2},
            "size_x":{"type":"Float","value":2.0},
            "size_y":{"type":"Float","value":2.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"triangles","params":{
            "src_cols":{"type":"Int","value":2},
            "src_rows":{"type":"Int","value":2}}},
        {"id":3,"typeId":"node.phong_material","nodeId":"material","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "ambient":{"type":"Float","value":0.05}}},
        {"id":4,"typeId":"node.scene_object","nodeId":"object"},
        {"id":5,"typeId":"node.orbit_camera","nodeId":"camera","params":{
            "orbit":{"type":"Float","value":0.7},
            "tilt":{"type":"Float","value":0.95},
            "distance":{"type":"Float","value":6.0},
            "fov_y":{"type":"Float","value":0.8}}},
        {"id":6,"typeId":"node.light","nodeId":"sun","params":{
            "mode":{"type":"Enum","value":0},
            "pos_y":{"type":"Float","value":10.0},
            "aim_y":{"type":"Float","value":0.0},
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "intensity":{"type":"Float","value":1.0},
            "cast_shadows":{"type":"Float","value":1.0}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":1},
            "rt_enabled":{"type":"Bool","value":true}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}
    ],"wires":[
        {"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":4,"toPort":"vertices"},
        {"fromNode":3,"fromPort":"out","toNode":4,"toPort":"material"},
        {"fromNode":4,"fromPort":"object","toNode":20,"toPort":"object_0"},
        {"fromNode":5,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":6,"fromPort":"out","toNode":20,"toPort":"light_0"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}
    ]}"#
}

#[test]
fn rt_dynamic_current_frame_first_frame_dispatches() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        scene_json(),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("current-frame RT scene graph must build");
    let target = h.make_target("rt-dynamic-current-frame");
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
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let mut status = None;
    let captures = harness::capture_rt_channels(|| {
        let mut enc = h.device.create_encoder("rt-dynamic-current-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
            status = Some(gpu.frame_status());
        }
        enc.commit_and_wait_completed();
    });

    assert_eq!(
        status,
        Some(FrameRenderStatus::Complete),
        "the first current-frame RT update must produce a valid frame"
    );
    assert!(
        !captures.is_empty(),
        "the first evaluated frame must produce real RT captures"
    );
}
