//! Gizmo line draw modes (BUG-b36b8): the one gizmo pass draws world lines
//! over a finished picture and tests them against the scene's own depth.
//!
//! Scene: `render_scene_object_visibility.rs`'s ground plane with the red
//! occluder hung above it. Green lines run over the ground, several of them
//! behind the occluder from the camera. Occluded must lose the hidden parts,
//! On Top must keep all of them, Ghost must keep them faded. A second render
//! pins the scene-level render mode (BUG-nn1ty): the `render_mode` param on
//! `render_scene` switches the shading with no modifier and no wire.
//!
//! Set `MANIFOLD_PROOF_OUT` to a directory to also write the PNGs.

use half::f16;
use manifold_foundation::scene_surface::GizmoDrawMode;
use manifold_gpu::BufferUse;
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_nodes_scene::node_graph::gizmo_pass::GizmoPass;
use manifold_nodes_scene::node_graph::viewport_overlay::{WorldLine, box_lines};

use super::render_scene_object_visibility::scene_json;

const W: u32 = 512;
const H: u32 = 512;
const GREEN: [u8; 4] = [40, 255, 40, 255];

struct Frame {
    color: RenderTarget,
    depth: Option<GpuTexture>,
    camera: manifold_node_engine::scene::camera::Camera,
}

fn render(scene: &str, gizmo_depth: bool) -> Frame {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &manifold_nodes_scene::testkit::scene_objects::bundle_scene_objects(scene),
        &registry,
        std::sync::Arc::clone(&h.device),
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("gizmo proof scene must build");
    let color = RenderTarget::new(
        &h.device,
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        "gizmo-proof-color",
    );
    for frame in 0..2 {
        runtime.set_gizmo_depth(gizmo_depth);
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
        let mut enc = h.device.create_encoder("gizmo-proof-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &color.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait().expect("GPU work completes");
    }
    let camera = runtime
        .write_rendered_view(&mut Vec::new())
        .expect("a rendered scene reports its camera")
        .1;
    Frame {
        color,
        depth: runtime.gizmo_depth().cloned(),
        camera,
    }
}

fn read(device: &GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    let bytes_per_row = texture.width * 8;
    let buf = device.create_buffer(u64::from(texture.height * bytes_per_row), BufferUse::Readback);
    let mut enc = device.create_encoder("gizmo-proof-readback");
    enc.copy_texture_to_buffer(texture, &buf, texture.width, texture.height, bytes_per_row);
    enc.commit_and_wait().expect("GPU work completes");
    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    unsafe {
        std::slice::from_raw_parts(ptr.cast::<u8>(), (texture.height * bytes_per_row) as usize)
    }
    .to_vec()
}

fn channels(bytes: &[u8]) -> Vec<[f32; 4]> {
    bytes
        .chunks_exact(8)
        .map(|px| std::array::from_fn(|c| f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32()))
        .collect()
}

fn save(name: &str, pixels: &[[f32; 4]]) {
    let Ok(dir) = std::env::var("MANIFOLD_PROOF_OUT") else {
        return;
    };
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|px| {
            [0, 1, 2]
                .map(|c| (px[c].clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0 + 0.5) as u8)
                .into_iter()
                .chain([255])
        })
        .collect();
    let path = std::path::Path::new(&dir).join(name);
    image::save_buffer(&path, &rgba, W, H, image::ColorType::Rgba8).expect("write proof png");
}

/// Green lines over the ground: a grid under the occluder, a post that
/// pierces it, so part of each line sits behind the red plane, and a box
/// whose corners show the edges meeting with no notch.
fn lines() -> Vec<WorldLine> {
    let mut lines = box_lines([1.4, 0.02, 1.4], [2.6, 1.2, 2.6], GREEN).to_vec();
    let mut add = |a: [f32; 3], b: [f32; 3]| {
        lines.push(WorldLine {
            a,
            b,
            color: GREEN,
            caps: false,
        })
    };
    for i in -2..=2 {
        let t = i as f32;
        add([t, 0.02, -3.0], [t, 0.02, 3.0]);
        add([-3.0, 0.02, t], [3.0, 0.02, t]);
    }
    add([0.0, 0.0, 0.0], [0.0, 3.0, 0.0]);
    lines
}

/// How far each pixel moved toward the line colour, summed over channels.
fn pulled(base: &[[f32; 4]], with: &[[f32; 4]]) -> Vec<f32> {
    base.iter()
        .zip(with)
        .map(|(a, b)| (0..3).map(|c| (a[c] - b[c]).abs()).sum())
        .collect()
}

fn with_lines(frame: &Frame, mode: GizmoDrawMode) -> Vec<[f32; 4]> {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let target = RenderTarget::new(
        &h.device,
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        "gizmo-proof-target",
    );
    let mut enc = h.device.create_encoder("gizmo-proof-draw");
    enc.copy_texture_to_texture(&frame.color.texture, &target.texture, W, H, 1);
    GizmoPass::default().draw(
        &h.device,
        &mut enc,
        &target.texture,
        frame.depth.as_ref(),
        &frame.camera,
        &lines(),
        mode,
    );
    enc.commit_and_wait().expect("GPU work completes");
    channels(&read(&h.device, &target.texture))
}

#[test]
fn gizmo_modes_hide_fade_or_keep_the_lines_behind_the_occluder() {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let frame = render(&scene_json(1.0), true);
    assert!(
        frame.depth.is_some(),
        "the scene keeps its depth for the gizmo pass when asked"
    );
    let base = channels(&read(&h.device, &frame.color.texture));
    save("gizmo_base.png", &base);

    let on_top = with_lines(&frame, GizmoDrawMode::OnTop);
    let occluded = with_lines(&frame, GizmoDrawMode::Occluded);
    let ghost = with_lines(&frame, GizmoDrawMode::Ghost);
    save("gizmo_on_top.png", &on_top);
    save("gizmo_occluded.png", &occluded);
    save("gizmo_ghost.png", &ghost);

    let strong = |p: &[f32]| p.iter().filter(|&&d| d > 0.9).count();
    let faint = |p: &[f32]| p.iter().filter(|&&d| d > 0.05).count();
    let (on_top, occluded, ghost) = (
        pulled(&base, &on_top),
        pulled(&base, &occluded),
        pulled(&base, &ghost),
    );
    eprintln!(
        "gizmo lines: strong on_top={} occluded={} ghost={}; touched on_top={} occluded={} ghost={}",
        strong(&on_top),
        strong(&occluded),
        strong(&ghost),
        faint(&on_top),
        faint(&occluded),
        faint(&ghost),
    );
    assert!(
        strong(&on_top) > 400,
        "On Top draws the lines ({} px)",
        strong(&on_top)
    );
    assert!(
        (faint(&occluded) as f32) < faint(&on_top) as f32 * 0.9,
        "Occluded loses the hidden parts: {} of {}",
        faint(&occluded),
        faint(&on_top),
    );
    assert!(
        (faint(&ghost) as f32) > faint(&on_top) as f32 * 0.97,
        "Ghost keeps the hidden parts: {} of {}",
        faint(&ghost),
        faint(&on_top),
    );
    assert!(
        (strong(&ghost) as f32) < strong(&on_top) as f32 * 0.9,
        "Ghost fades them: {} strong of {}",
        strong(&ghost),
        strong(&on_top),
    );
    // The visible parts are the same in Occluded and Ghost.
    let visible = |p: &[f32]| p.iter().filter(|&&d| d > 0.9).count() as f32;
    assert!((visible(&occluded) - visible(&ghost)).abs() < visible(&occluded) * 0.1 + 10.0);
}

#[test]
fn without_depth_every_mode_draws_on_top() {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let mut frame = render(&scene_json(1.0), false);
    assert!(frame.depth.is_none(), "no request, no depth kept");
    let base = channels(&read(&h.device, &frame.color.texture));
    frame.depth = None;
    let on_top = pulled(&base, &with_lines(&frame, GizmoDrawMode::OnTop));
    let occluded = pulled(&base, &with_lines(&frame, GizmoDrawMode::Occluded));
    assert_eq!(
        on_top, occluded,
        "no depth to test against, so nothing is hidden"
    );
}

#[test]
fn render_mode_param_switches_the_scene_shading_without_a_modifier() {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let mode = |mode: u32| {
        let mut scene: serde_json::Value = serde_json::from_str(&scene_json(1.0)).unwrap();
        let node = scene["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|n| n["id"] == 20)
            .unwrap();
        node["params"]["render_mode"] = serde_json::json!({"type": "Enum", "value": mode});
        channels(&read(
            &h.device,
            &render(&scene.to_string(), false).color.texture,
        ))
    };
    let rendered = mode(0);
    let wireframe = mode(2);
    save("render_mode_rendered.png", &rendered);
    save("render_mode_wireframe.png", &wireframe);
    let solid = mode(1);
    save("render_mode_solid.png", &solid);
    save("render_mode_points.png", &mode(3));
    let lit = |p: &[[f32; 4]]| p.iter().filter(|px| px[0] + px[1] + px[2] > 0.3).count();
    let magenta = |p: &[[f32; 4]]| {
        p.iter()
            .filter(|px| px[0] > 0.9 && px[1] < 0.1 && px[2] > 0.9)
            .count()
    };
    eprintln!(
        "render mode: lit rendered={} wireframe={} solid={}",
        lit(&rendered),
        lit(&wireframe),
        lit(&solid)
    );
    assert!(
        lit(&rendered) > 5000,
        "the rendered scene fills the picture"
    );
    assert!(lit(&wireframe) > 100, "wireframe draws lines");
    assert!(
        lit(&wireframe) * 2 < lit(&rendered),
        "and only lines, not surfaces"
    );
    assert_eq!(
        magenta(&solid),
        0,
        "Solid in a scene with no environment is clay, not the magenta fallback"
    );
    assert!(lit(&solid) > 5000, "Solid draws the surfaces");
}
