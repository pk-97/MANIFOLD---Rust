//! The output preview's navigated view through the content pipeline's real
//! per-frame order: `clear_preview` (the graph editor watches nothing), then
//! `set_scene_viewport_request`, then `render_all`. The viewport pass must be
//! built once and kept, so its ray-traced warmup finishes and the navigated
//! frame becomes presentable. A pass torn down every frame stays pending
//! forever and the performer sees the output camera instead.

use manifold_compositor::generator_renderer::GeneratorRenderer;
use manifold_core::clip::TimelineClip;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::project::Project;
use manifold_core::{Beats, LayerType, NodeId, PresetTypeId, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
use manifold_node_engine::scene::scene_viewport::SceneViewportConfig;
use manifold_node_engine::scene::viewport_camera::ViewportCamera;
use manifold_playback::renderer::ClipRenderer;

const W: u32 = 320;
const H: u32 = 200;
const FRAMES: usize = 24;

#[test]
fn navigated_frame_warms_up_under_the_pipeline_frame_order() {
    let device = manifold_gpu::testkit::test_device();
    let mut renderer =
        GeneratorRenderer::new_unwarmed(device.arc(), W, H, GpuTextureFormat::Rgba16Float, 0);
    let mut project = Project::default();
    project.settings.output_width = W as i32;
    project.settings.output_height = H as i32;
    let index = project.timeline.add_layer(
        "Navigated",
        LayerType::Generator,
        PresetTypeId::new("Scene"),
    );
    let layer = &mut project.timeline.layers[index];
    layer.gen_params_mut().unwrap().graph = Some(rt_scene());
    layer
        .clips
        .push(TimelineClip::new_generator(Beats(0.0), Beats(16.0)));
    let layer_id = layer.layer_id.clone();
    let clip = layer.clips[0].clone();
    assert!(
        renderer.start_clip(
            &clip,
            Seconds(0.0),
            &project.timeline.layers,
            index as i32,
            true
        ),
        "the scene layer starts"
    );

    let config = SceneViewportConfig {
        camera: ViewportCamera {
            pitch: -0.45,
            ..Default::default()
        },
        width: W,
        height: H,
    };
    let dt = 1.0 / 60.0;
    let mut first_complete = None;
    let mut presented = None;
    for frame in 0..FRAMES {
        renderer.clear_preview();
        renderer.set_scene_viewport_request(
            Some((layer_id.clone(), NodeId::new("scene"), config)),
            None,
        );
        let mut native_enc = device.create_encoder("scene viewport frame order");
        {
            let mut gpu = GpuEncoder::new(&mut native_enc, &device);
            renderer.render_all(
                &mut gpu,
                frame as f64 * dt as f64,
                0.0,
                dt,
                &project.timeline.layers,
                1,
                &[],
                None,
            );
        }
        native_enc.commit_and_wait_completed();

        let status = renderer
            .scene_viewport_status(&layer_id)
            .expect("the request stays accepted across frames");
        match (first_complete, status) {
            (None, FrameRenderStatus::Complete) => {
                first_complete = Some(frame);
                presented = renderer
                    .scene_viewport_texture(&layer_id)
                    .map(|texture| texture.raw_ptr());
                assert!(
                    presented.is_some(),
                    "a complete viewport frame is presentable"
                );
            }
            (None, FrameRenderStatus::PendingGeometry) => {}
            (Some(at), FrameRenderStatus::Complete) => assert_eq!(
                renderer
                    .scene_viewport_texture(&layer_id)
                    .map(|texture| texture.raw_ptr()),
                presented,
                "frame {frame}: the viewport pass was rebuilt after it completed at frame {at}"
            ),
            (_, other) => panic!("frame {frame}: viewport status {other:?}"),
        }
    }
    let at = first_complete.expect("the navigated frame never finished its ray-traced warmup");
    // A fresh pass's first frame waits on its acceleration build, so a pass
    // rebuilt every frame could never reach Complete: this proof can fail.
    assert!(
        at > 0,
        "the ray-traced warmup must take a pending frame for this proof to bite"
    );
    assert!(
        at < FRAMES - 4,
        "warmup finished too late to see the pass kept (frame {at})"
    );
}

/// A lit ground plane rendered through `render_scene` with ray tracing on,
/// so the viewport pass has an asynchronous acceleration build to finish.
fn rt_scene() -> EffectGraphDef {
    serde_json::from_str(
        &manifold_nodes_scene::testkit::scene_objects::bundle_scene_objects(
            r#"{"version":2,"name":"PreviewNavigationScene","nodes":[
        {"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{
            "max_capacity":{"type":"Int","value":8192},
            "resolution_x":{"type":"Int","value":20},
            "resolution_y":{"type":"Int","value":20},
            "size_x":{"type":"Float","value":8.0},
            "size_y":{"type":"Float","value":8.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{
            "src_cols":{"type":"Int","value":20},
            "src_rows":{"type":"Int","value":20}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"show_cam","params":{
            "orbit":{"type":"Float","value":0.7},
            "tilt":{"type":"Float","value":0.6},
            "distance":{"type":"Float","value":10.0},
            "fov_y":{"type":"Float","value":0.8}}},
        {"id":4,"typeId":"node.cel_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":0.8},
            "color_g":{"type":"Float","value":0.8},
            "color_b":{"type":"Float","value":0.9}}},
        {"id":5,"typeId":"node.light","nodeId":"sun","params":{
            "mode":{"type":"Enum","value":0},
            "pos_x":{"type":"Float","value":4.0},
            "pos_y":{"type":"Float","value":20.0},
            "pos_z":{"type":"Float","value":3.0},
            "intensity":{"type":"Float","value":1.0}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":1},
            "rt_enabled":{"type":"Bool","value":true}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}
    ],"wires":[
        {"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":5,"fromPort":"out","toNode":20,"toPort":"light_0"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}
    ]}"#,
        ),
    )
    .expect("viewport frame-order scene parses")
}
