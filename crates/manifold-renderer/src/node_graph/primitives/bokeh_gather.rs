//! Layered, tile-bounded depth of field. See CINEMATIC_POST_DESIGN.md D10.
//! Runtime and proofs use the same encoding method; caches belong to the node.
use super::standalone_pipeline::dispatch_standalone_2d;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use manifold_gpu::{
    GpuBinding, GpuComputePipeline, GpuDevice, GpuFilterMode, GpuSamplerDesc, GpuTexture,
    GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use std::borrow::Cow;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BokehGatherUniforms {
    max_radius: f32,
    enabled: u32,
    aperture: u32,
    quality: u32,
    blur_alpha: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BokehRadiusUniforms {
    radius: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BokehCompositeUniforms {
    radius: f32,
    blur_alpha: u32,
    _pad0: f32,
    _pad1: f32,
}

#[derive(Clone, Copy)]
pub(super) struct BokehSettings {
    pub radius: f32,
    pub aperture: u32,
    pub quality: u32,
    pub blur_alpha: bool,
}

// Cache the whole geometric pyramid so radius modulation, including values
// beyond the display range, never reallocates textures. Dispatch only used mips.
fn mip_level_count(w: u32, h: u32) -> u32 {
    w.max(h).max(1).ilog2() + 1
}
fn active_mip_levels(radius: f32, available: usize) -> usize {
    // half-res radius / 2-texel footprint, plus the trilinear upper level.
    (((radius.max(1.0) * 0.25).log2().ceil().max(0.0) as usize) + 1).min(available)
}

pub struct BokehResources {
    width: u32,
    height: u32,
    far_chain: GpuTexture,
    near_chain: GpuTexture,
    far_views: Vec<GpuTexture>,
    near_views: Vec<GpuTexture>,
    guide: GpuTexture,
    packed: GpuTexture,
    packed_views: Vec<GpuTexture>,
    tiles: GpuTexture,
    reach: GpuTexture,
    far_result: GpuTexture,
    near_result: GpuTexture,
}
pub struct BokehPipelines {
    prefilter: GpuComputePipeline,
    downsample: GpuComputePipeline,
    guide_downsample: GpuComputePipeline,
    tiles: GpuComputePipeline,
    reach: GpuComputePipeline,
    pack: GpuComputePipeline,
    reconstruct: GpuComputePipeline,
}

crate::primitive! {
    name: BokehGather,
    type_id: "node.bokeh_gather",
    purpose: "Layered depth of field with half-resolution aperture gathers, separate premultiplied near/far color pyramids, conservative tile bounds and depth-aware full-resolution reconstruction. Signed CoC: R is blur magnitude in [0,1], G is 1 for foreground and 0 for background/in-focus. max_radius uses source pixels and must match the CoC producer. In-focus color is preserved. blur_alpha filters scene transparency in premultiplied space; its legacy false default preserves source alpha. Circle, hexagonal and octagonal apertures are supported. Disabled aliases input to output. Internal dependent mip/tile passes retain the BarrieredReduction fusion exemption.",
    inputs: {
        in: Texture2D required,
        width: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_radius"),
            label: "Max Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(24.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("enabled"),
            // "Depth of Field", not "Enabled": this param is stamped onto the
            // scene panel's Camera card next to motion_blur's own toggle, and
            // two rows both labeled "Enabled" are indistinguishable (Peter
            // 2026-08-27).
            label: "Depth of Field",
            ty: ParamType::Bool,
            default: ParamValue::Bool(true),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("aperture"),
            label: "Aperture Shape",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 2.0)),
            enum_values: &["Circle", "Hexagon", "Octagon"],
        },
        ParamDef {
            name: Cow::Borrowed("quality"),
            label: "Quality",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1),
            range: Some((0.0, 2.0)),
            enum_values: &["Low", "Medium", "High"],
        },
        ParamDef {
            name: Cow::Borrowed("blur_alpha"),
            label: "Blur Transparency",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Connect source color and signed CoC from coc_from_depth. Keep max_radius equal on the CoC producer and this node. Half-resolution processing is internal, so existing graph dimensions and bindings remain unchanged. Near/far color is separated before reduction; camera graphs enable blur_alpha to feather transparent silhouettes; full-resolution reconstruction retains focused detail. No temporal history or frame-dependent jitter. The generated gather stays a BarrieredReduction boundary because mip levels and tile bounds depend on prior passes.",
    examples: ["preset.generator.cinematic_scene"],
    picker: { label: "Bokeh Gather", category: Atom },
    summary: "Depth-of-field blur with clean foreground coverage and circular or polygonal highlights.",
    category: BlurAndSharpen,
    role: Filter,
    aliases: ["bokeh", "bokeh blur", "circular dof", "disc blur", "depth of field", "bokeh gather"],
    boundary_reason: BarrieredReduction,
    wgsl_body: include_str!("shaders/bokeh_gather_body.wgsl"),
    input_access: [Gather, Gather],
    stencil_fetch: true,
    extra_fields: {
        resources: Option<BokehResources> = None,
        pipelines: Option<BokehPipelines> = None,
    },
}
impl BokehPipelines {
    fn new(device: &GpuDevice) -> Self {
        let make = |source, label| device.create_compute_pipeline(source, "cs_main", label);
        Self {
            prefilter: make(
                include_str!("shaders/bokeh_prefilter.wgsl"),
                "bokeh separate layers",
            ),
            downsample: make(
                include_str!("shaders/bokeh_mip_downsample.wgsl"),
                "bokeh layer mip",
            ),
            guide_downsample: make(
                include_str!("shaders/bokeh_guide_downsample.wgsl"),
                "bokeh guide mip",
            ),
            tiles: make(
                include_str!("shaders/bokeh_tiles.wgsl"),
                "bokeh tile bounds",
            ),
            reach: make(
                include_str!("shaders/bokeh_tile_dilate.wgsl"),
                "bokeh tile reach",
            ),
            pack: make(
                include_str!("shaders/bokeh_pack_guide.wgsl"),
                "bokeh packed guide",
            ),
            reconstruct: make(
                include_str!("shaders/bokeh_reconstruct.wgsl"),
                "bokeh reconstruction",
            ),
        }
    }
}
impl BokehResources {
    fn new(device: &GpuDevice, width: u32, height: u32) -> Self {
        let (hw, hh) = (width.div_ceil(2), height.div_ceil(2));
        let levels = mip_level_count(hw, hh);
        let make = |w, h, mip_levels, label| {
            device.create_texture(&GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format: GpuTextureFormat::Rgba16Float,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
                label,
                mip_levels,
            })
        };
        let far_chain = make(hw, hh, levels, "bokeh far pyramid");
        let near_chain = make(hw, hh, levels, "bokeh near pyramid");
        let packed = make(hw, hh, levels, "bokeh gather guide");
        let views = |t: &GpuTexture| {
            (0..levels)
                .map(|l| t.mip_level_view(l, (hw >> l).max(1), (hh >> l).max(1)))
                .collect()
        };
        Self {
            width,
            height,
            far_views: views(&far_chain),
            near_views: views(&near_chain),
            far_chain,
            near_chain,
            guide: make(hw, hh, 1, "bokeh original guide"),
            packed_views: views(&packed),
            packed,
            tiles: make(hw.div_ceil(8), hh.div_ceil(8), 1, "bokeh tile bounds"),
            reach: make(hw.div_ceil(8), hh.div_ceil(8), 1, "bokeh tile reach"),
            far_result: make(hw, hh, 1, "bokeh far result"),
            near_result: make(hw, hh, 1, "bokeh near result"),
        }
    }
}
impl BokehGather {
    pub fn prewarm_pipelines(device: &GpuDevice) {
        let _ = BokehPipelines::new(device);
        let source = crate::node_graph::freeze::codegen::standalone_for_boundary_spec::<Self>()
            .expect("bokeh gather codegen");
        let _ = device.create_compute_pipeline(
            &source,
            crate::node_graph::freeze::codegen::ENTRY,
            "node.bokeh_gather",
        );
    }

    // Shared production encoding seam for the node and image/performance proofs.
    pub(super) fn encode(
        &mut self,
        gpu: &mut crate::gpu_encoder::GpuEncoder<'_>,
        source: &GpuTexture,
        width_tex: &GpuTexture,
        out: &GpuTexture,
        settings: BokehSettings,
    ) {
        let (w, h) = (out.width, out.height);
        if w == 0 || h == 0 {
            return;
        }
        let radius = if settings.radius.is_finite() {
            settings.radius.max(0.0)
        } else {
            0.0
        };
        if self
            .resources
            .as_ref()
            .is_none_or(|r| r.width != w || r.height != h)
        {
            self.resources = Some(BokehResources::new(gpu.device, w, h));
        }
        let r = self.resources.as_ref().expect("bokeh resources allocated");
        let p = self
            .pipelines
            .get_or_insert_with(|| BokehPipelines::new(gpu.device));
        let gather = self.pipeline.get_or_insert_with(|| {
            let shader = crate::node_graph::freeze::codegen::standalone_for_boundary_spec::<Self>()
                .expect("bokeh gather codegen");
            gpu.device.create_compute_pipeline(
                &shader,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.bokeh_gather",
            )
        });
        let sampler = self.sampler.get_or_insert_with(|| {
            gpu.device.create_sampler(&GpuSamplerDesc {
                mip_filter: GpuFilterMode::Linear,
                ..GpuSamplerDesc::default()
            })
        });
        let grid = [r.guide.width.div_ceil(16), r.guide.height.div_ceil(16), 1];
        let composite = BokehCompositeUniforms {
            radius,
            blur_alpha: u32::from(settings.blur_alpha),
            _pad0: 0.0,
            _pad1: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &p.prefilter,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: source,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: width_tex,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: &r.far_views[0],
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: &r.near_views[0],
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: &r.guide,
                },
                GpuBinding::Bytes {
                    binding: 5,
                    data: bytemuck::bytes_of(&composite),
                },
            ],
            grid,
            "bokeh separate layers",
        );
        for views in [&r.far_views, &r.near_views] {
            for l in 1..active_mip_levels(radius, views.len()) {
                let dst = &views[l];
                gpu.native_enc.dispatch_compute(
                    &p.downsample,
                    &[
                        GpuBinding::Texture {
                            binding: 0,
                            texture: &views[l - 1],
                        },
                        GpuBinding::Sampler {
                            binding: 1,
                            sampler,
                        },
                        GpuBinding::Texture {
                            binding: 2,
                            texture: dst,
                        },
                    ],
                    [dst.width.div_ceil(16), dst.height.div_ceil(16), 1],
                    "bokeh layer mip",
                );
            }
        }
        let tile_grid = [r.tiles.width.div_ceil(8), r.tiles.height.div_ceil(8), 1];
        gpu.native_enc.dispatch_compute(
            &p.tiles,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: &r.guide,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: &r.tiles,
                },
            ],
            tile_grid,
            "bokeh tile bounds",
        );
        let half_radius = BokehRadiusUniforms {
            radius: radius * 0.5,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &p.reach,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&half_radius),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: &r.tiles,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: &r.reach,
                },
            ],
            tile_grid,
            "bokeh tile reach",
        );
        gpu.native_enc.dispatch_compute(
            &p.pack,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: &r.guide,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: &r.reach,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: &r.packed_views[0],
                },
            ],
            grid,
            "bokeh packed guide",
        );
        for l in 1..active_mip_levels(radius, r.packed_views.len()) {
            let dst = &r.packed_views[l];
            gpu.native_enc.dispatch_compute(
                &p.guide_downsample,
                &[
                    GpuBinding::Texture {
                        binding: 0,
                        texture: &r.packed_views[l - 1],
                    },
                    GpuBinding::Texture {
                        binding: 1,
                        texture: dst,
                    },
                ],
                [dst.width.div_ceil(16), dst.height.div_ceil(16), 1],
                "bokeh guide mip",
            );
        }
        for (field, chain, dst) in [
            (0, &r.far_chain, &r.far_result),
            (1, &r.near_chain, &r.near_result),
        ] {
            let uniforms = BokehGatherUniforms {
                max_radius: radius * 0.5,
                enabled: field,
                aperture: settings.aperture.min(2),
                quality: settings.quality.min(2),
                blur_alpha: u32::from(settings.blur_alpha),
                _pad0: 0,
                _pad1: 0,
                _pad2: 0,
            };
            dispatch_standalone_2d(
                gpu,
                gather,
                bytemuck::bytes_of(&uniforms),
                &[chain, &r.packed],
                Some(sampler),
                dst,
                if field == 0 {
                    "bokeh far gather"
                } else {
                    "bokeh near gather"
                },
            );
        }
        gpu.native_enc.dispatch_compute(
            &p.reconstruct,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&composite),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: source,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: width_tex,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: &r.packed,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: &r.far_result,
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: &r.near_result,
                },
                GpuBinding::Sampler {
                    binding: 6,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 7,
                    texture: out,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "bokeh reconstruction",
        );
    }
}
impl Primitive for BokehGather {
    fn skip_passthrough(
        &self,
        params: &ParamValues,
        _wired_inputs: &[&str],
    ) -> Option<(&'static str, &'static str)> {
        matches!(params.get("enabled"), Some(ParamValue::Bool(false))).then_some(("in", "out"))
    }
    fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
        Some(("in", "out"))
    }
    fn output_dims(
        &self,
        port: &str,
        _canvas_dims: (u32, u32),
        inputs: &[(&str, (u32, u32))],
        _params: &ParamValues,
    ) -> Option<(u32, u32)> {
        if port != "out" {
            return None;
        }
        inputs
            .iter()
            .find(|(name, _)| *name == "in")
            .map(|(_, dims)| *dims)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let radius = ctx.param_f32("max_radius", 24.0);
        let aperture = match ctx.params.get("aperture") {
            Some(ParamValue::Enum(v)) => *v,
            _ => 0,
        };
        let quality = match ctx.params.get("quality") {
            Some(ParamValue::Enum(v)) => *v,
            _ => 1,
        };
        let blur_alpha = matches!(ctx.params.get("blur_alpha"), Some(ParamValue::Bool(true)));
        let Some(source) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(width) = ctx.inputs.texture_2d("width") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        self.encode(
            ctx.gpu_encoder(),
            source,
            width,
            out,
            BokehSettings {
                radius,
                aperture,
                quality,
                blur_alpha,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;
    #[test]
    fn bounded_mips_cover_all_accessible_radii() {
        assert_eq!(mip_level_count(1, 1), 1);
        assert_eq!(mip_level_count(1920, 1080), 11);
        assert_eq!(active_mip_levels(24.0, 5), 4);
        assert_eq!(active_mip_levels(64.0, 5), 5);
        assert_eq!(active_mip_levels(0.0, 5), 1);
    }
    #[test]
    fn uniform_and_existing_ports_are_compatible() {
        assert_eq!(std::mem::size_of::<BokehGatherUniforms>(), 32);
        assert_eq!(
            BokehGather::INPUTS
                .iter()
                .map(|p| p.name.as_ref())
                .collect::<Vec<_>>(),
            ["in", "width"]
        );
        assert_eq!(BokehGather::PARAMS[0].name, "max_radius");
        assert_eq!(BokehGather::PARAMS[1].name, "enabled");
        assert_eq!(BokehGather::PARAMS[2].default, ParamValue::Enum(0));
        assert_eq!(
            BokehGather::BOUNDARY_REASON,
            Some(crate::node_graph::freeze::classify::BoundaryReason::BarrieredReduction)
        );
    }
    #[test]
    fn disabled_aliases_source() {
        let node = BokehGather::new();
        let mut params = ParamValues::default();
        assert_eq!(Primitive::skip_passthrough(&node, &params, &[]), None);
        params.insert(Cow::Borrowed("enabled"), ParamValue::Bool(false));
        assert_eq!(
            Primitive::skip_passthrough(&node, &params, &[]),
            Some(("in", "out"))
        );
    }
    #[test]
    fn boundary_atom_still_generates_standalone_kernel() {
        let shader =
            crate::node_graph::freeze::codegen::standalone_for_boundary_spec::<BokehGather>()
                .unwrap();
        assert!(shader.contains("textureSampleLevel(tex_in, samp, tap_uv, lod)"));
    }
}
#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "bokeh_gather_tests.rs"]
mod gpu_tests;
