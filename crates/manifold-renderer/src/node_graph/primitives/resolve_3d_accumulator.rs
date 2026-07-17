//! `node.resolve_scatter_3d` — convert a u32 fixed-point 3D
//! accumulator buffer into a float density Texture3D, with
//! self-clearing back to zero for the next frame.
//!
//! Bit-exact wrap of `generators/shaders/fluid_scatter_3d.wgsl`'s
//! `resolve_3d` entry point via include_str. Pairs with
//! `node.draw_particles_3d` upstream. First Texture3D-output
//! primitive in node_graph — exercises the new
//! `MetalBackend::pre_bind_texture_3d` path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: the `vol_res` / `vol_depth` Int params
/// (→ i32) + pad to 16 bytes. The generated standalone kernel is single-entry,
/// so the 112-byte same-binding-same-size padding the shared
/// `fluid_scatter_3d.wgsl` module needed is gone; and it derives its dims +
/// linear cell index from `textureDimensions(dst)`, so vol_res/vol_depth are
/// carried only as the user-facing params (the body ignores them). 2 words + 2
/// pad = 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Resolve3DUniforms {
    vol_res: i32,
    vol_depth: i32,
    fixed_point_scale: f32,
    _pad0: u32,
}

crate::primitive! {
    name: Resolve3DAccumulator,
    type_id: "node.resolve_scatter_3d",
    purpose: "Read a u32 fixed-point 3D accumulator buffer (produced by node.draw_particles_3d), divide by fixed_point_scale (default 4096, FluidSim3D's legacy FIXED_POINT_MULTIPLIER), and write the result as a density Texture3D. Self-clears the accumulator to zero atomically as part of the same dispatch so the next frame starts fresh.",
    inputs: {
        accum: Array(u32) required,
    },
    outputs: {
        density: Texture3D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("vol_res"),
            label: "Volume Resolution",
            ty: ParamType::Int,
            default: ParamValue::Float(128.0),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("vol_depth"),
            label: "Volume Depth",
            ty: ParamType::Int,
            default: ParamValue::Float(128.0),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
        // Mirrors the 2D resolve_scatter: raise together with the splat energy
        // when fractional per-particle energies are needed (density-normalized
        // containers run 16x = 65536).
        ParamDef {
            name: Cow::Borrowed("fixed_point_scale"),
            label: "Fixed Point Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(4096.0),
            range: Some((256.0, 1_048_576.0)),
            enum_values: &[],
        },
    ],
    // depth_rule: Texture3D output — outside the 2D depth channel
    depth_rule: Terminal,
    composition_notes: "vol_res × vol_res × vol_depth must match the producing ScatterParticles3D primitive. Output Texture3D must be Rgba16Float — the shader writes via texture_storage_3d<rgba16float, write>. The output volume is pre-bound by the chain build at the same dimensions; the accumulator buffer is sized vol_res² × vol_depth × 4 bytes.",
    examples: [],
    picker: { label: "Resolve Scatter (3D)", category: Atom },
    summary: "Reads back the 3D buffer that a 3D particle scatter wrote into and turns it into a volume you can sample.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["resolve scatter 3d", "resolve 3d accumulator", "accumulator", "volume read back"],
    fusion_kind: Boundary,
    boundary_reason: BarrieredReduction,
    wgsl_body: include_str!("shaders/resolve_3d_accumulator_body.wgsl"),
}

impl Primitive for Resolve3DAccumulator {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let vol_res = match ctx.params.get("vol_res") {
            Some(ParamValue::Float(n)) => n.round().max(1_f32) as u32,
            _ => 128,
        };
        let vol_depth = match ctx.params.get("vol_depth") {
            Some(ParamValue::Float(n)) => n.round().max(1_f32) as u32,
            _ => 128,
        };

        let fixed_point_scale = match ctx.params.get("fixed_point_scale") {
            Some(ParamValue::Float(n)) if *n > 0.0 => *n,
            _ => 4096.0,
        };

        let Some(accum) = ctx.inputs.array("accum") else {
            return;
        };
        let Some(density) = ctx.outputs.texture_3d("density") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (BUFFER→TEXTURE
            // 3D resolve — dims/idx from textureDimensions(dst), the body reads +
            // zeros the atomic accumulator and returns the density vec4). The
            // shared fluid_scatter_3d.wgsl resolve_3d is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.resolve_scatter_3d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.resolve_scatter_3d",
            )
        });

        let uniforms = Resolve3DUniforms {
            vol_res: vol_res as i32,
            vol_depth: vol_depth as i32,
            fixed_point_scale,
            _pad0: 0,
        };

        // Generated binding order follows the resolve path: uniform(0),
        // accum(1, atomic read_write), density(2, storage write). The hand
        // resolve_3d bound accum(0)/density(1)/uniform(2) — rebind. The generated
        // kernel is @workgroup_size(4,4,4) → dispatch div_ceil(4) (not the hand's
        // 8).
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
                    texture: density,
                },
            ],
            [
                vol_res.div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
                vol_res.div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
                vol_depth.div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
            ],
            "node.resolve_scatter_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn resolve_3d_declares_array_in_and_texture_3d_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let u32_layout = ArrayType::of_known::<u32>();
        assert_eq!(Resolve3DAccumulator::TYPE_ID, "node.resolve_scatter_3d");
        assert_eq!(Resolve3DAccumulator::INPUTS.len(), 1);
        assert_eq!(Resolve3DAccumulator::INPUTS[0].name, "accum");
        assert_eq!(
            Resolve3DAccumulator::INPUTS[0].ty,
            PortType::Array(u32_layout)
        );
        assert_eq!(Resolve3DAccumulator::OUTPUTS.len(), 1);
        assert_eq!(Resolve3DAccumulator::OUTPUTS[0].name, "density");
        assert_eq!(
            Resolve3DAccumulator::OUTPUTS[0].ty,
            PortType::Texture3D
        );
    }

    #[test]
    fn resolve_3d_uniform_struct_matches_generated_layout() {
        // The generated standalone kernel is single-entry → no 112-byte same-
        // binding padding. 2 i32 + fixed_point_scale f32 + 1 pad = 16 bytes.
        assert_eq!(std::mem::size_of::<Resolve3DUniforms>(), 16);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Resolve3DAccumulator::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.resolve_scatter_3d");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer→texture 3D resolve value oracle (freeze §12). Dispatches the
    //! generated kernel over a known u32 volume accumulator, reads back the
    //! density Texture3D, and asserts each voxel's R = raw / 4096 AND the
    //! accumulator is self-cleared. Proves the 3D resolve path: dims + the
    //! `z*vr*vr + y*vr + x` cell index derived from `textureDimensions(dst)`,
    //! atomicLoad/atomicStore on `buf_accum`, R-only density store.
    use super::*;
    use half::f16;
    use manifold_gpu::{
        GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    #[test]
    fn generated_resolve_3d_divides_and_self_clears() {
        let device = crate::test_device();
        let (vr, vd) = (4u32, 2u32); // dims (vr, vr, vd)
        let cells = (vr * vr * vd) as usize;

        // Cell i holds i * 4096 → expected density i.0 (exact in f16 for i < 2048).
        let accum_raw: Vec<u32> = (0..cells as u32).map(|i| i * 4096).collect();
        let accum = device.create_buffer_shared((cells * 4) as u64);
        unsafe {
            accum.write(0, bytemuck::cast_slice(&accum_raw));
        }

        let density = device.create_texture(&GpuTextureDesc {
            width: vr,
            height: vr,
            depth: vd,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D3,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "resolve3d-out",
            mip_levels: 1,
        });

        let gen_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<Resolve3DAccumulator>()
                .expect("resolve_3d_accumulator codegen");
        assert!(
            gen_wgsl.contains("array<atomic<u32>>"),
            "accumulator bound as atomic"
        );
        assert!(
            gen_wgsl.contains("texture_storage_3d"),
            "3D storage output"
        );
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "resolve3d-oracle",
        );

        let uniforms = Resolve3DUniforms {
            vol_res: vr as i32,
            vol_depth: vd as i32,
            fixed_point_scale: 4096.0,
            _pad0: 0,
        };
        let mut enc = device.create_encoder("resolve3d-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &accum, offset: 0 },
                GpuBinding::Texture { binding: 2, texture: &density },
            ],
            [
                vr.div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
                vr.div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
                vd.div_ceil(crate::node_graph::freeze::codegen::VOLUME_WORKGROUP_3D),
            ],
            "resolve3d-oracle",
        );
        enc.commit_and_wait_completed();

        // Read back the density volume (z-slice-major, 8 bytes/voxel).
        let bytes_per_row = vr * 8;
        let bytes_per_image = bytes_per_row * vr;
        let total = u64::from(bytes_per_image * vd);
        let readback = device.create_buffer_shared(total);
        let mut rb = device.create_encoder("resolve3d-readback");
        rb.copy_texture_3d_to_buffer(&density, &readback, vr, vr, vd, bytes_per_row);
        rb.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback");
        let px: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), cells * 4) };
        // u16 index of voxel (x,y,z) R channel: z*(vr*vr*4) + y*(vr*4) + x*4.
        for z in 0..vd {
            for y in 0..vr {
                for x in 0..vr {
                    let cell = (z * vr * vr + y * vr + x) as usize;
                    let u16_idx = (z * vr * vr * 4 + y * vr * 4 + x * 4) as usize;
                    let r = f16::from_bits(px[u16_idx]).to_f32();
                    let expected = accum_raw[cell] as f32 / 4096.0;
                    assert!(
                        (r - expected).abs() < 1e-3,
                        "voxel ({x},{y},{z}) cell {cell}: density {r} != {expected}"
                    );
                }
            }
        }

        // Self-clear.
        let aptr = accum.mapped_ptr().expect("shared accum");
        let cleared: &[u32] = unsafe { std::slice::from_raw_parts(aptr.cast::<u32>(), cells) };
        assert!(
            cleared.iter().all(|&v| v == 0),
            "3D accumulator not self-cleared"
        );
    }
}
