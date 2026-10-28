//! `node.mesh_texture_weights` — read a texture across a mesh as one weight
//! per vertex, by box projection from the vertex's place in its frame.
//!
//! UV coordinates jump at a mesh's seams (every face of a built-in cube has
//! its own), so a texture sampled by UV pulls a displaced mesh apart there.
//! This node samples by position instead: three planar samples blended by
//! the normal (Blender's Box projection), with mirrored repeat so any image
//! tiles without a seam. Mapping UV reads the mesh's own UVs instead, for an
//! image painted for them (a scan's height map, a terrain laid once across
//! a plane), as Blender's Displace does with UV coordinates. Wire its
//! `weights` into a deformer's weights (or a mask's incoming weights) to
//! displace by an image, a layer or noise.

/// Mapping's choices: box projection, or the mesh's own UVs.
const MAPPINGS: &[&str] = &["Box", "UV"];

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

/// Generated-codegen uniform layout: params in PARAMS order, then
/// `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TextureWeightsUniforms {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    bias: f32,
    frame_radius: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    mapping: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: MeshTextureWeights,
    type_id: "node.mesh_texture_weights",
    purpose: "One f32 weight per vertex of an Array<MeshVertex>: the red channel of `field` box-projected at the vertex's frame position q = (position + source_offset) / frame_radius * scale. Three samples on the yz, xz and xy planes (each shifted by offset_x/offset_y) blend by the normal's fourth power; coordinates repeat mirrored so any image tiles seamlessly. weight = blend - bias. A function of position and normal only, so split copies of a corner with one welded normal read one weight. Mapping UV reads one sample at the vertex's own uv * scale (shifted by offset_x/offset_y) instead, for an image painted for the mesh's UVs.",
    inputs: {
        in: Array(MeshVertex) required,
        field: Texture2D required,
        scale: ScalarF32 optional,
        offset_x: ScalarF32 optional,
        offset_y: ScalarF32 optional,
        bias: ScalarF32 optional,
        frame_radius: ScalarF32 optional,
        source_offset_x: ScalarF32 optional,
        source_offset_y: ScalarF32 optional,
        source_offset_z: ScalarF32 optional,
    },
    outputs: {
        weights: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("scale"), label: "Scale", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.01, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("offset_x"), label: "Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("offset_y"), label: "Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bias"), label: "Bias", ty: ParamType::Float, default: ParamValue::Float(0.5), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("frame_radius"), label: "Frame Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.000001, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("mapping"), label: "Mapping", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 1.0)), enum_values: MAPPINGS },
    ],
    depth_rule: Terminal,
    composition_notes: "Displace by an image: node.smooth_normals → this (in) and any Texture2D (a noise, node.layer_source, an image file) → field, then the weights into node.push_along_normals (directly or through node.mesh_spatial_mask's incoming weights), with the smoothed mesh as the push's direction. Bias 0.5 pushes dark parts in and light parts out. Scale is repeats per frame radius.",
    examples: [],
    picker: { label: "Mesh Texture Weights", category: Atom },
    summary: "Reads a picture across a 3D mesh from all three sides at once, giving every point a value that can push the surface in or out.",
    category: Geometry3D,
    role: Source,
    aliases: ["texture weights", "box projection", "triplanar", "displacement map"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/mesh_texture_weights_body.wgsl"),
}

impl Primitive for MeshTextureWeights {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "weights")
            .then(|| {
                input_capacities
                    .iter()
                    .find(|(p, _)| *p == "in")
                    .map(|(_, n)| *n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let uniforms = TextureWeightsUniforms {
            scale: ctx.scalar_or_param("scale", 1.0),
            offset_x: ctx.scalar_or_param("offset_x", 0.0),
            offset_y: ctx.scalar_or_param("offset_y", 0.0),
            bias: ctx.scalar_or_param("bias", 0.5),
            frame_radius: ctx.scalar_or_param("frame_radius", 1.0),
            source_offset_x: ctx.scalar_or_param("source_offset_x", 0.0),
            source_offset_y: ctx.scalar_or_param("source_offset_y", 0.0),
            source_offset_z: ctx.scalar_or_param("source_offset_z", 0.0),
            mapping: match ctx.params.get("mapping") {
                Some(ParamValue::Enum(v)) => (*v).min(1),
                Some(ParamValue::Float(v)) => v.round().clamp(0.0, 1.0) as u32,
                _ => 0,
            },
            dispatch_count: 0,
            _pad0: 0,
            _pad1: 0,
        };
        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(field) = ctx.inputs.texture_2d("field") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("weights") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let count = ((src.size / vertex_size) as u32).min((dst.size / 4) as u32);
        if count == 0 {
            return;
        }
        let uniforms = TextureWeightsUniforms {
            dispatch_count: count,
            ..uniforms
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: src,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: field,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.mesh_texture_weights",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level proof: the generated standalone kernel against the
    //! committed box-projection formula, computed on the CPU. The field is two
    //! texels wide and every sample lands on a texel centre, so filtering
    //! reads each texel exactly.
    use super::*;
    use manifold_gpu::{GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

    /// The two texels' red values: 51/255 and 255/255.
    const TEXELS: [f32; 2] = [0.2, 1.0];

    struct Settings {
        scale: f32,
        shift: [f32; 2],
        bias: f32,
        radius: f32,
        source_offset: [f32; 3],
        uv: bool,
    }

    const PLAIN: Settings = Settings {
        scale: 1.0,
        shift: [0.0, 0.0],
        bias: 0.5,
        radius: 1.0,
        source_offset: [0.0; 3],
        uv: false,
    };

    fn vertex(position: [f32; 3], normal: [f32; 3]) -> MeshVertex {
        MeshVertex {
            position,
            normal,
            color: [1.0; 4],
            ..bytemuck::Zeroable::zeroed()
        }
    }

    fn mirrored(t: f32) -> f32 {
        1.0 - ((t * 0.5).rem_euclid(1.0) * 2.0 - 1.0).abs()
    }

    /// A texel-centre read of the two-texel field.
    fn sample(u: f32) -> f32 {
        TEXELS[usize::from(u >= 0.5)]
    }

    /// The formula: three planar reads blended by the normal's fourth power.
    fn expected(v: &MeshVertex, s: &Settings) -> f32 {
        if s.uv {
            return sample(mirrored(v.uv[0] * s.scale + s.shift[0])) - s.bias;
        }
        let q: [f32; 3] =
            std::array::from_fn(|k| (v.position[k] + s.source_offset[k]) / s.radius * s.scale);
        let w: [f32; 3] = std::array::from_fn(|k| v.normal[k].powi(4));
        let total: f32 = w.iter().sum();
        let reads = [
            sample(mirrored(q[1] + s.shift[0])),
            sample(mirrored(q[0] + s.shift[0])),
            sample(mirrored(q[0] + s.shift[0])),
        ];
        (0..3).map(|k| w[k] / total * reads[k]).sum::<f32>() - s.bias
    }

    fn dispatch(src: &[MeshVertex], s: &Settings) -> Vec<f32> {
        let device = manifold_gpu::testkit::test_device();
        let wgsl =
            manifold_node_engine::freeze::codegen::standalone_for_spec::<MeshTextureWeights>()
                .expect("mesh_texture_weights buffer codegen");
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            manifold_node_engine::freeze::codegen::ENTRY,
            "texture-weights-test",
        );
        let field = device.create_texture(&GpuTextureDesc {
            width: 2,
            height: 1,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "texture-weights-field",
            mip_levels: 1,
        });
        device.upload_texture(&field, &[51, 0, 0, 255, 255, 0, 0, 255]);
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let input = device.create_buffer(
            std::mem::size_of_val(src) as u64,
            manifold_gpu::BufferUse::Upload,
        );
        unsafe {
            input.write(0, bytemuck::cast_slice(src));
        }
        let output = device.create_buffer((src.len() * 4) as u64, manifold_gpu::BufferUse::Upload);
        let uniforms = TextureWeightsUniforms {
            scale: s.scale,
            offset_x: s.shift[0],
            offset_y: s.shift[1],
            bias: s.bias,
            frame_radius: s.radius,
            source_offset_x: s.source_offset[0],
            source_offset_y: s.source_offset[1],
            source_offset_z: s.source_offset[2],
            mapping: u32::from(s.uv),
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
        };
        let mut enc = device.create_encoder("texture-weights-test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &input,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: &field,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler: &sampler,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &output,
                    offset: 0,
                },
            ],
            [(src.len() as u32).div_ceil(256), 1, 1],
            "texture-weights-test",
        );
        enc.commit_and_wait().expect("GPU work completes");
        let ptr = output.mapped_ptr().expect("shared weights buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const f32, src.len()) }.to_vec()
    }

    fn check(src: &[MeshVertex], s: &Settings) {
        for (i, (got, v)) in dispatch(src, s).iter().zip(src).enumerate() {
            let want = expected(v, s);
            assert!(
                (got - want).abs() < 2e-3,
                "vertex {i}: got {got}, want {want}"
            );
        }
    }

    #[test]
    fn box_projection_matches_the_formula() {
        let up = [0.0, 0.0, 1.0];
        let side = [1.0, 0.0, 0.0];
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let src = [
            // Facing Z: the xy plane, mirrored past 1.
            vertex([0.25, 0.0, 0.0], up),
            vertex([0.75, 0.0, 0.0], up),
            vertex([1.25, 0.0, 0.0], up),
            // Facing X: the yz plane.
            vertex([0.0, 0.25, 0.0], side),
            vertex([0.0, 0.75, 0.0], side),
            // Between X and Y: half of each plane.
            vertex([0.25, 0.75, 0.0], [half, half, 0.0]),
        ];
        check(&src, &PLAIN);
        assert!(
            (dispatch(&src[..1], &PLAIN)[0] - (0.2 - 0.5)).abs() < 2e-3,
            "a dark texel pushes in by the bias"
        );
        // Shift, scale, frame radius and source offset move where it reads.
        let moved = Settings {
            scale: 2.0,
            shift: [0.5, 0.0],
            bias: 0.0,
            radius: 2.0,
            source_offset: [0.5, 0.0, 0.0],
            uv: false,
        };
        check(&src, &moved);
    }

    /// Mapping UV reads the vertex's own UV, wherever the vertex sits and
    /// whichever way it faces.
    #[test]
    fn uv_mapping_reads_each_vertex_at_its_own_uv() {
        let at = |position: [f32; 3], normal: [f32; 3], u: f32| MeshVertex {
            uv: [u, 0.5],
            ..vertex(position, normal)
        };
        let src = [
            at([5.0, 0.0, 0.0], [0.0, 0.0, 1.0], 0.25),
            at([5.0, 0.0, 0.0], [1.0, 0.0, 0.0], 0.75),
            at([-3.0, 2.0, 1.0], [0.0, 1.0, 0.0], 1.25),
        ];
        check(&src, &Settings { uv: true, ..PLAIN });
        check(
            &src,
            &Settings {
                scale: 2.0,
                shift: [0.5, 0.0],
                bias: 0.0,
                uv: true,
                ..PLAIN
            },
        );
        let reads = dispatch(&src[..2], &Settings { uv: true, ..PLAIN });
        assert!(
            (reads[0] - (0.2 - 0.5)).abs() < 2e-3 && (reads[1] - (1.0 - 0.5)).abs() < 2e-3,
            "the same place reads by its UV: {reads:?}"
        );
    }
}
