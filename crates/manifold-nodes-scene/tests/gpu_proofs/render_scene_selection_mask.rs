//! The selection mask (D-102): `render_scene` draws the selected objects'
//! coverage through the camera it rendered with, for the editor's outline,
//! and the output picture stays byte-identical with or without a selection
//! (REALTIME_3D_DESIGN.md D9).
//!
//! Scene: `render_scene_object_visibility.rs`'s ground plane (slot 0) and
//! the red occluder plane hung above it (slot 1).

use half::f16;
use manifold_gpu::{GpuTexture, GpuTextureFormat};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;

use super::render_scene_object_visibility::scene_json;

/// The output bytes and, when `selected` is set, the mask's bytes.
fn render(selected: &[u32]) -> (Vec<u8>, Option<Vec<u8>>) {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &scene_json(1.0),
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("selection-mask scene graph must build");
    let target = h.make_target("render-scene-selection-mask");
    for frame in 0..2 {
        runtime.set_selection_mask(selected);
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
        let mut enc = h.device.create_encoder("render-scene-selection-mask-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }
    let mask = runtime.selection_mask().map(|mask| read_mask(&h.device, mask));
    (h.readback(&target.texture), mask)
}

fn read_mask(device: &manifold_gpu::GpuDevice, mask: &GpuTexture) -> Vec<u8> {
    assert_eq!(mask.format, GpuTextureFormat::R8Unorm);
    let buf = device.create_buffer_shared(u64::from(mask.width * mask.height));
    let mut enc = device.create_encoder("selection-mask-readback");
    enc.copy_texture_to_buffer(mask, &buf, mask.width, mask.height, mask.width);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), (mask.width * mask.height) as usize) }.to_vec()
}

/// R above G: only the red occluder is tinted.
fn red(px: &[u8]) -> bool {
    let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
    let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
    r > g + 0.05
}

#[test]
fn selection_mask_covers_the_selected_object_and_leaves_the_output_unchanged() {
    let (plain, no_mask) = render(&[]);
    assert!(no_mask.is_none(), "no selection, no mask");
    let (outlined, mask) = render(&[1]);
    assert!(plain == outlined, "D9: the output is byte-identical with a selection");
    let mask = mask.expect("a selection draws a mask");
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    assert_eq!(mask.len(), (h.width * h.height) as usize, "the mask is the output's size");

    let covered: Vec<bool> = mask.iter().map(|&m| m > 127).collect();
    let red_px: Vec<bool> = outlined.chunks_exact(8).map(red).collect();
    let covered_n = covered.iter().filter(|&&c| c).count();
    let red_n = red_px.iter().filter(|&&r| r).count();
    let both = covered.iter().zip(&red_px).filter(|(c, r)| **c && **r).count();
    eprintln!("selection mask: covered={covered_n} red={red_n} both={both}");
    assert!(red_n > 500, "the occluder shows ({red_n} px)");
    assert!(both as f64 > red_n as f64 * 0.95, "the mask covers the occluder: {both} of {red_n}");
    assert!((covered_n as f64) < red_n as f64 * 1.1, "and nothing else: {covered_n} vs {red_n}");

    let (_, ground) = render(&[0]);
    let ground_n = ground.expect("ground mask").iter().filter(|&&m| m > 127).count();
    assert!(ground_n > covered_n * 2, "the other slot masks the ground: {ground_n}");
}
