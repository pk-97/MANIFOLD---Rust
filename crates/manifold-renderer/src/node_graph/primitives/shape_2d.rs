//! `node.shape_2d` — curated 2D SDF shape primitive. Bit-exact port of
//! the legacy `BasicShapesSnapGenerator` shader: renders one of three
//! shapes (Square / Diamond / Octagon) as a centered SDF into an
//! RGBA16F texture, with trigger-driven cycling through 8 rotation
//! steps and a `fill_mode` enum picking the cycling strategy
//! (Solid / Mixed / Wireframe).
//!
//! Cycling is unconditional — `trigger_count % N` is naturally
//! unique-per-step (no two adjacent triggers land on the same variant
//! by construction), so this primitive does not carry a
//! `ClipTriggerCycle` invariant. To make the shape static, leave the
//! `trigger_count` input unwired (defaults to 0 → variant 0).

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const SHAPE_2D_FILL_MODES: &[&str] = &["Solid", "Mixed", "Wireframe"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Shape2DUniforms {
    aspect_ratio: f32,
    line_thickness: f32,
    uv_scale: f32,
    trigger_count: f32,
    fill_mode: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Shape2D,
    type_id: "node.shape_2d",
    purpose: "Curated 2D SDF shape primitive — Square / Diamond / Octagon rasterised into an RGBA16F texture with anti-aliased edges. Trigger-driven cycling steps through shape + rotation on each retrigger; `fill_mode` enum picks Solid (all filled), Mixed (alternates fill/wireframe), or Wireframe (all outlined). Bit-exact port of the legacy BasicShapesSnap generator shader.",
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
    ],
    composition_notes: "Wire `aspect` from system.generator_input.aspect and `trigger_count` from system.generator_input.trigger_count for the standard generator setup. Cycling is unconditional — `trigger_count` advances through 3 shapes × 8 rotations (Solid/Wireframe modes) or 6 variants × 8 rotations (Mixed mode). `scale` is inverted internally so larger values zoom out (matches legacy behaviour). `line` only affects Wireframe / Mixed-wireframe variants.",
    examples: [],
    picker: { label: "Shape 2D", category: Atom },
}

impl Primitive for Shape2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let aspect = ctx.scalar_or_param("aspect", 1.0);
        let trigger_count = ctx.scalar_or_param("trigger_count", 0.0);
        let line = ctx.scalar_or_param("line", 0.015);
        let scale = ctx.scalar_or_param("scale", 1.0);

        let fill_mode = match ctx.params.get("fill_mode") {
            Some(ParamValue::Enum(v)) => *v as f32,
            Some(ParamValue::Float(f)) => f.round().clamp(0.0, 2.0),
            _ => 1.0,
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
            trigger_count,
            fill_mode,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
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
    fn shape_2d_has_fill_line_scale_params() {
        let names: Vec<&str> = Shape2D::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec!["fill_mode", "line", "scale", "aspect", "trigger_count"]
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
}

#[cfg(test)]
mod gpu_tests {
    //! GPU smoke tests for `node.shape_2d`. Since the shader is a
    //! byte-identical port of the legacy `BasicShapesSnapGenerator`
    //! compute shader (just renamed bindings), bit parity is guaranteed
    //! by the file copy + identical uniform layout. These tests verify
    //! the primitive driver glue:
    //!
    //! - Uniform fields are wired to the right params
    //! - Cycling responds to trigger_count
    //! - fill_mode enum picks the correct cycling strategy
    //! - The output is non-zero (not all black) at the centered shape
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

    /// Run Shape2D standalone through the graph executor with the
    /// given (fill_mode, trigger_count). Returns the rendered RGBA
    /// texture readback as f32.
    fn run_shape_2d(fill_mode: u32, trigger_count: f32, w: u32, h: u32) -> Vec<[f32; 4]> {
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let prim = g.add_node(Box::new(Shape2D::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(prim, "fill_mode", ParamValue::Enum(fill_mode))
            .unwrap();
        g.set_param(prim, "trigger_count", ParamValue::Float(trigger_count))
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

    /// At default params (Mixed mode, tc=0) the center pixel of a
    /// centred SDF shape is fully inside the shape — center luma
    /// should be ~1.0. Catches uniform-routing bugs that would put
    /// `fill_mode` or `scale` in the wrong slot.
    #[test]
    fn solid_variant_lights_up_the_centre() {
        // tc=0 in Solid mode → shape_idx=0 (Square), rotation=0,
        // is_wireframe=false → centered square fills the middle.
        let out = run_shape_2d(0, 0.0, 32, 32);
        let c = center_luma(&out, 32, 32);
        assert!(
            c > 0.9,
            "expected centre of solid square ≈ 1.0, got {c}"
        );
    }

    /// In Wireframe mode the centre of the shape is OUTSIDE the line
    /// band — centre luma should be ~0 (well below the solid case).
    /// Locks in that `fill_mode` actually toggles the wireframe path.
    #[test]
    fn wireframe_variant_leaves_centre_dark() {
        let out = run_shape_2d(2, 0.0, 32, 32);
        let c = center_luma(&out, 32, 32);
        assert!(
            c < 0.1,
            "expected centre of wireframe shape ≈ 0.0, got {c}"
        );
    }

    /// Cycling — trigger_count = 1 lands on a different variant than
    /// trigger_count = 0 in every mode. Compare a corner pixel that's
    /// shape-sensitive (square vs diamond have different SDFs at
    /// off-axis sample points).
    #[test]
    fn trigger_count_advances_the_cycle() {
        let out0 = run_shape_2d(0, 0.0, 32, 32);
        let out1 = run_shape_2d(0, 1.0, 32, 32);
        // Sample at (8, 8) — well off-axis, where square and diamond
        // SDFs disagree.
        let p0 = out0[8 * 32 + 8][0];
        let p1 = out1[8 * 32 + 8][0];
        assert!(
            (p0 - p1).abs() > 0.05,
            "expected tc=0 vs tc=1 to differ off-axis, got {p0} vs {p1}"
        );
    }

    /// Mixed mode's variant 3 is the first wireframe — at tc=3, the
    /// centre should be dark (wireframe). Locks in the Mixed cycling
    /// formula `variant=tc%6; is_wireframe = variant>=3u`.
    #[test]
    fn mixed_mode_switches_to_wireframe_at_tc_three() {
        let out_solid = run_shape_2d(1, 0.0, 32, 32);
        let out_wire = run_shape_2d(1, 3.0, 32, 32);
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
