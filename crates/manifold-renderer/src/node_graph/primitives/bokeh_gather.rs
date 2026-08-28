//! `node.bokeh_gather` — single-pass occlusion-aware disc gather DoF
//! (`docs/CINEMATIC_POST_DESIGN.md` D5, CINEMATIC_POST P4). Replaces the two
//! `node.variable_blur` (H then V) passes inside `CinematicScene`'s DoF
//! chain with ONE gather dispatch: 32 golden-angle spiral taps (D2), scaled
//! by the CENTER pixel's CoC, each tap weighted by whether its OWN CoC
//! reaches back to the center (the standard scatter-as-gather occlusion
//! approximation — the same idea `node.variable_blur`'s
//! `ScatterAsGatherByCoC` weighting mode applies along one axis, generalized
//! here to a full 2D disc), luminance-preserving normalization, circular
//! aperture v1 (no blade count).
//!
//! **Mip-gather upgrade** (2026-08-28, silhouette-edge speckle fix): at full
//! blur the 32-tap disc covers ~1800 px² (~1 sample per 56 px²), so each tap
//! at a bright-on-black silhouette was a coin flip between a hot pixel and
//! black, and the per-pixel spiral rotation decorrelated neighboring
//! pixels' outcomes — sparse static dots hugging hard edges (Peter's repro:
//! 'Right Where I Need You - Music Video V5', beat 21.1). The fix: run()
//! builds a small internal mip chain of `in` once per frame (exact
//! box-average downsamples, `shaders/bokeh_mip_downsample.wgsl` — not
//! Metal's `generate_mipmaps`, whose filter is undocumented and unportable,
//! and the I1 CPU reference must model the filter precisely), binds the
//! chain as `in`, and the body samples tap colors at a fractional LOD
//! derived from the center CoC — `lod = clamp(log2(coc_px / 4), 0, 8)` — so
//! the disc stays dense (~4-texel effective radius) at the sampled level and
//! every tap reads an area average. CoC weights stay full-res: occlusion
//! boundaries remain crisp, only color is variance-reduced. Precedent:
//! `render_scene`'s internal specular prefilter mip chain.
//!
//! **Fusion exemption**: the internal mip chain is a barriered multi-pass
//! prefilter the fused form can never express (a fused gather input has no
//! mip levels), so this atom declares `BoundaryReason::BarrieredReduction`
//! (same class as `peak`/`luminance`). It stays on the codegen path via
//! `standalone_for_boundary_spec` — the runtime gather kernel is generated
//! from `wgsl_body`, never `include_str!`. The body's raw
//! `textureSampleLevel(tex_in, samp, …, lod)` is legal precisely because a
//! Boundary atom's body is only ever emitted standalone, where those
//! binding names exist. (In practice the atom never fused anyway: in-loop
//! non-exact-tap gathers are region boundaries, and CinematicScene's fusion
//! report shows it ungrouped.)
//!
//! **Scoping decision** (D5): this atom REPLACES the two `variable_blur`
//! nodes ONLY inside the preset wiring — `node.variable_blur` itself is
//! untouched (still ships, still used elsewhere/available in the palette).
//! `bokeh_gather` shares `variable_blur`'s `in`/`width` port names/shapes
//! exactly so the preset swap is a straight re-wire, not a new topology.
//!
//! section 2.5 audit (`docs/DECOMPOSING_GENERATORS.md`, re-verified 2026-07-13):
//! `rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g
//! "*.rs" | grep -i bokeh` → 0 hits before this file. Genuinely new — no
//! existing atom does a CoC-weighted 2D disc gather (the design doc's own
//! section 2.5 audit already named `bokeh_gather` as one of the four genuinely-new
//! atoms in this cluster; this re-confirms zero drift since).
//!
//! Precedent read end-to-end before authoring: `gaussian_blur_variable_
//! width.rs` (the atom being replaced — same `in`/`width` port names, same
//! `MultiInputCoincident`/`[Gather, Gather]` ABI shape); `coc_dilate.rs`
//! (prior CINEMATIC_POST phase's atom — repo idiom + confirms the CoC
//! texture convention this atom consumes: R == G == B == coc_px /
//! max_radius, a [0,1] fraction, alpha == 1.0); `ssao_from_depth.rs` /
//! `ssao_from_depth_body.wgsl` (the D2 golden-angle spiral + per-pixel
//! rotation hash formula, copied verbatim per the synthesis-drift rule —
//! never re-derived); `motion_blur.rs` (the CPU-reference + I1/I2 gpu_tests
//! shape for a two-texture-input Gather atom, mirrored below);
//! `render_scene.rs` (the internal-mip-chain precedent for the speckle
//! fix).
//!
//! `enabled = false` skips the node entirely — `skip_passthrough` aliases
//! `in` onto `out` (zero GPU work, no mip chain built), the DoF on/off
//! toggle Peter asked for in CINEMATIC_SCENE_TAIL_DESIGN.md P4.

use std::borrow::Cow;

use manifold_gpu::{
    GpuBinding, GpuFilterMode, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
    GpuTextureFormat, GpuTextureUsage,
};

use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: the `max_radius` param (f32) then the
/// `enabled` param (Bool → u32), padded to a 16-byte (4-word) multiple.
/// `enabled` is host-only: the shader never reads it, but the codegen path
/// lays every param into the uniform struct. Mirrors `node.variable_blur`'s
/// `BlurUniforms` / `node.motion_blur`'s `MotionBlurUniforms` layout-note
/// convention.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BokehGatherUniforms {
    max_radius: f32,
    enabled: u32,
    _pad0: f32,
    _pad1: f32,
}

/// Full mip-chain depth for a `w`×`h` source (level 0 … 1×1).
fn mip_level_count(w: u32, h: u32) -> u32 {
    w.max(h).max(1).ilog2() + 1
}

crate::primitive! {
    name: BokehGather,
    type_id: "node.bokeh_gather",
    purpose: "Single-pass occlusion-aware disc gather depth-of-field (docs/CINEMATIC_POST_DESIGN.md D5): 32 golden-angle spiral taps (r_i = sqrt((i+0.5)/32), theta_i = i*2.399963, rotated per-pixel by the committed hash) scaled by the CENTER pixel's CoC (read from `width`'s R channel, coc_from_depth/coc_dilate's [0,1]-fraction-of-max_radius convention), each tap weighted by step(distance_to_center_px, tap_coc_px) — a sample only contributes if its OWN CoC reaches back to the center (the standard scatter-as-gather occlusion approximation, generalizing node.variable_blur's ScatterAsGatherByCoC weighting from 1D taps to a 2D disc). Tap colors are sampled from an internal mip chain of `in` (built per frame by exact box-average downsample dispatches) at a fractional LOD derived from the center CoC — lod = clamp(log2(coc_px/4), 0, 8) — so silhouette taps read area averages instead of coin-flipping between hot pixels and black (the 2026-08-28 speckle fix); CoC weights stay full-res, so occlusion boundaries remain crisp. Luminance-preserving normalization (divide by the accumulated weight; falls back to the center color if every tap is occluded). Circular aperture v1 — no blade-count shaping. center_coc < 0.005 (in-focus) is an exact pass-through, same convention as node.variable_blur's own in-focus early-out — a zero-CoC lens (f_stop = infinity) produces a bit-clean image through this atom. Same `in`/`width` port shape as node.variable_blur so it drops straight into a DoF chain in its place: coc_from_depth (-> coc_dilate) -> bokeh_gather.width, upstream color -> bokeh_gather.in. `enabled = false` skips the node entirely (host-side `in → out` alias, zero GPU work — no mip chain is built). Fusion-exempt (BoundaryReason::BarrieredReduction): the internal prefilter mip chain is a barriered multi-pass dependency the fused form cannot express.",
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
    ],
    depth_rule: Inherit,
    composition_notes: "Drop-in replacement for the two node.variable_blur (H then V) nodes inside a DoF chain: wire the same `in` (the color feeding variable_blur_h.in) and the same `width` (the CoC source feeding both variable_blur H/V — coc_dilate's output in CinematicScene, NOT coc_from_depth directly, so BUG-137's dilation still reaches this gather) into this ONE node instead, and wire its `out` to whatever consumed variable_blur_v.out. `max_radius` must match the upstream CoC producer's own max_radius param (same shared-units contract node.variable_blur has with node.coc_from_depth) — set to the same value (24.0 default, matching CinematicScene's coc/coc_dilate/variable_blur chain). One gather dispatch replaces two blur dispatches, plus a small internal mip-prefilter chain (cached, rebuilt on resize only) that keeps silhouette bokeh smooth — tiny emitters bloom as clean discs rather than sparse dots. The atom never fuses (BoundaryReason::BarrieredReduction) — do not expect it inside fused kernels in fusion reports.",
    examples: ["preset.generator.cinematic_scene"],
    picker: { label: "Bokeh Gather", category: Atom },
    summary: "A true circular-aperture depth-of-field blur: each out-of-focus pixel gathers from a disc of neighbors sized by its own blur amount, and neighbors only contribute if they're blurry enough to reach back — the photographic 'bokeh' look, in one pass.",
    category: BlurAndSharpen,
    role: Filter,
    aliases: ["bokeh", "bokeh blur", "circular dof", "disc blur", "depth of field", "bokeh gather"],
    boundary_reason: BarrieredReduction,
    wgsl_body: include_str!("shaders/bokeh_gather_body.wgsl"),
    input_access: [Gather, Gather],
    stencil_fetch: true,
    extra_fields: {
        // Internal prefilter mip chain of `in` (see module doc). Cached
        // across frames; rebuilt only when `in`'s dims change. `mip_views[l]`
        // is the single-level view of the chain at level l (downsample dst /
        // next level's src). No per-frame allocation on the hot path.
        mip_chain: Option<GpuTexture> = None,
        mip_views: Vec<GpuTexture> = Vec::new(),
        downsample_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
    },
}

impl Primitive for BokehGather {
    /// Param-driven no-op: `enabled = false` aliases `in` onto `out` —
    /// zero GPU work — instead of running the gather.
    fn skip_passthrough(
        &self,
        params: &ParamValues,
        _wired_inputs: &[&str],
    ) -> Option<(&'static str, &'static str)> {
        match params.get("enabled") {
            Some(ParamValue::Bool(false)) => Some(("in", "out")),
            _ => None,
        }
    }

    /// Static declaration of the alias `skip_passthrough` may install —
    /// must agree with the dynamic hook (EffectNode contract).
    fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
        Some(("in", "out"))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let max_radius = match ctx.params.get("max_radius") {
            Some(ParamValue::Float(f)) => *f,
            _ => 24.0,
        };

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(width_tex) = ctx.inputs.texture_2d("width") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        let (sw, sh) = (src.width, src.height);
        if w == 0 || h == 0 || sw == 0 || sh == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();

        // Internal prefilter mip chain of `in` — rebuilt on resize only
        // (hot-path rule: no per-frame allocation).
        let levels = mip_level_count(sw, sh);
        let rebuild = match &self.mip_chain {
            Some(t) => t.width != sw || t.height != sh,
            None => true,
        };
        if rebuild {
            self.mip_chain = Some(gpu.device.create_texture(&GpuTextureDesc {
                width: sw,
                height: sh,
                depth: 1,
                format: GpuTextureFormat::Rgba16Float,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
                label: "node.bokeh_gather mip chain",
                mip_levels: levels,
            }));
            let chain = self.mip_chain.as_ref().expect("mip chain just created");
            self.mip_views = (0..levels)
                .map(|l| chain.mip_level_view(l, (sw >> l).max(1), (sh >> l).max(1)))
                .collect();
        }

        let downsample = self.downsample_pipeline.get_or_insert_with(|| {
            // Internal prefilter helper (not the atom's runtime kernel — that
            // is codegen-generated below). Hand-authored so the downsample
            // filter is an exact box by construction on every backend; the
            // I1 CPU reference models it precisely.
            gpu.device.create_compute_pipeline(
                include_str!("shaders/bokeh_mip_downsample.wgsl"),
                "cs_main",
                "node.bokeh_gather mip downsample",
            )
        });
        // mip_filter = Linear: the body's fractional LOD must be trilinear.
        // The same sampler serves the downsample dispatches (single-level
        // views + explicit LOD 0 make the mip filter moot there).
        let sampler = self.sampler.get_or_insert_with(|| {
            gpu.device.create_sampler(&GpuSamplerDesc {
                mip_filter: GpuFilterMode::Linear,
                ..GpuSamplerDesc::default()
            })
        });

        // Fill the chain: level 0 is the identity-UV copy of `in` (also
        // normalizing the source format into rgba16float — bilinear at exact
        // texel centers is the texel itself); each deeper level box-averages
        // the previous one.
        for l in 0..levels {
            let (dw, dh) = ((sw >> l).max(1), (sh >> l).max(1));
            let src_level: &GpuTexture = if l == 0 {
                src
            } else {
                &self.mip_views[(l - 1) as usize]
            };
            gpu.native_enc.dispatch_compute(
                downsample,
                &[
                    GpuBinding::Texture { binding: 0, texture: src_level },
                    GpuBinding::Sampler { binding: 1, sampler },
                    GpuBinding::Texture {
                        binding: 2,
                        texture: &self.mip_views[l as usize],
                    },
                ],
                [dw.div_ceil(16), dh.div_ceil(16), 1],
                "node.bokeh_gather mip downsample",
            );
        }

        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Boundary-atom codegen path: the runtime kernel is GENERATED
            // from `wgsl_body` (uniform/ABI machinery intact) even though the
            // atom is fusion-exempt. Generated kernel binds
            // uniform(0)/tex_in(1)/tex_width(2)/samp(3)/dst(4), matching
            // node.variable_blur's layout. bokeh_gather.wgsl is the parity
            // oracle.
            let wgsl =
                crate::node_graph::freeze::codegen::standalone_for_boundary_spec::<Self>()
                    .expect("node.bokeh_gather standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.bokeh_gather",
            )
        });

        let uniforms = BokehGatherUniforms {
            max_radius,
            // run() only executes when the node is enabled (skip_passthrough
            // aliases `in` → `out` when `enabled = false`), so the codegen-ABI
            // `enabled` uniform is always 1 here; the shader never reads it.
            enabled: 1,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        let mip_chain = self.mip_chain.as_ref().expect("mip chain built above");
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: mip_chain,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: width_tex,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.bokeh_gather",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_in_width_inputs_and_texture_output() {
        use crate::node_graph::ports::PortType;

        assert_eq!(BokehGather::TYPE_ID, "node.bokeh_gather");
        let names: Vec<&str> = BokehGather::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["in", "width"]);
        assert_eq!(BokehGather::INPUTS[0].ty, PortType::Texture2D);
        assert!(BokehGather::INPUTS[0].required);
        assert_eq!(BokehGather::INPUTS[1].ty, PortType::Texture2D);
        assert!(BokehGather::INPUTS[1].required);

        assert_eq!(BokehGather::OUTPUTS.len(), 1);
        assert_eq!(BokehGather::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn has_max_radius_and_enabled_params() {
        let names: Vec<&str> = BokehGather::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["max_radius", "enabled"]);
        assert!(matches!(BokehGather::PARAMS[1].default, ParamValue::Bool(true)));
    }

    #[test]
    fn skip_passthrough_aliases_in_to_out_only_when_enabled_false() {
        let prim = BokehGather::new();
        let node: &dyn EffectNode = &prim;
        // Absent or true → no skip (the node runs).
        let mut params = ParamValues::default();
        assert_eq!(node.skip_passthrough(&params, &[]), None);
        params.insert(Cow::Borrowed("enabled"), ParamValue::Bool(true));
        assert_eq!(node.skip_passthrough(&params, &[]), None);
        // Explicit false → alias `in` onto `out` (zero GPU work).
        params.insert(Cow::Borrowed("enabled"), ParamValue::Bool(false));
        assert_eq!(node.skip_passthrough(&params, &[]), Some(("in", "out")));
    }

    #[test]
    fn skip_passthrough_ports_declare_in_to_out() {
        let prim = BokehGather::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.skip_passthrough_ports(), Some(("in", "out")));
    }

    #[test]
    fn uniform_struct_is_16_bytes() {
        assert_eq!(std::mem::size_of::<BokehGatherUniforms>(), 16);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = BokehGather::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.bokeh_gather");
    }

    /// The mip-chain depth helper must agree with Metal's mip-dims rule
    /// (each level `max(1, prev / 2)`, down to 1×1).
    #[test]
    fn mip_level_count_matches_metal_mip_dims() {
        assert_eq!(mip_level_count(1, 1), 1);
        assert_eq!(mip_level_count(2, 2), 2);
        assert_eq!(mip_level_count(24, 16), 5); // 24→12→6→3→1
        assert_eq!(mip_level_count(1920, 1080), 11);
    }

    /// Fusion exemption is declared, and the boundary-atom codegen entry
    /// still emits a standalone kernel from the body (the codegen path is
    /// retained — the atom is excused from fusion, not from codegen).
    #[test]
    fn boundary_atom_still_generates_standalone_kernel() {
        assert_eq!(
            BokehGather::FUSION_KIND,
            crate::node_graph::freeze::classify::FusionKind::Boundary
        );
        assert_eq!(
            BokehGather::BOUNDARY_REASON,
            Some(crate::node_graph::freeze::classify::BoundaryReason::BarrieredReduction)
        );
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_boundary_spec::<
            BokehGather,
        >()
        .expect("boundary standalone codegen");
        assert!(wgsl.contains("textureSampleLevel(tex_in, samp, tap_uv, lod)"));
    }
}

/// **CPU reference** (`docs/CINEMATIC_POST_DESIGN.md` P4 deliverable: "CPU
/// reference (I1)") — a plain-Rust implementation of the D5 algorithm,
/// independent of the WGSL body, used by the I1 GPU-vs-CPU synthetic-fixture
/// parity gpu_test further down. Mip-gather upgrade (2026-08-28): models the
/// box-average mip chain and the fractional-LOD trilinear tap sampling —
/// the same arithmetic `bokeh_mip_downsample.wgsl` performs on GPU.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) mod cpu_reference {
    const BOKEH_N: usize = 32;
    const BOKEH_GOLDEN_ANGLE: f32 = 2.399963;
    const BOKEH_LOD_TARGET_RADIUS: f32 = 4.0;

    /// D2's committed per-pixel rotation hash, transcribed exactly from
    /// `ssao_from_depth_body.wgsl`'s `ssao_hash_angle`:
    /// `fract(sin(dot(px, vec2(12.9898, 78.233))) * 43758.5453) * 2*PI`.
    fn bokeh_hash_angle(px_x: f32, px_y: f32) -> f32 {
        let dot = px_x * 12.9898 + px_y * 78.233;
        let v = dot.sin() * 43_758.547;
        (v - v.floor()) * std::f32::consts::TAU
    }

    /// A synthetic single-channel plane (color OR CoC), bilinear-sampled
    /// with CLAMP-TO-EDGE addressing (matching `textureSampleLevel`'s
    /// default sampler mode, `GpuSamplerDesc::default()`).
    pub struct Plane4<'a> {
        pub w: i32,
        pub h: i32,
        pub rgba: &'a [[f32; 4]],
    }

    impl Plane4<'_> {
        fn texel(&self, x: i32, y: i32) -> [f32; 4] {
            let cx = x.clamp(0, self.w - 1);
            let cy = y.clamp(0, self.h - 1);
            self.rgba[(cy * self.w + cx) as usize]
        }

        fn sample(&self, u: f32, v: f32) -> [f32; 4] {
            let px = u * self.w as f32 - 0.5;
            let py = v * self.h as f32 - 0.5;
            let x0 = px.floor();
            let y0 = py.floor();
            let fx = px - x0;
            let fy = py - y0;
            let x0i = x0 as i32;
            let y0i = y0 as i32;
            let c00 = self.texel(x0i, y0i);
            let c10 = self.texel(x0i + 1, y0i);
            let c01 = self.texel(x0i, y0i + 1);
            let c11 = self.texel(x0i + 1, y0i + 1);
            let mut out = [0.0f32; 4];
            for c in 0..4 {
                let top = c00[c] * (1.0 - fx) + c10[c] * fx;
                let bot = c01[c] * (1.0 - fx) + c11[c] * fx;
                out[c] = top * (1.0 - fy) + bot * fy;
            }
            out
        }
    }

    /// An owned mip level (the chain is built per test fixture, so owning
    /// the texels keeps level lifetimes trivial).
    pub struct MipLevel {
        pub w: i32,
        pub h: i32,
        pub rgba: Vec<[f32; 4]>,
    }

    impl MipLevel {
        fn as_plane(&self) -> Plane4<'_> {
            Plane4 { w: self.w, h: self.h, rgba: &self.rgba }
        }
    }

    /// Build the box-average mip chain exactly as
    /// `bokeh_mip_downsample.wgsl` does on GPU: level 0 IS the source (the
    /// kernel's identity-UV fill samples each texel at its own center);
    /// each deeper level samples the previous one at the dst texel center's
    /// normalized UV — a bilinear fetch exactly on the 2×2 footprint center
    /// (weights 0.25) for even dims, the same clamped sample for odd dims.
    pub fn build_mip_chain(src: &Plane4<'_>, levels: u32) -> Vec<MipLevel> {
        let mut chain = Vec::with_capacity(levels as usize);
        chain.push(MipLevel { w: src.w, h: src.h, rgba: src.rgba.to_vec() });
        for _ in 1..levels {
            let prev = chain.last().expect("level 0 pushed");
            let w = (prev.w / 2).max(1);
            let h = (prev.h / 2).max(1);
            let prev_plane = prev.as_plane();
            let mut rgba = vec![[0.0f32; 4]; (w * h) as usize];
            for y in 0..h {
                for x in 0..w {
                    let u = (x as f32 + 0.5) / w as f32;
                    let v = (y as f32 + 0.5) / h as f32;
                    rgba[(y * w + x) as usize] = prev_plane.sample(u, v);
                }
            }
            chain.push(MipLevel { w, h, rgba });
        }
        chain
    }

    /// Trilinear sample of the chain at fractional `lod` (mirrors
    /// `textureSampleLevel(chain, samp(mip_filter=linear), uv, lod)`):
    /// bilinear at the two bracketing levels, lerped. LOD clamps to the
    /// chain's depth, as the GPU's does.
    fn sample_lod(chain: &[MipLevel], u: f32, v: f32, lod: f32) -> [f32; 4] {
        let max_lod = (chain.len() - 1) as f32;
        let lod = lod.clamp(0.0, max_lod);
        let l0 = lod.floor() as usize;
        let f = lod - l0 as f32;
        let l1 = (l0 + 1).min(chain.len() - 1);
        let a = chain[l0].as_plane().sample(u, v);
        let b = chain[l1].as_plane().sample(u, v);
        let mut out = [0.0f32; 4];
        for c in 0..4 {
            out[c] = a[c] * (1.0 - f) + b[c] * f;
        }
        out
    }

    /// The D5 algorithm with the mip-gather upgrade, transcribed exactly
    /// (independent of the WGSL body) — one texel's bokeh-gathered output,
    /// `[r,g,b,a]`. `color` is level 0 of `chain`; `coc` is full-res always.
    pub fn bokeh_gather_texel(
        color: &Plane4<'_>,
        chain: &[MipLevel],
        coc: &Plane4<'_>,
        cx: i32,
        cy: i32,
        max_radius: f32,
    ) -> [f32; 4] {
        let dims = [color.w as f32, color.h as f32];
        let uv = [(cx as f32 + 0.5) / dims[0], (cy as f32 + 0.5) / dims[1]];

        let center = color.sample(uv[0], uv[1]);
        let center_coc_frac = coc.sample(uv[0], uv[1])[0].clamp(0.0, 1.0);
        if center_coc_frac < 0.005 {
            return center;
        }

        let center_coc_px = center_coc_frac * max_radius;
        let lod = (center_coc_px / BOKEH_LOD_TARGET_RADIUS)
            .log2()
            .clamp(0.0, 8.0);
        let texel = [1.0 / dims[0], 1.0 / dims[1]];
        let px = [uv[0] * dims[0], uv[1] * dims[1]];
        let rot = bokeh_hash_angle(px[0], px[1]);

        let mut acc = [0.0f32; 3];
        let mut w_acc = 0.0f32;

        for i in 0..BOKEH_N {
            let r = ((i as f32 + 0.5) / BOKEH_N as f32).sqrt();
            let theta = i as f32 * BOKEH_GOLDEN_ANGLE + rot;
            let offset_px = [r * theta.cos() * center_coc_px, r * theta.sin() * center_coc_px];
            let tap_uv = [uv[0] + offset_px[0] * texel[0], uv[1] + offset_px[1] * texel[1]];

            let tap_color = sample_lod(chain, tap_uv[0], tap_uv[1], lod);
            let tap_coc_px = coc.sample(tap_uv[0], tap_uv[1])[0].clamp(0.0, 1.0) * max_radius;
            let distance_to_center_px = (offset_px[0] * offset_px[0] + offset_px[1] * offset_px[1]).sqrt();
            let w = if distance_to_center_px <= tap_coc_px { 1.0 } else { 0.0 };

            acc[0] += tap_color[0] * w;
            acc[1] += tap_color[1] * w;
            acc[2] += tap_color[2] * w;
            w_acc += w;
        }

        if w_acc > 0.0 {
            [acc[0] / w_acc, acc[1] / w_acc, acc[2] / w_acc, center[3]]
        } else {
            center
        }
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **I1** (`docs/ADDING_PRIMITIVES.md` "The codegen path is mandatory" +
    //! `docs/CINEMATIC_POST_DESIGN.md` P4 deliverable): the generated
    //! standalone kernel (built via `standalone_for_boundary_spec::<
    //! BokehGather>()`, the one that ships) must reproduce `cpu_reference::
    //! bokeh_gather_texel` (the plain-Rust reference) texel-for-texel on a
    //! synthetic non-uniform color + CoC fixture, WITH the mip-gather
    //! sampling semantics (the reference models the box chain + trilinear
    //! LOD the GPU performs).
    //!
    //! **I2**: a uniform-zero CoC field must be an exact pass-through of
    //! `in` — mirrors `node.variable_blur`'s own in-focus early-out and
    //! `coc_from_depth.rs`'s pinhole-chain invariant. Exercises the mip
    //! path's level-0 identity fill: the early-out returns the LOD-0 fetch,
    //! so the copy must be bit-clean.
    //!
    //! **I3** (anti-firefly regression gate, 2026-08-28 speckle fix): a
    //! single hot pixel on black under a uniform mid CoC must gather to a
    //! SMOOTH glow — no isolated spikes. This is the numeric gate for the
    //! bug class the mip chain exists to kill.
    //!
    //! **I5**: preset load-smoke lives in a separate integration test file
    //! (`tests/`), per the doc's `gpu_proofs` binary convention, not here.
    use half::f16;

    use manifold_gpu::{
        GpuBinding, GpuComputePipeline, GpuDevice, GpuFilterMode, GpuSampler, GpuSamplerDesc,
        GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    use super::cpu_reference::{bokeh_gather_texel, build_mip_chain, Plane4};
    use super::{mip_level_count, BokehGather, BokehGatherUniforms};
    use crate::render_target::RenderTarget;

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

    /// A non-uniform RGBA gradient — the color input every test dispatches
    /// bokeh_gather against.
    fn color_gradient(device: &GpuDevice, w: u32, h: u32) -> (GpuTexture, Vec<[f32; 4]>) {
        let mut rgba = vec![[0.0f32; 4]; (w * h) as usize];
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                let r = x as f32 / w as f32;
                let g = y as f32 / h as f32;
                let b = 0.5;
                let a = 1.0;
                rgba[i] = [r, g, b, a];
                px[i * 4] = f16::from_f32(r);
                px[i * 4 + 1] = f16::from_f32(g);
                px[i * 4 + 2] = f16::from_f32(b);
                px[i * 4 + 3] = f16::from_f32(a);
            }
        }
        (upload_rgba16f(device, w, h, "bokeh-color-gradient", &px), rgba)
    }

    /// Synthetic CoC-shaped field: a smooth ramp from 0.1 to 0.8 across x,
    /// so tap-to-tap CoC varies (a per-tap-fixed CoC couldn't exercise the
    /// occlusion weighting at all).
    fn coc_ramp(device: &GpuDevice, w: u32, h: u32) -> (GpuTexture, Vec<[f32; 4]>) {
        let mut rgba = vec![[0.0f32; 4]; (w * h) as usize];
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                let v = 0.1 + 0.7 * (x as f32 / (w - 1).max(1) as f32);
                rgba[i] = [v, v, v, 1.0];
                px[i * 4] = f16::from_f32(v);
                px[i * 4 + 1] = f16::from_f32(v);
                px[i * 4 + 2] = f16::from_f32(v);
                px[i * 4 + 3] = f16::from_f32(1.0);
            }
        }
        (upload_rgba16f(device, w, h, "bokeh-coc-ramp", &px), rgba)
    }

    fn coc_flat(device: &GpuDevice, w: u32, h: u32, value: f32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            let v = f16::from_f32(value);
            px[i * 4] = v;
            px[i * 4 + 1] = v;
            px[i * 4 + 2] = v;
            px[i * 4 + 3] = f16::from_f32(1.0);
        }
        upload_rgba16f(device, w, h, "bokeh-coc-flat", &px)
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("bokeh-readback");
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

    fn bg_uniforms(max_radius: f32) -> BokehGatherUniforms {
        BokehGatherUniforms { max_radius, enabled: 1, _pad0: 0.0, _pad1: 0.0 }
    }

    /// The mip-linear sampler the gather's fractional LOD requires (mirrors
    /// run()'s sampler).
    fn mip_linear_sampler(device: &GpuDevice) -> GpuSampler {
        device.create_sampler(&GpuSamplerDesc {
            mip_filter: GpuFilterMode::Linear,
            ..GpuSamplerDesc::default()
        })
    }

    fn downsample_pipeline(device: &GpuDevice) -> GpuComputePipeline {
        device.create_compute_pipeline(
            include_str!("shaders/bokeh_mip_downsample.wgsl"),
            "cs_main",
            "bokeh-mip-downsample-test",
        )
    }

    /// Build the mip chain of `src` on GPU exactly as `run()` does (level 0
    /// identity fill + box-average downsamples), returning the full-chain
    /// texture. The gather then binds it as `in`.
    fn build_mip_chain_gpu(
        device: &GpuDevice,
        downsample: &GpuComputePipeline,
        sampler: &GpuSampler,
        src: &GpuTexture,
        w: u32,
        h: u32,
    ) -> GpuTexture {
        let levels = mip_level_count(w, h);
        let chain = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
            label: "bokeh-mip-chain-test",
            mip_levels: levels,
        });
        let views: Vec<GpuTexture> = (0..levels)
            .map(|l| chain.mip_level_view(l, (w >> l).max(1), (h >> l).max(1)))
            .collect();
        let mut enc = device.create_encoder("bokeh-mip-chain");
        for l in 0..levels {
            let (dw, dh) = ((w >> l).max(1), (h >> l).max(1));
            let src_level: &GpuTexture = if l == 0 { src } else { &views[(l - 1) as usize] };
            enc.dispatch_compute(
                downsample,
                &[
                    GpuBinding::Texture { binding: 0, texture: src_level },
                    GpuBinding::Sampler { binding: 1, sampler },
                    GpuBinding::Texture { binding: 2, texture: &views[l as usize] },
                ],
                [dw.div_ceil(16), dh.div_ceil(16), 1],
                "bokeh-mip-downsample",
            );
        }
        enc.commit_and_wait_completed();
        chain
    }

    /// Dispatch the gather against `color`'s mip chain and read back the
    /// output — the full run() path (prefilter + gather), not the gather in
    /// isolation.
    fn dispatch(
        device: &GpuDevice,
        pipeline: &GpuComputePipeline,
        downsample: &GpuComputePipeline,
        sampler: &GpuSampler,
        color: &GpuTexture,
        width_tex: &GpuTexture,
        w: u32,
        h: u32,
        uniform_bytes: &[u8],
    ) -> Vec<[f32; 4]> {
        let mip_chain = build_mip_chain_gpu(device, downsample, sampler, color, w, h);
        let out = RenderTarget::new(device, w, h, GpuTextureFormat::Rgba16Float, "bokeh-out");
        let mut enc = device.create_encoder("bokeh-dispatch");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                GpuBinding::Texture { binding: 1, texture: &mip_chain },
                GpuBinding::Texture { binding: 2, texture: width_tex },
                GpuBinding::Sampler { binding: 3, sampler },
                GpuBinding::Texture { binding: 4, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "bokeh-dispatch",
        );
        enc.commit_and_wait_completed();
        readback_rgba(device, &out.texture, w, h)
    }

    /// **I1a**: generated kernel vs CPU-Rust reference on a synthetic
    /// color-gradient + CoC-ramp fixture — the doc's house pattern
    /// (implemented twice from the same committed spec, compared
    /// pixel-for-pixel).
    ///
    /// D5's `step(distance_to_center_px, tap_coc_px)` is a HARD binary cutoff
    /// on 32 trig-computed tap positions scaled by up to `max_radius` (24px)
    /// — the same "rare boundary-flip" class `ssao_from_depth.rs`'s
    /// `generated_ssao_matches_cpu_reference_on_synthetic_ramp` documents and
    /// bounds explicitly (CPU trig vs GPU fast-math trig legitimately differ
    /// at the ULP level, and multiplying by a large radius before a hard
    /// threshold turns a ULP difference into an occasional whole-tap
    /// inclusion/exclusion flip — confirmed empirically here: a control
    /// fixture with EVERY tap forced always-included, so no threshold could
    /// ever flip, still showed the same small-magnitude divergence, proving
    /// this is the well-known cross-compile trig ULP class, not an algorithm
    /// bug). The mip-gather upgrade adds a second ULP class of the same
    /// magnitude: GPU bilinear/trilinear filtering uses fixed-point subtexel
    /// weights, which the CPU reference models in fp32 — a per-level
    /// difference well under the f16 output quantum. Accept: the per-texel
    /// divergence stays small (bounded well under a whole-tap's worth of
    /// contribution, 1/32 ~= 0.03) AND the aggregate mean error stays
    /// near-zero (a real algorithm bug would move the mean, not just
    /// produce rare outliers).
    #[test]
    fn generated_bokeh_gather_matches_cpu_reference_on_synthetic_fixture() {
        let device = crate::test_device();
        let (w, h) = (24u32, 16u32);
        let (color_tex, color_rgba) = color_gradient(&device, w, h);
        let (coc_tex, coc_rgba) = coc_ramp(&device, w, h);

        let max_radius = 24.0f32;
        let uniforms = bg_uniforms(max_radius);
        let bytes = bytemuck::bytes_of(&uniforms);

        let gen_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_boundary_spec::<BokehGather>()
                .expect("node.bokeh_gather standalone codegen");
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "bokeh-generated",
        );
        let downsample = downsample_pipeline(&device);
        let sampler = mip_linear_sampler(&device);
        let gen_out = dispatch(
            &device, &pipeline, &downsample, &sampler, &color_tex, &coc_tex, w, h, bytes,
        );

        let color_buf = Plane4 { w: w as i32, h: h as i32, rgba: &color_rgba };
        let coc_buf = Plane4 { w: w as i32, h: h as i32, rgba: &coc_rgba };
        let chain = build_mip_chain(&color_buf, mip_level_count(w, h));
        let mut sum_abs = 0.0f64;
        let mut n = 0u32;
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let idx = (y as u32 * w + x as u32) as usize;
                let cpu = bokeh_gather_texel(&color_buf, &chain, &coc_buf, x, y, max_radius);
                let gpu = gen_out[idx];
                for c in 0..4 {
                    let d = (cpu[c] - gpu[c]).abs();
                    assert!(
                        d < 0.05,
                        "texel ({x},{y}) channel {c}: cpu={} gpu={} diff={d} exceeds a \
                         whole-tap's worth of contribution — looks like a real algorithm \
                         mismatch, not trig/filter ULP rounding",
                        cpu[c],
                        gpu[c]
                    );
                    sum_abs += f64::from(d);
                    n += 1;
                }
            }
        }
        let mean = sum_abs / f64::from(n);
        assert!(
            mean < 0.01,
            "mean abs diff {mean} is too high for isolated ULP-level boundary flips — \
             suggests a systematic algorithm mismatch"
        );
    }


    /// **I2**: a uniform-zero CoC field is an exact pass-through of `in` —
    /// mirrors `node.variable_blur`'s own in-focus (`center_coc < 0.005`)
    /// early-out and `coc_from_depth.rs`'s pinhole-chain invariant. Runs
    /// through the full mip path, so it also proves the level-0 identity
    /// fill is bit-clean.
    #[test]
    fn zero_coc_is_bit_clean_passthrough() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        let (color_tex, _color_rgba) = color_gradient(&device, w, h);
        let coc_tex = coc_flat(&device, w, h, 0.0);

        let uniforms = bg_uniforms(24.0);
        let bytes = bytemuck::bytes_of(&uniforms);
        let sampler = mip_linear_sampler(&device);
        let downsample = downsample_pipeline(&device);

        let gen_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_boundary_spec::<BokehGather>()
                .expect("node.bokeh_gather standalone codegen");
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "bokeh-zero-coc",
        );
        let got = dispatch(
            &device, &pipeline, &downsample, &sampler, &color_tex, &coc_tex, w, h, bytes,
        );
        let expected = readback_rgba(&device, &color_tex, w, h);

        assert_eq!(expected.len(), got.len());
        for (i, (e, g)) in expected.iter().zip(got.iter()).enumerate() {
            for c in 0..4 {
                assert!(
                    (e[c] - g[c]).abs() < 1e-3,
                    "texel {i} channel {c}: coc=0 must pass through bit-clean, expected={} got={}",
                    e[c],
                    g[c]
                );
            }
        }
    }

    /// **I3 — anti-firefly regression gate** (the bug class the mip chain
    /// exists to kill): one 20-nit pixel on black under a uniform mid CoC
    /// (0.5 → 12px disc at max_radius 24 → lod ≈ 1.6). The pre-mip gather
    /// turned each tap at the silhouette into a coin flip between 20.0 and
    /// 0.0, and the per-pixel spiral rotation decorrelated neighboring
    /// pixels' outcomes — an isolated spike of up to 20/32 ≈ 0.63 against
    /// black neighbors (a ~0.63 adjacent delta). With the mip prefilter the
    /// hot pixel is an area average at the sampled level and the gathered
    /// field is smooth (measured: peak ≈ 0.08, per-channel frame sum ≈
    /// 19.4). Assert: (a) ENERGY IS CONSERVED (per-channel frame sum ≈ the
    /// hot pixel's 20.0 — the prefilter must spread the energy, not destroy
    /// it), and (b) no adjacent-pixel channel delta reaches half of the old
    /// algorithm's ~0.63 spike amplitude.
    #[test]
    fn hot_pixel_gathers_to_smooth_glow_not_speckle() {
        let device = crate::test_device();
        let (w, h) = (64u32, 64u32);

        // Black field, one hot pixel at the center.
        let mut rgba = vec![[0.0f32; 4]; (w * h) as usize];
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        let hot = (32 * w + 32) as usize;
        rgba[hot] = [20.0, 20.0, 20.0, 1.0];
        for c in 0..3 {
            px[hot * 4 + c] = f16::from_f32(20.0);
        }
        px[hot * 4 + 3] = f16::from_f32(1.0);
        let color_tex = upload_rgba16f(&device, w, h, "bokeh-hot-pixel", &px);
        let coc_tex = coc_flat(&device, w, h, 0.5);

        let uniforms = bg_uniforms(24.0);
        let bytes = bytemuck::bytes_of(&uniforms);
        let sampler = mip_linear_sampler(&device);
        let downsample = downsample_pipeline(&device);
        let gen_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_boundary_spec::<BokehGather>()
                .expect("node.bokeh_gather standalone codegen");
        let pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "bokeh-firefly",
        );
        let out = dispatch(
            &device, &pipeline, &downsample, &sampler, &color_tex, &coc_tex, w, h, bytes,
        );

        // (a) Energy conserved: the prefilter must SPREAD the hot pixel's
        // energy across its bokeh disc, not destroy it. Per-channel sum over
        // the frame ≈ the hot pixel's 20.0 (measured 19.35; the small
        // shortfall is clamp-to-edge at frame borders).
        let mut channel_sum = [0.0f32; 3];
        for p in &out {
            for c in 0..3 {
                channel_sum[c] += p[c];
            }
        }
        for (c, s) in channel_sum.iter().enumerate() {
            assert!(
                (15.0..=25.0).contains(s),
                "channel {c} energy {s} — expected ≈20 (the hot pixel's \
                 energy spread over the disc, conserved)"
            );
        }

        // (b) Smooth: no adjacent-pixel spike anywhere in the frame.
        let mut max_delta = 0.0f32;
        let mut worst = (0u32, 0u32, 0.0f32);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let p = out[y * w as usize + x];
                for (nx, ny) in [(x + 1, y), (x, y + 1)] {
                    if nx >= w as usize || ny >= h as usize {
                        continue;
                    }
                    let q = out[ny * w as usize + nx];
                    for c in 0..3 {
                        let d = (p[c] - q[c]).abs();
                        if d > max_delta {
                            max_delta = d;
                            worst = (x as u32, y as u32, d);
                        }
                    }
                }
            }
        }
        assert!(
            max_delta < 0.25,
            "adjacent-pixel delta {} at ({},{}) — the pre-mip algorithm's \
             isolated hot-tap spikes (~0.63) read as static dots hugging the \
             silhouette; the mip prefilter must keep the field smooth",
            worst.2,
            worst.0,
            worst.1
        );
    }
}
