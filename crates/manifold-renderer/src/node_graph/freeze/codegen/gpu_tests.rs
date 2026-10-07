use crate::node_graph::effect_node::NodeInstanceId;

use crate::node_graph::freeze::codegen::generate_fused;
use crate::node_graph::freeze::codegen::{generate_standalone, StandaloneKernelSpec};
use crate::node_graph::freeze::codegen::{FusionRegion, InputSource, RegionNode, ENTRY};
use crate::node_graph::effect_node::EffectNode;
use crate::node_graph::freeze::TextureDiff;
use crate::node_graph::primitives::Gain;
use manifold_gpu::GpuBinding;


use crate::testkit::codegen_support::*;
/// Determinism (design section 12.3): the generator emits byte-identical WGSL
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
    output_capacity: None,
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
    output_capacity: None,
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

/// BUG-agfh (Codegen: buffer atom with several outputs, one atomic): the
/// generated standalone kernel for an aliased pointwise particle output with an
/// atomic `i32` side output, dispatched on one buffer bound to both `points`
/// and `points_out` (the production aliased shape), matches the CPU reference
/// exactly — every particle word, and every fixed-point momentum sum.
///
/// Inputs are dyadic (velocities k/64, drag 1/4, scale 64), so every product is
/// exact in f32 and the fixed-point words carry fractional parts of both signs:
/// the `i32(...)` truncation toward zero is checked too. Integer `atomicAdd` is
/// order-independent, so the sums are exact whatever order the threads land in.
#[test]
fn generated_atomic_side_output_matches_cpu_reference() {
    use crate::particles::Particle;
    use crate::testkit::test_multi_output_atomic_fixture::{
        cpu_reference, TestMultiOutputAtomic, Uniforms, MOMENTUM_WORDS,
    };

    let device = crate::test_device();
    let wgsl = super::standalone_for_spec::<TestMultiOutputAtomic>()
        .expect("multi-output atomic shape generates");
    let pipeline = device.create_compute_pipeline(&wgsl, ENTRY, "agfh-multi-output-atomic");

    // Not a multiple of the 256 workgroup, so the dispatch guard is exercised.
    const N: usize = 1000;
    let points: Vec<Particle> = (0..N)
        .map(|i| {
            let mut p = <Particle as bytemuck::Zeroable>::zeroed();
            p.position = [i as f32 / N as f32, 0.5, 0.0];
            p.velocity = [
                ((i % 37) as f32 - 18.0) / 64.0,
                ((i * 7 % 29) as f32 - 14.0) / 64.0,
                ((i % 11) as f32 - 5.0) / 64.0,
            ];
            p.life = if i % 5 == 3 { 0.0 } else { 1.0 };
            p.age = i as f32;
            p.color = [1.0, 0.5, 0.25, 1.0];
            p
        })
        .collect();
    let (drag, scale) = (0.25f32, 64.0f32);
    let (expected_points, expected_momentum) = cpu_reference(&points, drag, scale);
    assert!(
        expected_momentum.iter().any(|&m| m < 0) && expected_momentum.iter().any(|&m| m > 0),
        "fixture must produce sums of both signs (non-vacuous)"
    );

    let point_bytes: &[u8] = bytemuck::cast_slice(&points);
    let points_buf = device.create_buffer_shared(point_bytes.len() as u64);
    let momentum_buf = device.create_buffer_shared(u64::from(MOMENTUM_WORDS) * 4);
    unsafe {
        std::ptr::copy_nonoverlapping(
            point_bytes.as_ptr(),
            points_buf.mapped_ptr().expect("shared points buffer"),
            point_bytes.len(),
        );
        std::ptr::write_bytes(
            momentum_buf.mapped_ptr().expect("shared momentum buffer"),
            0,
            MOMENTUM_WORDS as usize * 4,
        );
    }

    let uniforms = Uniforms { drag, fixed_point_scale: scale, dispatch_count: N as u32, _pad0: 0 };
    let mut enc = device.create_encoder("agfh-multi-output-atomic");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &points_buf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &points_buf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &momentum_buf, offset: 0 },
        ],
        [(N as u32).div_ceil(256), 1, 1],
        "agfh-multi-output-atomic",
    );
    enc.commit_and_wait_completed();

    let got_points: Vec<Particle> = unsafe {
        std::slice::from_raw_parts(points_buf.mapped_ptr().unwrap().cast::<Particle>(), N)
    }
    .to_vec();
    let got_momentum: Vec<i32> = unsafe {
        std::slice::from_raw_parts(
            momentum_buf.mapped_ptr().unwrap().cast::<i32>(),
            MOMENTUM_WORDS as usize,
        )
    }
    .to_vec();

    assert_eq!(got_momentum, expected_momentum, "atomic fixed-point sums must match exactly");
    // Field bits only: std430 padding words carry no data and the kernel's
    // struct store need not preserve them.
    let bits = |p: &Particle| -> Vec<u32> {
        p.position
            .iter()
            .chain(&p.velocity)
            .chain([&p.life, &p.age])
            .chain(&p.color)
            .map(|f| f.to_bits())
            .collect()
    };
    let first_diff = got_points
        .iter()
        .zip(&expected_points)
        .position(|(g, w)| bits(g) != bits(w));
    assert_eq!(
        first_diff,
        None,
        "aliased plain output must match the CPU reference bit for bit: got {:?}, want {:?}",
        first_diff.map(|i| bits(&got_points[i])),
        first_diff.map(|i| bits(&expected_points[i]))
    );
}
