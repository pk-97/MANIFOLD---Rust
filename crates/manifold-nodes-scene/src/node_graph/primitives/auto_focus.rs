//! `node.auto_focus` — keeps the lens focused on what the camera looks at.
//!
//! A rig that aims at a point (orbit, look-at) carries how far along its
//! view that point lies (`Camera::aim_distance`), and focus follows it
//! exactly. A rig that aims at nothing (free, loop) focuses on the scene at
//! the frame centre: one GPU thread takes the median depth of nine points
//! around the centre into a CPU-read buffer, read once its frame has
//! finished, a frame or two later; live never waits on the GPU. Focus eases
//! toward that depth in dioptres (1/distance), the way a focus puller racks,
//! closing 63% of the gap every `PULL_SECONDS`. A cut snaps to the first
//! depth measured after it. With `auto` off the camera passes through.

use std::borrow::Cow;

use manifold_gpu::{BufferUse, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::camera::linearize_depth;

const SHADER: &str = include_str!("shaders/auto_focus.wgsl");

/// Seconds for focus to close 63% of the way to a new subject.
const PULL_SECONDS: f64 = 0.25;
/// Probe readbacks in flight. A frame finding every one still pending skips
/// its probe.
const PROBE_SLOTS: usize = 3;

pub struct ProbeSlot {
    buffer: GpuBuffer,
    stamp: u64,
    pending: bool,
}

/// Focus `dt` seconds on from `from`, easing toward `to` in dioptres.
fn ease_focus(from: f32, to: f32, dt: f64) -> f32 {
    let keep = (-dt / PULL_SECONDS).exp() as f32;
    1.0 / (1.0 / to + (1.0 / from - 1.0 / to) * keep)
}

manifold_node_engine::primitive! {
    name: AutoFocus,
    type_id: "node.auto_focus",
    purpose: "Sets a Camera's lens focus_distance to what the camera looks at. A camera with an aim point (node.orbit_camera's target, node.look_at_camera's target; Camera::aim_distance) focuses exactly there. One without (node.free_camera, node.loop_camera) focuses on the median depth of nine points around the frame centre in `depth` (render_scene's raw reversed-Z depth), read back a frame or two late and eased in dioptres over about 0.25 s; a cut snaps. `auto = false` passes the camera through unchanged. Every other camera field passes through.",
    inputs: {
        camera: Camera required,
        depth: Texture2D optional,
    },
    outputs: {
        out: Camera,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("auto"),
            label: "Auto Focus",
            ty: ParamType::Bool,
            default: ParamValue::Bool(true),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Sits between node.camera_lens and the depth-of-field consumers (node.coc_from_depth), with the same render_scene's `depth` wired in. Render_scene and motion blur don't read focus, so they keep the lens camera.",
    examples: [],
    picker: { label: "Auto Focus", category: Atom },
    summary: "Keeps the lens focused on what the camera looks at: its target, or whatever sits at the frame centre.",
    category: Geometry3D,
    role: Filter,
    aliases: ["auto focus", "autofocus", "focus", "focus pull", "rack focus"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        probe_pipeline: Option<GpuComputePipeline> = None,
        // The GPU thread's output, copied into a readback slot.
        probe: Option<GpuBuffer> = None,
        slots: Vec<ProbeSlot> = Vec::new(),
        focus: Option<f32> = None,
        shot: u64 = 0,
        // A cut happened and no depth measured after it has arrived yet.
        snap: bool = false,
    },
}

impl AutoFocus {
    pub fn prewarm_pipelines(device: &GpuDevice) {
        let _ = device.create_compute_pipeline(SHADER, "cs_main", "auto focus probe");
    }
}

impl Primitive for AutoFocus {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(mut cam) = ctx.inputs.camera("camera") else {
            return;
        };
        if matches!(ctx.params.get("auto"), Some(ParamValue::Bool(false))) {
            ctx.outputs.set_camera("out", cam);
            return;
        }
        if cam.shot != self.shot {
            self.shot = cam.shot;
            self.snap = true;
            // Depth measured before the cut belongs to the other shot.
            self.slots.iter_mut().for_each(|slot| slot.pending = false);
        }
        if let Some(distance) = cam.aim_distance {
            self.focus = Some(distance);
            self.snap = false;
        } else if let Some(depth) = ctx.inputs.texture_2d("depth") {
            let dt = ctx.time.delta.0;
            let gpu = ctx.gpu_encoder();
            let clock = gpu.device.frame_clock();
            let done = |slot: &ProbeSlot| clock.as_ref().is_none_or(|c| c.is_complete(slot.stamp));
            let mut newest: Option<(u64, f32)> = None;
            for slot in self.slots.iter_mut().filter(|slot| slot.pending) {
                if !done(slot) {
                    continue;
                }
                slot.pending = false;
                let ptr = slot
                    .buffer
                    .mapped_ptr()
                    .expect("readback buffers are mapped");
                // SAFETY: a mapped readback buffer of one f32, its frame complete.
                let raw = unsafe { std::ptr::read_unaligned(ptr.cast::<f32>()) };
                if newest.is_none_or(|(stamp, _)| slot.stamp >= stamp) {
                    newest = Some((slot.stamp, raw));
                }
            }
            if let Some((_, raw)) = newest {
                let target = linearize_depth(raw, cam.near, cam.far);
                self.focus = Some(match self.focus {
                    Some(from) if !self.snap => ease_focus(from, target, dt),
                    _ => target,
                });
                self.snap = false;
            }

            let free = self.slots.iter().position(|slot| !slot.pending);
            let index = match free {
                Some(index) => Some(index),
                None if self.slots.len() < PROBE_SLOTS => {
                    self.slots.push(ProbeSlot {
                        buffer: gpu.device.create_buffer(4, BufferUse::Readback),
                        stamp: 0,
                        pending: false,
                    });
                    Some(self.slots.len() - 1)
                }
                None => None,
            };
            if let Some(index) = index {
                let pipeline = self.probe_pipeline.get_or_insert_with(|| {
                    gpu.device
                        .create_compute_pipeline(SHADER, "cs_main", "auto focus probe")
                });
                let probe = self
                    .probe
                    .get_or_insert_with(|| gpu.device.create_buffer(4, BufferUse::Gpu));
                gpu.native_enc.dispatch_compute(
                    pipeline,
                    &[
                        GpuBinding::Texture {
                            binding: 0,
                            texture: depth,
                        },
                        GpuBinding::Buffer {
                            binding: 1,
                            buffer: probe,
                            offset: 0,
                        },
                    ],
                    [1, 1, 1],
                    "auto focus probe",
                );
                let slot = &mut self.slots[index];
                gpu.native_enc.copy_buffer_to_buffer(probe, &slot.buffer, 4);
                slot.stamp = clock.as_ref().map_or(0, |c| c.stamp());
                slot.pending = true;
            }
        }
        if let Some(focus) = self.focus {
            cam.lens.focus_distance = focus;
        }
        ctx.outputs.set_camera("out", cam);
    }
}

#[cfg(test)]
mod tests {
    use super::ease_focus;

    /// A pull from 2 m to 8 m closes 63% of the gap in dioptres after one
    /// pull time, lands nearer the near subject than a linear ease would,
    /// and two half steps match one whole step at any frame rate.
    #[test]
    fn focus_eases_in_dioptres_at_any_frame_rate() {
        let after = ease_focus(2.0, 8.0, 0.25);
        let expected = 1.0 / (1.0 / 8.0 + (0.5 - 1.0 / 8.0) * (-1.0f32).exp());
        assert!((after - expected).abs() < 1e-5, "{after} vs {expected}");
        assert!(after < 2.0 + (8.0 - 2.0) * 0.632);
        let halves = ease_focus(ease_focus(2.0, 8.0, 0.125), 8.0, 0.125);
        assert!((halves - after).abs() < 1e-4, "{halves} vs {after}");
    }
}
