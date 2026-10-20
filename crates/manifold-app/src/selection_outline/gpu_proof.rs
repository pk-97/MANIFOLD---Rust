//! D-102 after render: a scene object far from the camera, selected, as the
//! preview draws it: the scene's picture, the outline from its selection
//! mask, then the handles through the camera it rendered with. Writes
//! `/tmp/d102_gizmo_outline_after.png`.

use half::f16;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_nodes_scene::node_graph::scene_vm::{SceneVm, ShapeOwner};
use manifold_nodes_scene::node_graph::viewport_gizmo::{
    GizmoAxis, GizmoChrome, GizmoMode, GizmoView, HANDLE_VIEW_FRACTION, compose_lines, gizmo_targets, outline_slots,
};

use super::SelectionOutline;

const W: u32 = 960;
const H: u32 = 540;

/// A wide ground and a red panel (object 41, render slot 1) seen from 70 m.
const SCENE: &str = r#"{"version":2,"name":"D102OutlineAfter","nodes":[
  {"id":0,"typeId":"system.generator_input","nodeId":"input"},
  {"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{"max_capacity":{"type":"Int","value":8192},"resolution_x":{"type":"Int","value":20},"resolution_y":{"type":"Int","value":20},"size_x":{"type":"Float","value":60.0},"size_y":{"type":"Float","value":60.0}}},
  {"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{"src_cols":{"type":"Int","value":20},"src_rows":{"type":"Int","value":20}}},
  {"id":5,"typeId":"node.grid_mesh","nodeId":"panel_grid","params":{"max_capacity":{"type":"Int","value":8192},"resolution_x":{"type":"Int","value":10},"resolution_y":{"type":"Int","value":10},"size_x":{"type":"Float","value":3.0},"size_y":{"type":"Float","value":3.0}}},
  {"id":6,"typeId":"node.make_triangles","nodeId":"panel_tris","params":{"src_cols":{"type":"Int","value":10},"src_rows":{"type":"Int","value":10}}},
  {"id":7,"typeId":"node.transform_3d","nodeId":"panel_xform","params":{"pos_y":{"type":"Float","value":1.5}}},
  {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{"orbit":{"type":"Float","value":0.7},"tilt":{"type":"Float","value":0.6},"distance":{"type":"Float","value":70.0},"fov_y":{"type":"Float","value":0.8}}},
  {"id":4,"typeId":"node.cel_material","nodeId":"ground_mat","params":{"color_r":{"type":"Float","value":0.5},"color_g":{"type":"Float","value":0.55},"color_b":{"type":"Float","value":0.6},"band_low":{"type":"Float","value":0.05}}},
  {"id":8,"typeId":"node.cel_material","nodeId":"panel_mat","params":{"color_r":{"type":"Float","value":1.0},"color_g":{"type":"Float","value":0.1},"color_b":{"type":"Float","value":0.1},"band_low":{"type":"Float","value":0.05}}},
  {"id":30,"typeId":"node.light","nodeId":"sun","params":{"mode":{"type":"Enum","value":0},"pos_x":{"type":"Float","value":3.0},"pos_y":{"type":"Float","value":20.0},"pos_z":{"type":"Float","value":3.0},"color_r":{"type":"Float","value":1.0},"color_g":{"type":"Float","value":1.0},"color_b":{"type":"Float","value":1.0},"intensity":{"type":"Float","value":1.0}}},
  {"id":40,"typeId":"node.scene_object","nodeId":"obj0","params":{"visible":{"type":"Float","value":1.0}}},
  {"id":41,"typeId":"node.scene_object","nodeId":"obj1","params":{"visible":{"type":"Float","value":1.0}}},
  {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{"objects":{"type":"Int","value":2},"lights":{"type":"Int","value":1}}},
  {"id":99,"typeId":"system.final_output","nodeId":"out"}
],"wires":[
  {"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
  {"fromNode":2,"fromPort":"out","toNode":40,"toPort":"vertices"},
  {"fromNode":4,"fromPort":"out","toNode":40,"toPort":"material"},
  {"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"},
  {"fromNode":6,"fromPort":"out","toNode":41,"toPort":"vertices"},
  {"fromNode":8,"fromPort":"out","toNode":41,"toPort":"material"},
  {"fromNode":7,"fromPort":"transform","toNode":41,"toPort":"transform"},
  {"fromNode":40,"fromPort":"object","toNode":20,"toPort":"object_0"},
  {"fromNode":41,"fromPort":"object","toNode":20,"toPort":"object_1"},
  {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
  {"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"},
  {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}
]}"#;

fn target(device: &GpuDevice, label: &str) -> GpuTexture {
    device.create_texture(&manifold_gpu::GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: manifold_gpu::GpuTextureDimension::D2,
        usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
        label,
        mip_levels: 1,
    })
}

fn readback(device: &GpuDevice, texture: &GpuTexture) -> Vec<[f32; 4]> {
    let buf = device.create_buffer_shared(u64::from(W * H * 8));
    let mut enc = device.create_encoder("d102 readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, W * 8);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), (W * H * 8) as usize) };
    bytes
        .chunks_exact(8)
        .map(|px| std::array::from_fn(|c| f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32()))
        .collect()
}

#[test]
fn a_far_selected_object_shows_readable_handles_and_its_outline() {
    let device = std::sync::Arc::new(GpuDevice::new_queued("d102 after render"));
    let registry = PrimitiveRegistry::with_builtin();
    let def: EffectGraphDef = serde_json::from_str(SCENE).expect("scene json");
    let scene = SceneVm::from_def(&def).expect("a scene");
    let mut slots = Vec::new();
    outline_slots(&scene, 41, &mut slots);
    assert_eq!(slots, [1], "the panel draws in render slot 1");

    let mut runtime = PresetRuntime::from_json_str_with_device(SCENE, &registry, device.clone(), W, H, GpuTextureFormat::Rgba16Float, None)
        .expect("scene builds");
    let picture = target(&device, "d102 picture");
    for frame in 0..2 {
        runtime.set_selection_mask(&slots);
        let ctx = PresetContext {
            time: 0.1,
            beat: 0.2,
            dt: 1.0 / 60.0,
            width: W,
            height: H,
            output_width: W,
            output_height: H,
            aspect: W as f32 / H as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = device.create_encoder("d102 scene");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            runtime.render(&mut gpu, &picture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }
    let mut lights = Vec::new();
    let (_, camera) = runtime.write_rendered_view(&mut lights).expect("the scene reports its camera");
    let mask = runtime.selection_mask().expect("a selection draws a mask").clone();

    // The preview's chrome over the picture: outline, then the handles.
    let view = GizmoView { camera, width: W, height: H };
    let panel = gizmo_targets(&scene).find(|target| target.id.owner == ShapeOwner::Object(41)).expect("the panel's target");
    let mut lines = Vec::new();
    compose_lines(
        &GizmoChrome { view, lights: &lights, domains: &[], physics: &[], selected: Some(&panel), mode: GizmoMode::Move, fade_handles: false },
        &mut lines,
    );
    let composite = target(&device, "d102 composite");
    let mut enc = device.create_encoder("d102 chrome");
    enc.copy_texture_to_texture(&picture, &composite, W, H, 1);
    SelectionOutline::new(&device).draw(&mut enc, &composite, &mask, (0.0, 0.0, W as f32, H as f32), 1.0);
    let mut ui = manifold_ui_paint::ui_renderer::UIRenderer::new(&device, manifold_compositor::presentation::UI_FORMAT);
    ui.begin_frame();
    crate::scene_viewport::draw_world_lines(&mut ui, manifold_ui::Rect::new(0.0, 0.0, W as f32, H as f32), &camera, W, H, &lines);
    if ui.prepare(&device, W, H, 1.0) {
        ui.render(&mut enc, &composite, manifold_gpu::GpuLoadAction::Load);
    }
    enc.commit_and_wait_completed();

    let shown = readback(&device, &composite);
    let plain = readback(&device, &picture);
    // The outline colour where the picture had none.
    let outlined = shown
        .iter()
        .zip(&plain)
        .filter(|(s, p)| (s[0] - p[0]) > 0.3 && s[0] > 0.8 && s[1] > 0.45 && s[1] < 0.8 && s[2] < 0.35)
        .count();
    eprintln!("d102: outline pixels {outlined}");
    assert!(outlined > 40, "the outline shows round the far panel ({outlined} px)");
    let origin = camera.project_to_pixel(panel.origin, W, H).expect("origin on screen");
    let tip = camera.project_to_pixel(panel.grip(GizmoMode::Move, GizmoAxis::Y, &view).expect("move y"), W, H).expect("tip on screen");
    let length = (tip.px - origin.px).hypot(tip.py - origin.py);
    eprintln!("d102: move Y handle {length:.1} px of {H}");
    assert!(length > HANDLE_VIEW_FRACTION * H as f32 * 0.5, "the handle stays readable from 70 m: {length} px");

    let mut png = Vec::with_capacity((W * H * 4) as usize);
    for px in &shown {
        for (c, v) in px.iter().enumerate() {
            let v = if c == 3 { 1.0 } else { (v / (1.0 + v)).clamp(0.0, 1.0).powf(1.0 / 2.2) };
            png.push((v * 255.0).round() as u8);
        }
    }
    image::save_buffer("/tmp/d102_gizmo_outline_after.png", &png, W, H, image::ExtendedColorType::Rgba8).expect("write png");
}
