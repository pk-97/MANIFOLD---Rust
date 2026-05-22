//! `node.shape_2d` — curated 2D SDF shape primitive. Renders one of
//! three shapes (Square / Diamond / Octagon) as a centered SDF into an
//! RGBA16F texture, with optional clip-trigger-gated cycling through
//! shape + 8 rotation steps. The `fill_mode` enum picks the cycling
//! strategy (Solid / Mixed / Wireframe).
//!
//! When `clip_trigger` is OFF, the shape freezes at variant 0 (Square,
//! rotation 0) — wireframe-only if `fill_mode == Wireframe`. When ON,
//! `trigger_count` drives the shape + rotation snap and the rotation
//! eases from the previous angle to the new target over the quarter
//! beat following each trigger (BPM-coupled, decoupled from beat phase).
//!
//! Cycling math is unique-per-step by construction (`shape_idx = tc %
//! 3` advances every trigger), so this primitive does not need a
//! `ClipTriggerCycle` invariant. Rotation only changes every 3 / 6
//! triggers depending on fill mode — between those, the ease is a no-op.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const SHAPE_2D_FILL_MODES: &[&str] = &["Solid", "Mixed", "Wireframe"];

const WIREFRAME_FILL_MODE: u32 = 2;
/// Quarter-beat ease window: tween_t = saturate(elapsed_beats * 4.0).
const TWEEN_BEATS_INV: f32 = 4.0;
/// Sentinel marking "no trigger observed yet" so the first detected
/// edge initialises prev = curr (no ease-in animation on layer create).
const TRIGGER_COUNT_UNINIT: u32 = u32::MAX;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Shape2DUniforms {
    aspect_ratio: f32,
    line_thickness: f32,
    uv_scale: f32,
    shape_idx: f32,
    is_wireframe: f32,
    rotation: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: Shape2D,
    type_id: "node.shape_2d",
    purpose: "Curated 2D SDF shape primitive — Square / Diamond / Octagon rasterised into an RGBA16F texture with anti-aliased edges. The `clip_trigger` toggle gates trigger-driven cycling: ON drives shape + rotation snap from `trigger_count` and eases the rotation over the quarter beat following each trigger (BPM-coupled, trigger-elapsed). OFF freezes the shape at variant 0 with no rotation animation. `fill_mode` picks the cycling strategy: Solid (all filled), Mixed (alternates fill/wireframe), or Wireframe (all outlined).",
    inputs: {
        // Standard generator-input scalars, port-shadowable so a
        // generator graph can drive them from system.generator_input.
        aspect: ScalarF32 optional,
        trigger_count: ScalarF32 optional,
        line: ScalarF32 optional,
        scale: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "fill_mode",
            label: "Fill",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1), // Mixed (legacy default)
            range: Some((0.0, 2.0)),
            enum_values: SHAPE_2D_FILL_MODES,
        },
        ParamDef {
            name: "line",
            label: "Line Thickness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.015),
            range: Some((0.0005, 0.03)),
            enum_values: &[],
        },
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.25, 3.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "aspect",
            label: "Aspect Ratio",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "trigger_count",
            label: "Trigger Count",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "clip_trigger",
            label: "Clip Trigger",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
    ],
    composition_notes: "Wire `aspect` from system.generator_input.aspect and `trigger_count` from system.generator_input.trigger_count for the standard generator setup. The `clip_trigger` param gates the snap: when ON, the shape steps through 3 × 8 (Solid/Wireframe) or 6 × 8 (Mixed) variants on each retrigger and the rotation eases over the next quarter beat. When OFF, the primitive is static at variant 0. `scale` is inverted internally so larger values zoom out (matches legacy behaviour). `line` only affects Wireframe / Mixed-wireframe variants.",
    examples: [],
    picker: { label: "Shape 2D", category: Atom },
    extra_fields: {
        last_trigger_count: u32 = TRIGGER_COUNT_UNINIT,
        trigger_started_at_beat: f32 = -1.0,
        prev_rotation: f32 = 0.0,
        curr_rotation: f32 = 0.0,
    },
}

impl Primitive for Shape2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let aspect = ctx.scalar_or_param("aspect", 1.0);
        let trigger_count_raw = ctx.scalar_or_param("trigger_count", 0.0);
        let line = ctx.scalar_or_param("line", 0.015);
        let scale = ctx.scalar_or_param("scale", 1.0);

        let fill_mode = match ctx.params.get("fill_mode") {
            Some(ParamValue::Enum(v)) => (*v).min(2),
            Some(ParamValue::Float(f)) => (f.round().clamp(0.0, 2.0)) as u32,
            _ => 1,
        };

        let clip_trigger = match ctx.params.get("clip_trigger") {
            Some(ParamValue::Bool(b)) => *b,
            Some(ParamValue::Float(f)) => *f > 0.5,
            _ => false,
        };

        let (shape_idx, is_wireframe, rotation) = if clip_trigger {
            self.compute_active_state(trigger_count_raw, fill_mode, ctx.time.beats.0 as f32)
        } else {
            self.reset_trigger_state();
            (0u32, fill_mode == WIREFRAME_FILL_MODE, 0.0_f32)
        };

        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };

        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/shape_2d.wgsl"),
                "cs_main",
                "node.shape_2d",
            )
        });

        let uniforms = Shape2DUniforms {
            aspect_ratio: aspect,
            line_thickness: line,
            uv_scale,
            shape_idx: shape_idx as f32,
            is_wireframe: if is_wireframe { 1.0 } else { 0.0 },
            rotation,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.shape_2d",
        );
    }
}

impl Shape2D {
    /// Trigger-elapsed eased rotation. On a new trigger, sample the
    /// current visible angle, lock it as `prev_rotation`, and lock the
    /// freshly computed target as `curr_rotation` — then `tween_t`
    /// linearly grows from 0 → 1 over the next quarter beat, with
    /// `ease_out_cubic` smoothing the visible interpolation.
    fn compute_active_state(
        &mut self,
        trigger_count_raw: f32,
        fill_mode: u32,
        beat: f32,
    ) -> (u32, bool, f32) {
        let tc = trigger_count_raw.floor().max(0.0) as u32;
        let (shape_idx, is_wireframe, rot_step) = decompose_trigger(tc, fill_mode);
        let new_target = signed_rotation(rot_step);

        if tc != self.last_trigger_count {
            if self.last_trigger_count == TRIGGER_COUNT_UNINIT {
                // First trigger after init / reset: lock in the target
                // immediately, no ease-in animation.
                self.prev_rotation = new_target;
                self.curr_rotation = new_target;
            } else {
                self.prev_rotation = self.current_visible_rotation(beat);
                self.curr_rotation = new_target;
            }
            self.trigger_started_at_beat = beat;
            self.last_trigger_count = tc;
        }

        (shape_idx, is_wireframe, self.current_visible_rotation(beat))
    }

    fn reset_trigger_state(&mut self) {
        self.last_trigger_count = TRIGGER_COUNT_UNINIT;
        self.trigger_started_at_beat = -1.0;
        self.prev_rotation = 0.0;
        self.curr_rotation = 0.0;
    }

    fn current_visible_rotation(&self, beat: f32) -> f32 {
        if self.trigger_started_at_beat < 0.0 {
            return self.curr_rotation;
        }
        let elapsed = (beat - self.trigger_started_at_beat).max(0.0);
        let t = (elapsed * TWEEN_BEATS_INV).clamp(0.0, 1.0);
        let eased = ease_out_cubic(t);
        lerp(self.prev_rotation, self.curr_rotation, eased)
    }
}

fn decompose_trigger(tc: u32, fill_mode: u32) -> (u32, bool, u32) {
    match fill_mode {
        0 => (tc % 3, false, (tc / 3) % 8),
        1 => {
            let variant = tc % 6;
            (variant % 3, variant >= 3, (tc / 6) % 8)
        }
        _ => (tc % 3, true, (tc / 3) % 8),
    }
}

fn signed_rotation(rot_step: u32) -> f32 {
    let target_angle = (rot_step % 4) as f32 * std::f32::consts::FRAC_PI_4;
    let direction = if rot_step >= 4 { -1.0 } else { 1.0 };
    target_angle * direction
}

fn ease_out_cubic(t: f32) -> f32 {
    let t1 = 1.0 - t;
    1.0 - t1 * t1 * t1
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn shape_2d_declares_four_optional_scalar_inputs_and_one_texture_output() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(Shape2D::TYPE_ID, "node.shape_2d");
        let ins = Shape2D::INPUTS;
        assert_eq!(ins.len(), 4);
        for (i, name) in ["aspect", "trigger_count", "line", "scale"].iter().enumerate() {
            assert_eq!(ins[i].name, *name);
            assert!(!ins[i].required);
            assert_eq!(ins[i].ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(Shape2D::OUTPUTS.len(), 1);
        assert_eq!(Shape2D::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn shape_2d_has_fill_line_scale_clip_trigger_params() {
        let names: Vec<&str> = Shape2D::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec![
                "fill_mode",
                "line",
                "scale",
                "aspect",
                "trigger_count",
                "clip_trigger",
            ]
        );
    }

    #[test]
    fn shape_2d_fill_modes_cover_three_variants() {
        assert_eq!(SHAPE_2D_FILL_MODES.len(), 3);
        assert_eq!(SHAPE_2D_FILL_MODES[0], "Solid");
        assert_eq!(SHAPE_2D_FILL_MODES[1], "Mixed");
        assert_eq!(SHAPE_2D_FILL_MODES[2], "Wireframe");
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Shape2D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.shape_2d");
    }

    #[test]
    fn decompose_trigger_solid_advances_shape_every_step() {
        assert_eq!(decompose_trigger(0, 0), (0, false, 0));
        assert_eq!(decompose_trigger(1, 0), (1, false, 0));
        assert_eq!(decompose_trigger(2, 0), (2, false, 0));
        // rot_step advances at tc=3 (3 shapes per rotation step).
        assert_eq!(decompose_trigger(3, 0), (0, false, 1));
    }

    #[test]
    fn decompose_trigger_mixed_switches_to_wireframe_at_variant_three() {
        // Mixed: 6 variants, the second half are wireframe.
        assert_eq!(decompose_trigger(2, 1), (2, false, 0));
        assert_eq!(decompose_trigger(3, 1), (0, true, 0));
        assert_eq!(decompose_trigger(5, 1), (2, true, 0));
        // rot_step advances at tc=6 (6 variants per rotation step).
        assert_eq!(decompose_trigger(6, 1), (0, false, 1));
    }

    #[test]
    fn signed_rotation_negative_for_steps_4_through_7() {
        assert!(signed_rotation(0).abs() < 1e-6);
        assert!((signed_rotation(2) - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        // rot_step = 5 → angle = 45° × -1 = -PI/4
        assert!((signed_rotation(5) + std::f32::consts::FRAC_PI_4).abs() < 1e-6);
    }

    #[test]
    fn first_trigger_skips_ease_in() {
        // Sentinel last_trigger_count means "no trigger seen yet" — the
        // first detection should snap prev/curr to the target directly
        // so the layer's first frame doesn't ease from 0 to a nonzero
        // angle (which would briefly render the wrong rotation).
        let mut prim = Shape2D::new();
        let (_, _, rot) = prim.compute_active_state(3.0, 0, 1.0);
        // tc=3 in Solid mode → rot_step=1 → target = PI/4
        assert!((rot - std::f32::consts::FRAC_PI_4).abs() < 1e-5);
        assert!((prim.prev_rotation - std::f32::consts::FRAC_PI_4).abs() < 1e-5);
        assert!((prim.curr_rotation - std::f32::consts::FRAC_PI_4).abs() < 1e-5);
    }

    #[test]
    fn second_trigger_starts_ease_from_previous_settled_angle() {
        let mut prim = Shape2D::new();
        // First trigger at tc=3 (rot_step=1, target=PI/4): snaps immediately.
        prim.compute_active_state(3.0, 0, 1.0);
        // Advance 0.5 beats — still inside the (no-op) tween window but
        // prev == curr so visible stays at PI/4 anyway. Advance to a new
        // rotation step: tc=6 → rot_step=2 → target = PI/2. Expect the
        // visible rotation at the trigger frame to STILL read PI/4 (the
        // tween_t = 0 endpoint) and `curr_rotation` to be PI/2.
        let (_, _, rot) = prim.compute_active_state(6.0, 0, 2.0);
        assert!(
            (rot - std::f32::consts::FRAC_PI_4).abs() < 1e-5,
            "tween_t=0 should still show the previous angle, got {rot}"
        );
        assert!((prim.curr_rotation - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert!((prim.prev_rotation - std::f32::consts::FRAC_PI_4).abs() < 1e-5);
    }

    #[test]
    fn ease_completes_after_quarter_beat() {
        let mut prim = Shape2D::new();
        prim.compute_active_state(3.0, 0, 0.0);
        prim.compute_active_state(6.0, 0, 1.0); // arm tween at beat 1, target PI/2 from PI/4
        // Beat 1.25 == quarter beat later → tween_t = 1 → visible == curr_rotation.
        let visible = prim.current_visible_rotation(1.25);
        assert!(
            (visible - std::f32::consts::FRAC_PI_2).abs() < 1e-5,
            "ease should be complete at +0.25 beats, got {visible}"
        );
    }

    #[test]
    fn clip_trigger_off_resets_state() {
        let mut prim = Shape2D::new();
        prim.compute_active_state(5.0, 1, 1.0);
        prim.reset_trigger_state();
        assert_eq!(prim.last_trigger_count, TRIGGER_COUNT_UNINIT);
        assert!(prim.trigger_started_at_beat < 0.0);
        assert_eq!(prim.prev_rotation, 0.0);
        assert_eq!(prim.curr_rotation, 0.0);
    }
}

#[cfg(test)]
mod gpu_tests {
    //! GPU smoke tests for `node.shape_2d`. The SDF math + AA pipeline
    //! is unchanged from the legacy `BasicShapesSnapGenerator` shader;
    //! the new tests verify the primitive-level wiring around the
    //! `clip_trigger` gate and trigger-elapsed eased rotation by
    //! checking that the rendered texture responds correctly to:
    //!
    //! - clip_trigger OFF → static variant 0 regardless of trigger_count
    //! - clip_trigger ON  → cycling responds to trigger_count
    //! - fill_mode toggles between solid and wireframe paths
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::Slot;
    use crate::node_graph::{
        Executor, FinalOutput, FrameTime, Graph, MetalBackend, ParamValue, compile,
    };

    use super::Shape2D;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    /// Run Shape2D standalone through the graph executor with the given
    /// (fill_mode, trigger_count, clip_trigger). Returns the rendered
    /// RGBA texture readback as f32.
    fn run_shape_2d(
        fill_mode: u32,
        trigger_count: f32,
        clip_trigger: bool,
        w: u32,
        h: u32,
    ) -> Vec<[f32; 4]> {
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let prim = g.add_node(Box::new(Shape2D::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(prim, "fill_mode", ParamValue::Enum(fill_mode))
            .unwrap();
        g.set_param(prim, "trigger_count", ParamValue::Float(trigger_count))
            .unwrap();
        g.set_param(prim, "clip_trigger", ParamValue::Bool(clip_trigger))
            .unwrap();
        g.connect((prim, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let backend = MetalBackend::new(&device, w, h, format);
        // Shape2D's output is the first (and only) lazily-allocated
        // Texture2D — the slot index lands on the backend's high-water
        // mark at execute time. Capture it before run so the pool
        // releasing the binding doesn't lose the texture handle.
        let out_slot = Slot(backend.slot_count());

        let mut native_enc = device.create_encoder("shape2d-test");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let out_tex = exec
            .backend()
            .texture_2d(out_slot)
            .expect("output texture retained");
        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("shape2d-readback");
        readback_enc.copy_texture_to_buffer(out_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared buffer pointer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        (0..(w * h) as usize)
            .map(|i| {
                let o = i * 4;
                [
                    f16::from_bits(halves[o]).to_f32(),
                    f16::from_bits(halves[o + 1]).to_f32(),
                    f16::from_bits(halves[o + 2]).to_f32(),
                    f16::from_bits(halves[o + 3]).to_f32(),
                ]
            })
            .collect()
    }

    fn center_luma(out: &[[f32; 4]], w: u32, h: u32) -> f32 {
        let cx = (w / 2) as usize;
        let cy = (h / 2) as usize;
        out[cy * w as usize + cx][0]
    }

    /// clip_trigger OFF → static Square (variant 0). Solid fill at the
    /// centre should be ~1.0 regardless of the trigger_count input.
    #[test]
    fn clip_trigger_off_freezes_at_solid_square() {
        let out_tc0 = run_shape_2d(0, 0.0, false, 32, 32);
        let out_tc5 = run_shape_2d(0, 5.0, false, 32, 32);
        let c0 = center_luma(&out_tc0, 32, 32);
        let c5 = center_luma(&out_tc5, 32, 32);
        assert!(c0 > 0.9, "expected solid square centre ~1.0, got {c0}");
        // Both renders should be identical (no cycling).
        for (a, b) in out_tc0.iter().zip(out_tc5.iter()) {
            assert!(
                (a[0] - b[0]).abs() < 1e-3,
                "clip_trigger OFF must produce identical output for any trigger_count"
            );
        }
        assert!((c0 - c5).abs() < 1e-3);
    }

    /// clip_trigger OFF + Wireframe fill → static wireframe square at
    /// variant 0. Centre of the wireframe outline is empty (dark).
    #[test]
    fn clip_trigger_off_wireframe_mode_still_outlines() {
        let out = run_shape_2d(2, 0.0, false, 32, 32);
        let c = center_luma(&out, 32, 32);
        assert!(
            c < 0.1,
            "expected wireframe centre dark when clip_trigger OFF + fill=Wireframe, got {c}"
        );
    }

    /// clip_trigger ON, Solid fill, tc=0 → first trigger snaps the
    /// rotation to target=0 immediately (no ease-in), so the centre of
    /// the solid square should be lit at ~1.0.
    #[test]
    fn clip_trigger_on_solid_variant_lights_up_the_centre() {
        let out = run_shape_2d(0, 0.0, true, 32, 32);
        let c = center_luma(&out, 32, 32);
        assert!(
            c > 0.9,
            "expected centre of solid square ~1.0 with clip_trigger ON, got {c}"
        );
    }

    /// Cycling advances on trigger_count — tc=0 vs tc=1 in Solid mode
    /// yields a different shape (Square vs Diamond) — pixel at (8,8) is
    /// off-axis where the SDFs disagree.
    #[test]
    fn clip_trigger_on_advances_the_cycle() {
        let out0 = run_shape_2d(0, 0.0, true, 32, 32);
        let out1 = run_shape_2d(0, 1.0, true, 32, 32);
        let p0 = out0[8 * 32 + 8][0];
        let p1 = out1[8 * 32 + 8][0];
        assert!(
            (p0 - p1).abs() > 0.05,
            "expected tc=0 vs tc=1 to differ off-axis with clip_trigger ON, got {p0} vs {p1}"
        );
    }

    /// Mixed mode's variant 3 is the first wireframe — at tc=3 with
    /// clip_trigger ON, the centre should be dark (wireframe square at
    /// rot_step=0). Locks in the Mixed cycling formula
    /// `variant = tc % 6; is_wireframe = variant >= 3u`.
    #[test]
    fn clip_trigger_on_mixed_mode_switches_to_wireframe_at_tc_three() {
        let out_solid = run_shape_2d(1, 0.0, true, 32, 32);
        let out_wire = run_shape_2d(1, 3.0, true, 32, 32);
        let c_solid = center_luma(&out_solid, 32, 32);
        let c_wire = center_luma(&out_wire, 32, 32);
        assert!(
            c_solid > 0.9,
            "Mixed/tc=0 expected solid centre, got {c_solid}"
        );
        assert!(
            c_wire < 0.1,
            "Mixed/tc=3 expected wireframe centre, got {c_wire}"
        );
    }
}
