//! `node.resolve_scatter` — convert a u32 fixed-point
//! accumulator buffer (produced by `node.draw_particles`) into
//! a float density texture.
//!
//! Phase A.7 of `BUFFER_PORT_PLAN`. Reads each accumulator cell,
//! divides by `fixed_point_scale` (default 4096, matching FluidSim),
//! and writes the result as a uniform RGB density into an
//! `Rgba16Float` storage texture. Output alpha is always 1.0.
//!
//! This is the bridge from the Array(u32) wire family back to the
//! Texture2D wire family — downstream Mix / Blur / Feedback /
//! display primitives can consume the result as a normal texture.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: the `fixed_point_scale` param (f32) + pad
/// to 16 bytes. The generated kernel derives its dims (and thus the dispatch
/// guard + linear cell index) from `textureDimensions(dst)`, so width/height are
/// no longer uniform fields; inv_scale is computed in-body from
/// fixed_point_scale. 1 word + 3 pad = 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResolveUniforms {
    fixed_point_scale: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: ResolveAccumulator,
    type_id: "node.resolve_scatter",
    purpose: "Read a u32 fixed-point accumulator buffer (produced by node.draw_particles), divide by `fixed_point_scale`, and write the result as a grayscale density texture. The bridge from Array(u32) back to Texture2D for downstream texture-domain primitives. Dimensions are taken from the output Texture2D — which the backend allocates at canvas size — so resolve always covers every pixel of the density texture, matching whatever scatter wrote (also canvas-sized via `canvas_sized_array_outputs()`).",
    inputs: {
        accum: Array(u32) required,
    },
    outputs: {
        density: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("fixed_point_scale"),
            label: "Fixed-Point Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(4096.0),
            range: Some((1.0, 65536.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Output texture must be Rgba16Float — the shader writes via texture_storage_2d<rgba16float, write>. Dispatch dimensions match the output texture (allocated by the backend at canvas size), so paired ScatterParticles + ResolveAccumulator automatically span the full canvas without param tuning. fixed_point_scale = scatter's scaled_energy gives unit-density output.",
    examples: [],
    picker: { label: "Resolve Scatter", category: Atom },
    summary: "Reads back the buffer that Draw Particles wrote into and turns it into a normal image. The pickup step after a particle splat.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["resolve scatter", "resolve accumulator", "accumulator", "read back"],
    fusion_kind: Boundary,
    boundary_reason: BarrieredReduction,
    wgsl_body: include_str!("shaders/resolve_accumulator_body.wgsl"),
}

impl Primitive for ResolveAccumulator {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let fixed_point_scale = match ctx.params.get("fixed_point_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 4096.0,
        };

        let Some(accum) = ctx.inputs.array("accum") else {
            return;
        };
        let Some(density_out) = ctx.outputs.texture_2d("density") else {
            return;
        };
        // Dimensions come from the output texture (allocated by the
        // backend at canvas size). Matches scatter's canvas-sized
        // accumulator so the resolve covers every pixel.
        let width = density_out.width.max(1);
        let height = density_out.height.max(1);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (BUFFER→TEXTURE
            // resolve — dims/idx from textureDimensions(dst), the body reads +
            // zeros the atomic accumulator and returns the density vec4).
            // resolve_accumulator.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.resolve_scatter standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.resolve_scatter",
            )
        });

        let uniforms = ResolveUniforms {
            fixed_point_scale,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // Generated binding order matches the hand kernel: uniform(0),
        // accum(1, atomic read_write), density(2, storage write).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: accum,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: density_out,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.resolve_scatter",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn resolve_accumulator_declares_array_in_and_texture_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let u32_layout = ArrayType::of_known::<u32>();

        assert_eq!(ResolveAccumulator::TYPE_ID, "node.resolve_scatter");
        assert_eq!(ResolveAccumulator::INPUTS.len(), 1);
        assert_eq!(ResolveAccumulator::INPUTS[0].name, "accum");
        assert_eq!(
            ResolveAccumulator::INPUTS[0].ty,
            PortType::Array(u32_layout)
        );

        assert_eq!(ResolveAccumulator::OUTPUTS.len(), 1);
        assert_eq!(ResolveAccumulator::OUTPUTS[0].name, "density");
        assert_eq!(ResolveAccumulator::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ResolveAccumulator::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.resolve_scatter");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer→texture resolve value oracle (freeze §12). Dispatches the generated
    //! kernel over a known u32 accumulator, reads back the density texture, and
    //! asserts (a) each pixel's R = raw / fixed_point_scale and (b) the
    //! accumulator is self-cleared to zero. The generated kernel derives dims +
    //! the linear cell index from `textureDimensions(dst)` and reads/zeros the
    //! atomic `buf_accum` global — this proves that whole path end to end.
    use super::*;
    use half::f16;
    use manifold_gpu::GpuTextureFormat;

    use crate::render_target::RenderTarget;

    #[test]
    fn generated_resolve_divides_and_self_clears() {
        let device = crate::test_device();
        let (w, h) = (4u32, 2u32);
        let cells = (w * h) as usize;
        let scale = 4096.0f32;

        // Cell i holds i * scale → expected density i.0 (exact in f16).
        let accum_raw: Vec<u32> = (0..cells as u32).map(|i| i * 4096).collect();
        let accum = device.create_buffer_shared((cells * 4) as u64);
        unsafe {
            accum.write(0, bytemuck::cast_slice(&accum_raw));
        }

        let out_target =
            RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "resolve-out");

        let gen_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<ResolveAccumulator>()
                .expect("resolve_accumulator codegen");
        assert!(
            gen_wgsl.contains("array<atomic<u32>>"),
            "accumulator bound as atomic"
        );
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "resolve-oracle",
        );

        let uniforms = ResolveUniforms {
            fixed_point_scale: scale,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let mut enc = device.create_encoder("resolve-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &accum, offset: 0 },
                GpuBinding::Texture { binding: 2, texture: &out_target.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "resolve-oracle",
        );
        enc.commit_and_wait_completed();

        // Read back the density texture (Rgba16Float = 8 bytes/pixel).
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut rb = device.create_encoder("resolve-readback");
        rb.copy_texture_to_buffer(&out_target.texture, &readback, w, h, bytes_per_row);
        rb.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback");
        let px: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), cells * 4) };
        for i in 0..cells {
            let r = f16::from_bits(px[i * 4]).to_f32();
            let expected = accum_raw[i] as f32 / scale;
            assert!(
                (r - expected).abs() < 1e-3,
                "cell {i}: density {r} != {expected}"
            );
        }

        // Self-clear: every accumulator cell must be zero after the resolve.
        let aptr = accum.mapped_ptr().expect("shared accum");
        let cleared: &[u32] = unsafe { std::slice::from_raw_parts(aptr.cast::<u32>(), cells) };
        assert!(
            cleared.iter().all(|&v| v == 0),
            "accumulator not self-cleared: {cleared:?}"
        );
    }
}
