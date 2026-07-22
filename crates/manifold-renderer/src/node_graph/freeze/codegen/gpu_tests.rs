use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::freeze::markers::Marker;

use super::fused::generate_fused;
use super::standalone::{generate_standalone, StandaloneKernelSpec};
use super::types::{FusedVirtualChain, FusionRegion, InputSource, RegionNode, ENTRY};
use crate::node_graph::effect_node::EffectNode;
use crate::node_graph::freeze::TextureDiff;
use crate::node_graph::primitives::Gain;
use crate::render_target::RenderTarget;
use half::f16;
use manifold_gpu::{
    GpuBinding, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
    GpuTextureFormat, GpuTextureUsage,
};

const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

fn gradient(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
    let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            px[i] = f16::from_f32(x as f32 / w as f32);
            px[i + 1] = f16::from_f32(y as f32 / h as f32);
            px[i + 2] = f16::from_f32(0.5);
            px[i + 3] = f16::from_f32(1.0);
        }
    }
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: FMT,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_SRC,
        label: "codegen-input",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    tex
}

/// Dispatch a coincident two-input kernel: uniform(0), a(1), b(2),
/// sampler(3), dst(4). `param_bytes` is the 16-byte uniform payload.
fn dispatch_coincident(
    device: &GpuDevice,
    wgsl: &str,
    a: &GpuTexture,
    b: &GpuTexture,
    param_bytes: &[u8],
) -> RenderTarget {
    let (w, h) = (a.width, a.height);
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-test-mix");
    let sampler = device.create_sampler(&GpuSamplerDesc::default());
    let out = RenderTarget::new(device, w, h, FMT, "codegen-out-mix");
    let mut enc = device.create_encoder("codegen-test-mix");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes { binding: 0, data: param_bytes },
            GpuBinding::Texture { binding: 1, texture: a },
            GpuBinding::Texture { binding: 2, texture: b },
            GpuBinding::Sampler { binding: 3, sampler: &sampler },
            GpuBinding::Texture { binding: 4, texture: &out.texture },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-test-mix",
    );
    enc.commit_and_wait_completed();
    out
}

/// A second gradient with a different layout, so a + b differ per texel
/// (so the blend + crossfade is actually exercised).
fn gradient_b(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
    let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            px[i] = f16::from_f32(0.8 - 0.6 * (x as f32 / w as f32));
            px[i + 1] = f16::from_f32(0.2);
            px[i + 2] = f16::from_f32(y as f32 / h as f32);
            px[i + 3] = f16::from_f32(0.5);
        }
    }
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: FMT,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_SRC,
        label: "codegen-input-b",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    tex
}

/// Dispatch a standard pointwise kernel: uniform(0), src(1), sampler(2),
/// dst(3). `param_bytes` is the 16-byte uniform payload.
fn dispatch_pointwise(
    device: &GpuDevice,
    wgsl: &str,
    input: &GpuTexture,
    param_bytes: &[u8],
) -> RenderTarget {
    let (w, h) = (input.width, input.height);
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-test");
    let sampler = device.create_sampler(&GpuSamplerDesc::default());
    let out = RenderTarget::new(device, w, h, FMT, "codegen-out");
    let mut enc = device.create_encoder("codegen-test");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes { binding: 0, data: param_bytes },
            GpuBinding::Texture { binding: 1, texture: input },
            GpuBinding::Sampler { binding: 2, sampler: &sampler },
            GpuBinding::Texture { binding: 3, texture: &out.texture },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-test",
    );
    enc.commit_and_wait_completed();
    out
}

/// Determinism (design §12.3): the generator emits byte-identical WGSL
/// across calls — the cross-session pipeline-cache key depends on it.
#[test]
fn generated_wgsl_is_deterministic() {
    let g = Gain::new();
    let body = g.wgsl_body().unwrap();
    let a = generate_standalone(&StandaloneKernelSpec { fusion_kind: g.fusion_kind(), body, inputs: g.inputs(), params: g.parameters(), input_access: g.input_access(), derived_uniforms: g.derived_uniforms(), outputs: g.outputs(), stencil_fetch: false, includes: &[] }).unwrap();
    let b = generate_standalone(&StandaloneKernelSpec { fusion_kind: g.fusion_kind(), body, inputs: g.inputs(), params: g.parameters(), input_access: g.input_access(), derived_uniforms: g.derived_uniforms(), outputs: g.outputs(), stencil_fetch: false, includes: &[] }).unwrap();
    assert_eq!(a, b, "codegen must be deterministic");
    assert!(a.contains("fn cs_main"), "must emit the cs_main entry");
    assert!(!a.contains("cs_main_"), "no symbol may have cs_main as a prefix");
}

/// Regression for the NV_EPS-class bug: a body declaring a top-level `const`
/// before its `fn body` must carry that const into the fused kernel's shared
/// prelude. The standalone path keeps it verbatim; the fused path splits into
/// fns and would otherwise drop it (`no definition in scope`). Two atoms
/// sharing the const emit it exactly once (deduped).
#[test]
fn fused_prelude_carries_and_dedups_top_level_consts() {
    use crate::node_graph::freeze::classify::FusionKind;
    let body = "const K: f32 = 0.25;\n\nfn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {\n    return c * K;\n}\n";
    let id = NodeInstanceId;
    let region = FusionRegion {
        nodes: vec![
            RegionNode {
                node_id: id(0),
                fusion_kind: FusionKind::Pointwise,
                body,
                params: &[],
                inputs: vec![InputSource::External(0)],
                input_access: vec![],
                node_inputs: &[],
                node_outputs: &[],
                node_includes: &[],
                derived_uniforms: &[], type_id: String::new(), derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            },
            RegionNode {
                node_id: id(1),
                fusion_kind: FusionKind::Pointwise,
                body,
                params: &[],
                inputs: vec![InputSource::Node(id(0))],
                input_access: vec![],
                node_inputs: &[],
                node_outputs: &[],
                node_includes: &[],
                derived_uniforms: &[], type_id: String::new(), derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            },
        ],
        num_external_inputs: 1,
        outputs: vec![(id(1), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(), camera_externals: 0,
    };
    let g = generate_fused(&region).expect("a region whose body declares a const fuses");
    assert_eq!(
        g.wgsl.matches("const K: f32 = 0.25;").count(),
        1,
        "the top-level const is carried into the fused kernel exactly once (deduped)"
    );
    assert!(g.wgsl.contains("fn n0_body"), "first body namespaced");
    assert!(g.wgsl.contains("fn n1_body"), "second body namespaced");
}

/// CROSS-RESOLUTION externals (workstream 4 — the Watercolor/Bloom unlock).
/// A coincident external whose producer lives at a different element space is
/// listed in `sampled_externals`. cs_main must read it through the shared
/// sampler at the fragment UV (`textureSampleLevel`), exactly the unfused
/// atom's resolution-robust read — a `textureLoad` at the kernel's own canvas
/// coord would misread a half-res producer. A same-space external stays
/// `textureLoad`. The body sees `ext_<e>` either way, so the only difference
/// is the pre-read line + the now-mandatory sampler binding.
#[test]
fn cross_resolution_external_sampled_at_uv() {
    use crate::node_graph::freeze::classify::FusionKind;
    // A 2-input coincident mix: in0 is a same-space external (textureLoad),
    // in1 is a cross-res external (sampled). Chained into a second pointwise.
    let mix = "fn body(a: vec4<f32>, b: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {\n    return mix(a, b, 0.5);\n}\n";
    let gain = "fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {\n    return c * 2.0;\n}\n";
    let id = NodeInstanceId;
    let region = FusionRegion {
        nodes: vec![
            RegionNode {
                node_id: id(0),
                fusion_kind: FusionKind::MultiInputCoincident,
                body: mix,
                params: &[],
                inputs: vec![InputSource::External(0), InputSource::External(1)],
                input_access: vec![],
                node_inputs: &[],
                node_outputs: &[],
                node_includes: &[],
                derived_uniforms: &[], type_id: String::new(), derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            },
            RegionNode {
                node_id: id(1),
                fusion_kind: FusionKind::Pointwise,
                body: gain,
                params: &[],
                inputs: vec![InputSource::Node(id(0))],
                input_access: vec![],
                node_inputs: &[],
                node_outputs: &[],
                node_includes: &[],
                derived_uniforms: &[], type_id: String::new(), derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            },
        ],
        num_external_inputs: 2,
        outputs: vec![(id(1), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: vec![1], camera_externals: 0,
    };
    let g = generate_fused(&region).expect("cross-res region fuses");
    assert!(
        naga::front::wgsl::parse_str(&g.wgsl).is_ok(),
        "cross-res fused kernel parses:\n{}",
        g.wgsl
    );
    // The cross-res external is sampled at uv; the same-space one is loaded.
    assert!(
        g.wgsl.contains("let ext_1 = textureSampleLevel(src_1, samp, uv, 0.0);"),
        "cross-res external sampled at uv:\n{}",
        g.wgsl
    );
    assert!(
        g.wgsl.contains("let ext_0 = textureLoad(src_0, coord, 0);"),
        "same-space external still textureLoad'd:\n{}",
        g.wgsl
    );
    // The sampler must exist even with no gather member.
    assert!(g.wgsl.contains("var samp: sampler;"), "shared sampler bound:\n{}", g.wgsl);
}

/// BUG-135: the fused TEXTURE-domain path must emit a member's
/// `node_includes` exactly like `generate_fused_buffer` already does.
/// `node.coc_from_depth` declares `wgsl_includes: [DEPTH_COMMON]` — its
/// body calls the shared `linearize_depth` helper. Fuse it with a
/// pointwise `Gain` neighbour (the real shape a DoF chain forms) and
/// assert the shared header text lands in the kernel exactly once and the
/// whole thing parses through naga — before this fix, `linearize_depth`
/// was never emitted and naga rejected the kernel with "no definition in
/// scope for identifier: linearize_depth" (BUG-141's exact symptom).
#[test]
fn fused_texture_region_carries_and_dedups_wgsl_includes() {
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::{CocFromDepth, Gain};
    let id = NodeInstanceId;
    let region = FusionRegion {
        nodes: vec![
            RegionNode {
                node_id: id(0),
                fusion_kind: CocFromDepth::FUSION_KIND,
                body: CocFromDepth::WGSL_BODY.unwrap(),
                params: CocFromDepth::PARAMS,
                inputs: vec![InputSource::External(0)],
                input_access: CocFromDepth::INPUT_ACCESS.to_vec(),
                node_inputs: &[],
                node_outputs: &[],
                node_includes: CocFromDepth::WGSL_INCLUDES,
                derived_uniforms: CocFromDepth::DERIVED_UNIFORMS,
                type_id: CocFromDepth::TYPE_ID.to_string(),
                derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            },
            RegionNode {
                node_id: id(1),
                fusion_kind: Gain::FUSION_KIND,
                body: Gain::WGSL_BODY.unwrap(),
                params: Gain::PARAMS,
                inputs: vec![InputSource::Node(id(0))],
                input_access: vec![],
                node_inputs: &[],
                node_outputs: &[],
                node_includes: &[],
                derived_uniforms: &[],
                type_id: String::new(),
                derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            },
        ],
        num_external_inputs: 1,
        outputs: vec![(id(1), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(),
        camera_externals: 0,
    };
    let g = generate_fused(&region).expect("coc_from_depth + Gain region fuses");
    assert!(
        g.wgsl.contains("fn linearize_depth"),
        "the shared depth_common.wgsl helper must be carried into the fused kernel:\n{}",
        g.wgsl
    );
    assert_eq!(
        g.wgsl.matches("fn linearize_depth").count(),
        1,
        "the include is deduped, not duplicated:\n{}",
        g.wgsl
    );
    assert!(
        naga::front::wgsl::parse_str(&g.wgsl).is_ok(),
        "fused texture kernel with a wgsl_includes member parses through naga (BUG-135/BUG-141):\n{}",
        g.wgsl
    );
}

/// Buffer-domain multi-atom fusion: a chain of two per-element instance atoms
/// fuses into one `var<storage>` kernel. The element struct is synthesized,
/// every input and the output bind as storage arrays, the dispatch is a 1D
/// `arrayLength`-guarded loop (the `node.wgsl_compute` buffer convention, with
/// no `dispatch_count` uniform), the first body's element register threads
/// into the second, and the shared `noise_common` include is prepended once
/// (so parse resolves the helper calls; were the include dropped, naga parse
/// would fail here). The buffer analogue of
/// `fused_gather_binds_sampler_and_passes_texture`. End-to-end numerical
/// parity rides the render-parity oracle once the finder emits buffer regions
/// on the live path.
#[test]
fn fused_buffer_region_threads_element_registers() {
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::InstanceRotationJitter as J;
    let id = NodeInstanceId;
    let mk = |i: u32, src: InputSource| RegionNode {
        node_id: id(i),
        fusion_kind: J::FUSION_KIND,
        body: J::WGSL_BODY.unwrap(),
        params: J::PARAMS,
        inputs: vec![src],
        input_access: J::INPUT_ACCESS.to_vec(),
        node_inputs: J::INPUTS,
        node_outputs: J::OUTPUTS,
        node_includes: J::WGSL_INCLUDES,
        derived_uniforms: J::DERIVED_UNIFORMS,
        type_id: J::TYPE_ID.to_string(),
        derived_camera_ext: None,
        output_storage: "rgba16float",
        stencil_fetch: false,
        quantize_f16: false,
    };
    let region = FusionRegion {
        nodes: vec![mk(0, InputSource::External(0)), mk(1, InputSource::Node(id(0)))],
        num_external_inputs: 1,
        outputs: vec![(id(1), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(), camera_externals: 0,
    };
    let g = generate_fused(&region).expect("buffer region fuses");
    assert!(
        naga::front::wgsl::parse_str(&g.wgsl).is_ok(),
        "fused buffer kernel parses through naga (validates the body ABI + includes):\n{}",
        g.wgsl
    );
    // Inputs are read-only (forward deps); the output is a FRESH write-only
    // `dst` tagged `// @fused_output` (not aliased). This is what keeps the
    // node ordered after its producers.
    assert!(
        g.wgsl.contains("var<storage, read> src_0"),
        "external input bound read-only:\n{}",
        g.wgsl
    );
    assert!(g.wgsl.contains(&Marker::FusedOutput.emit()), "fresh output tagged @fused_output");
    assert!(
        g.wgsl.contains("var<storage, read_write> dst:"),
        "fresh dst output array declared:\n{}",
        g.wgsl
    );
    assert!(g.wgsl.contains("arrayLength(&src_0)"), "1D dispatch keyed on an input array length");
    assert!(g.wgsl.contains("let e_0 = src_0[idx];"), "external element pre-read once");
    assert!(g.wgsl.contains("let r0 = n0_body"), "first member's element register");
    assert!(g.wgsl.contains("let r1 = n1_body"), "second member threads r0");
    assert!(g.wgsl.contains("dst[idx] = r1;"), "region result written to the fresh output");
}

/// BUG-008: a buffer region with TWO array externals pre-reads BOTH at `[idx]`.
/// The dispatch count must be bounded by the SHORTER external so neither read
/// goes out of bounds when the two inputs have different lengths (the unfused
/// atoms clamp to `min(a, b, …)` for exactly this reason). `LerpInstanceFields`
/// (two required `Array<InstanceTransform>` inputs) is the shipped shape.
#[test]
fn fused_buffer_region_two_array_externals_bounds_count_by_min() {
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::LerpInstanceFields as L;
    let id = NodeInstanceId;
    let region = FusionRegion {
        nodes: vec![RegionNode {
            node_id: id(0),
            fusion_kind: L::FUSION_KIND,
            body: L::WGSL_BODY.unwrap(),
            params: L::PARAMS,
            inputs: vec![InputSource::External(0), InputSource::External(1)],
            input_access: L::INPUT_ACCESS.to_vec(),
            node_inputs: L::INPUTS,
            node_outputs: L::OUTPUTS,
            node_includes: L::WGSL_INCLUDES,
            derived_uniforms: L::DERIVED_UNIFORMS,
            type_id: L::TYPE_ID.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba16float",
            stencil_fetch: false,
            quantize_f16: false,
        }],
        num_external_inputs: 2,
        outputs: vec![(id(0), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(), camera_externals: 0,
    };
    let g = generate_fused(&region).expect("two-external buffer region fuses");
    assert!(
        naga::front::wgsl::parse_str(&g.wgsl).is_ok(),
        "fused two-external buffer kernel parses:\n{}",
        g.wgsl
    );
    assert!(
        g.wgsl.contains("let e_0 = src_0[idx];") && g.wgsl.contains("let e_1 = src_1[idx];"),
        "both array externals are pre-read at [idx]:\n{}",
        g.wgsl
    );
    assert!(
        g.wgsl
            .contains("let count = min(arrayLength(&src_0), arrayLength(&src_1));"),
        "count bounded by the SHORTER external so neither pre-read is OOB (BUG-008):\n{}",
        g.wgsl
    );
}

/// STENCIL tier — a virtual chain emits `n{i}_vsrc_<port>` (per-corner
/// address wrap + chain bodies over textureLoad'ed externals + q16 tail) and
/// `n{i}_fetch_<port>` (manual f32 bilinear over four corners); the chain
/// member is skipped by cs_main, its params still join the merged uniform,
/// and the kernel parses. Region shape: blur(stencil, Virtual(0)) with one
/// absorbed gain reading external 0.
#[test]
fn fused_virtual_chain_emits_fetch_and_skips_cs_main() {
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::{Gain, GaussianBlur};
    let id = NodeInstanceId;
    let region = FusionRegion {
        nodes: vec![
            RegionNode {
                node_id: id(0),
                fusion_kind: GaussianBlur::FUSION_KIND,
                body: GaussianBlur::WGSL_BODY.unwrap(),
                params: GaussianBlur::PARAMS,
                inputs: vec![InputSource::Virtual(0)],
                input_access: GaussianBlur::INPUT_ACCESS.to_vec(),
                node_inputs: GaussianBlur::INPUTS,
                node_outputs: GaussianBlur::OUTPUTS,
                node_includes: &[],
                derived_uniforms: &[], type_id: String::new(), derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: true,
                quantize_f16: false,
            },
            RegionNode {
                node_id: id(1),
                fusion_kind: Gain::FUSION_KIND,
                body: Gain::WGSL_BODY.unwrap(),
                params: Gain::PARAMS,
                inputs: vec![InputSource::External(0)],
                input_access: vec![],
                node_inputs: Gain::INPUTS,
                node_outputs: Gain::OUTPUTS,
                node_includes: &[],
                derived_uniforms: &[], type_id: String::new(), derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            },
        ],
        num_external_inputs: 1,
        outputs: vec![(id(0), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: vec![FusedVirtualChain {
            consumer: 0,
            input_index: 0,
            members: vec![1],
            output: 1,
        }],
        sampled_externals: Vec::new(), camera_externals: 0,
    };
    let g = generate_fused(&region).expect("virtual-chain region fuses");
    assert!(
        naga::front::wgsl::parse_str(&g.wgsl).is_ok(),
        "fused stencil kernel parses:\n{}",
        g.wgsl
    );
    assert!(g.wgsl.contains("fn n0_vsrc_in"), "per-corner chain evaluator emitted");
    assert!(g.wgsl.contains("fn n0_fetch_in"), "bilinear fetch emitted");
    assert!(
        g.wgsl.contains("let v1 = n1_body(textureSampleLevel(src_0, samp, vuv, 0.0)"),
        "chain external sampled at the corner uv (resolution-robust, like the unfused atom)"
    );
    assert!(g.wgsl.contains("return q16(v1)"), "chain output reproduces the f16 store");
    assert!(!g.wgsl.contains("let r1 ="), "the chain member is not evaluated by cs_main");
    assert!(g.wgsl.contains("params.n1_gain"), "the chain member's param stays a live uniform field");
    assert!(g.wgsl.contains("textureStore(dst, coord, r0);"), "the blur register is the region output");
    assert!(g.wgsl.contains("var samp"), "chain external reads bind the shared sampler");
}

/// Tier 3 — a gather input binds a sampler and is passed to the body as a
/// texture handle (the body samples it itself at a coord it computes), and is
/// NOT pre-read into a register. sharpen (a Gather) → invert (Coincident): the
/// kernel binds `samp`, calls `n0_body(src_0, samp, …)`, never emits
/// `let ext_0`, and threads sharpen's register into invert.
#[test]
fn fused_gather_binds_sampler_and_passes_texture() {
    use crate::node_graph::freeze::classify::InputAccess;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::{Invert, Sharpen};
    let id = NodeInstanceId;
    let region = FusionRegion {
        nodes: vec![
            RegionNode {
                node_id: id(0),
                fusion_kind: Sharpen::FUSION_KIND,
                body: Sharpen::WGSL_BODY.unwrap(),
                params: Sharpen::PARAMS,
                inputs: vec![InputSource::External(0)],
                input_access: vec![InputAccess::Gather],
                node_inputs: &[],
                node_outputs: &[],
                node_includes: &[],
                derived_uniforms: &[], type_id: String::new(), derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            },
            RegionNode {
                node_id: id(1),
                fusion_kind: Invert::FUSION_KIND,
                body: Invert::WGSL_BODY.unwrap(),
                params: Invert::PARAMS,
                inputs: vec![InputSource::Node(id(0))],
                input_access: vec![InputAccess::Coincident],
                node_inputs: &[],
                node_outputs: &[],
                node_includes: &[],
                derived_uniforms: &[], type_id: String::new(), derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            },
        ],
        num_external_inputs: 1,
        outputs: vec![(id(1), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(), camera_externals: 0,
    };
    let g = generate_fused(&region).expect("gather region fuses");
    assert!(g.wgsl.contains("var samp: sampler"), "a sampler is bound for the gather");
    assert!(
        g.wgsl.contains("n0_body(src_0, samp,"),
        "sharpen receives the texture + shared sampler and samples it itself"
    );
    assert!(
        !g.wgsl.contains("let ext_0 ="),
        "a gather-only external is never pre-read into a register"
    );
    assert!(g.wgsl.contains("fn n1_body"), "invert namespaced + threads sharpen's register");
}

/// Fan-out — a region with two escaping members emits two `dst_<k>` storage
/// bindings and two `textureStore`s (one per output register), and takes its
/// dispatch dims from `dst_0`. gain forks into invert (output 0) and contrast
/// (output 1); both thread gain's register. The single-output path is
/// unchanged (every other test asserts the byte-identical `var dst`).
#[test]
fn fused_fanout_emits_two_dst_bindings() {
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::{Contrast, Gain, Invert};
    let id = NodeInstanceId;
    let region = FusionRegion {
        nodes: vec![
            RegionNode {
                node_id: id(0),
                fusion_kind: Gain::FUSION_KIND,
                body: Gain::WGSL_BODY.unwrap(),
                params: Gain::PARAMS,
                inputs: vec![InputSource::External(0)],
                input_access: vec![],
                node_inputs: &[],
                node_outputs: &[],
                node_includes: &[],
                derived_uniforms: &[], type_id: String::new(), derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            },
            RegionNode {
                node_id: id(1),
                fusion_kind: Invert::FUSION_KIND,
                body: Invert::WGSL_BODY.unwrap(),
                params: Invert::PARAMS,
                inputs: vec![InputSource::Node(id(0))],
                input_access: vec![],
                node_inputs: &[],
                node_outputs: &[],
                node_includes: &[],
                derived_uniforms: &[], type_id: String::new(), derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            },
            RegionNode {
                node_id: id(2),
                fusion_kind: Contrast::FUSION_KIND,
                body: Contrast::WGSL_BODY.unwrap(),
                params: Contrast::PARAMS,
                inputs: vec![InputSource::Node(id(0))],
                input_access: vec![],
                node_inputs: &[],
                node_outputs: &[],
                node_includes: &[],
                derived_uniforms: &[], type_id: String::new(), derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            },
        ],
        num_external_inputs: 1,
        outputs: vec![(id(1), "out".to_string()), (id(2), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(), camera_externals: 0,
    };
    let g = generate_fused(&region).expect("fan-out region fuses");
    assert!(g.wgsl.contains("var dst_0:"), "first output binding");
    assert!(g.wgsl.contains("var dst_1:"), "second output binding");
    assert!(!g.wgsl.contains("var dst:"), "no single-output `dst` in a fan-out kernel");
    assert!(
        g.wgsl.contains("textureDimensions(dst_0)"),
        "dims come from the first output (all outputs are coincident)"
    );
    // invert = output 0 (register r1), contrast = output 1 (register r2).
    assert!(g.wgsl.contains("textureStore(dst_0, coord, r1)"), "invert → dst_0");
    assert!(g.wgsl.contains("textureStore(dst_1, coord, r2)"), "contrast → dst_1");
}

/// The generated standalone gain kernel reproduces the hand-written
/// gain.wgsl — same math, same center-UV sampling, same f16 store — so it
/// is a drop-in (single-source cutover, build step 1b). Both are single
/// kernels reading the same input: diff directly via the oracle.
#[test]
fn generated_gain_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let input = gradient(&device, w, h);

    let g = Gain::new();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: g.fusion_kind(),
        body: g.wgsl_body().unwrap(),
        inputs: g.inputs(),
        params: g.parameters(),
        input_access: g.input_access(),
        derived_uniforms: g.derived_uniforms(),
        outputs: g.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("gain generates");
    let original = include_str!("../../primitives/shaders/gain.wgsl");

    // uniform payload: gain = 1.7, then padding (matches both structs).
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&1.7f32.to_le_bytes());

    let from_original = dispatch_pointwise(&device, original, &input, &bytes);
    let from_generated = dispatch_pointwise(&device, &generated, &input, &bytes);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &from_original.texture, &from_generated.texture, 1e-5, 1e-5);
    assert_eq!(
        r.over_count, 0,
        "generated gain must reproduce gain.wgsl (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
    assert!(
        r.max_abs < 1e-5,
        "same math + sampling should be ~bit-identical, got max_abs={}",
        r.max_abs
    );
}

/// Pack f32 params into a 16-byte-multiple uniform payload.
fn pack_f32(params: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for p in params {
        bytes.extend_from_slice(&p.to_le_bytes());
    }
    while bytes.len() % 16 != 0 {
        bytes.push(0);
    }
    bytes
}

/// 1b safety gate: every remaining pointwise ColorGrade atom's GENERATED
/// standalone kernel reproduces its hand-written shader bit-for-bit (same
/// math, same center-UV sampling). Once green, deleting the hand shaders
/// (the single-source cutover) cannot change rendering. Originals read from
/// disk so this test self-documents which shaders the cutover will retire.
#[test]
fn generated_pointwise_atoms_match_originals() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let input = gradient(&device, w, h);
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let shaders_dir =
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");

    // (type_id, original shader file, representative non-identity params).
    let cases: &[(&str, &str, &[f32])] = &[
        ("node.saturation", "saturation.wgsl", &[1.4]),
        ("node.hue_saturation", "hue_saturation.wgsl", &[30.0, 1.3, 0.9]),
        ("node.contrast", "contrast.wgsl", &[1.5]),
        ("node.colorize", "colorize.wgsl", &[0.5, 200.0, 0.7, 0.6]),
        ("node.clamp", "clamp_texture.wgsl", &[0.1, 0.8]),
        // Vocabulary widening (design §12.3): pure-pointwise color/tone atoms
        // converted to single-source bodies. Partial invert exercises the
        // mix; levels uses the MetallicGlass height shape; posterize at 6.
        ("node.invert", "invert.wgsl", &[0.5]),
        ("node.levels", "levels.wgsl", &[1.26, 0.29, 0.0, 1.0, 0.8]),
        ("node.posterize", "posterize.wgsl", &[6.0]),
        // Positional atom: pixel = uv*dims is identical in both kernels on
        // the square test input, so the per-pixel hash matches bit-for-bit.
        ("node.film_grain", "film_grain.wgsl", &[0.3]),
        // Math/convert pointwise atoms (overnight vocabulary sweep).
        ("node.wrap", "fract_texture.wgsl", &[3.0]),
        ("node.power", "power_texture.wgsl", &[2.5]),
        ("node.scale_offset_image", "scale_offset_texture.wgsl", &[1.5, -0.25]),
        ("node.smoothstep", "smoothstep_texture.wgsl", &[0.2, 0.8]),
        ("node.field_combine", "field_combine.wgsl", &[1.5, -0.5, 0.25]),
    ];
    let differ = TextureDiff::new(&device);
    for (type_id, shader_file, params) in cases {
        let node = registry.construct(type_id).unwrap();
        let generated = generate_standalone(&StandaloneKernelSpec {
            fusion_kind: node.fusion_kind(),
            body: node.wgsl_body().unwrap(),
            inputs: node.inputs(),
            params: node.parameters(),
            input_access: node.input_access(),
            derived_uniforms: node.derived_uniforms(),
            outputs: node.outputs(),
            stencil_fetch: false,
            includes: &[],
        })
        .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
        let original = std::fs::read_to_string(format!("{shaders_dir}/{shader_file}"))
            .unwrap_or_else(|e| panic!("read {shader_file}: {e}"));
        let bytes = pack_f32(params);

        let from_original = dispatch_pointwise(&device, &original, &input, &bytes);
        let from_generated = dispatch_pointwise(&device, &generated, &input, &bytes);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "{type_id}: generated kernel must reproduce {shader_file} \
             (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }
}


/// The coincident two-input path: the generated standalone mix kernel
/// reproduces mix.wgsl (two textures, blend mode + alpha lerp). Exercises
/// the generator's MultiInputCoincident branch before the 1b cutover.
#[test]
fn generated_mix_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let a = gradient(&device, w, h);
    let b = gradient_b(&device, w, h);

    let m = crate::node_graph::primitives::Mix::new();
    let node: &dyn EffectNode = &m;
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("mix generates");
    let original = include_str!("../../primitives/shaders/mix.wgsl");

    // uniform payload: amount = 0.6 (f32), mode = 4 (Multiply, u32), pad.
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&0.6f32.to_le_bytes());
    bytes[4..8].copy_from_slice(&4u32.to_le_bytes());

    let from_original = dispatch_coincident(&device, original, &a, &b, &bytes);
    let from_generated = dispatch_coincident(&device, &generated, &a, &b, &bytes);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &from_original.texture, &from_generated.texture, 1e-5, 1e-5);
    assert_eq!(
        r.over_count, 0,
        "generated mix must reproduce mix.wgsl (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
    assert!(r.max_abs < 1e-5, "coincident path should be ~bit-identical, got {}", r.max_abs);
}

/// Positional-atom parity (design §12.3 vocabulary widening). vignette is the
/// first atom that reads its pixel POSITION via the ambient `uv`/`dims` args.
/// The generated standalone kernel derives `aspect = dims.x/dims.y` itself and
/// must reproduce the hand vignette.wgsl (which takes `aspect` as a uniform)
/// bit-for-bit — so the two uniform payloads differ (hand carries aspect,
/// generated doesn't). Verified on a NON-SQUARE canvas so the aspect-correct
/// Circle is exercised, plus the uv-only Rectangle.
#[test]
fn generated_vignette_matches_original() {
    let device = crate::test_device();
    let (w, h) = (160u32, 128u32); // aspect 1.25, deliberately non-square
    let input = gradient(&device, w, h);
    let aspect = w as f32 / h as f32;

    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let node = registry.construct("node.vignette").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("vignette generates");
    let original = include_str!("../../primitives/shaders/vignette.wgsl");
    let differ = TextureDiff::new(&device);

    // (shape, size, softness, strength): Circle (aspect-sensitive) + Rectangle.
    for (shape, size, softness, strength) in
        [(0u32, 0.6f32, 0.4f32, 1.0f32), (2u32, 0.95, 0.06, 1.0)]
    {
        // Hand uniform: shape, size, softness, strength, aspect + pad → 32 B.
        let mut hand_bytes = Vec::new();
        hand_bytes.extend_from_slice(&shape.to_le_bytes());
        hand_bytes.extend_from_slice(&size.to_le_bytes());
        hand_bytes.extend_from_slice(&softness.to_le_bytes());
        hand_bytes.extend_from_slice(&strength.to_le_bytes());
        hand_bytes.extend_from_slice(&aspect.to_le_bytes());
        while hand_bytes.len() % 16 != 0 {
            hand_bytes.push(0);
        }
        // Generated uniform: shape, size, softness, strength → 16 B (aspect
        // is recovered from dims inside the body, not plumbed through).
        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&shape.to_le_bytes());
        gen_bytes.extend_from_slice(&size.to_le_bytes());
        gen_bytes.extend_from_slice(&softness.to_le_bytes());
        gen_bytes.extend_from_slice(&strength.to_le_bytes());

        let from_original = dispatch_pointwise(&device, original, &input, &hand_bytes);
        let from_generated = dispatch_pointwise(&device, &generated, &input, &gen_bytes);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-4,
            1e-4,
        );
        assert_eq!(
            r.over_count, 0,
            "vignette shape {shape}: generated must reproduce vignette.wgsl \
             (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }
}

/// Dispatch a two-input EXACT-TEXEL kernel: uniform(0), a(1), b(2), dst(3) —
/// NO sampler (both inputs are textureLoad'd). Mirrors dither's binding set.
fn dispatch_two_texel(
    device: &GpuDevice,
    wgsl: &str,
    a: &GpuTexture,
    b: &GpuTexture,
    param_bytes: &[u8],
) -> RenderTarget {
    let (w, h) = (a.width, a.height);
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-test-dither");
    let out = RenderTarget::new(device, w, h, FMT, "codegen-out-dither");
    let mut enc = device.create_encoder("codegen-test-dither");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes { binding: 0, data: param_bytes },
            GpuBinding::Texture { binding: 1, texture: a },
            GpuBinding::Texture { binding: 2, texture: b },
            GpuBinding::Texture { binding: 3, texture: &out.texture },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-test-dither",
    );
    enc.commit_and_wait_completed();
    out
}

/// CoincidentTexel parity (design §12.3 read-semantics generalization).
/// dither is the first atom with exact-texel inputs and NO sampler — both
/// `in` and `pattern` are textureLoad'd at the fragment texel (sampling the
/// threshold map would blend neighbouring thresholds and smear the dither).
/// The generated standalone kernel must reproduce hand dither.wgsl
/// bit-for-bit AND emit the sampler-free binding set (uniform(0), in(1),
/// pattern(2), dst(3)) so it's a drop-in for dither's run().
#[test]
fn generated_dither_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let source = gradient(&device, w, h);
    let pattern = gradient_b(&device, w, h); // R channel = the threshold map

    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let node = registry.construct("node.dither").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("dither generates");

    // Structural: the all-texel atom binds NO sampler and reads both inputs
    // via textureLoad (the new CoincidentTexel read-path).
    assert!(
        !generated.contains("var samp: sampler"),
        "an all-CoincidentTexel atom must bind no sampler:\n{generated}"
    );
    assert_eq!(
        generated.matches("textureLoad(").count(),
        2,
        "both dither inputs must be textureLoad'd:\n{generated}"
    );

    let original = include_str!("../../primitives/shaders/dither.wgsl");
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&0.5f32.to_le_bytes()); // amount

    let from_original = dispatch_two_texel(&device, original, &source, &pattern, &bytes);
    let from_generated = dispatch_two_texel(&device, &generated, &source, &pattern, &bytes);
    let differ = TextureDiff::new(&device);
    let r = differ.compare(
        &device,
        &from_original.texture,
        &from_generated.texture,
        1e-5,
        1e-5,
    );
    assert_eq!(
        r.over_count, 0,
        "generated dither must reproduce dither.wgsl (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
}

/// Dispatch an N-input coincident kernel: uniform(0), inputs(1..=N),
/// sampler(N+1), dst(N+2) — the generated MultiInputCoincident layout for any
/// arity. Generalizes `dispatch_coincident` (which is fixed at 2 inputs).
fn dispatch_coincident_n(
    device: &GpuDevice,
    wgsl: &str,
    inputs: &[&GpuTexture],
    param_bytes: &[u8],
) -> RenderTarget {
    let (w, h) = (inputs[0].width, inputs[0].height);
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-coincident-n");
    let sampler = device.create_sampler(&GpuSamplerDesc::default());
    let out = RenderTarget::new(device, w, h, FMT, "codegen-out-coincident-n");
    let mut bindings: Vec<GpuBinding> =
        vec![GpuBinding::Bytes { binding: 0, data: param_bytes }];
    for (i, t) in inputs.iter().enumerate() {
        bindings.push(GpuBinding::Texture { binding: (i + 1) as u32, texture: t });
    }
    bindings.push(GpuBinding::Sampler {
        binding: (inputs.len() + 1) as u32,
        sampler: &sampler,
    });
    bindings.push(GpuBinding::Texture {
        binding: (inputs.len() + 2) as u32,
        texture: &out.texture,
    });
    let mut enc = device.create_encoder("codegen-coincident-n");
    enc.dispatch_compute(
        &pipeline,
        &bindings,
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-coincident-n",
    );
    enc.commit_and_wait_completed();
    out
}

/// Coincident multi-input parity (overnight vocabulary sweep): each blend
/// atom's generated kernel reproduces its hand shader bit-for-bit. Inputs
/// alternate the two gradients — parity is generated-vs-hand on identical
/// inputs, so the specific textures don't matter, only that both kernels see
/// the same set. Covers arities 2, 3, and 5.
#[test]
fn generated_coincident_atoms_match_originals() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let ga = gradient(&device, w, h);
    let gb = gradient_b(&device, w, h);
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let shaders_dir =
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
    let differ = TextureDiff::new(&device);

    // (type_id, hand shader, #texture inputs, f32 params in PARAMS order).
    let cases: &[(&str, &str, usize, &[f32])] = &[
        ("node.wet_dry", "wet_dry_mix.wgsl", 2, &[0.6]),
        ("node.hdr_mix", "hdr_retention_mix.wgsl", 2, &[0.7]),
        ("node.masked_mix", "masked_mix.wgsl", 3, &[0.8]),
        ("node.texture_sum_5", "texture_sum_5.wgsl", 5, &[5.0]),
    ];
    for (type_id, shader_file, n_inputs, params) in cases {
        let node = registry.construct(type_id).unwrap();
        let generated = generate_standalone(&StandaloneKernelSpec {
            fusion_kind: node.fusion_kind(),
            body: node.wgsl_body().unwrap(),
            inputs: node.inputs(),
            params: node.parameters(),
            input_access: node.input_access(),
            derived_uniforms: node.derived_uniforms(),
            outputs: node.outputs(),
            stencil_fetch: false,
            includes: &[],
        })
        .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
        let original = std::fs::read_to_string(format!("{shaders_dir}/{shader_file}"))
            .unwrap_or_else(|e| panic!("read {shader_file}: {e}"));
        let texs: Vec<&GpuTexture> =
            (0..*n_inputs).map(|i| if i % 2 == 0 { &ga } else { &gb }).collect();
        let bytes = pack_f32(params);
        let from_original = dispatch_coincident_n(&device, &original, &texs, &bytes);
        let from_generated = dispatch_coincident_n(&device, &generated, &texs, &bytes);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "{type_id}: generated must reproduce {shader_file} (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }
}

/// Enum/int pointwise parity (overnight sweep): atoms whose uniform mixes f32
/// and u32 (Enum -> u32) fields, so the payload is packed by hand rather than
/// via pack_f32. flash branches on `mode`; reinhard on `curve`. Standard
/// pointwise layout (uniform(0), tex(1), sampler(2), dst(3)).
#[test]
fn generated_enum_pointwise_atoms_match_originals() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let input = gradient(&device, w, h);
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let shaders_dir =
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
    let differ = TextureDiff::new(&device);

    // flash: amount=0.7 (f32), mode=2 Gain (u32), pad to 16.
    let mut flash_bytes = Vec::new();
    flash_bytes.extend_from_slice(&0.7f32.to_le_bytes());
    flash_bytes.extend_from_slice(&2u32.to_le_bytes());
    while flash_bytes.len() < 16 {
        flash_bytes.push(0);
    }
    // reinhard: intensity=1.5, contrast=1.2 (f32), curve=1 Simple (u32), pad.
    let mut reinhard_bytes = Vec::new();
    reinhard_bytes.extend_from_slice(&1.5f32.to_le_bytes());
    reinhard_bytes.extend_from_slice(&1.2f32.to_le_bytes());
    reinhard_bytes.extend_from_slice(&1u32.to_le_bytes());
    while reinhard_bytes.len() < 16 {
        reinhard_bytes.push(0);
    }
    // reinhard again with curve=2 Log — the third arm has its own
    // generated-vs-hand row so a codegen drift in the Log branch can't
    // hide behind the Simple case.
    let mut reinhard_log_bytes = Vec::new();
    reinhard_log_bytes.extend_from_slice(&1.5f32.to_le_bytes());
    reinhard_log_bytes.extend_from_slice(&1.2f32.to_le_bytes());
    reinhard_log_bytes.extend_from_slice(&2u32.to_le_bytes());
    while reinhard_log_bytes.len() < 16 {
        reinhard_log_bytes.push(0);
    }

    // chroma_key: key_color Vec3 (3 f32) + tolerance, softness (f32) + mode
    // (Enum -> u32) + pad → 32 B. The Vec3 param expands to 3 uniform floats,
    // matching the hand shader's key_r/g/b layout.
    let mut chroma_bytes = Vec::new();
    chroma_bytes.extend_from_slice(&0.0f32.to_le_bytes()); // key R (greenscreen)
    chroma_bytes.extend_from_slice(&1.0f32.to_le_bytes()); // key G
    chroma_bytes.extend_from_slice(&0.0f32.to_le_bytes()); // key B
    chroma_bytes.extend_from_slice(&0.4f32.to_le_bytes()); // tolerance
    chroma_bytes.extend_from_slice(&0.1f32.to_le_bytes()); // softness
    chroma_bytes.extend_from_slice(&1u32.to_le_bytes()); // mode = Reject
    while chroma_bytes.len() < 32 {
        chroma_bytes.push(0);
    }
    let cases: &[(&str, &str, &[u8])] = &[
        ("node.flash", "flash.wgsl", flash_bytes.as_slice()),
        ("node.reinhard_tone_map", "reinhard_tone_map.wgsl", reinhard_bytes.as_slice()),
        ("node.reinhard_tone_map", "reinhard_tone_map.wgsl", reinhard_log_bytes.as_slice()),
        ("node.chroma_key", "chroma_key.wgsl", chroma_bytes.as_slice()),
    ];
    for (type_id, shader_file, bytes) in cases {
        let node = registry.construct(type_id).unwrap();
        let generated = generate_standalone(&StandaloneKernelSpec {
            fusion_kind: node.fusion_kind(),
            body: node.wgsl_body().unwrap(),
            inputs: node.inputs(),
            params: node.parameters(),
            input_access: node.input_access(),
            derived_uniforms: node.derived_uniforms(),
            outputs: node.outputs(),
            stencil_fetch: false,
            includes: &[],
        })
        .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
        let original = std::fs::read_to_string(format!("{shaders_dir}/{shader_file}"))
            .unwrap_or_else(|e| panic!("read {shader_file}: {e}"));
        let from_original = dispatch_pointwise(&device, &original, &input, bytes);
        let from_generated = dispatch_pointwise(&device, &generated, &input, bytes);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "{type_id}: generated must reproduce {shader_file} (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }
}

/// Dispatch a PARAMLESS pointwise kernel: tex(0), sampler(1), dst(2) — no
/// uniform binding (a paramless atom's generated kernel binds none).
fn dispatch_paramless_pointwise(
    device: &GpuDevice,
    wgsl: &str,
    input: &GpuTexture,
) -> RenderTarget {
    let (w, h) = (input.width, input.height);
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-paramless");
    let sampler = device.create_sampler(&GpuSamplerDesc::default());
    let out = RenderTarget::new(device, w, h, FMT, "codegen-out-paramless");
    let mut enc = device.create_encoder("codegen-paramless");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Texture { binding: 0, texture: input },
            GpuBinding::Sampler { binding: 1, sampler: &sampler },
            GpuBinding::Texture { binding: 2, texture: &out.texture },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-paramless",
    );
    enc.commit_and_wait_completed();
    out
}

/// Paramless parity (overnight sweep): abs_texture has zero params, so the
/// generated kernel emits NO uniform and starts its textures at binding 0 —
/// a drop-in for the hand abs_texture.wgsl, which also has no uniform. Proves
/// the paramless codegen path matches bit-for-bit.
#[test]
fn generated_paramless_atom_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let input = gradient(&device, w, h);
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let node = registry.construct("node.absolute_value").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("abs_texture generates");

    // Structural: no uniform, textures start at binding 0.
    assert!(
        !generated.contains("var<uniform>"),
        "a paramless atom must bind no uniform:\n{generated}"
    );
    assert!(
        generated.contains("@group(0) @binding(0) var tex_in"),
        "paramless tex must start at binding 0:\n{generated}"
    );

    let original = include_str!("../../primitives/shaders/abs_texture.wgsl");
    let from_original = dispatch_paramless_pointwise(&device, original, &input);
    let from_generated = dispatch_paramless_pointwise(&device, &generated, &input);
    let differ = TextureDiff::new(&device);
    let r = differ.compare(
        &device,
        &from_original.texture,
        &from_generated.texture,
        1e-5,
        1e-5,
    );
    assert_eq!(
        r.over_count, 0,
        "generated abs_texture must reproduce abs_texture.wgsl (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
}

/// Gather parity (design §11.B): remap is the first GATHER atom — `source` is
/// sampled at a coord the body COMPUTES, so the codegen passes it as a
/// texture+sampler arg (not a pre-read register), while `uv_field` is
/// coincident. The hand remap.wgsl interleaves the sampler between its two
/// textures (uniform0/src1/samp2/field3/out4); the generated kernel binds the
/// textures consecutively then the sampler (uniform0/src1/field2/samp3/dst4),
/// so each is dispatched with its own layout. wrap=Mirror exercises the
/// wrap_coord helper.
#[test]
fn generated_remap_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let source = gradient(&device, w, h);
    let field = gradient_b(&device, w, h); // .rg carry the target UVs
    let sampler = device.create_sampler(&GpuSamplerDesc::default());

    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let node = registry.construct("node.remap").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("remap generates");

    // Structural: gather `source` is NOT pre-read; textures then sampler.
    assert!(
        generated.contains("@group(0) @binding(1) var tex_source"),
        "source at binding 1:\n{generated}"
    );
    assert!(
        generated.contains("@group(0) @binding(2) var tex_uv_field"),
        "uv_field at binding 2:\n{generated}"
    );
    assert!(
        generated.contains("@group(0) @binding(3) var samp"),
        "sampler after the textures:\n{generated}"
    );
    assert!(
        !generated.contains("let c_source"),
        "a Gather input must not be pre-sampled into a register:\n{generated}"
    );

    let original = include_str!("../../primitives/shaders/remap.wgsl");
    // wrap=2 (Mirror), mode=0 (Absolute).
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&2u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&0u32.to_le_bytes());

    // Hand layout: uniform(0), source(1), sampler(2), uv_field(3), out(4).
    let hand_out = RenderTarget::new(&device, w, h, FMT, "remap-hand");
    {
        let pipeline = device.create_compute_pipeline(original, ENTRY, "remap-hand");
        let mut enc = device.create_encoder("remap-hand");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &bytes },
                GpuBinding::Texture { binding: 1, texture: &source },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &field },
                GpuBinding::Texture { binding: 4, texture: &hand_out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "remap-hand",
        );
        enc.commit_and_wait_completed();
    }
    // Generated layout: uniform(0), source(1), uv_field(2), sampler(3), dst(4).
    let gen_out = RenderTarget::new(&device, w, h, FMT, "remap-gen");
    {
        let pipeline = device.create_compute_pipeline(&generated, ENTRY, "remap-gen");
        let mut enc = device.create_encoder("remap-gen");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &bytes },
                GpuBinding::Texture { binding: 1, texture: &source },
                GpuBinding::Texture { binding: 2, texture: &field },
                GpuBinding::Sampler { binding: 3, sampler: &sampler },
                GpuBinding::Texture { binding: 4, texture: &gen_out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "remap-gen",
        );
        enc.commit_and_wait_completed();
    }

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &hand_out.texture, &gen_out.texture, 1e-5, 1e-5);
    assert_eq!(
        r.over_count, 0,
        "generated remap must reproduce remap.wgsl (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
}

/// More gather atoms, now the Gather codegen exists. chromatic_displace
/// (3-tap RGB split) and uv_displace_by_flow both bind
/// uniform0/tex1/tex2/samp3/dst4 for BOTH the hand shader and the generated
/// kernel, so dispatch_coincident covers them directly. The first texture is
/// the gathered `in`, the second the coincident field.
#[test]
fn generated_gather_atoms_match_originals() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let ga = gradient(&device, w, h);
    let gb = gradient_b(&device, w, h);
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let shaders_dir =
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
    let differ = TextureDiff::new(&device);

    // color_lut: `in` coincident (a = the centre sample) + `lut` gathered
    // (b, sampled at the luminance-indexed coord). amount=0.5 exercises the
    // crossfade; the LUT texture is just gradient_b sampled at y=0.5.
    let cases: &[(&str, &str, &[f32])] = &[
        ("node.rgb_split", "chromatic_displace.wgsl", &[2.0]),
        ("node.uv_displace_by_flow", "uv_displace_by_flow.wgsl", &[0.05, 0.5]),
        ("node.color_lut", "lut1d.wgsl", &[0.5, 1.5]),
        // slope_displace: base (a) + image (b) both gathered; strength/step/weight.
        ("node.slope_displace", "slope_displace.wgsl", &[5.0, 5.0, 0.001]),
        // texture_advect: in (a) gathered at adv_uv + velocity (b) coincident;
        // dt + boundary(0=Repeat, body ignores it — the test sampler is Clamp,
        // but adv samples land in-bounds for this velocity so wrap is moot).
        ("node.texture_advect", "texture_advect.wgsl", &[2.0, 0.0]),
    ];
    for (type_id, shader_file, params) in cases {
        let node = registry.construct(type_id).unwrap();
        let generated = generate_standalone(&StandaloneKernelSpec {
            fusion_kind: node.fusion_kind(),
            body: node.wgsl_body().unwrap(),
            inputs: node.inputs(),
            params: node.parameters(),
            input_access: node.input_access(),
            derived_uniforms: node.derived_uniforms(),
            outputs: node.outputs(),
            stencil_fetch: false,
            includes: &[],
        })
        .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
        let original = std::fs::read_to_string(format!("{shaders_dir}/{shader_file}"))
            .unwrap_or_else(|e| panic!("read {shader_file}: {e}"));
        let bytes = pack_f32(params);
        let from_original = dispatch_coincident(&device, &original, &ga, &gb, &bytes);
        let from_generated = dispatch_coincident(&device, &generated, &ga, &gb, &bytes);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "{type_id}: generated must reproduce {shader_file} (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }
}

/// Single-input GATHER parity: the neighbourhood-filter family (sharpen,
/// edge_detect) reads `in` at offsets the body computes, so it binds the
/// 1-input layout uniform(0)/tex(1)/samp(2)/dst(3) — identical to a pointwise
/// atom — and the body samples `in` itself. Both recover the texel step from
/// the ambient `dims` (= output size), so the generated kernel ignores any
/// texel_size_* fields the hand uniform carries; the parity payload still
/// packs those fields (= 1/dims at the test size) so the hand shader reads the
/// matching step. `dispatch_pointwise` covers the shared 1-input layout.
#[test]
fn generated_single_input_gather_atoms_match_originals() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let input = gradient(&device, w, h);
    let texel = 1.0f32 / 128.0;
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let shaders_dir =
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
    let differ = TextureDiff::new(&device);

    // sharpen PARAMS: [amount]. edge_detect PARAMS: [amount, threshold]; its
    // hand uniform additionally carries [texel_x, texel_y] = 1/dims.
    let sharpen_bytes = pack_f32(&[1.5]);
    let edge_bytes = pack_f32(&[0.7, 0.2, texel, texel]);
    // gradient_central_diff PARAMS: [channel, scale_mode, wrap_mode] (all
    // Enum->u32). channel=G, scale_mode=UV (exercises the dims*0.5 branch);
    // wrap_mode is host-side sampler-only so the body ignores it. The hand
    // uniform is {channel, scale_mode, _pad, _pad}; the generated Params is
    // {channel, scale_mode, wrap_mode, _pad}. Both read channel/scale_mode
    // from the same offsets, so one 16-byte payload drives both.
    let mut grad_bytes = vec![0u8; 16];
    grad_bytes[0..4].copy_from_slice(&1u32.to_le_bytes()); // channel = G
    grad_bytes[4..8].copy_from_slice(&1u32.to_le_bytes()); // scale_mode = UV
    // convolution_2d_9tap PARAMS: [k0..k8, bias, normalise (Bool->u32)] —
    // identical layout to the hand ConvUniforms. A normalising box blur
    // exercises the sum-normalise divide + the centre-alpha passthrough.
    let mut conv_bytes = vec![0u8; 48];
    for i in 0..9 {
        conv_bytes[i * 4..i * 4 + 4].copy_from_slice(&1.0f32.to_le_bytes());
    }
    conv_bytes[36..40].copy_from_slice(&0.0f32.to_le_bytes()); // bias
    conv_bytes[40..44].copy_from_slice(&1u32.to_le_bytes()); // normalise = true
    // mirror_axis PARAMS: [angle] — Gather sampled at the mirrored UV; the body
    // computes cos/sin from angle on the GPU (matching the hand), bit-exact.
    let mirror_bytes = pack_f32(&[std::f32::consts::FRAC_PI_4]);
    // heightmap_to_normal PARAMS: [z_scale, aspect, coord_space]; coord_space=0
    // (TangentZ) packs as f32 0.0 = u32 0.
    let heightmap_bytes = pack_f32(&[0.5, 1.0, 0.0]);
    let cases: &[(&str, &str, &[u8])] = &[
        ("node.sharpen", "sharpen.wgsl", sharpen_bytes.as_slice()),
        ("node.edge_detect", "edge_detect.wgsl", edge_bytes.as_slice()),
        ("node.edge_slope", "gradient_central_diff.wgsl", grad_bytes.as_slice()),
        ("node.custom_convolution", "convolution_2d_9tap.wgsl", conv_bytes.as_slice()),
        ("node.flip", "mirror_axis.wgsl", mirror_bytes.as_slice()),
        ("node.surface_bumps", "heightmap_to_normal.wgsl", heightmap_bytes.as_slice()),
    ];
    for (type_id, shader_file, bytes) in cases {
        let node = registry.construct(type_id).unwrap();
        let generated = generate_standalone(&StandaloneKernelSpec {
            fusion_kind: node.fusion_kind(),
            body: node.wgsl_body().unwrap(),
            inputs: node.inputs(),
            params: node.parameters(),
            input_access: node.input_access(),
            derived_uniforms: node.derived_uniforms(),
            outputs: node.outputs(),
            stencil_fetch: false,
            includes: &[],
        })
        .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
        // Structural: the gather input is NOT pre-sampled into a register.
        assert!(
            !generated.contains("let c_in"),
            "{type_id}: a Gather input must not be pre-sampled:\n{generated}"
        );
        let original = std::fs::read_to_string(format!("{shaders_dir}/{shader_file}"))
            .unwrap_or_else(|e| panic!("read {shader_file}: {e}"));
        let from_original = dispatch_pointwise(&device, &original, &input, bytes);
        let from_generated = dispatch_pointwise(&device, &generated, &input, bytes);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "{type_id}: generated must reproduce {shader_file} (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }
}

/// Dual-packed GATHER parity: node.gaussian_blur is a single-input gather
/// whose hand uniform interleaves computed texel_x/texel_y fields the body no
/// longer reads (it recovers the step from `dims`), and whose generated Params
/// instead carries the address_mode param (host-side sampler only). So the two
/// kernels take DIFFERENT 32-byte uniform layouts for the same logical params:
/// the hand layout {kernel_size, axis, step, texel_x, texel_y, radius_mode,
/// radius, _pad} and the generated layout {kernel_size, axis, step,
/// radius_mode, radius, address_mode, _pad, _pad}. Pack each, dispatch via the
/// shared 1-input layout, diff. Covers Fixed (9/17-tap) and Dynamic modes on
/// both axes; the default Clamp sampler matches address_mode=0.
#[test]
fn generated_separable_gaussian_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let input = gradient(&device, w, h);
    let texel = 1.0f32 / 128.0;
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let differ = TextureDiff::new(&device);

    let node = registry.construct("node.gaussian_blur").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: node.stencil_fetch(),
        includes: node.wgsl_includes(),
    })
    .expect("gaussian_blur generates");
    assert!(
        !generated.contains("let c_in"),
        "a Gather input must not be pre-sampled:\n{generated}"
    );
    let original = include_str!("../../primitives/shaders/separable_gaussian.wgsl");

    let pack_hand = |ks: u32, axis: u32, step: f32, rmode: u32, radius: f32| -> Vec<u8> {
        let mut b = vec![0u8; 32];
        b[0..4].copy_from_slice(&ks.to_le_bytes());
        b[4..8].copy_from_slice(&axis.to_le_bytes());
        b[8..12].copy_from_slice(&step.to_le_bytes());
        b[12..16].copy_from_slice(&texel.to_le_bytes()); // texel_x
        b[16..20].copy_from_slice(&texel.to_le_bytes()); // texel_y
        b[20..24].copy_from_slice(&rmode.to_le_bytes());
        b[24..28].copy_from_slice(&radius.to_le_bytes());
        b
    };
    let pack_gen = |ks: u32, axis: u32, step: f32, rmode: u32, radius: f32| -> Vec<u8> {
        let mut b = vec![0u8; 32];
        b[0..4].copy_from_slice(&ks.to_le_bytes());
        b[4..8].copy_from_slice(&axis.to_le_bytes());
        b[8..12].copy_from_slice(&step.to_le_bytes());
        b[12..16].copy_from_slice(&rmode.to_le_bytes());
        b[16..20].copy_from_slice(&radius.to_le_bytes());
        // address_mode = 0 (Clamp) at [20..24], pads at [24..32].
        b
    };

    // (kernel_size, axis, step, radius_mode, radius).
    let sets: &[(u32, u32, f32, u32, f32)] = &[
        (1, 0, 2.0, 0, 0.0),  // Fixed 17-tap, horizontal, step 2
        (0, 1, 1.0, 0, 0.0),  // Fixed 9-tap, vertical
        (2, 0, 1.0, 0, 0.0),  // Fixed 25-tap, horizontal
        (1, 0, 1.0, 1, 10.0), // Dynamic, horizontal, radius 10
        (1, 1, 1.0, 1, 5.0),  // Dynamic, vertical, radius 5
    ];
    for &(ks, axis, step, rmode, radius) in sets {
        let hand_bytes = pack_hand(ks, axis, step, rmode, radius);
        let gen_bytes = pack_gen(ks, axis, step, rmode, radius);
        let from_original = dispatch_pointwise(&device, original, &input, &hand_bytes);
        let from_generated = dispatch_pointwise(&device, &generated, &input, &gen_bytes);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "gaussian_blur set (ks={ks}, axis={axis}, step={step}, rmode={rmode}, \
             radius={radius}): generated must reproduce separable_gaussian.wgsl \
             (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }
}

/// Dual-packed SOURCE parity: node.basic_shape is a generator whose run()
/// used to preprocess its params before packing (uv_scale = 1/scale, shape
/// index as f32, wireframe thresholded) into a reordered hand uniform. The
/// body now does that preprocessing, so the generated Params carry the RAW
/// params in declaration order. The two kernels take DIFFERENT 32-byte
/// layouts for the same logical inputs — pack each, dispatch as a Source, and
/// diff across all three shapes (solid + wireframe + rotated).
#[test]
fn generated_basic_shape_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let differ = TextureDiff::new(&device);

    let node = registry.construct("node.basic_shape").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("basic_shape generates");
    let original = include_str!("../../primitives/shaders/basic_shape.wgsl");

    // Hand layout: {aspect, line, uv_scale=1/scale, shape_idx(f32), is_wireframe
    // (thresholded 0/1), rotation, _pad, _pad}.
    let pack_hand = |shape: u32, aspect: f32, scale: f32, line: f32, rot: f32, wf: f32| -> Vec<u8> {
        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let wf_flag = if wf > 0.5 { 1.0f32 } else { 0.0 };
        let mut b = vec![0u8; 32];
        b[0..4].copy_from_slice(&aspect.to_le_bytes());
        b[4..8].copy_from_slice(&line.to_le_bytes());
        b[8..12].copy_from_slice(&uv_scale.to_le_bytes());
        b[12..16].copy_from_slice(&(shape as f32).to_le_bytes());
        b[16..20].copy_from_slice(&wf_flag.to_le_bytes());
        b[20..24].copy_from_slice(&rot.to_le_bytes());
        b
    };
    // Generated layout: {shape(u32), aspect, scale, line, rotation, is_wireframe
    // (raw), _pad, _pad}.
    let pack_gen = |shape: u32, aspect: f32, scale: f32, line: f32, rot: f32, wf: f32| -> Vec<u8> {
        let mut b = vec![0u8; 32];
        b[0..4].copy_from_slice(&shape.to_le_bytes());
        b[4..8].copy_from_slice(&aspect.to_le_bytes());
        b[8..12].copy_from_slice(&scale.to_le_bytes());
        b[12..16].copy_from_slice(&line.to_le_bytes());
        b[16..20].copy_from_slice(&rot.to_le_bytes());
        b[20..24].copy_from_slice(&wf.to_le_bytes());
        b
    };

    // (shape, aspect, scale, line, rotation, is_wireframe).
    let sets: &[(u32, f32, f32, f32, f32, f32)] = &[
        (0, 1.0, 1.0, 0.015, 0.0, 0.0),  // Square, solid
        (1, 1.5, 0.8, 0.02, 0.5, 1.0),   // Diamond, wireframe, rotated, aspect
        (2, 1.0, 1.2, 0.01, -0.3, 0.0),  // Octagon, solid, rotated, scaled
    ];
    for &(shape, aspect, scale, line, rot, wf) in sets {
        let hand_bytes = pack_hand(shape, aspect, scale, line, rot, wf);
        let gen_bytes = pack_gen(shape, aspect, scale, line, rot, wf);
        let from_original = dispatch_source(&device, original, Some(&hand_bytes), w, h);
        let from_generated = dispatch_source(&device, &generated, Some(&gen_bytes), w, h);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "basic_shape (shape={shape}, scale={scale}, wf={wf}): generated must \
             reproduce basic_shape.wgsl (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }
}

/// TABLE-param SOURCE parity: node.gradient's `stops` Table param expands
/// in the generated uniform to a `stops_count` header word + a fixed
/// `array<vec4<f32>, 16>` after the aligned header, and the body receives
/// (stops_count, stops). The hand uniform is {count, domain, _pad, _pad, stops}
/// (count first); the generated layout is {domain, count, _pad, _pad, stops}
/// (scalar params before table counts) — the array sits at the same offset 16
/// in both, only the two header scalars swap. Pack each, dispatch as a Source,
/// diff. domain=2 exercises the past-last-stop extrapolation tail.
#[test]
fn generated_gradient_ramp_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let differ = TextureDiff::new(&device);

    let node = registry.construct("node.gradient").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("gradient_ramp generates");
    // Structural: the Table param expands to a count word + a vec4 array.
    assert!(
        generated.contains("stops_count: u32"),
        "table count word missing:\n{generated}"
    );
    assert!(
        generated.contains("stops: array<vec4<f32>, 16>"),
        "table array missing:\n{generated}"
    );
    let original = include_str!("../../primitives/shaders/gradient_ramp.wgsl");

    let count: u32 = 3;
    let domain: f32 = 2.0;
    let stops: [[f32; 4]; 3] = [
        [0.0, 0.0, 0.0, 0.0], // black at t=0
        [0.5, 1.0, 0.0, 0.0], // red at t=0.5
        [1.0, 1.0, 1.0, 0.0], // yellow at t=1.0 (then extrapolated to t=2)
    ];
    // 16 vec4 array = 256 bytes, first 3 stops filled.
    let mut stops_bytes = vec![0u8; 256];
    for (i, s) in stops.iter().enumerate() {
        for (j, v) in s.iter().enumerate() {
            let off = i * 16 + j * 4;
            stops_bytes[off..off + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    // Hand header: {count, domain, _pad, _pad}.
    let mut hand = vec![0u8; 16];
    hand[0..4].copy_from_slice(&count.to_le_bytes());
    hand[4..8].copy_from_slice(&domain.to_le_bytes());
    hand.extend_from_slice(&stops_bytes);
    // Generated header: {domain, count, _pad, _pad}.
    let mut gen_bytes = vec![0u8; 16];
    gen_bytes[0..4].copy_from_slice(&domain.to_le_bytes());
    gen_bytes[4..8].copy_from_slice(&count.to_le_bytes());
    gen_bytes.extend_from_slice(&stops_bytes);

    let from_original = dispatch_source(&device, original, Some(&hand), w, h);
    let from_generated = dispatch_source(&device, &generated, Some(&gen_bytes), w, h);
    let r = differ.compare(
        &device,
        &from_original.texture,
        &from_generated.texture,
        1e-5,
        1e-5,
    );
    assert_eq!(
        r.over_count, 0,
        "generated gradient_ramp must reproduce gradient_ramp.wgsl \
         (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
}

/// RESAMPLE GATHER parity: node.downsample's output is SMALLER than its input
/// (a box filter), so it can't reuse dispatch_pointwise (which sizes output ==
/// input). The body is a single-input Gather that reads `in` via textureLoad at
/// input-pixel coords, recovering its output pixel id from uv and the box
/// factor from in_dims/out_dims. Dispatch a 128→64 (factor 2) reduction for
/// both the hand and generated kernels and diff. The uniform `factor` is
/// diagnostic (the shader uses the dim ratio), so one 16-byte payload drives
/// both.
#[test]
fn generated_downsample_matches_original() {
    let device = crate::test_device();
    let input = gradient(&device, 128, 128);
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let differ = TextureDiff::new(&device);

    let node = registry.construct("node.downsample").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("downsample generates");
    assert!(
        !generated.contains("let c_in"),
        "a Gather input must not be pre-sampled:\n{generated}"
    );
    let original = include_str!("../../primitives/shaders/downsample.wgsl");

    let dispatch = |wgsl: &str| -> RenderTarget {
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-downsample");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let out = RenderTarget::new(&device, 64, 64, FMT, "codegen-out-downsample");
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&4u32.to_le_bytes()); // diagnostic factor
        let mut enc = device.create_encoder("codegen-downsample");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &bytes },
                GpuBinding::Texture { binding: 1, texture: &input },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
            ],
            [64u32.div_ceil(16), 64u32.div_ceil(16), 1],
            "codegen-downsample",
        );
        enc.commit_and_wait_completed();
        out
    };

    let from_original = dispatch(original);
    let from_generated = dispatch(&generated);
    let r = differ.compare(
        &device,
        &from_original.texture,
        &from_generated.texture,
        1e-5,
        1e-5,
    );
    assert_eq!(
        r.over_count, 0,
        "generated downsample must reproduce downsample.wgsl (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
}

/// SPECIALIZATION + 2-input GATHER parity: node.variable_blur
/// gathers `in` + `width` along one axis and selects its tap count / weighting
/// via the QUALITY_LEVEL / WEIGHTING_MODE specialization tokens (run() compiles
/// the GENERATED WGSL through create_specialized_compute_pipeline, same as the
/// hand kernel). Both kernels take the identical binding layout (uniform0/in1/
/// width2/samp3/dst4) and the body reads only direction+max_radius from the
/// uniform, so one 16-byte payload drives both; specialize each with the SAME
/// (quality, weighting) and diff across three combos.
#[test]
fn generated_gaussian_blur_variable_width_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let src = gradient(&device, w, h);
    let width = gradient_b(&device, w, h); // R channel varies → CoC varies
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let differ = TextureDiff::new(&device);

    let node = registry.construct("node.variable_blur").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: node.stencil_fetch(),
        includes: node.wgsl_includes(),
    })
    .expect("variable-width blur generates");
    assert!(
        !generated.contains("let c_in") && !generated.contains("let c_width"),
        "both gather inputs must avoid pre-sampling:\n{generated}"
    );
    let original =
        include_str!("../../primitives/shaders/gaussian_blur_variable_width.wgsl");

    // {direction (0=H), max_radius, _pad, _pad}; the body reads only these two.
    let mut bytes = [0u8; 16];
    bytes[4..8].copy_from_slice(&12.0f32.to_le_bytes());

    let dispatch = |wgsl: &str, q: &str, wt: &str| -> RenderTarget {
        let pipeline = device.create_specialized_compute_pipeline(
            wgsl,
            ENTRY,
            &[("QUALITY_LEVEL", q), ("WEIGHTING_MODE", wt)],
            "vbw-test",
        );
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let out = RenderTarget::new(&device, w, h, FMT, "vbw-out");
        let mut enc = device.create_encoder("vbw-test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &bytes },
                GpuBinding::Texture { binding: 1, texture: &src },
                GpuBinding::Texture { binding: 2, texture: &width },
                GpuBinding::Sampler { binding: 3, sampler: &sampler },
                GpuBinding::Texture { binding: 4, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "vbw-test",
        );
        enc.commit_and_wait_completed();
        out
    };

    for (q, wt) in [("1u", "0u"), ("2u", "1u"), ("0u", "0u")] {
        let h_out = dispatch(original, q, wt);
        let g_out = dispatch(&generated, q, wt);
        let r = differ.compare(&device, &h_out.texture, &g_out.texture, 1e-5, 1e-5);
        assert_eq!(
            r.over_count, 0,
            "variable-width blur (Q={q}, W={wt}): generated must reproduce the hand \
             kernel (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }
}

/// Allocate an n×n×n 3D texture with the given usage.
fn make_3d_texture(device: &GpuDevice, n: u32, usage: GpuTextureUsage, label: &'static str) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: n,
        height: n,
        depth: n,
        format: FMT,
        dimension: GpuTextureDimension::D3,
        usage,
        label,
        mip_levels: 1,
    })
}

/// 3D-VOLUME GATHER parity: node.blur_3d blurs a Texture3D along one
/// axis. The hand fluid_blur_3d.wgsl has two entry points (blur_scalar /
/// blur_vector); the generated kernel merges them behind a runtime `mode`
/// branch and runs through the dim-aware (texture_storage_3d, @workgroup_size
/// (4,4,4), vec3 id/uv) wrapper. The input is filled on-GPU with a 3D gradient;
/// both kernels read it and their output volumes are read back (full depth via
/// copy_texture_3d_to_buffer) and compared per voxel. Dual-packed: the hand
/// uniform is {vol_res, axis, radius, _pad}, the generated is {mode, axis,
/// vol_res, radius}.
#[test]
fn generated_blur_3d_separable_matches_original() {
    let device = crate::test_device();
    let n = 32u32;
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();

    let node = registry.construct("node.blur_3d").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("blur_3d generates");
    // Structural: 3D texture types + 3D dispatch.
    assert!(
        generated.contains("texture_storage_3d<rgba16float, write>"),
        "3D storage output missing:\n{generated}"
    );
    assert!(
        generated.contains("var tex_in: texture_3d<f32>"),
        "3D sampled input missing:\n{generated}"
    );
    assert!(
        generated.contains("@compute @workgroup_size(4, 4, 4)"),
        "3D workgroup missing:\n{generated}"
    );
    let original =
        include_str!("../../../generators/shaders/fluid_blur_3d.wgsl");

    // Fill the input volume with a 3D gradient (varies along every axis).
    let input = make_3d_texture(
        &device,
        n,
        GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
        "blur3d-in",
    );
    let fill_wgsl = "\
@group(0) @binding(0) var vol: texture_storage_3d<rgba16float, write>;\n\
@compute @workgroup_size(4, 4, 4)\n\
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {\n\
let d = textureDimensions(vol);\n\
if id.x >= d.x || id.y >= d.y || id.z >= d.z { return; }\n\
let f = vec3<f32>(id) / vec3<f32>(d);\n\
textureStore(vol, vec3<i32>(id), vec4<f32>(f.x, f.y, f.z, 0.5 + 0.5 * f.x));\n\
}\n";
    {
        let pipeline = device.create_compute_pipeline(fill_wgsl, ENTRY, "blur3d-fill");
        let mut enc = device.create_encoder("blur3d-fill");
        enc.dispatch_compute(
            &pipeline,
            &[GpuBinding::Texture { binding: 0, texture: &input }],
            [n.div_ceil(4), n.div_ceil(4), n.div_ceil(4)],
            "blur3d-fill",
        );
        enc.commit_and_wait_completed();
    }

    let run = |wgsl: &str, entry: &str, param_bytes: &[u8]| -> Vec<u16> {
        let pipeline = device.create_compute_pipeline(wgsl, entry, "blur3d");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let out = make_3d_texture(
            &device,
            n,
            GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
            "blur3d-out",
        );
        let mut enc = device.create_encoder("blur3d");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: param_bytes },
                GpuBinding::Texture { binding: 1, texture: &input },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out },
            ],
            [n.div_ceil(4), n.div_ceil(4), n.div_ceil(4)],
            "blur3d",
        );
        enc.commit_and_wait_completed();
        let bytes_per_row = n * 8; // rgba16float
        let total = u64::from(bytes_per_row) * u64::from(n) * u64::from(n);
        let buf = device.create_buffer_shared(total);
        let mut renc = device.create_encoder("blur3d-readback");
        renc.copy_texture_3d_to_buffer(&out, &buf, n, n, n, bytes_per_row);
        renc.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared buffer pointer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (n * n * n * 4) as usize) };
        halves.to_vec()
    };

    // axis=0, radius=4. Hand layout {vol_res, axis, radius, _pad}.
    let mut hand = [0u8; 16];
    hand[0..4].copy_from_slice(&n.to_le_bytes()); // vol_res
    hand[8..12].copy_from_slice(&4.0f32.to_le_bytes()); // radius
    // Generated layout {mode, axis, vol_res, radius}.
    let gen_bytes = |mode: u32| -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&mode.to_le_bytes());
        b[8..12].copy_from_slice(&(n as i32).to_le_bytes()); // vol_res
        b[12..16].copy_from_slice(&4.0f32.to_le_bytes()); // radius
        b
    };

    for (mode, entry) in [(0u32, "blur_scalar"), (1u32, "blur_vector")] {
        let hand_vol = run(original, entry, &hand);
        let gen_vol = run(&generated, ENTRY, &gen_bytes(mode));
        assert_eq!(hand_vol.len(), gen_vol.len());
        let mut max_abs = 0.0f32;
        for (a, b) in hand_vol.iter().zip(gen_vol.iter()) {
            let fa = half::f16::from_bits(*a).to_f32();
            let fb = half::f16::from_bits(*b).to_f32();
            max_abs = max_abs.max((fa - fb).abs());
        }
        assert!(
            max_abs < 1e-3,
            "blur_3d mode={mode} ({entry}): generated must reproduce the hand kernel \
             (max_abs={max_abs})"
        );
    }
}

/// Fill an n³ density volume on-GPU with a 3D gradient (varies along x/y/z).
fn fill_volume_gradient(device: &GpuDevice, vol: &GpuTexture, n: u32) {
    let fill_wgsl = "\
@group(0) @binding(0) var vol: texture_storage_3d<rgba16float, write>;\n\
@compute @workgroup_size(4, 4, 4)\n\
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {\n\
let d = textureDimensions(vol);\n\
if id.x >= d.x || id.y >= d.y || id.z >= d.z { return; }\n\
let f = vec3<f32>(id) / vec3<f32>(d);\n\
textureStore(vol, vec3<i32>(id), vec4<f32>(f.x, f.y, f.z, 0.5 + 0.5 * f.x));\n\
}\n";
    let pipeline = device.create_compute_pipeline(fill_wgsl, ENTRY, "vol-fill");
    let mut enc = device.create_encoder("vol-fill");
    enc.dispatch_compute(
        &pipeline,
        &[GpuBinding::Texture { binding: 0, texture: vol }],
        [n.div_ceil(4), n.div_ceil(4), n.div_ceil(4)],
        "vol-fill",
    );
    enc.commit_and_wait_completed();
}

/// Read back a full n³ volume as f16 bits.
fn readback_volume(device: &GpuDevice, vol: &GpuTexture, n: u32) -> Vec<u16> {
    let bytes_per_row = n * 8; // rgba16float
    let total = u64::from(bytes_per_row) * u64::from(n) * u64::from(n);
    let buf = device.create_buffer_shared(total);
    let mut renc = device.create_encoder("vol-readback");
    renc.copy_texture_3d_to_buffer(vol, &buf, n, n, n, bytes_per_row);
    renc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared buffer pointer");
    let halves: &[u16] =
        unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (n * n * n * 4) as usize) };
    halves.to_vec()
}

/// 3D GatherTexel parity: node.edge_slope_3d reads its density
/// volume via integer textureLoad (6-tap central difference, toroidal wrap, NO
/// sampler). The generated kernel binds uniform(0)/tex(1)/dst(2) — identical to
/// the hand layout (GatherTexel emits no sampler) — so one uniform drives both.
/// The hand entry is `main`; the generated is `cs_main`.
#[test]
fn generated_gradient_central_diff_3d_matches_original() {
    let device = crate::test_device();
    let n = 32u32;
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();

    let node = registry.construct("node.edge_slope_3d").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("gradient_central_diff_3d generates");
    assert!(
        !generated.contains("var samp: sampler"),
        "a GatherTexel input must bind no sampler:\n{generated}"
    );
    assert!(
        generated.contains("var tex_density: texture_3d<f32>"),
        "3D sampled input missing:\n{generated}"
    );
    let original = include_str!("../../primitives/shaders/gradient_central_diff_3d.wgsl");

    let density = make_3d_texture(
        &device,
        n,
        GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
        "grad3d-in",
    );
    fill_volume_gradient(&device, &density, n);

    // {vol_res, vol_depth, _pad, _pad} — same bits for hand (u32) + generated (i32).
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&n.to_le_bytes());
    bytes[4..8].copy_from_slice(&n.to_le_bytes());

    let run = |wgsl: &str, entry: &str| -> Vec<u16> {
        let pipeline = device.create_compute_pipeline(wgsl, entry, "grad3d");
        let out = make_3d_texture(
            &device,
            n,
            GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
            "grad3d-out",
        );
        let mut enc = device.create_encoder("grad3d");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &bytes },
                GpuBinding::Texture { binding: 1, texture: &density },
                GpuBinding::Texture { binding: 2, texture: &out },
            ],
            [n.div_ceil(4), n.div_ceil(4), n.div_ceil(4)],
            "grad3d",
        );
        enc.commit_and_wait_completed();
        readback_volume(&device, &out, n)
    };

    let hand_vol = run(original, "main");
    let gen_vol = run(&generated, ENTRY);
    let mut max_abs = 0.0f32;
    for (a, b) in hand_vol.iter().zip(gen_vol.iter()) {
        let fa = half::f16::from_bits(*a).to_f32();
        let fb = half::f16::from_bits(*b).to_f32();
        max_abs = max_abs.max((fa - fb).abs());
    }
    assert!(
        max_abs < 1e-3,
        "gradient_central_diff_3d: generated must reproduce the hand kernel (max_abs={max_abs})"
    );
}

/// 3D CoincidentTexel parity (dual-packed): node.swirl_force_3d reads its
/// gradient volume at the OWN voxel (integer textureLoad, no sampler) and
/// combines curl + slope around the single CPU-normalized ref_axis. The hand uniform pads vol_res/vol_depth to 16 (48
/// bytes); the generated Params are contiguous (32 bytes) — pack each from
/// the same logical values.
#[test]
fn generated_curl_slope_force_3d_matches_original() {
    let device = crate::test_device();
    let n = 32u32;
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();

    let node = registry.construct("node.swirl_force_3d").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("curl_slope_force_3d generates");
    assert!(
        !generated.contains("var samp: sampler"),
        "a CoincidentTexel input binds no sampler:\n{generated}"
    );
    let original = include_str!("../../primitives/shaders/curl_slope_force_3d.wgsl");

    let gradient = make_3d_texture(
        &device,
        n,
        GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
        "curl3d-in",
    );
    fill_volume_gradient(&device, &gradient, n);

    // Pre-normalized ref_axis (CPU), curl=2, slope=-1.
    let raw = [0.3f32, 0.8, 0.5];
    let inv = (raw[0] * raw[0] + raw[1] * raw[1] + raw[2] * raw[2]).sqrt().recip();
    let ax = [raw[0] * inv, raw[1] * inv, raw[2] * inv];
    let (curl, slope) = (2.0f32, -1.0f32);

    // Hand layout: {vol_res, vol_depth, _pad, _pad, curl, slope, ax, ay, az, _pad×3} = 48B.
    let mut hand = vec![0u8; 48];
    hand[0..4].copy_from_slice(&n.to_le_bytes());
    hand[4..8].copy_from_slice(&n.to_le_bytes());
    hand[16..20].copy_from_slice(&curl.to_le_bytes());
    hand[20..24].copy_from_slice(&slope.to_le_bytes());
    hand[24..28].copy_from_slice(&ax[0].to_le_bytes());
    hand[28..32].copy_from_slice(&ax[1].to_le_bytes());
    hand[32..36].copy_from_slice(&ax[2].to_le_bytes());
    // Generated layout: {vol_res, vol_depth, curl, slope, ax, ay, az, _pad} = 32B.
    let mut gen_bytes = vec![0u8; 32];
    gen_bytes[0..4].copy_from_slice(&n.to_le_bytes());
    gen_bytes[4..8].copy_from_slice(&n.to_le_bytes());
    gen_bytes[8..12].copy_from_slice(&curl.to_le_bytes());
    gen_bytes[12..16].copy_from_slice(&slope.to_le_bytes());
    gen_bytes[16..20].copy_from_slice(&ax[0].to_le_bytes());
    gen_bytes[20..24].copy_from_slice(&ax[1].to_le_bytes());
    gen_bytes[24..28].copy_from_slice(&ax[2].to_le_bytes());

    let run = |wgsl: &str, entry: &str, bytes: &[u8]| -> Vec<u16> {
        let pipeline = device.create_compute_pipeline(wgsl, entry, "curl3d");
        let out = make_3d_texture(
            &device,
            n,
            GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
            "curl3d-out",
        );
        let mut enc = device.create_encoder("curl3d");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytes },
                GpuBinding::Texture { binding: 1, texture: &gradient },
                GpuBinding::Texture { binding: 2, texture: &out },
            ],
            [n.div_ceil(4), n.div_ceil(4), n.div_ceil(4)],
            "curl3d",
        );
        enc.commit_and_wait_completed();
        readback_volume(&device, &out, n)
    };

    let hand_vol = run(original, "main", &hand);
    let gen_vol = run(&generated, ENTRY, &gen_bytes);
    let mut max_abs = 0.0f32;
    for (a, b) in hand_vol.iter().zip(gen_vol.iter()) {
        let fa = half::f16::from_bits(*a).to_f32();
        let fb = half::f16::from_bits(*b).to_f32();
        max_abs = max_abs.max((fa - fb).abs());
    }
    assert!(
        max_abs < 1e-3,
        "curl_slope_force_3d: generated must reproduce the hand kernel (max_abs={max_abs})"
    );
}

/// Vector-op parity: length_vec2 + normalize_vec2 are paramless Pointwise
/// (tex0/samp1/dst2); rotate_vec2_by_angle is Pointwise — the hand shader still
/// reads CPU-precomputed cos_a/sin_a while the generated body computes them from
/// `angle`, so they're dual-packed and compared at f16 precision (the output is
/// f16, so the sub-f16 GPU-vs-CPU trig difference is below the store).
#[test]
fn generated_vector_op_atoms_match_originals() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let input = gradient(&device, w, h); // .rg = (x/w, y/h)
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let shaders_dir =
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
    let differ = TextureDiff::new(&device);

    // Paramless vector ops.
    for (type_id, shader) in [
        ("node.vector_length", "length_vec2.wgsl"),
        ("node.normalize", "normalize_vec2.wgsl"),
    ] {
        let node = registry.construct(type_id).unwrap();
        let generated = generate_standalone(&StandaloneKernelSpec {
            fusion_kind: node.fusion_kind(),
            body: node.wgsl_body().unwrap(),
            inputs: node.inputs(),
            params: node.parameters(),
            input_access: node.input_access(),
            derived_uniforms: node.derived_uniforms(),
            outputs: node.outputs(),
            stencil_fetch: false,
            includes: &[],
        })
        .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
        let original = std::fs::read_to_string(format!("{shaders_dir}/{shader}"))
            .unwrap_or_else(|e| panic!("read {shader}: {e}"));
        let from_original = dispatch_paramless_pointwise(&device, &original, &input);
        let from_generated = dispatch_paramless_pointwise(&device, &generated, &input);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "{type_id}: generated must reproduce {shader} (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    // rotate_vec2_by_angle: dual-packed (hand cos/sin vs generated angle).
    let node = registry.construct("node.rotate_vector").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("rotate_vec2 generates");
    let original = std::fs::read_to_string(format!("{shaders_dir}/rotate_vec2_by_angle.wgsl"))
        .expect("read rotate_vec2_by_angle.wgsl");
    let angle = 0.7f32;
    let hand_bytes = pack_f32(&[angle.cos(), angle.sin()]); // hand reads cos_a/sin_a
    let gen_bytes = pack_f32(&[angle]); // generated reads angle
    let from_original = dispatch_pointwise(&device, &original, &input, &hand_bytes);
    let from_generated = dispatch_pointwise(&device, &generated, &input, &gen_bytes);
    // f16-level tolerance: the GPU-vs-CPU trig difference is sub-f16.
    let r = differ.compare(
        &device,
        &from_original.texture,
        &from_generated.texture,
        3e-3,
        3e-3,
    );
    assert_eq!(
        r.over_count, 0,
        "rotate_vec2_by_angle: generated must reproduce the hand kernel at f16 \
         (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
}

/// Single-input CoincidentTexel parity: node.hash_field_by_seed reads `field`
/// at the OWN texel via integer textureLoad (no sampler) and hashes it with a
/// seed. Binding layout uniform(0)/tex(1)/dst(2) — no sampler — for both the
/// hand and generated kernel, so one payload drives both. hash2/hash1 use GPU
/// sin (matching the hand), so it's bit-exact.
#[test]
fn generated_hash_field_by_seed_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let input = gradient(&device, w, h); // .rg = a value field
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let differ = TextureDiff::new(&device);

    let node = registry.construct("node.hash_field_by_seed").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("hash_field_by_seed generates");
    assert!(
        !generated.contains("var samp: sampler"),
        "a CoincidentTexel input binds no sampler:\n{generated}"
    );
    let original = include_str!("../../primitives/shaders/hash_field_by_seed.wgsl");

    // {seed, seed_x, seed_y, mode} = 16B; mode=0 (Hash2).
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&3.0f32.to_le_bytes());
    bytes[4..8].copy_from_slice(&1.73f32.to_le_bytes());
    bytes[8..12].copy_from_slice(&2.91f32.to_le_bytes());

    let run = |wgsl: &str| -> RenderTarget {
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "hashseed");
        let out = RenderTarget::new(&device, w, h, FMT, "hashseed-out");
        let mut enc = device.create_encoder("hashseed");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &bytes },
                GpuBinding::Texture { binding: 1, texture: &input },
                GpuBinding::Texture { binding: 2, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "hashseed",
        );
        enc.commit_and_wait_completed();
        out
    };

    let from_original = run(original);
    let from_generated = run(&generated);
    let r = differ.compare(
        &device,
        &from_original.texture,
        &from_generated.texture,
        1e-5,
        1e-5,
    );
    assert_eq!(
        r.over_count, 0,
        "generated hash_field_by_seed must reproduce the hand kernel (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
}

/// OPTIONAL-INPUT use-flag parity: node.pack_rgba combines 4 optional
/// coincident inputs (r/g/b/a) into RGBA, falling back to default_* when an
/// input is unwired (use_*==0). The codegen injects a use_<name> flag per
/// optional input. Dual-packed: the hand uniform is {use_r..use_a, defaults[4]}
/// (use first), the generated is {default_r..a, use_r..a} (params then injected
/// flags). use=[1,0,1,1] exercises both the wired-read and default-fallback
/// paths. Binding layout uniform(0)/r(1)/g(2)/b(3)/a(4)/samp(5)/dst(6) for both.
#[test]
fn generated_pack_channels_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let ga = gradient(&device, w, h);
    let gb = gradient_b(&device, w, h);
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let differ = TextureDiff::new(&device);

    let node = registry.construct("node.pack_rgba").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("pack_channels generates");
    assert!(
        generated.contains("use_r: u32"),
        "optional-input use flag missing:\n{generated}"
    );
    let original = include_str!("../../primitives/shaders/pack_channels.wgsl");

    let use_flags = [1u32, 0, 1, 1]; // g unwired → falls back to default_g
    let defaults = [0.1f32, 0.5, 0.2, 1.0];
    // Hand: {use_r..use_a, defaults[4]}.
    let mut hand = Vec::new();
    for u in use_flags {
        hand.extend_from_slice(&u.to_le_bytes());
    }
    for d in defaults {
        hand.extend_from_slice(&d.to_le_bytes());
    }
    // Generated: {default_r..a, use_r..a}.
    let mut gen_bytes = Vec::new();
    for d in defaults {
        gen_bytes.extend_from_slice(&d.to_le_bytes());
    }
    for u in use_flags {
        gen_bytes.extend_from_slice(&u.to_le_bytes());
    }

    let inputs = [&ga, &gb, &ga, &gb];
    let from_original = dispatch_coincident_n(&device, original, &inputs, &hand);
    let from_generated = dispatch_coincident_n(&device, &generated, &inputs, &gen_bytes);
    let r = differ.compare(
        &device,
        &from_original.texture,
        &from_generated.texture,
        1e-5,
        1e-5,
    );
    assert_eq!(
        r.over_count, 0,
        "generated pack_channels must reproduce the hand kernel (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
}

/// trig_texture parity: 3 coincident inputs (in + optional freq_tex/phase_tex)
/// with injected use-flags. The uniform layout already matches (params then
/// flags), but the HAND shader binds its output at 3 (before the optional
/// textures) while the generated kernel is regular (textures/sampler/output),
/// so the hand needs a custom dispatch and the generated uses
/// dispatch_coincident_n. use_freq_tex=1 (per-pixel freq) + use_phase_tex=0
/// (scalar phase) exercises both paths; GPU sin matches bit-exact.
#[test]
fn generated_trig_texture_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let in_tex = gradient(&device, w, h);
    let freq_t = gradient_b(&device, w, h);
    let phase_t = gradient(&device, w, h); // unused (use_phase_tex=0)
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let differ = TextureDiff::new(&device);

    let node = registry.construct("node.sine_cosine").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("trig_texture generates");
    assert!(
        generated.contains("use_freq_tex: u32") && generated.contains("use_phase_tex: u32"),
        "optional-input use flags missing:\n{generated}"
    );
    let original = include_str!("../../primitives/shaders/trig_texture.wgsl");

    // {freq, phase, mode, use_freq_tex, use_phase_tex, _pad×3} = 32B.
    let mut bytes = vec![0u8; 32];
    bytes[0..4].copy_from_slice(&2.0f32.to_le_bytes()); // freq
    bytes[4..8].copy_from_slice(&0.5f32.to_le_bytes()); // phase
    // mode = 0 (Sin)
    bytes[12..16].copy_from_slice(&1u32.to_le_bytes()); // use_freq_tex = per-pixel
    // use_phase_tex = 0 (scalar)

    let from_generated =
        dispatch_coincident_n(&device, &generated, &[&in_tex, &freq_t, &phase_t], &bytes);
    // Hand layout: uniform(0), in(1), sampler(2), out(3), freq_tex(4), phase_tex(5).
    let hand_out = {
        let pipeline = device.create_compute_pipeline(original, ENTRY, "trig-hand");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let out = RenderTarget::new(&device, w, h, FMT, "trig-hand-out");
        let mut enc = device.create_encoder("trig-hand");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &bytes },
                GpuBinding::Texture { binding: 1, texture: &in_tex },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
                GpuBinding::Texture { binding: 4, texture: &freq_t },
                GpuBinding::Texture { binding: 5, texture: &phase_t },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "trig-hand",
        );
        enc.commit_and_wait_completed();
        out
    };
    let r = differ.compare(
        &device,
        &hand_out.texture,
        &from_generated.texture,
        1e-5,
        1e-5,
    );
    assert_eq!(
        r.over_count, 0,
        "generated trig_texture must reproduce the hand kernel (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
}

/// TIME-PARAM + MULTI-OUTPUT SOURCE parity: node.block_displace_field emits
/// `offset` + `hash` from a per-block hash animated by `time` (now a backing
/// param so the generated body reads it from the uniform). Dual-packed: the
/// hand uniform is {amount, block_size, speed, time} (16B), the generated adds
/// the multi-output write flags (32B). Both bind uniform(0)/out(1)/out(2); diff
/// each output. bdf_hash2 uses GPU sin, so it's bit-exact.
#[test]
fn generated_block_displace_field_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let differ = TextureDiff::new(&device);

    let node = registry.construct("node.block_displace_field").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("block_displace_field generates");
    assert!(
        generated.contains("struct BodyOutputs") && generated.contains("write_offset: u32"),
        "multi-output struct/flags missing:\n{generated}"
    );
    let original = include_str!("../../primitives/shaders/block_displace_field.wgsl");

    let (amount, block_size, speed, time) = (0.8f32, 16.0f32, 2.0f32, 1.0f32);
    let mut hand = vec![0u8; 16];
    hand[0..4].copy_from_slice(&amount.to_le_bytes());
    hand[4..8].copy_from_slice(&block_size.to_le_bytes());
    hand[8..12].copy_from_slice(&speed.to_le_bytes());
    hand[12..16].copy_from_slice(&time.to_le_bytes());
    let mut gen_bytes = vec![0u8; 32];
    gen_bytes[0..16].copy_from_slice(&hand);
    gen_bytes[16..20].copy_from_slice(&1u32.to_le_bytes()); // write_offset
    gen_bytes[20..24].copy_from_slice(&1u32.to_le_bytes()); // write_hash

    let (h_off, h_hash) = dispatch_two_output_source(&device, original, &hand, w, h);
    let (g_off, g_hash) = dispatch_two_output_source(&device, &generated, &gen_bytes, w, h);
    let r_off = differ.compare(&device, &h_off.texture, &g_off.texture, 1e-5, 1e-5);
    assert_eq!(
        r_off.over_count, 0,
        "block_displace `offset`: generated must reproduce the hand kernel (max_abs={})",
        r_off.max_abs
    );
    let r_hash = differ.compare(&device, &h_hash.texture, &g_hash.texture, 1e-5, 1e-5);
    assert_eq!(
        r_hash.over_count, 0,
        "block_displace `hash`: generated must reproduce the hand kernel (max_abs={})",
        r_hash.max_abs
    );
}

/// lic_integrate parity: 2-input gather (source + velocity both walked along
/// the streamline). `steps` is an Int param (i32), so it can't go through
/// pack_f32 (f32 bits would mis-read as int) — hand-pack steps=16 (i32) + dt=2
/// (f32). Both kernels read the same bits; dispatch_coincident binds
/// uniform(0)/source(1)/velocity(2)/samp(3)/dst(4) for both.
#[test]
fn generated_lic_integrate_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let ga = gradient(&device, w, h); // source
    let gb = gradient_b(&device, w, h); // velocity (.rg)
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let differ = TextureDiff::new(&device);

    let node = registry.construct("node.flow_lines").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("lic_integrate generates");
    let original = include_str!("../../primitives/shaders/lic_integrate.wgsl");

    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&16i32.to_le_bytes()); // steps
    bytes[4..8].copy_from_slice(&2.0f32.to_le_bytes()); // dt

    let from_original = dispatch_coincident(&device, original, &ga, &gb, &bytes);
    let from_generated = dispatch_coincident(&device, &generated, &ga, &gb, &bytes);
    let r = differ.compare(
        &device,
        &from_original.texture,
        &from_generated.texture,
        1e-5,
        1e-5,
    );
    assert_eq!(
        r.over_count, 0,
        "generated lic_integrate must reproduce the hand kernel (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
}

/// MIXED-DIM parity: node.slice_volume gathers a Texture3D `in` at a slice
/// coord and writes a Texture2D `out`. The generated kernel must bind tex_in as
/// texture_3d (per-input dim) while the wrapper stays 2D (output dim). Fill a
/// 32^3 volume, sample it into a 2D output; both kernels share uniform(0)/
/// volume(1)/samp(2)/dst(3) and the same payload.
#[test]
fn generated_sample_volume_2d_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let n = 32u32;
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let differ = TextureDiff::new(&device);

    let node = registry.construct("node.slice_volume").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("sample_volume_2d generates");
    assert!(
        generated.contains("var tex_in: texture_3d<f32>"),
        "3D input binding missing:\n{generated}"
    );
    assert!(
        generated.contains("var dst: texture_storage_2d<rgba16float, write>"),
        "2D output binding missing:\n{generated}"
    );
    let original = include_str!("../../primitives/shaders/sample_volume_2d.wgsl");

    let volume = make_3d_texture(
        &device,
        n,
        GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
        "svol-in",
    );
    fill_volume_gradient(&device, &volume, n);

    // {slice_z, uv_scale, center_x, center_y}.
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&0.5f32.to_le_bytes()); // slice_z
    bytes[4..8].copy_from_slice(&1.0f32.to_le_bytes()); // uv_scale

    let run = |wgsl: &str| -> RenderTarget {
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "svol");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let out = RenderTarget::new(&device, w, h, FMT, "svol-out");
        let mut enc = device.create_encoder("svol");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &bytes },
                GpuBinding::Texture { binding: 1, texture: &volume },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "svol",
        );
        enc.commit_and_wait_completed();
        out
    };

    let from_original = run(original);
    let from_generated = run(&generated);
    let r = differ.compare(
        &device,
        &from_original.texture,
        &from_generated.texture,
        1e-5,
        1e-5,
    );
    assert_eq!(
        r.over_count, 0,
        "generated sample_volume_2d must reproduce the hand kernel (max_abs={}, max_rel={})",
        r.max_abs, r.max_rel
    );
}

/// Dispatch a two-output SOURCE kernel: uniform(0), dst_a(1), dst_b(2). Both
/// outputs get their own texture (no aliasing) so each can be diffed.
fn dispatch_two_output_source(
    device: &GpuDevice,
    wgsl: &str,
    param_bytes: &[u8],
    w: u32,
    h: u32,
) -> (RenderTarget, RenderTarget) {
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-multi-out");
    let a = RenderTarget::new(device, w, h, FMT, "codegen-out-a");
    let b = RenderTarget::new(device, w, h, FMT, "codegen-out-b");
    let mut enc = device.create_encoder("codegen-multi-out");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes { binding: 0, data: param_bytes },
            GpuBinding::Texture { binding: 1, texture: &a.texture },
            GpuBinding::Texture { binding: 2, texture: &b.texture },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-multi-out",
    );
    enc.commit_and_wait_completed();
    (a, b)
}

/// MULTI-OUTPUT SOURCE parity: node.voronoi_2d writes two storage textures
/// (`out` = F1/F2/edge/cell_hash, `cell_id` = the F1-winning cell coordinate).
/// The generated kernel declares both as dst_<port>, the body returns a
/// BodyOutputs struct, and each store is gated on an injected write_<port>
/// flag. Those flags land at the same offsets as the hand uniform's
/// write_out/write_cell_id, so the generated Params layout equals
/// VoronoiUniforms exactly — one payload drives both kernels. Diff each output
/// independently (both write flags on, distinct textures, no aliasing).
#[test]
fn generated_voronoi_2d_matches_original() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let differ = TextureDiff::new(&device);

    let node = registry.construct("node.voronoi_2d").unwrap();
    let generated = generate_standalone(&StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body: node.wgsl_body().unwrap(),
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: false,
        includes: &[],
    })
    .expect("voronoi_2d generates");
    // Structural: two storage outputs, a struct return, and per-output gates.
    assert!(
        generated.contains("var dst_out: texture_storage_2d<rgba16float, write>"),
        "dst_out binding missing:\n{generated}"
    );
    assert!(
        generated.contains("var dst_cell_id: texture_storage_2d<rgba16float, write>"),
        "dst_cell_id binding missing:\n{generated}"
    );
    assert!(generated.contains("struct BodyOutputs"), "struct missing:\n{generated}");
    assert!(generated.contains("write_out: u32"), "write_out flag missing:\n{generated}");
    assert!(
        generated.contains("write_cell_id: u32"),
        "write_cell_id flag missing:\n{generated}"
    );
    let original = include_str!("../../primitives/shaders/voronoi_2d.wgsl");

    // {scale, offset_x, offset_y, jitter, out_scale, write_out, write_cell_id, _pad}.
    let mut bytes = vec![0u8; 32];
    bytes[0..4].copy_from_slice(&8.0f32.to_le_bytes()); // scale
    bytes[12..16].copy_from_slice(&1.0f32.to_le_bytes()); // jitter (full random)
    bytes[16..20].copy_from_slice(&1.0f32.to_le_bytes()); // out_scale
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes()); // write_out
    bytes[24..28].copy_from_slice(&1u32.to_le_bytes()); // write_cell_id

    let (h_out, h_cell) = dispatch_two_output_source(&device, original, &bytes, w, h);
    let (g_out, g_cell) = dispatch_two_output_source(&device, &generated, &bytes, w, h);
    let r_out = differ.compare(&device, &h_out.texture, &g_out.texture, 1e-5, 1e-5);
    assert_eq!(
        r_out.over_count, 0,
        "voronoi `out`: generated must reproduce voronoi_2d.wgsl (max_abs={}, max_rel={})",
        r_out.max_abs, r_out.max_rel
    );
    let r_cell = differ.compare(&device, &h_cell.texture, &g_cell.texture, 1e-5, 1e-5);
    assert_eq!(
        r_cell.over_count, 0,
        "voronoi `cell_id`: generated must reproduce voronoi_2d.wgsl (max_abs={}, max_rel={})",
        r_cell.max_abs, r_cell.max_rel
    );
}

/// Dispatch a SOURCE (generator) kernel: [uniform(0)], output. No texture
/// inputs, no sampler — a paramless source binds only its output at binding 0.
fn dispatch_source(
    device: &GpuDevice,
    wgsl: &str,
    param_bytes: Option<&[u8]>,
    w: u32,
    h: u32,
) -> RenderTarget {
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-source");
    let out = RenderTarget::new(device, w, h, FMT, "codegen-out-source");
    let mut bindings: Vec<GpuBinding> = Vec::new();
    let mut next = 0u32;
    if let Some(bytes) = param_bytes {
        bindings.push(GpuBinding::Bytes { binding: 0, data: bytes });
        next = 1;
    }
    bindings.push(GpuBinding::Texture { binding: next, texture: &out.texture });
    let mut enc = device.create_encoder("codegen-source");
    enc.dispatch_compute(
        &pipeline,
        &bindings,
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-source",
    );
    enc.commit_and_wait_completed();
    out
}

/// Source (generator) parity (overnight sweep): a 0-input atom produces from
/// uv/dims/params, no colour input. checkerboard (params → uniform0/out1) and
/// the paramless uv_field (out0 only — exercises the no-uniform Source path)
/// both reproduce their hand shaders bit-for-bit.
#[test]
fn generated_source_atoms_match_originals() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let shaders_dir =
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
    let differ = TextureDiff::new(&device);

    // checkerboard PARAMS: [scale, offset_x, offset_y].
    let checker_bytes = pack_f32(&[8.0, 0.0, 0.0]);
    // centered_uv PARAMS: [cx, cy, scale_x, scale_y] (16-byte uniform).
    let centered_bytes = pack_f32(&[0.5, 0.5, 2.0, 2.0]);
    // linear_gradient PARAMS: [cx, cy, rotation, softness]; its hand uniform
    // is padded to 32 bytes, so pack 32 (satisfies the 32-byte hand decl AND
    // the 16-byte generated decl — a larger buffer binds fine to a smaller
    // uniform).
    let lg_bytes = pack_f32(&[0.5, 0.5, 0.785, 0.3, 0.0, 0.0, 0.0, 0.0]);
    // distance_to_point [cx,cy,scale,scale_x,scale_y] (32B hand uniform).
    let dist_bytes = pack_f32(&[0.3, 0.7, 1.5, 2.0, 1.0, 0.0, 0.0, 0.0]);
    // polar_field [cx,cy] (16B).
    let polar_bytes = pack_f32(&[0.3, 0.7]);
    // box_mask [cx,cy,half_width,half_height,rotation,softness] (32B).
    let box_bytes = pack_f32(&[0.5, 0.5, 0.25, 0.25, 0.785, 0.1, 0.0, 0.0]);
    // mirror_fold_uv [mode] (Enum -> u32), packed by hand to 16B.
    let mut mirror_bytes = vec![0u8; 16];
    mirror_bytes[0..4].copy_from_slice(&8u32.to_le_bytes()); // FoldBoth
    // radial_fold_uv [segments, cx, cy] (16B).
    let radial_bytes = pack_f32(&[6.0, 0.5, 0.5]);
    // ellipse_mask [cx,cy,radius_x,radius_y,rotation,softness] (32B).
    let ellipse_bytes = pack_f32(&[0.5, 0.5, 0.3, 0.2, 0.785, 0.1, 0.0, 0.0]);
    // dither_pattern [algorithm] (Enum -> u32), 16B; 0 = Bayer (the LUT path).
    let mut dither_pat_bytes = vec![0u8; 16];
    dither_pat_bytes[0..4].copy_from_slice(&0u32.to_le_bytes());
    // simplex_field_2d [scale_x, scale_y, offset_x, offset_y, z, output_channel
    // (u32)], 32B — packed by hand for the mid-struct u32.
    let mut simplex_bytes = vec![0u8; 32];
    simplex_bytes[0..4].copy_from_slice(&3.0f32.to_le_bytes());
    simplex_bytes[4..8].copy_from_slice(&3.0f32.to_le_bytes());
    simplex_bytes[16..20].copy_from_slice(&0.5f32.to_le_bytes()); // z
    // offset_x/y = 0, output_channel = 0 (R) — already zeroed.
    // node.noise [type(u32), scale, offset_x, offset_y, octaves(i32),
    // lacunarity, persistence], 32B — one case per branch to exercise every
    // helper (Perlin fBM / Simplex snoise / Random hash).
    let noise_case = |ty: i32, scale: f32, octaves: i32| {
        let mut b = vec![0u8; 32];
        b[0..4].copy_from_slice(&ty.to_le_bytes());
        b[4..8].copy_from_slice(&scale.to_le_bytes());
        b[16..20].copy_from_slice(&octaves.to_le_bytes());
        b[20..24].copy_from_slice(&2.0f32.to_le_bytes()); // lacunarity
        b[24..28].copy_from_slice(&0.5f32.to_le_bytes()); // persistence
        b
    };
    let noise_perlin = noise_case(0, 4.0, 3); // Perlin + fBM (3 octaves)
    let noise_simplex = noise_case(1, 4.0, 1); // Simplex
    let noise_random = noise_case(2, 8.0, 1); // Random hash
    // radial_offset_field [mode (u32), angle, falloff], 16B; mode=0 (Radial).
    let mut radial_offset_bytes = vec![0u8; 16];
    radial_offset_bytes[0..4].copy_from_slice(&0u32.to_le_bytes()); // mode = Radial
    radial_offset_bytes[8..12].copy_from_slice(&0.5f32.to_le_bytes()); // falloff
    // uv_strip_clamp [width, mode (u32)], 16B; mode=2 (Both).
    let mut strip_bytes = vec![0u8; 16];
    strip_bytes[0..4].copy_from_slice(&0.5f32.to_le_bytes()); // width
    strip_bytes[4..8].copy_from_slice(&2u32.to_le_bytes()); // mode = Both
    // scanline_jitter_field [amount, scanline, speed, time], 16B. GPU sin →
    // bit-exact; time is a backing param packed by run().
    let scanline_bytes = pack_f32(&[0.8, 0.3, 2.0, 1.0]);
    // flow_field_noise [time, z_scale, warp_scale, resolution], 16B. warp=0.5
    // exercises the domain warp; resolution slot ignored by the body.
    let flow_bytes = pack_f32(&[1.0, 0.01, 0.5, 0.0]);
    let cases: &[(&str, &str, Option<&[u8]>)] = &[
        ("node.checkerboard", "checkerboard.wgsl", Some(checker_bytes.as_slice())),
        ("node.uv_field", "uv_field.wgsl", None),
        ("node.centered_uv", "centered_uv.wgsl", Some(centered_bytes.as_slice())),
        ("node.linear_gradient", "linear_gradient.wgsl", Some(lg_bytes.as_slice())),
        ("node.distance_to_point", "distance_to_point.wgsl", Some(dist_bytes.as_slice())),
        ("node.polar_field", "polar_field.wgsl", Some(polar_bytes.as_slice())),
        ("node.rectangle_mask", "box_mask.wgsl", Some(box_bytes.as_slice())),
        ("node.mirror", "mirror_fold_uv.wgsl", Some(mirror_bytes.as_slice())),
        ("node.kaleidoscope", "radial_fold_uv.wgsl", Some(radial_bytes.as_slice())),
        ("node.circle_mask", "ellipse_mask.wgsl", Some(ellipse_bytes.as_slice())),
        ("node.dither_pattern", "dither_pattern.wgsl", Some(dither_pat_bytes.as_slice())),
        ("node.simplex_field_2d", "simplex_field_2d.wgsl", Some(simplex_bytes.as_slice())),
        ("node.noise", "noise.wgsl", Some(noise_perlin.as_slice())),
        ("node.noise", "noise.wgsl", Some(noise_simplex.as_slice())),
        ("node.noise", "noise.wgsl", Some(noise_random.as_slice())),
        ("node.radial_offset_field", "radial_offset_field.wgsl", Some(radial_offset_bytes.as_slice())),
        ("node.edge_stretch", "uv_strip_clamp.wgsl", Some(strip_bytes.as_slice())),
        ("node.scanline_jitter_field", "scanline_jitter_field.wgsl", Some(scanline_bytes.as_slice())),
        ("node.flow_field_noise", "flow_field_noise.wgsl", Some(flow_bytes.as_slice())),
    ];
    for (type_id, shader_file, bytes) in cases {
        let node = registry.construct(type_id).unwrap();
        let generated = generate_standalone(&StandaloneKernelSpec {
            fusion_kind: node.fusion_kind(),
            body: node.wgsl_body().unwrap(),
            inputs: node.inputs(),
            params: node.parameters(),
            input_access: node.input_access(),
            derived_uniforms: node.derived_uniforms(),
            outputs: node.outputs(),
            stencil_fetch: false,
            includes: &[],
        })
        .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
        let original = std::fs::read_to_string(format!("{shaders_dir}/{shader_file}"))
            .unwrap_or_else(|e| panic!("read {shader_file}: {e}"));
        let from_original = dispatch_source(&device, &original, *bytes, w, h);
        let from_generated = dispatch_source(&device, &generated, *bytes, w, h);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "{type_id}: generated must reproduce {shader_file} (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }
}
