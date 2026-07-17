//! `node.surface_bumps` — scalar height field → unit normal map via
//! central-difference gradient.
//!
//! Reads `in.r` as height per pixel, computes `(dh/dx, dh/dy)` via
//! half-difference of adjacent samples. Two output coordinate spaces,
//! picked by the `coord_space` param:
//!
//! - **TangentZ** (default — flat-surface tangent-space convention used
//!   by `lambert_directional`, `matcap_two_tone`, `blinn_specular` and
//!   the rest of the OilyFluid-shaped screen-space PBR family):
//!   `N = normalize(-dh/dx, -dh/dy * aspect, z_scale)`. Surface-normal
//!   direction sits on the `.z` channel.
//!
//! - **WorldYUp** (3D mesh laid out in the world XZ plane with Y up —
//!   matches the MetallicGlass full-resolution-reflection trick where a
//!   per-pixel surface normal is derived from the displaced height field
//!   rather than from the under-sampled vertex normals):
//!   `N = normalize(-dh/dx, z_scale, -dh/dy * aspect)`. Surface-normal
//!   direction sits on the `.y` channel.
//!
//! The `aspect` input scales the Y-axis gradient so non-square world
//! quads keep the right relative slope (default 1.0 = no correction).
//! Larger `z_scale` = flatter normals; smaller = steeper. Output is
//! signed (range [-1, 1] per channel), alpha = 1.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HeightmapNormalUniforms {
    z_scale: f32,
    aspect: f32,
    coord_space: u32, // 0 = TangentZ (default), 1 = WorldYUp
    _pad0: f32,
}

crate::primitive! {
    name: HeightmapToNormal,
    type_id: "node.surface_bumps",
    purpose: "Scalar height field (read from `in.r`) → unit normal map (RGB) via central-difference gradient. Coord_space picks the output convention: TangentZ for flat-surface tangent-space shading (OilyFluid lambert/matcap/blinn), WorldYUp for 3D meshes laid out in the XZ plane with Y up (MetallicGlass full-resolution-reflection trick). Larger `z_scale` flattens the normal; smaller steepens it. `aspect` scales the Y-axis gradient for non-square world quads. Output is SIGNED (range [-1, 1] per channel).",
    inputs: {
        in: Texture2D required,
        z_scale: ScalarF32 optional,
        aspect: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("z_scale"),
            label: "Z Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.001, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("aspect"),
            label: "Aspect",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("coord_space"),
            label: "Coord Space",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: &["TangentZ", "WorldYUp"],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Height is read from the R channel only. If your height is a derived quantity (e.g. `length(color.rg)` in the oily-fluid family) wire `node.vector_length` upstream first. TangentZ (default) pairs with `node.basic_light`, `node.matcap_two_tone`, `node.rim_light`, `node.shininess` for the flat-surface screen-space shading family. WorldYUp feeds `node.render_mesh`'s `normal_map` input for full PBR on a 3D mesh laid out in the XZ plane (the renderer's `node.pbr_material` owns the Cook-Torrance + IBL shading) — the MetallicGlass pattern. The `aspect` input is normally wired from `system.generator_input.aspect` so reflections stay correct across canvas aspect ratios.",
    examples: [],
    picker: { label: "Surface Bumps", category: Atom },
    summary: "Turns a grayscale height image into a normal map, so light and dark become bumps and dents the lighting can catch. The way to add surface detail from a texture.",
    category: MaterialsAndLighting,
    role: Filter,
    aliases: ["surface bumps", "heightmap to normal", "normal map", "bump map", "heightmap"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/heightmap_to_normal_body.wgsl"),
    input_access: [Gather],
    // D6(a) deliberately does NOT mark `in` precision_critical here, despite
    // DEPTH_RELIGHT_DESIGN.md §D6 naming this atom as a "differentiate"
    // consumer: `in` is `Gather` (see the body — 4 `textureSampleLevel`
    // taps through a real filtering sampler), not `GatherTexel`. A
    // non-filterable `Rgba32Float` producer can't back a filtering sampler
    // read on Apple GPUs, so promoting an upstream intermediate here would
    // break this atom's own read — the meta-test
    // `precision_critical_inputs_are_texel_exact` (freeze::classify) is
    // exactly the guard that would catch this if it were mis-marked.
}

impl Primitive for HeightmapToNormal {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read = |name: &str, default: f32| -> f32 {
            match ctx.inputs.scalar(name) {
                Some(ParamValue::Float(f)) => f,
                _ => match ctx.params.get(name) {
                    Some(ParamValue::Float(f)) => *f,
                    _ => default,
                },
            }
        };
        let z_scale = read("z_scale", 0.5);
        let aspect = read("aspect", 1.0);
        let coord_space: u32 = match ctx.params.get("coord_space") {
            Some(ParamValue::Enum(v)) => *v,
            _ => 0,
        };

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (target.width, target.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // `in` is a Gather input (4-neighbour central difference). Generated
            // kernel binds uniform(0)/tex(1)/samp(2)/dst(3); the body recovers the
            // texel step from dims. heightmap_to_normal.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.surface_bumps standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.surface_bumps",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = HeightmapNormalUniforms {
            z_scale,
            aspect,
            coord_space,
            _pad0: 0.0,
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
                    texture: src,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.surface_bumps",
        );
    }
}
