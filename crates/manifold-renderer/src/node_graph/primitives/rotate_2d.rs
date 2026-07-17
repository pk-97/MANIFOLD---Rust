//! `node.rotate_coordinates` — rotate a coordinate field by an angle.
//!
//! Reads (x, y) from the input's R/G channels and writes the rotated
//! (x', y') back to R/G:
//!
//! ```text
//! x' = x * cos(angle) - y * sin(angle)
//! y' = x * sin(angle) + y * cos(angle)
//! ```
//!
//! Operates on coordinate textures (output of `node.centered_uv`,
//! `node.uv_field`, etc.) — not pixel-sampled images. The whole
//! `angle → cos / sin / -sin → field_combine(a=cos, b=-sin)` chain
//! that an explicit rotation decomposition would require collapses
//! into this one primitive plus a downstream channel pick.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Rotate2DUniforms {
    angle: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Rotate2D,
    type_id: "node.rotate_coordinates",
    purpose: "Rotate a 2D coordinate field around the origin by `angle` (radians). Reads (x, y) from input R/G, writes rotated (x', y') back to R/G. Collapses the `angle → cos / sin / neg_sin → field_combine(cos, -sin)` chain that any rotated-projection effect would otherwise need.",
    inputs: {
        in: Texture2D required,
        // Port-shadowable for animation: drive `angle` from a time
        // wire times some rate.
        angle: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("angle"),
            label: "Angle",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
    ],
    depth_rule: Warp,
    composition_notes: "Use upstream of node.field_combine to extract a rotated coordinate channel as a scalar field (Plasma's v5 rotated-X term). Counter-clockwise: positive angle rotates +X toward +Y. Input must be a coordinate texture (centered_uv, uv_field, etc.) — the primitive does not resample image content.",
    examples: [],
    picker: { label: "Rotate Coordinates", category: Atom },
    summary: "Rotates a coordinate field around the centre. This spins the coordinates used to build a warp, not the image itself. For the picture, use Flip or a transform.",
    category: FieldsAndCoordinates,
    role: Map,
    aliases: ["rotate coordinates", "rotate 2d", "rotate field", "spin"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/rotate_2d_body.wgsl"),
}

impl Primitive for Rotate2D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let angle = match ctx.inputs.scalar("angle") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("angle") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel is
            // generated from `wgsl_body` so the atom fuses. The hand shader
            // (`shaders/rotate_2d.wgsl`) is retained only as the gpu_tests
            // parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.rotate_coordinates standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.rotate_coordinates",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = Rotate2DUniforms {
            angle,
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
                    texture: in_tex,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.rotate_coordinates",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn rotate_2d_declares_required_in_and_optional_angle() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(Rotate2D::TYPE_ID, "node.rotate_coordinates");
        let ins = Rotate2D::INPUTS;
        assert_eq!(ins.len(), 2);
        assert_eq!(ins[0].name, "in");
        assert!(ins[0].required);
        assert_eq!(ins[0].ty, PortType::Texture2D);
        assert_eq!(ins[1].name, "angle");
        assert!(!ins[1].required);
        assert_eq!(ins[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(Rotate2D::OUTPUTS.len(), 1);
        assert_eq!(Rotate2D::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn rotate_2d_has_angle_param() {
        let names: Vec<&str> = Rotate2D::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, vec!["angle"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Rotate2D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.rotate_coordinates");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **Generated-vs-hand parity** (`docs/ADDING_PRIMITIVES.md` "The codegen
    //! path is mandatory") — the standalone kernel built via
    //! `standalone_for_spec::<Rotate2D>()` must reproduce `shaders/rotate_2d.wgsl`
    //! (the hand oracle) texel-for-texel across a spread of angles, on a
    //! synthetic coordinate field whose R/G span well outside [0,1] (a
    //! rotation is only exercised meaningfully off-axis).
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
        GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::Rotate2D;
    use crate::render_target::RenderTarget;

    /// The hand shader's OWN uniform layout (`{cos_a, sin_a, pad, pad}`) —
    /// deliberately NOT `Rotate2DUniforms` (which now packs `{angle, pad,
    /// pad, pad}` to match the generated `Params` struct's PARAMS-order
    /// layout). The two structs diverge on purpose: the hand oracle still
    /// pre-computes cos/sin on the CPU side (`shaders/rotate_2d.wgsl`),
    /// while the codegen body computes them in WGSL from the raw `angle`
    /// param (`shaders/rotate_2d_body.wgsl`).
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct HandUniforms {
        cos_a: f32,
        sin_a: f32,
        _pad0: f32,
        _pad1: f32,
    }

    fn upload_rgba16f(device: &GpuDevice, w: u32, h: u32, label: &str, px: &[f16]) -> GpuTexture {
        assert_eq!(px.len(), (w * h * 4) as usize);
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label,
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// Coordinate field spanning [-2, 2] on R and [-1, 3] on G so a rotation
    /// visibly mixes channels rather than staying near-identity at small
    /// values.
    fn coord_field(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let tx = x as f32 / (w.saturating_sub(1).max(1)) as f32;
                let ty = y as f32 / (h.saturating_sub(1).max(1)) as f32;
                px[i] = f16::from_f32(-2.0 + tx * 4.0);
                px[i + 1] = f16::from_f32(-1.0 + ty * 4.0);
                px[i + 2] = f16::from_f32(0.0);
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        upload_rgba16f(device, w, h, "rotate2d-coord-field", &px)
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("rotate2d-readback");
        enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
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

    fn dispatch_rotate(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        src: &GpuTexture,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<[f32; 4]> {
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "rotate2d-out");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let mut enc = device.create_encoder("rotate2d-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: src },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "rotate2d-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    #[test]
    fn generated_rotate_2d_matches_hand_kernel_across_angles() {
        let device = crate::test_device();
        let (w, h) = (16u32, 4u32);
        let src = coord_field(&device, w, h);

        let hand_wgsl = include_str!("shaders/rotate_2d.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "rotate2d-hand");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Rotate2D>()
            .expect("node.rotate_coordinates standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "rotate2d-generated",
        );

        for angle in [0.0_f32, 0.3, std::f32::consts::FRAC_PI_2, 2.1, -1.7] {
            let hand_uniforms = HandUniforms {
                cos_a: angle.cos(),
                sin_a: angle.sin(),
                _pad0: 0.0,
                _pad1: 0.0,
            };
            let hand_bytes = bytemuck::bytes_of(&hand_uniforms).to_vec();

            // Generated `Params` struct follows PARAMS order (just `angle`,
            // padded to a 16-byte uniform).
            let mut gen_bytes = Vec::new();
            gen_bytes.extend_from_slice(&angle.to_le_bytes());
            gen_bytes.extend_from_slice(&[0u8; 12]);

            let hand_out = dispatch_rotate(&device, &hand_pipeline, &src, w, h, &hand_bytes);
            let gen_out = dispatch_rotate(&device, &gen_pipeline, &src, w, h, &gen_bytes);

            for (i, (h_px, g_px)) in hand_out.iter().zip(gen_out.iter()).enumerate() {
                for c in 0..4 {
                    assert!(
                        (h_px[c] - g_px[c]).abs() < 2e-3,
                        "angle={angle} texel={i} ch={c}: hand={} gen={}",
                        h_px[c],
                        g_px[c]
                    );
                }
            }
        }
    }
}
