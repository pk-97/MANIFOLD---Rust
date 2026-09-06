//! Real-rendering regression for clip starts after short gaps.
#![cfg(all(test, target_os = "macos"))]

use crossbeam_channel::Sender;
use manifold_core::{Beats, PresetTypeId};
use manifold_gpu::GpuTexture;

use crate::content_state::ContentState;
use crate::content_thread::ContentThread;
use crate::headless_harness::headless_content_thread;

fn state_channel() -> (
    Sender<ContentState>,
    crossbeam_channel::Receiver<ContentState>,
) {
    crossbeam_channel::unbounded()
}

fn readback_mean_rgb(device: &manifold_gpu::GpuDevice, tex: &GpuTexture) -> [f64; 3] {
    assert_eq!(tex.format, manifold_gpu::GpuTextureFormat::Rgba16Float);
    let bpp = tex.format.bytes_per_pixel();
    let row_bytes = bpp * tex.width;
    let buffer = device.create_buffer_shared((row_bytes * tex.height) as u64);
    let mut encoder = device.create_encoder("gap-start-regression-readback");
    encoder.copy_texture_to_buffer(tex, &buffer, tex.width, tex.height, row_bytes);
    encoder.commit_and_wait_completed();
    let ptr = buffer.mapped_ptr().expect("readback buffer must be mapped");
    let mut sum = [0.0; 3];
    let count = (tex.width * tex.height) as f64;
    for y in 0..tex.height as usize {
        for x in 0..tex.width as usize {
            let offset = y * row_bytes as usize + x * bpp as usize;
            for (channel, value) in sum.iter_mut().enumerate() {
                let at = offset + channel * 2;
                let bits = unsafe { u16::from_le_bytes([*ptr.add(at), *ptr.add(at + 1)]) };
                *value += half::f16::from_bits(bits).to_f32() as f64;
            }
        }
    }
    [sum[0] / count, sum[1] / count, sum[2] / count]
}

fn generated_project() -> manifold_core::project::Project {
    use manifold_core::clip::TimelineClip;
    use manifold_core::effects::PresetInstance;
    use manifold_core::layer::Layer;
    let mut project = manifold_core::project::Project::default();
    project.settings.bpm = manifold_core::Bpm(120.0);
    let mut layer = Layer::new_generator("Plasma gap regression".into(), PresetTypeId::PLASMA, 0);
    let mut feedback = PresetInstance::new(PresetTypeId::STYLIZED_FEEDBACK);
    feedback.init_defaults();
    assert!(
        feedback.get_param("amount") > 0.0,
        "feedback defaults must be active"
    );
    layer.effects = Some(vec![feedback]);
    for start in [0.0, 0.5, 1.0, 1.5] {
        layer
            .clips
            .push(TimelineClip::new_generator(Beats(start), Beats(0.25)));
    }
    project.timeline.layers.push(layer);
    project
}

#[test]
fn gap_start_black_frame_probe() {
    let mut ct: ContentThread = headless_content_thread(generated_project(), 320, 180);
    let (state_tx, _state_rx) = state_channel();
    ct.timer.set_frame_clocked(true);
    ct.handle_command(crate::content_command::ContentCommand::SeekToBeat(
        Beats::ZERO,
    ));
    ct.handle_command(crate::content_command::ContentCommand::Play);
    let layer = &ct
        .engine
        .project()
        .expect("generated project exists")
        .timeline
        .layers[0];
    assert_eq!(layer.clips.len(), 4);
    let layer_id = layer.layer_id.to_string();
    assert!(
        layer.effects.as_ref().is_some_and(|fx| fx
            .iter()
            .any(|e| e.effect_type() == &PresetTypeId::STYLIZED_FEEDBACK)),
        "generated layer must carry StylizedFeedback"
    );
    let mut transitions = 0;
    let mut was_active = false;
    let mut saw_gap = false;
    let mut saw_nonblack = false;
    for frame in 0..60 {
        ct.tick_frame(&state_tx);
        let beat = ct.engine.current_beat_f64();
        let active = ct.engine.project().unwrap().timeline.layers[0]
            .clips
            .iter()
            .any(|c| beat >= c.start_beat.0 && beat < c.end_beat().0);
        let device = ct
            .content_pipeline
            .native_device()
            .expect("native Metal device");
        let rgb = readback_mean_rgb(device, ct.content_pipeline.export_output_texture());
        assert!(
            rgb.iter().all(|v| v.is_finite()),
            "frame {frame} at beat {beat:.5} produced non-finite RGB {rgb:?}"
        );
        let nonblack = rgb.iter().sum::<f64>() > 0.01;
        if active {
            assert!(
                nonblack,
                "active clip at beat {beat:.5} rendered black: {rgb:?}"
            );
            saw_nonblack = true;
        }
        if active {
            assert!(
                ct.content_pipeline.chain_debug_info(&layer_id).is_some(),
                "StylizedFeedback chain must resolve during active clips"
            );
            if !was_active {
                // Localize a regression to source preparation versus feedback
                // output at each onset, using the existing readback seams.
                for (name, texture) in [
                    (
                        "layer source",
                        ct.content_pipeline.layer_scratch_texture(&layer_id),
                    ),
                    (
                        "feedback output",
                        ct.content_pipeline.chain_output_texture(&layer_id),
                    ),
                ] {
                    let texture = texture.expect("active feedback texture must exist");
                    let rgb = readback_mean_rgb(device, texture);
                    assert!(
                        rgb.iter().all(|v| v.is_finite()) && rgb.iter().sum::<f64>() > 0.01,
                        "{name} at onset beat {beat:.8} is invalid or black: {rgb:?}"
                    );
                }
                if saw_gap {
                    transitions += 1;
                }
            }
        } else {
            // No source clip in a gap: the compositor must clear the layer.
            assert!(
                rgb.iter().sum::<f64>() < 0.01,
                "empty gap at beat {beat:.8} retained visible content: {rgb:?}"
            );
            saw_gap = true;
        }
        was_active = active;
    }
    eprintln!("[gap-probe] verified {transitions} gap-to-clip transitions across 60 frames");
    assert!(saw_nonblack, "no active clip frame was nonblack");
    assert!(
        transitions >= 3,
        "observed only {transitions} clip transitions; probe was vacuous"
    );
}
