//! First end-to-end fusion proof (design build-sequence: the thin vertical
//! slice before codegen). Renders a real unfused `Source -> Gain -> Gain ->
//! FinalOutput` chain through the production executor, renders a hand-fused
//! single-kernel equivalent, and checks them against the [`TextureDiff`]
//! oracle. Two claims:
//!
//! 1. A *correct* fusion clears the oracle — even though it is NOT bit-exact
//!    with the unfused chain (the chain rounds to f16 between every pass; the
//!    fused kernel keeps f32 in registers and rounds once). The two-sided
//!    tolerance absorbs that f16-accumulation drift, which is the whole reason
//!    it is two-sided (design §11.D).
//! 2. A *wrong* fusion fails the oracle. Without this, "the oracle passed"
//!    would be meaningless — so we deliberately mis-fuse (product off by 1.5×)
//!    and assert it is flagged.
//!
//! This makes the diff core earn its keep on a genuine fusion, not a planted
//! perturbation, and gives the eventual codegen a known-good reference target.

use super::TextureDiff;
use super::markers::Marker;
use super::reference::{ColorGradeParams, colorgrade_pipeline, dispatch_fused_colorgrade};
use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId, compile};
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitives::Gain;
use crate::node_graph::{
    EffectGraphDefExt, Executor, FinalOutput, FrameTime, MetalBackend, NodeInstanceId,
    PrimitiveRegistry, Source,
};
use crate::render_target::RenderTarget;
use half::f16;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, Seconds};
use manifold_gpu::{
    GpuBinding, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};

const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

/// The §7.4 "out-of-loop ≈ulp" precision-contract tolerance (freeze §7,
/// `docs/FREEZE_COMPILER_MAP.md`): the shared per-texel (abs, rel) bound for
/// every out-of-loop texture-region fusion proof — f16-round-trip drift
/// through pointwise/gather/warp chains, amplified by discontinuities
/// (hue wrap, smoothstep edges, PBR specular). Precedent named in the doc:
/// the quarter-res oracle's 1e-2 band; also covers the known-shipping
/// MetallicGlass noise-chain ~1 ulp → max_abs≈1.8 specular-shimmer instance.
/// This is the texel-level bound only — each proof still tunes its own
/// `passes(max_over_fraction)` / `over_count` budget for that kernel's
/// discontinuity profile; that fraction is not part of this contract.
const OUT_OF_LOOP_ULP_ABS_TOL: f32 = 1.0e-2;
const OUT_OF_LOOP_ULP_REL_TOL: f32 = 3.0e-2;

fn frame_time() -> FrameTime {
    FrameTime {
        beats: Beats(0.0),
        seconds: Seconds(0.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    }
}

fn find_node(graph: &Graph, type_id: &str) -> NodeInstanceId {
    graph
        .nodes()
        .find(|n| n.node.type_id().as_str() == type_id)
        .map(|n| n.id)
        .unwrap_or_else(|| panic!("ColorGrade graph missing a `{type_id}` node"))
}

fn set_f(graph: &mut Graph, type_id: &str, param: &str, v: f32) {
    let id = find_node(graph, type_id);
    graph
        .set_param(id, param, ParamValue::Float(v))
        .unwrap_or_else(|e| panic!("set {type_id}.{param}: {e:?}"));
}

fn resource_for_output(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    for step in plan.steps() {
        if step.node == node {
            for &(name, id) in &step.outputs {
                if name == port {
                    return id;
                }
            }
        }
    }
    panic!("no output `{port}` on node {node:?}");
}

/// CPU-built RGBA gradient as a CPU-uploadable source texture — spatially
/// varying so a pointwise fusion bug that's invisible on a flat fill can't
/// hide. R ramps in x, G in y, B fixed, A = 1.
fn gradient_input(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
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
        label: "freeze-proof-input",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    tex
}

/// Render an effect graph to a standalone texture (the unfused / oracle side).
/// Copies `input` into the source slot, runs one frame, copies the bound
/// output into a fresh target that outlives the backend.
fn render_graph(
    device: &std::sync::Arc<GpuDevice>,
    graph: &mut Graph,
    plan: &ExecutionPlan,
    source_res: ResourceId,
    input: &GpuTexture,
    output_res: ResourceId,
) -> RenderTarget {
    let (w, h) = (input.width, input.height);

    let src_rt = RenderTarget::new(device, w, h, FMT, "freeze-src");
    {
        let mut e = device.create_encoder("freeze-src-fill");
        e.copy_texture_to_texture(input, &src_rt.texture, w, h, 1);
        e.commit_and_wait_completed();
    }
    let out_rt = RenderTarget::new(device, w, h, FMT, "freeze-graph-out");

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FMT);
    backend.pre_bind_texture_2d(source_res, src_rt);
    let out_slot = backend.pre_bind_texture_2d(output_res, out_rt);

    let mut enc = device.create_encoder("freeze-graph-exec");
    let mut exec = Executor::new(Box::new(backend));
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
        exec.execute_frame_with_gpu(graph, plan, frame_time(), &mut gpu);
    }
    enc.commit_and_wait_completed();

    let result = RenderTarget::new(device, w, h, FMT, "freeze-graph-result");
    let out_tex = exec
        .backend()
        .texture_2d(out_slot)
        .expect("graph output texture retained");
    {
        let mut e = device.create_encoder("freeze-graph-copy");
        e.copy_texture_to_texture(out_tex, &result.texture, w, h, 1);
        e.commit_and_wait_completed();
    }
    result
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FusedGainU {
    product: f32,
    _pad: [f32; 3],
}

/// Render the hand-fused Gain kernel: `out.rgb = in.rgb * product`, alpha kept.
/// One read, one multiply, one write — the bandwidth collapse of an N-Gain
/// chain.
fn render_fused_gain(device: &GpuDevice, input: &GpuTexture, product: f32) -> RenderTarget {
    let (w, h) = (input.width, input.height);
    let pipeline = device.create_compute_pipeline(
        include_str!("shaders/gain_fused.wgsl"),
        "cs_main",
        "freeze.gain_fused",
    );
    let out_rt = RenderTarget::new(device, w, h, FMT, "freeze-fused-out");
    let u = FusedGainU {
        product,
        _pad: [0.0; 3],
    };
    let mut enc = device.create_encoder("freeze-fused-exec");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&u),
            },
            GpuBinding::Texture {
                binding: 1,
                texture: input,
            },
            GpuBinding::Texture {
                binding: 3,
                texture: &out_rt.texture,
            },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "freeze.gain_fused",
    );
    enc.commit_and_wait_completed();
    out_rt
}

/// Build the unfused `Source -> Gain(g1) -> Gain(g2) -> FinalOutput` chain and
/// render it. Returns (rendered texture, the source ResourceId is internal).
fn render_unfused_two_gain(device: &std::sync::Arc<GpuDevice>, input: &GpuTexture, g1: f32, g2: f32) -> RenderTarget {
    let mut g = Graph::new();
    let src = g.add_node(Box::new(Source::new()));
    let a = g.add_node(Box::new(Gain::new()));
    let b = g.add_node(Box::new(Gain::new()));
    let fout = g.add_node(Box::new(FinalOutput::new()));
    g.set_param(a, "gain", ParamValue::Float(g1)).unwrap();
    g.set_param(b, "gain", ParamValue::Float(g2)).unwrap();
    g.connect((src, "out"), (a, "in")).unwrap();
    g.connect((a, "out"), (b, "in")).unwrap();
    g.connect((b, "out"), (fout, "in")).unwrap();

    let plan = compile(&g).unwrap();
    let source_res = resource_for_output(&plan, src, "out");
    let output_res = resource_for_output(&plan, b, "out");
    render_graph(device, &mut g, &plan, source_res, input, output_res)
}

#[test]
fn fused_gain_chain_matches_unfused_within_tolerance() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let input = gradient_input(&device, w, h);
    let (g1, g2) = (0.75_f32, 1.2_f32);

    let unfused = render_unfused_two_gain(&device.arc(), &input, g1, g2);
    let fused = render_fused_gain(&device, &input, g1 * g2);

    let differ = TextureDiff::new(&device);
    // abs 4e-3 / rel 1e-2: comfortably above the ~1 f16-ULP intermediate-
    // rounding drift, far below any real fusion error.
    let r = differ.compare(&device, &unfused.texture, &fused.texture, 4e-3, 1e-2);

    assert_eq!(
        r.over_count, 0,
        "correct fusion must clear the oracle (max_abs={}, max_rel={}, over={}/{})",
        r.max_abs, r.max_rel, r.over_count, r.total
    );
    assert!(
        r.max_abs < 4e-3,
        "the only diff should be sub-tolerance f16 accumulation, got max_abs={}",
        r.max_abs
    );
    assert!(r.passes(0.0), "correct fusion must pass the verdict at zero fraction");
}

#[test]
fn oracle_catches_wrong_fusion() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let input = gradient_input(&device, w, h);
    let (g1, g2) = (0.75_f32, 1.2_f32);

    let unfused = render_unfused_two_gain(&device.arc(), &input, g1, g2);
    // Mis-fuse: product off by 1.5× — a real fusion bug the oracle MUST catch.
    let wrong = render_fused_gain(&device, &input, g1 * g2 * 1.5);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &unfused.texture, &wrong.texture, 4e-3, 1e-2);

    assert!(
        r.over_count > 0,
        "oracle must flag a wrong fusion (max_abs={}, over={}/{})",
        r.max_abs, r.over_count, r.total
    );
    assert!(
        !r.passes(0.01),
        "a 1.5×-off fusion must fail the verdict (over_fraction={})",
        r.over_fraction()
    );
}

/// The real target: the shipped ColorGrade preset (9 nodes, 7 pointwise atoms
/// fanning source into both a grade chain and a mix) hand-fused into one
/// kernel and validated against the unfused preset at non-trivial params.
#[test]
fn fused_colorgrade_matches_unfused_within_tolerance() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input(&device, w, h);

    // One source of truth for the params; drives both sides.
    let params = ColorGradeParams {
        gain: 1.15,
        sat_s: 1.3,
        hue_deg: 25.0,
        sat_h: 1.2,
        val_h: 1.0,
        contrast: 1.2,
        col_amount: 0.4,
        col_hue: 210.0,
        col_sat: 0.8,
        col_focus: 0.6,
        mix_amount: 1.0, // full chain output (a-branch crossfaded out)
        mix_mode: 0,
        clamp_min: 0.0,
        clamp_max: 65000.0,
        _pad0: 0.0,
        _pad1: 0.0,
    };

    // Unfused: load the SHIPPED preset, set the same params, render it.
    let json = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/effect-presets/ColorGrade.json"
    ))
    .expect("read ColorGrade.json");
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse ColorGrade.json");
    let mut graph = def.into_graph(&registry).expect("build ColorGrade graph");
    set_f(&mut graph, "node.exposure", "gain", params.gain);
    set_f(&mut graph, "node.saturation", "saturation", params.sat_s);
    set_f(&mut graph, "node.hue_saturation", "hue", params.hue_deg);
    set_f(&mut graph, "node.hue_saturation", "saturation", params.sat_h);
    set_f(&mut graph, "node.hue_saturation", "value", params.val_h);
    set_f(&mut graph, "node.contrast", "contrast", params.contrast);
    set_f(&mut graph, "node.colorize", "amount", params.col_amount);
    set_f(&mut graph, "node.colorize", "hue", params.col_hue);
    set_f(&mut graph, "node.colorize", "saturation", params.col_sat);
    set_f(&mut graph, "node.colorize", "focus", params.col_focus);
    set_f(&mut graph, "node.mix", "amount", params.mix_amount);

    let plan = compile(&graph).expect("compile ColorGrade");
    let src_res = resource_for_output(&plan, find_node(&graph, "system.source"), "out");
    let out_res = resource_for_output(&plan, find_node(&graph, "node.clamp"), "out");
    let unfused = render_graph(&device.arc(), &mut graph, &plan, src_res, &input, out_res);

    // Fused: one kernel.
    let pipeline = colorgrade_pipeline(&device);
    let fused = RenderTarget::new(&device, w, h, FMT, "freeze-cg-fused");
    {
        let mut enc = device.create_encoder("freeze-cg-fused");
        dispatch_fused_colorgrade(&mut enc, &pipeline, &input, &fused.texture, &params);
        enc.commit_and_wait_completed();
    }

    let differ = TextureDiff::new(&device);
    // Looser than Gain: 7 stages of f16 round-trips through HSV + smoothstep
    // discontinuities (hue wrap, colorize edges) drift more, and a handful of
    // boundary texels can land on opposite sides of a step. Tolerate ≤0.5% of
    // texels failing both bounds (§11.D discontinuity-aware metric).
    let r = differ.compare(&device, &unfused.texture, &fused.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005),
        "fused ColorGrade must match unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// CHAIN FUSION CHECKPOINT (docs/CHAIN_FUSION_DESIGN.md §8) — the fail-fast
/// gate before any cross-card generalization. Two pointwise cards are rendered
/// the way the chain renders them today (card A's graph to a texture, that
/// texture fed into card B's graph — the full-canvas seam round-trip), and
/// against the fused concatenated segment def (one kernel, no seam). The fused
/// side must match within the same f16-accumulation budget as every pointwise
/// proof. Card params drive the fused kernel through the segment's namespaced
/// retarget map — proving the binding surface survives the card boundary.
#[test]
fn chain_segment_fused_matches_sequential_per_card() {
    use super::install::{FusedDef, fuse_canonical_def};
    use super::segment::concat_defs;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input(&device, w, h);

    let card_a: EffectGraphDef = serde_json::from_str(
        r#"{
        "version": 1, "name": "segA", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
            { "id": 2, "typeId": "node.contrast", "nodeId": "contrast" },
            { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
        ]
    }"#,
    )
    .expect("parse card A");
    let card_b: EffectGraphDef = serde_json::from_str(
        r#"{
        "version": 1, "name": "segB", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.saturation", "nodeId": "sat" },
            { "id": 2, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
        ]
    }"#,
    )
    .expect("parse card B");

    // Non-trivial params, applied to both sides.
    let (gain, contrast, saturation) = (1.35_f32, 1.25_f32, 0.6_f32);

    // ── Sequential per-card: today's chain semantics, seam round-trip included. ──
    let mut graph_a = card_a.clone().into_graph(&registry).expect("card A graph");
    set_f(&mut graph_a, "node.exposure", "gain", gain);
    set_f(&mut graph_a, "node.contrast", "contrast", contrast);
    let plan_a = compile(&graph_a).expect("compile card A");
    let a_src = resource_for_output(&plan_a, find_node(&graph_a, "system.source"), "out");
    let a_out = resource_for_output(&plan_a, find_node(&graph_a, "node.contrast"), "out");
    let a_result = render_graph(&device.arc(), &mut graph_a, &plan_a, a_src, &input, a_out);

    let mut graph_b = card_b.clone().into_graph(&registry).expect("card B graph");
    set_f(&mut graph_b, "node.saturation", "saturation", saturation);
    let plan_b = compile(&graph_b).expect("compile card B");
    let b_src = resource_for_output(&plan_b, find_node(&graph_b, "system.source"), "out");
    let b_out = resource_for_output(&plan_b, find_node(&graph_b, "node.saturation"), "out");
    let sequential =
        render_graph(&device.arc(), &mut graph_b, &plan_b, b_src, &a_result.texture, b_out);

    // ── Fused segment: concat → one region across the seam → one kernel. ──
    let seg = concat_defs(&[&card_a, &card_b]).expect("segment concat builds");
    let FusedDef { def: fused_def, retarget, .. } =
        fuse_canonical_def(&seg, &registry).expect("two pointwise cards fuse across the seam");
    let mut fused_graph = fused_def.into_graph(&registry).expect("fused segment graph builds");
    let fused_node = find_node(&fused_graph, "node.wgsl_compute");
    for (node_id, param, v) in [
        ("c0.gain", "gain", gain),
        ("c0.contrast", "contrast", contrast),
        ("c1.sat", "saturation", saturation),
    ] {
        let (_, field) = retarget
            .get(&(node_id.to_string(), param.to_string()))
            .unwrap_or_else(|| panic!("segment retarget missing {node_id}.{param}"));
        fused_graph
            .set_param(fused_node, field, ParamValue::Float(v))
            .unwrap_or_else(|e| panic!("set fused {field}: {e:?}"));
    }
    let fused_plan = compile(&fused_graph).expect("compile fused segment");
    let f_src = resource_for_output(&fused_plan, find_node(&fused_graph, "system.source"), "out");
    let f_out = resource_for_output(&fused_plan, fused_node, "dst");
    let fused = render_graph(&device.arc(), &mut fused_graph, &fused_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    // Pointwise-only: same budget as the gain-chain proof. The sequential side
    // rounds to f16 at the seam; the fused side keeps registers — that drift is
    // the tolerance's entire job.
    let r = differ.compare(&device, &sequential.texture, &fused.texture, 4e-3, 1e-2);
    assert_eq!(
        r.over_count, 0,
        "fused two-card segment must match sequential per-card rendering: \
         max_abs={}, max_rel={}, over={}/{}",
        r.max_abs, r.max_rel, r.over_count, r.total
    );
}

/// CPU-built gradient with spatially-varying alpha (A ramps in x), so the
/// faithful per-atom alpha threading (mix lerps a.a→b.a in Lerp mode, and
/// passes a.a through untouched in every other mode — BUG-181) is observable
/// in the diff — the §12.4 hardened-fixture alpha axis. R/G/B as in
/// `gradient_input`.
fn gradient_input_varying_alpha(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
    let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            px[i] = f16::from_f32(x as f32 / w as f32);
            px[i + 1] = f16::from_f32(y as f32 / h as f32);
            px[i + 2] = f16::from_f32(0.5);
            px[i + 3] = f16::from_f32(0.25 + 0.7 * (x as f32 / w as f32));
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
        label: "freeze-proof-input-alpha",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    tex
}

/// Deterministic LCG (Numerical Recipes constants) — a fuzzer needs random
/// coverage but a *reproducible* seed so a failure can be replayed exactly
/// (design §12.3 step 7 reproducer). Not for crypto; just spreads samples.
fn lcg_next(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (*state >> 33) as u32
}

fn lcg_f32(state: &mut u64, lo: f32, hi: f32) -> f32 {
    let u = lcg_next(state) as f32 / u32::MAX as f32;
    lo + u * (hi - lo)
}

/// **Step-6 fuzz hardening (design §12.3 step 6 / §12.4).** The single hardened
/// fixture proves correctness at one point; this sweeps the param space so we
/// aren't trusting one vector. For many random in-range param sets — including
/// every `mix` blend mode (0..7, so the `switch` + `safe_div` divide path are
/// all exercised) — it renders the unfused shipped preset and the AUTO-fused def
/// through the executor and asserts they agree: no divergent NaN/Inf
/// (`special_count == 0`, the hard gate) and within the discontinuity-aware
/// fraction budget. A fixed seed makes any failure replayable.
#[test]
fn colorgrade_fuzz_fused_agrees_with_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (192u32, 192u32);
    let input = gradient_input_varying_alpha(&device, w, h);

    let json = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/effect-presets/ColorGrade.json"
    ))
    .expect("read ColorGrade.json");
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse ColorGrade.json");

    // (stable node_id, param, lo, hi) — every modulatable float, at its real
    // range. clamp.min/max kept in a sane band so the clamp is exercised
    // without flattening the whole frame.
    let fields: &[(&str, &str, f32, f32)] = &[
        ("gain", "gain", 0.0, 2.0),
        ("saturation", "saturation", 0.0, 2.0),
        ("hue", "hue", -180.0, 180.0),
        ("hue", "saturation", 0.0, 2.0),
        ("hue", "value", 0.0, 2.0),
        ("contrast", "contrast", 0.0, 2.0),
        ("colorize", "amount", 0.0, 1.0),
        ("colorize", "hue", 0.0, 360.0),
        ("colorize", "saturation", 0.0, 2.0),
        ("colorize", "focus", 0.0, 1.0),
        ("grade_mix", "amount", 0.0, 1.0),
        ("clamp", "min", 0.0, 0.1),
        ("clamp", "max", 0.9, 2.0),
    ];

    // Build both graphs once; per iteration we only refresh params + re-render.
    let FusedDef { def: fused_def, retarget, .. } =
        fuse_canonical_def(&def, &registry).expect("ColorGrade fuses");
    let mut unfused_graph = def.clone().into_graph(&registry).expect("unfused graph");
    let mut fused_graph = fused_def.into_graph(&registry).expect("fused graph");
    let fused_node = find_node(&fused_graph, "node.wgsl_compute");

    let unfused_plan = compile(&unfused_graph).expect("compile unfused");
    let u_src =
        resource_for_output(&unfused_plan, find_node(&unfused_graph, "system.source"), "out");
    let u_out =
        resource_for_output(&unfused_plan, find_node(&unfused_graph, "node.clamp"), "out");
    let fused_plan = compile(&fused_graph).expect("compile fused");
    let f_src = resource_for_output(&fused_plan, find_node(&fused_graph, "system.source"), "out");
    let f_out = resource_for_output(&fused_plan, fused_node, "dst");

    let set_unfused = |g: &mut Graph, node_id: &str, param: &str, v: ParamValue| {
        let id = g
            .node_id_by_handle(node_id)
            .or_else(|| g.instance_by_node_id(&manifold_core::NodeId::new(node_id)))
            .unwrap_or_else(|| panic!("unfused graph missing node `{node_id}`"));
        g.set_param(id, param, v).unwrap_or_else(|e| panic!("set {node_id}.{param}: {e:?}"));
    };

    let differ = TextureDiff::new(&device);
    let seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut state = seed;
    const ITERS: u32 = 32;

    for it in 0..ITERS {
        // Draw one shared param vector, then apply it identically to both sides.
        let mode = lcg_next(&mut state) % 8;
        let vals: Vec<(&str, &str, f32)> = fields
            .iter()
            .map(|(nid, p, lo, hi)| (*nid, *p, lcg_f32(&mut state, *lo, *hi)))
            .collect();

        for (nid, p, v) in &vals {
            set_unfused(&mut unfused_graph, nid, p, ParamValue::Float(*v));
            let (_, field) = retarget
                .get(&((*nid).to_string(), (*p).to_string()))
                .unwrap_or_else(|| panic!("retarget missing {nid}.{p}"));
            fused_graph
                .set_param(fused_node, field, ParamValue::Float(*v))
                .unwrap_or_else(|e| panic!("set fused {field}: {e:?}"));
        }
        // mix mode: Enum on the unfused atom, the namespaced u32 field on the
        // fused kernel (WgslCompute carries it as Int/Float — see the storage
        // collapse). Drives the blend_rgb switch across all 8 branches.
        set_unfused(&mut unfused_graph, "grade_mix", "mode", ParamValue::Enum(mode));
        let (_, mode_field) = retarget
            .get(&("grade_mix".to_string(), "mode".to_string()))
            .expect("retarget has grade_mix.mode");
        fused_graph
            .set_param(fused_node, mode_field, ParamValue::Float(mode as f32))
            .expect("set fused mode");

        let unfused = render_graph(&device.arc(), &mut unfused_graph, &unfused_plan, u_src, &input, u_out);
        let fused = render_graph(&device.arc(), &mut fused_graph, &fused_plan, f_src, &input, f_out);

        let r = differ.compare(&device, &unfused.texture, &fused.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
        // Hard gate: NO divergent NaN/Inf, ever. Plus a discontinuity-aware
        // fraction budget loose enough to absorb extreme-param boundary bands.
        assert!(
            r.passes(0.03),
            "fuzz iter {it} (seed={seed:#x}, mode={mode}) diverged: special={}, \
             max_abs={}, max_rel={}, over={}/{} ({:.4}); params={vals:?}",
            r.special_count,
            r.max_abs,
            r.max_rel,
            r.over_count,
            r.total,
            r.over_fraction(),
        );
    }
}

/// **The step-4 production gate (design §12.3 step 5).** Drives the *install*
/// path end-to-end through the real executor: the region-grower
/// ([`super::install::fuse_canonical_def`]) auto-discovers the ColorGrade region
/// and rewrites the def into one `node.wgsl_compute` fused node carrying the
/// auto-generated kernel; `into_graph` builds it; the executor runs it through
/// the same WgslCompute introspection + dispatch the live chain uses. Diffed
/// against the unfused shipped preset rendered the same way.
///
/// This is strictly stronger than `fused_colorgrade_matches_unfused_within_tolerance`
/// above (which dispatches the *hand* kernel directly): it exercises the
/// def-rewrite, the WgslCompute uniform introspection, the per-atom param
/// seeding, and the executor — i.e. exactly what renders on stage.
///
/// Hardened fixture (§12.4): interior `mix_amount = 0.35` so the source→mix.a
/// fork materially contributes (not crossfaded out), plus a spatially-varying
/// input alpha so faithful alpha threading is exercised. Both sides run the
/// same alpha-faithful atom bodies, so alpha must agree exactly.
#[test]
fn auto_fused_colorgrade_via_executor_matches_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input_varying_alpha(&device, w, h);

    let json = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/effect-presets/ColorGrade.json"
    ))
    .expect("read ColorGrade.json");
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse ColorGrade.json");

    // One fixture, both sides. Interior mix_amount makes the fork matter.
    // (stable node_id, param, value) — drives both graphs identically.
    let fixture: &[(&str, &str, f32)] = &[
        ("gain", "gain", 1.15),
        ("saturation", "saturation", 1.3),
        ("hue", "hue", 25.0),
        ("hue", "saturation", 1.2),
        ("hue", "value", 1.0),
        ("contrast", "contrast", 1.2),
        ("colorize", "amount", 0.4),
        ("colorize", "hue", 210.0),
        ("colorize", "saturation", 0.8),
        ("colorize", "focus", 0.6),
        ("grade_mix", "amount", 0.35),
    ];

    // ── Unfused: the shipped preset graph, params set by node id. ──
    let mut unfused_graph = def.clone().into_graph(&registry).expect("unfused graph");
    let set_by_node_id = |g: &mut Graph, node_id: &str, param: &str, v: f32| {
        let id = g
            .node_id_by_handle(node_id)
            .or_else(|| g.instance_by_node_id(&manifold_core::NodeId::new(node_id)))
            .unwrap_or_else(|| panic!("unfused graph missing node `{node_id}`"));
        g.set_param(id, param, ParamValue::Float(v))
            .unwrap_or_else(|e| panic!("set {node_id}.{param}: {e:?}"));
    };
    for (node_id, param, v) in fixture {
        set_by_node_id(&mut unfused_graph, node_id, param, *v);
    }
    let unfused_plan = compile(&unfused_graph).expect("compile unfused");
    let u_src = resource_for_output(&unfused_plan, find_node(&unfused_graph, "system.source"), "out");
    let u_out =
        resource_for_output(&unfused_plan, find_node(&unfused_graph, "node.clamp"), "out");
    let unfused = render_graph(&device.arc(), &mut unfused_graph, &unfused_plan, u_src, &input, u_out);

    // ── Auto-fused: region-grow + def-rewrite, then run through the executor. ──
    let FusedDef { def: fused_def, retarget, .. } =
        fuse_canonical_def(&def, &registry).expect("ColorGrade is a whole-card fusable region");
    let mut fused_graph = fused_def.into_graph(&registry).expect("fused graph builds");
    let fused_node = find_node(&fused_graph, "node.wgsl_compute");
    for (node_id, param, v) in fixture {
        let (_, field) = retarget
            .get(&((*node_id).to_string(), (*param).to_string()))
            .unwrap_or_else(|| panic!("retarget missing {node_id}.{param}"));
        fused_graph
            .set_param(fused_node, field, ParamValue::Float(*v))
            .unwrap_or_else(|e| panic!("set fused {field}: {e:?}"));
    }
    let fused_plan = compile(&fused_graph).expect("compile fused");
    let f_src = resource_for_output(&fused_plan, find_node(&fused_graph, "system.source"), "out");
    let f_out = resource_for_output(&fused_plan, fused_node, "dst");
    let fused = render_graph(&device.arc(), &mut fused_graph, &fused_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    // Same discontinuity-aware budget as the hand-kernel test, plus an absolute
    // cap on the failing-texel count so a contiguous failure band can't hide in
    // the 0.5% fraction (§12.4 verdict tightening).
    let r = differ.compare(&device, &unfused.texture, &fused.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005) && r.over_count < 64,
        "auto-fused ColorGrade (via executor) must match unfused: \
         max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// **I6** (D7/P0 amendment, `docs/CINEMATIC_POST_DESIGN.md`): a graph chaining
/// a camera-derived Pointwise TEXTURE atom (`test.camera_pointwise` — the I6
/// test fixture; see its doc comment) with a Pointwise neighbour
/// (`node.invert`) must render byte-identical fused vs unfused. This is the
/// texture-fusion half of P0's contract: the fused kernel recomputes
/// `cam_x` (the wired `node.free_camera`'s `pos.x`) every frame via
/// `derived_uniform_registry::recompute`, routed onto the fused node's
/// synthesized `camera_ext_0` port — the SAME mechanism that lets a real
/// future camera-derived atom (P1's `coc_from_depth`) fuse with a pointwise
/// neighbour instead of being a permanent boundary. Same precision tier as
/// the ColorGrade proof above (freeze §7 tier 4, "out-of-loop texture
/// regions: ≈1 ulp, not bit-exact, and cannot be" — the unfused chain
/// round-trips the intermediate through an actual rgba16float texture
/// between the two members; the fused kernel keeps it in an f32 register).
/// Measured gap: max_abs ≈ 1/1024 (one f16 ULP at this value range) — the
/// SAME tolerance band `auto_fused_colorgrade_via_executor_matches_unfused`
/// uses.
#[test]
fn camera_derived_pointwise_atom_fuses_and_matches_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};
    use crate::node_graph::primitives::test_camera_pointwise_fixture::TestCameraPointwise;

    let device = crate::test_device();
    // The fixture is deliberately NOT globally inventory-registered (see its
    // doc comment) so `catalog_gen`'s completeness tests never see it — build
    // a registry that adds it on top of the real builtins for this test only.
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("test.camera_pointwise", || Box::new(TestCameraPointwise::new()));
    let (w, h) = (64u32, 64u32);
    let input = gradient_input(&device, w, h);

    let json = r#"{
        "version": 1, "name": "CameraDerivedFusion", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.free_camera", "nodeId": "cam" },
            { "id": 2, "typeId": "test.camera_pointwise", "nodeId": "cam_atom" },
            { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
            { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "camera" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
            { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
        ]
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).expect("parse fixture graph");

    // One fixture value set, both sides: cam.pos_x (a surviving boundary —
    // camera producers never fuse) set directly on both graphs; cam_atom.gain
    // set by node id on the unfused side, by retarget field on the fused side.
    let cam_pos_x = 2.5f32;
    let gain = 1.4f32;

    // ── Unfused: the canonical graph, params set by node id. ──
    let mut unfused_graph = def.clone().into_graph(&registry).expect("unfused graph");
    let set_by_node_id = |g: &mut Graph, node_id: &str, param: &str, v: f32| {
        let id = g
            .node_id_by_handle(node_id)
            .or_else(|| g.instance_by_node_id(&manifold_core::NodeId::new(node_id)))
            .unwrap_or_else(|| panic!("unfused graph missing node `{node_id}`"));
        g.set_param(id, param, ParamValue::Float(v))
            .unwrap_or_else(|e| panic!("set {node_id}.{param}: {e:?}"));
    };
    set_by_node_id(&mut unfused_graph, "cam", "pos_x", cam_pos_x);
    set_by_node_id(&mut unfused_graph, "cam_atom", "gain", gain);
    let unfused_plan = compile(&unfused_graph).expect("compile unfused");
    let u_src = resource_for_output(&unfused_plan, find_node(&unfused_graph, "system.source"), "out");
    let u_out =
        resource_for_output(&unfused_plan, find_node(&unfused_graph, "node.invert"), "out");
    let unfused = render_graph(&device.arc(), &mut unfused_graph, &unfused_plan, u_src, &input, u_out);

    // ── Fused: cam_atom + invert must collapse into ONE node.wgsl_compute,
    // with `cam` surviving as a boundary (a Camera producer never fuses) and
    // its output routed onto the fused node's synthesized `camera_ext_0`. ──
    let FusedDef { def: fused_def, retarget, .. } =
        fuse_canonical_def(&def, &registry).expect("cam_atom + invert is one fusable region");
    assert_eq!(
        fused_def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").count(),
        1,
        "cam_atom and invert must collapse to exactly one fused node"
    );
    assert!(
        fused_def.nodes.iter().any(|n| n.type_id == "node.free_camera"),
        "the camera producer must survive as a boundary, not fuse away"
    );
    assert!(
        fused_def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.wgsl_compute")
            .and_then(|n| n.wgsl_source.as_deref())
            .is_some_and(|s| s.contains("@camera_external: camera_ext_0")
                && s.contains("@derived_uniform_member:")),
        "the fused kernel must carry BOTH D7/P0 markers (camera_ext port + \
         derived-uniform recompute), not just fuse structurally"
    );

    let mut fused_graph = fused_def.into_graph(&registry).expect("fused graph builds");
    set_by_node_id(&mut fused_graph, "cam", "pos_x", cam_pos_x);
    let fused_node = find_node(&fused_graph, "node.wgsl_compute");
    let (_, gain_field) = retarget
        .get(&("cam_atom".to_string(), "gain".to_string()))
        .expect("retarget carries cam_atom.gain");
    fused_graph
        .set_param(fused_node, gain_field, ParamValue::Float(gain))
        .unwrap_or_else(|e| panic!("set fused {gain_field}: {e:?}"));
    let fused_plan = compile(&fused_graph).expect("compile fused");
    let f_src = resource_for_output(&fused_plan, find_node(&fused_graph, "system.source"), "out");
    let f_out = resource_for_output(&fused_plan, fused_node, "dst");
    let fused = render_graph(&device.arc(), &mut fused_graph, &fused_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    // Out-of-loop texture tier (freeze §7.4): ≈1 f16 ULP, same tolerance band
    // the ColorGrade proof above uses.
    let r = differ.compare(&device, &unfused.texture, &fused.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005) && r.over_count < 64,
        "camera-derived pointwise fusion must match unfused within the \
         out-of-loop tolerance: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// BUG-135/BUG-141: the real glb-import-shaped region — a camera-derived
/// `wgsl_includes` TEXTURE atom (`node.coc_from_depth`, whose body calls
/// `depth_common.wgsl`'s `linearize_depth`) fused with a Pointwise neighbour
/// (`node.invert`). This is the exact shape the CoC-computation half of DoF
/// v1 forms once its downstream isn't a Gather consumer (I6's
/// `test.camera_pointwise` fixture proved the camera-derived-uniform
/// mechanism but declared no `wgsl_includes`, so it never exercised this gap
/// — see BUG-135's writeup). Before the fix, `generate_fused`'s texture path
/// never emitted `node_includes`, so naga rejected the fused kernel with
/// "no definition in scope for identifier: linearize_depth" and
/// `fuse_canonical_def` fell back to `None` (the whole card renders unfused,
/// silently — BUG-141's exact glb-import symptom). `.expect(...)` below is
/// the direct regression guard: it panics on that fallback.
#[test]
fn coc_from_depth_fuses_with_pointwise_neighbor_and_matches_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (64u32, 64u32);
    // Stand-in "depth" — CocFromDepth reads it as raw [0,1] clip depth
    // (render_scene's contract); a plain gradient exercises `linearize_depth`
    // across a real value range without needing a full mesh render.
    let input = gradient_input(&device, w, h);

    let json = r#"{
        "version": 1, "name": "CocFromDepthFusion", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.free_camera", "nodeId": "cam" },
            { "id": 2, "typeId": "node.camera_lens", "nodeId": "lens" },
            { "id": 3, "typeId": "node.coc_from_depth", "nodeId": "coc" },
            { "id": 4, "typeId": "node.invert", "nodeId": "invert" },
            { "id": 5, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "camera" },
            { "fromNode": 0, "fromPort": "out", "toNode": 3, "toPort": "depth" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "camera" },
            { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" },
            { "fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "in" }
        ]
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).expect("parse fixture graph");

    // A finite lens (real thin-lens math exercises linearize_depth with real
    // values, not the f_stop=infinity pinhole shortcut that zeroes the whole
    // CoC buffer — CocFromDepth's own I2 invariant).
    let focus_distance = 3.0f32;
    let f_stop = 2.8f32;

    // ── Unfused: the canonical graph, params set by node id. ──
    let mut unfused_graph = def.clone().into_graph(&registry).expect("unfused graph");
    let set_by_node_id = |g: &mut Graph, node_id: &str, param: &str, v: f32| {
        let id = g
            .node_id_by_handle(node_id)
            .or_else(|| g.instance_by_node_id(&manifold_core::NodeId::new(node_id)))
            .unwrap_or_else(|| panic!("unfused graph missing node `{node_id}`"));
        g.set_param(id, param, ParamValue::Float(v))
            .unwrap_or_else(|e| panic!("set {node_id}.{param}: {e:?}"));
    };
    set_by_node_id(&mut unfused_graph, "lens", "focus_distance", focus_distance);
    set_by_node_id(&mut unfused_graph, "lens", "f_stop", f_stop);
    let unfused_plan = compile(&unfused_graph).expect("compile unfused");
    let u_src = resource_for_output(&unfused_plan, find_node(&unfused_graph, "system.source"), "out");
    let u_out =
        resource_for_output(&unfused_plan, find_node(&unfused_graph, "node.invert"), "out");
    let unfused = render_graph(&device.arc(), &mut unfused_graph, &unfused_plan, u_src, &input, u_out);

    // ── Fused: coc + invert must collapse into ONE node.wgsl_compute, with
    // `cam`/`lens` surviving as boundaries (a Camera producer never fuses)
    // and `lens`'s output routed onto the fused node's synthesized
    // `camera_ext_0`. If BUG-135 were still present, the fused kernel would
    // fail naga parse and `fuse_canonical_def` would return `None` — the
    // `.expect` below is the regression guard. ──
    let FusedDef { def: fused_def, retarget, .. } =
        fuse_canonical_def(&def, &registry).expect("coc + invert is one fusable region");
    assert_eq!(
        fused_def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").count(),
        1,
        "coc and invert must collapse to exactly one fused node"
    );
    assert!(
        fused_def.nodes.iter().any(|n| n.type_id == "node.camera_lens"),
        "the camera/lens producers must survive as boundaries, not fuse away"
    );
    let fused_wgsl = fused_def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.wgsl_compute")
        .and_then(|n| n.wgsl_source.as_deref())
        .expect("fused node carries its generated WGSL");
    assert!(
        fused_wgsl.contains("fn linearize_depth"),
        "the shared depth_common.wgsl helper must be carried into the fused kernel (BUG-135):\n{}",
        fused_wgsl
    );
    assert!(
        fused_wgsl.contains("@camera_external: camera_ext_0")
            && fused_wgsl.contains("@derived_uniform_member:"),
        "the fused kernel must carry both D7/P0 markers (camera_ext port + \
         derived-uniform recompute):\n{}",
        fused_wgsl
    );

    let mut fused_graph = fused_def.into_graph(&registry).expect("fused graph builds");
    set_by_node_id(&mut fused_graph, "lens", "focus_distance", focus_distance);
    set_by_node_id(&mut fused_graph, "lens", "f_stop", f_stop);
    let fused_node = find_node(&fused_graph, "node.wgsl_compute");
    // coc_from_depth has one exposed param (max_radius) but this fixture
    // leaves it at its default on both sides, so no retarget lookup is
    // needed here beyond confirming the field exists (parity with I6's
    // pattern of driving the fused node's port-shadow through `retarget`).
    let _ = retarget;
    let fused_plan = compile(&fused_graph).expect("compile fused");
    let f_src = resource_for_output(&fused_plan, find_node(&fused_graph, "system.source"), "out");
    let f_out = resource_for_output(&fused_plan, fused_node, "dst");
    let fused = render_graph(&device.arc(), &mut fused_graph, &fused_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    // Out-of-loop texture tier (freeze §7.4): ≈1 f16 ULP, same tolerance band
    // the ColorGrade / I6 proofs above use.
    let r = differ.compare(&device, &unfused.texture, &fused.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005) && r.over_count < 64,
        "coc_from_depth + invert fusion must match unfused within the \
         out-of-loop tolerance: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Broad safety net for activating partial-region fusion library-wide: every
/// bundled preset the finder fuses must render one frame through its fused view
/// without panicking — the structural-breakage class (invalid generated WGSL, a
/// binding/dispatch mismatch in the multi-region wiring, a stranded resource).
/// This is the fused twin of `bundled_presets::every_bundled_preset_executes_
/// one_frame` (which renders the UNFUSED canonical defs and so never exercises
/// fusion). Renders only — numerical agreement vs unfused is the per-effect
/// oracle's job + Peter's visual sign-off; this catches the "does it even run"
/// class across the whole library, which is exactly what broadening fusion past
/// ColorGrade puts at risk.
#[test]
fn every_fused_preset_executes_one_frame() {
    use crate::node_graph::state_store::StateStore;
    use std::panic::AssertUnwindSafe;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (192u32, 192u32);
    let ft = frame_time();
    let mut failures: Vec<String> = Vec::new();
    let mut fused_count = 0usize;

    for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Effect) {
        let preset_id = type_id.as_str().to_string();
        // (The long-standing WireframeDepth skip is gone: the 42×42 same-size-
        // blit panic in its depth path was fixed 2026-06-12 — the source →
        // analysis copy is a sampling resize now — and the graph decomposition
        // replaced the legacy impl under the WireframeDepth type id.)
        let Some(base) = crate::node_graph::loaded_preset_view_by_id(&type_id) else {
            continue;
        };
        let Some(fused) = super::install::fuse_canonical_def(&base.canonical_def, &registry) else {
            continue; // no fusable region — nothing to validate
        };
        fused_count += 1;

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut graph = fused.def.into_graph(&registry).expect("fused def builds a graph");
            let plan = compile(&graph).expect("fused graph compiles");
            let r_src = resource_for_output(&plan, find_node(&graph, "system.source"), "out");
            let src_target = RenderTarget::new(&device, w, h, FMT, "fused-smoke-src");
            let mut backend = MetalBackend::new(device.arc(), w, h, FMT);
            backend.pre_bind_texture_2d(r_src, src_target);
            let mut exec = Executor::new(Box::new(backend));
            let mut state = StateStore::new();
            let mut native_enc = device.create_encoder("fused-smoke");
            {
                let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
                exec.execute_frame_with_state(&mut graph, &plan, ft, &mut gpu, &mut state, 0);
            }
            native_enc.commit_and_wait_completed();
        }));

        if let Err(panic) = result {
            let msg = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&'static str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "<non-string panic>".to_string());
            failures.push(format!("{preset_id}: {msg}"));
        }
    }

    assert!(fused_count > 0, "expected at least ColorGrade to produce a fused view");
    assert!(
        failures.is_empty(),
        "{fused_count} presets fuse; these panicked rendering their fused view:\n  - {}",
        failures.join("\n  - "),
    );
}

/// Tier 2 oracle — a Source generator folded into a region renders identically
/// fused vs unfused. `checkerboard` (Source, 0 inputs) is blended with the
/// incoming source texture by `mix`: the region has BOTH a 0-input head (the
/// generator produces from uv/dims) and an external read (the source), so it
/// exercises the full Source-as-producer path through the executor. Both sides
/// use the atom defaults, so the one fused kernel must match the two-pass chain.
#[test]
fn fused_source_region_matches_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input_varying_alpha(&device, w, h);

    // source → mix.a, checkerboard → mix.b, mix → final_output.
    let json = r#"{
        "version": 1, "name": "overlay", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.checkerboard", "nodeId": "checker" },
            { "id": 2, "typeId": "node.mix", "nodeId": "mix" },
            { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "a" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "b" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
        ]
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).unwrap();

    // ── Unfused: the two-pass chain (checkerboard, then mix). ──
    let mut unfused = def.clone().into_graph(&registry).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.mix"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    // ── Fused: checkerboard + mix collapse into one kernel. ──
    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the Source region fuses");
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph(&device.arc(), &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005) && r.over_count < 64,
        "fused Source region must match unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Tier 2 on REAL generators. Generator presets are the same `EffectGraphDef`
/// graphs as effects (just loaded from a separate registry), and they're built
/// largely out of Source atoms — exactly what tier 2 unlocks. The live generator
/// render path doesn't yet swap in fused views (that plumbing rides the
/// effect/generator unification), but the finder + codegen are path-agnostic, so
/// we can fuse each generator's canonical def here and prove every generated
/// kernel is valid WGSL. (Generators may have no `system.source`, so we validate
/// by compiling the kernels rather than rendering — the synthetic oracle above
/// already proves the Source render/binding path end-to-end.)
#[test]
fn every_fused_generator_kernel_compiles() {
    use std::panic::AssertUnwindSafe;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let mut failures: Vec<String> = Vec::new();
    let mut fused_generators = 0usize;
    let mut fused_kernels = 0usize;

    for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Generator) {
        let Some(json) = crate::node_graph::bundled_presets::bundled_preset_json(&type_id) else {
            continue;
        };
        let Ok(def) = serde_json::from_str::<EffectGraphDef>(&json) else {
            continue;
        };
        let Some(fused) = super::install::fuse_canonical_def(&def, &registry) else {
            continue; // no fusable region in this generator
        };
        fused_generators += 1;
        for node in fused.def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute") {
            fused_kernels += 1;
            let wgsl = node.wgsl_source.as_deref().expect("fused node carries WGSL");
            let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _ = device.create_compute_pipeline(wgsl, super::codegen::ENTRY, "gen-kernel-smoke");
            }));
            if res.is_err() {
                failures.push(format!("{}: a fused kernel failed to compile", type_id.as_str()));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "fused generator kernels must be valid WGSL:\n  - {}",
        failures.join("\n  - "),
    );
    // Not an assertion — generator coverage is informational (some generators are
    // all wgsl_compute / 3D / buffer and fuse nothing until tiers 3+). Logged so
    // the real reach of tier 2 on generators is visible, never silently zero.
    eprintln!(
        "tier 2: {fused_generators} generator presets fused {fused_kernels} kernel(s)"
    );
}

/// Diagnostic for the Infrared "black background goes navy" report: render the
/// REAL bundled Infrared preset (full palette bank group + mux, Arctic palette,
/// amount 1, contrast 1) on a pure-black input through the production executor,
/// unfused — the path the live compositor actually runs. The centre pixel must
/// be black; if it's navy (Arctic's low stop) the preset graph itself lifts
/// black, independent of the live compositor/input.
#[test]
fn infrared_preset_black_stays_black() {
    use crate::node_graph::chain_spec::splice_def_into_chain;
    use crate::node_graph::parameters::ParamValue;
    use manifold_core::PresetTypeId;

    fn black_input(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
        let px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: FMT,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "ir-black-in",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                px.as_ptr().cast::<u8>(),
                std::mem::size_of_val(px.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn center_rgb(device: &GpuDevice, rt: &RenderTarget) -> [f32; 3] {
        let (w, h) = (rt.width, rt.height);
        let bpr = w * 8;
        let buf = device.create_buffer_shared(u64::from(h * bpr));
        let mut e = device.create_encoder("ir-read");
        e.copy_texture_to_buffer(&rt.texture, &buf, w, h, bpr);
        e.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("mapped");
        let all =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), (h * bpr) as usize) };
        let o = (((h / 2) * w + w / 2) * 8) as usize;
        [
            f16::from_le_bytes([all[o], all[o + 1]]).to_f32(),
            f16::from_le_bytes([all[o + 2], all[o + 3]]).to_f32(),
            f16::from_le_bytes([all[o + 4], all[o + 5]]).to_f32(),
        ]
    }

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (128u32, 128u32);
    let input = black_input(&device, w, h);

    let id = PresetTypeId::new("Infrared");
    let def = crate::node_graph::bundled_presets::bundled_preset_def(&id)
        .expect("Infrared preset def");

    let mut graph = Graph::new();
    let src = graph.add_node(Box::new(Source::new()));
    let result =
        splice_def_into_chain(&mut graph, (src, "out"), def, &registry, None).expect("splice");
    let names: Vec<&str> = result.handles.iter().map(|(n, _)| n.as_ref()).collect();
    eprintln!("handles: {names:?}");
    let find = |name: &str| -> Option<NodeInstanceId> {
        result
            .handles
            .iter()
            .find(|(n, _)| n.as_ref() == name)
            .map(|(_, id)| *id)
    };
    // Arctic (6) if the mux handle is reachable; otherwise default palette
    // (White Hot) — texel 0 is black for both, so black→black either way.
    if let Some(mux) = find("Palette Bank/palette_mux").or_else(|| find("palette_mux")) {
        graph.set_param(mux, "selector", ParamValue::Float(6.0)).unwrap();
    }
    if let Some(ir) = find("infrared") {
        graph.set_param(ir, "amount", ParamValue::Float(1.0)).unwrap();
        graph.set_param(ir, "contrast", ParamValue::Float(1.0)).unwrap();
    }
    let fout = graph.add_node(Box::new(FinalOutput::new()));
    graph.connect(result.output, (fout, "in")).unwrap();

    let plan = compile(&graph).expect("compile");
    let src_res = resource_for_output(&plan, src, "out");
    let out_res = resource_for_output(&plan, result.output.0, result.output.1);
    let img = render_graph(&device.arc(), &mut graph, &plan, src_res, &input, out_res);
    let rgb = center_rgb(&device, &img);

    eprintln!("Infrared preset (Arctic, black input) centre = {rgb:?}");
    // Pure black must read back PURE black, not a faint navy. With the old
    // centre LUT mapping this was [0, 0.0006, 0.0046] (visible blue); the
    // endpoint mapping (gradient_ramp texel 0 == first stop) drives it to ~0.
    assert!(
        rgb[0] < 5e-4 && rgb[1] < 5e-4 && rgb[2] < 5e-4,
        "Infrared lifted pure black to {rgb:?} — the LUT's texel 0 is not the \
         first stop (centre-vs-endpoint mapping regression in gradient_ramp)",
    );
}

/// Diagnostic for the "navy background" report: render the REAL Wireframe
/// generator through the production `PresetRuntime` path and characterise its
/// background. The invariant under test is "pure black carries through as pure
/// black" — if the generator's empty regions come out > 0, the floor is born
/// here (before any effect), which is what Infrared then colours.
#[test]
fn wireframe_generator_background_is_black() {
    use crate::node_graph::bundled_presets::bundled_preset_json;
    use crate::preset_context::PresetContext;
    use crate::preset_runtime::PresetRuntime;
    use manifold_core::PresetTypeId;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (192u32, 192u32);

    let id = PresetTypeId::new("Wireframe");
    let json = bundled_preset_json(&id).expect("Wireframe json");
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse");

    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };

    let mut generator = PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, FMT, None)
        .expect("generator builds");
    let target = RenderTarget::new(&device, w, h, FMT, "wz-bg");
    for _ in 0..3 {
        let mut enc = device.create_encoder("wz-render");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            generator.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }

    // Read the whole frame, report per-channel min/max and the corner pixel
    // (almost certainly background for a centred wireframe).
    let bpr = w * 8;
    let buf = device.create_buffer_shared(u64::from(h * bpr));
    let mut renc = device.create_encoder("wz-read");
    renc.copy_texture_to_buffer(&target.texture, &buf, w, h, bpr);
    renc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("mapped");
    let all = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), (h * bpr) as usize) };
    let at = |x: u32, y: u32| -> [f32; 4] {
        let o = ((y * w + x) * 8) as usize;
        [
            f16::from_le_bytes([all[o], all[o + 1]]).to_f32(),
            f16::from_le_bytes([all[o + 2], all[o + 3]]).to_f32(),
            f16::from_le_bytes([all[o + 4], all[o + 5]]).to_f32(),
            f16::from_le_bytes([all[o + 6], all[o + 7]]).to_f32(),
        ]
    };
    let mut minc = [f32::INFINITY; 4];
    let mut maxc = [f32::NEG_INFINITY; 4];
    for y in 0..h {
        for x in 0..w {
            let p = at(x, y);
            for c in 0..4 {
                minc[c] = minc[c].min(p[c]);
                maxc[c] = maxc[c].max(p[c]);
            }
        }
    }
    eprintln!("Wireframe per-channel min = {minc:?}");
    eprintln!("Wireframe per-channel max = {maxc:?}");
    eprintln!("corner(0,0)     = {:?}", at(0, 0));
    eprintln!("corner(w-1,0)   = {:?}", at(w - 1, 0));
    eprintln!("edge(0,h/2)     = {:?}", at(0, h / 2));
    // The darkest pixel in the frame IS the background. It must be black.
    assert!(
        minc[0] < 0.01 && minc[1] < 0.01 && minc[2] < 0.01,
        "generator background floor is not black: per-channel min = {minc:?}",
    );
}

/// Tier 3 oracle — a gather atom folded into a region renders identically fused
/// vs unfused. source → sharpen(Gather) → invert → final: in the fused kernel,
/// sharpen samples the source (bound `src_0` + the internal sampler) at the
/// neighbour offsets it computes, then threads its register to invert; unfused,
/// it's two passes. Both run the same `sharpen`/`invert` bodies, so they must
/// agree (the discontinuity-aware budget absorbs any edge-sampler f16 drift).
#[test]
fn fused_gather_region_matches_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input_varying_alpha(&device, w, h);

    let json = r#"{
        "version": 1, "name": "warp", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.sharpen", "nodeId": "sharp" },
            { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
            { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
        ]
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).unwrap();

    let mut unfused = def.clone().into_graph(&registry).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.invert"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the gather region fuses");
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph(&device.arc(), &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.02),
        "fused gather region must match unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Tiers 2 + 3 together — the canonical UV-warp. `remap` reads `source` as a
/// GATHER (samples it at coords from a field) and `uv_field` as a COINCIDENT
/// register; `uv_field` is a SOURCE generator producing those coords. So
/// source → remap.source, uv_field → remap.uv_field, remap → final fuses the
/// Source head + the mixed gather/coincident atom into ONE kernel: uv_field
/// produces the coords register, remap samples the bound external source at them.
/// This is the warp family's fusion path, proven to match its two-pass form.
#[test]
fn fused_warp_region_matches_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input_varying_alpha(&device, w, h);

    let json = r#"{
        "version": 1, "name": "warp2", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.uv_field", "nodeId": "uvf" },
            { "id": 2, "typeId": "node.remap", "nodeId": "remap" },
            { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "source" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "uv_field" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
        ]
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).unwrap();

    let mut unfused = def.clone().into_graph(&registry).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.remap"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the warp region fuses");
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph(&device.arc(), &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.02),
        "fused warp region must match unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// High-frequency deterministic noise input — the WORST case for the stencil
/// tier's manual-bilinear-vs-hardware-filter gap (neighbouring texels differ by
/// up to the full range, so any filter-weight difference is maximally visible).
/// LCG-seeded, reproducible.
fn noise_input(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
    let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    let mut state = 0x5EED_5EEDu64;
    for v in px.iter_mut() {
        *v = f16::from_f32((lcg_next(&mut state) & 0xFFFF) as f32 / 65535.0);
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
        label: "freeze-proof-noise",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    tex
}

/// Render the stencil checkpoint def (source → gain → gaussian_blur → final,
/// blur params supplied) unfused and fused-with-virtual-chain, and return the
/// diff. Asserts the structural expectations on the way: the region fuses, the
/// gain is absorbed (deleted from the installed def), one wgsl_compute node.
fn stencil_checkpoint_diff(
    radius_mode: u32,
    radius: f32,
    kernel_size: u32,
    step: f32,
) -> super::DiffResult {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = noise_input(&device, w, h);

    let json = format!(
        r#"{{
        "version": 1, "name": "stencil-cp", "nodes": [
            {{ "id": 0, "typeId": "system.source", "nodeId": "source" }},
            {{ "id": 1, "typeId": "node.exposure", "nodeId": "gain",
               "params": {{ "gain": {{ "type": "Float", "value": 1.3 }} }} }},
            {{ "id": 2, "typeId": "node.gaussian_blur", "nodeId": "blur",
               "params": {{
                 "radius_mode": {{ "type": "Enum", "value": {radius_mode} }},
                 "radius": {{ "type": "Float", "value": {radius} }},
                 "kernel_size": {{ "type": "Enum", "value": {kernel_size} }},
                 "step": {{ "type": "Float", "value": {step} }},
                 "axis": {{ "type": "Enum", "value": 0 }}
               }} }},
            {{ "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }}
        ], "wires": [
            {{ "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" }},
            {{ "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }},
            {{ "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }}
        ]
    }}"#
    );
    let def: EffectGraphDef = serde_json::from_str(&json).unwrap();

    let mut unfused = def.clone().into_graph(&registry).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.gaussian_blur"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the stencil region fuses");
    assert!(
        !fdef.nodes.iter().any(|n| n.type_id == "node.exposure"),
        "the gain must be absorbed into the blur's fetch"
    );
    assert!(
        !fdef.nodes.iter().any(|n| n.type_id == "node.gaussian_blur"),
        "the blur folds into the fused kernel"
    );
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph(&device.arc(), &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    differ.compare(&device, &u_img.texture, &f_img.texture, 1.0e-3, 1.0e-3)
}

/// STENCIL CHECKPOINT, integer taps — Fixed mode at step 1.0 puts every tap on
/// a texel center, where the hardware filter snaps to the exact texel; the
/// fetch's corner values are bit-identical to the unfused chain's stores, so
/// the whole pipeline should agree to ~an f16 ulp even on pure noise.
#[test]
fn stencil_virtual_chain_integer_tap_blur_matches_unfused() {
    let r = stencil_checkpoint_diff(0, 0.0, 1, 1.0);
    assert!(
        r.max_abs < 1.5e-3,
        "integer-tap stencil fusion must be ulp-exact: max_abs={}, over={}/{}",
        r.max_abs,
        r.over_count,
        r.total
    );
}

/// STENCIL CHECKPOINT, fractional taps — Dynamic mode at radius 7.3 uses the
/// bilinear tap-pair offsets, so every tap exercises the manual-f32-lerp vs
/// hardware-filter-unit gap on worst-case noise. This is the fail-fast gate
/// from the tier design: if this can't hold the documented proof tolerance,
/// fractional-tap stencil fusion is invalid and the tier narrows to integer
/// taps. The blur averages ~5 taps with sub-1 weights, so per-tap filter error
/// must stay within the two-sided budget.
#[test]
fn stencil_virtual_chain_fractional_tap_blur_matches_unfused() {
    let r = stencil_checkpoint_diff(1, 7.3, 1, 1.0);
    eprintln!(
        "[stencil checkpoint] fractional taps on noise: max_abs={} max_rel={} over={}/{}",
        r.max_abs, r.max_rel, r.over_count, r.total
    );
    assert!(
        r.passes(0.005),
        "fractional-tap stencil fusion exceeded the documented tolerance \
         (manual bilinear vs hardware filter): max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// STENCIL + SPECIALIZATION — the variable-width blur (DoF's kernel) fuses with
/// its QUALITY_LEVEL / WEIGHTING_MODE tokens substituted from the def's static
/// params, an absorbed upstream gain in its `in` fetch, the source gathered as
/// the real `width` external, and a downstream invert threading its register.
/// Proven at a non-default specialization (25-tap + scatter-as-gather) so a
/// wrong substitution can't hide behind the default kernel.
#[test]
fn fused_variable_width_blur_matches_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input_varying_alpha(&device, w, h);

    let json = r#"{
        "version": 1, "name": "vbw", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.exposure", "nodeId": "gain",
              "params": { "gain": { "type": "Float", "value": 1.2 } } },
            { "id": 2, "typeId": "node.variable_blur", "nodeId": "blur",
              "params": {
                "quality": { "type": "Enum", "value": 2 },
                "weighting_mode": { "type": "Enum", "value": 1 },
                "max_radius": { "type": "Float", "value": 9.0 }
              } },
            { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
            { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "width" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
            { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
        ]
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).unwrap();

    let mut unfused = def.clone().into_graph(&registry).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.invert"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the specialized blur fuses");
    assert!(
        !fdef.nodes.iter().any(|n| n.type_id == "node.exposure"),
        "the gain is absorbed into the blur's in-fetch"
    );
    let wgsl = fdef
        .nodes
        .iter()
        .find(|n| n.type_id == "node.wgsl_compute")
        .and_then(|n| n.wgsl_source.as_deref())
        .expect("fused kernel present");
    assert!(
        !wgsl.contains("QUALITY_LEVEL") && !wgsl.contains("WEIGHTING_MODE"),
        "specialization tokens must be substituted, not free"
    );
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph(&device.arc(), &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, 2.0e-3, 1.0e-3);
    assert!(
        r.passes(0.005),
        "fused variable-width blur must match unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// STENCIL + GATHER CHAIN — the Watercolor diffuse shape: a uv-displace warp
/// (itself a sampler-Gather atom, fed by a HALF-RES flow field) is the sole
/// producer of a Linear blur's input, so it absorbs into the blur's fetch.
/// The fetch re-evaluates the warp per tap corner: the warp's `in` stays a
/// bound texture it samples at its own computed coords, its `flow` reads
/// through the shared sampler at the corner uv — the same resolution-robust
/// read the unfused atom made of the half-res field. Integer Linear taps ⇒
/// near-exact agreement.
#[test]
fn stencil_chain_absorbs_gather_warp_with_half_res_flow() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = noise_input(&device, w, h);

    let json = r#"{
        "version": 1, "name": "wc-diffuse", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.flow_field_noise", "nodeId": "flow",
              "params": { "warp_scale": { "type": "Float", "value": 0.0 },
                          "resolution": { "type": "Enum", "value": 1 } } },
            { "id": 2, "typeId": "node.uv_displace_by_flow", "nodeId": "flow_warp",
              "params": { "weight": { "type": "Float", "value": 0.004 },
                          "bias": { "type": "Float", "value": 0.5 } } },
            { "id": 3, "typeId": "node.gaussian_blur", "nodeId": "blur_h",
              "params": { "radius_mode": { "type": "Enum", "value": 2 },
                          "radius": { "type": "Float", "value": 2.0 },
                          "axis": { "type": "Enum", "value": 0 } } },
            { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 1, "fromPort": "flow", "toNode": 2, "toPort": "flow" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
            { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
        ]
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).unwrap();

    // Structural expectation first: the warp is absorbed (deleted), the flow
    // field survives standalone, the blur folds into the fused kernel.
    let regions = crate::node_graph::freeze::region::partition_regions(&def, &registry);
    assert_eq!(regions.len(), 1, "blur + absorbed warp form one region");
    assert_eq!(regions[0].virtual_chains.len(), 1, "the warp is a virtual chain");
    assert_eq!(regions[0].virtual_chains[0].members[0].doc_id, 2);

    let mut unfused = def.clone().into_graph(&registry).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.gaussian_blur"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the warp-chain region fuses");
    assert!(
        !fdef.nodes.iter().any(|n| n.type_id == "node.uv_displace_by_flow"),
        "the warp must be absorbed into the blur's fetch"
    );
    assert!(
        fdef.nodes.iter().any(|n| n.type_id == "node.flow_field_noise"),
        "the half-res flow field survives as the chain's sampled external"
    );
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph(&device.arc(), &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, 2.0e-3, 1.0e-3);
    eprintln!(
        "[stencil chain] warp+half-res flow into Linear blur: max_abs={} over={}/{}",
        r.max_abs, r.over_count, r.total
    );
    assert!(
        r.passes(0.005),
        "absorbed warp chain must match unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Render an EFFECT def for `frames` frames through the state-aware executor
/// (feedback loops warm up across frames), source pre-bound to `input`, and
/// return the texture feeding `final_output` after the last frame.
fn render_effect_frames_with_state(
    device: &std::sync::Arc<GpuDevice>,
    registry: &PrimitiveRegistry,
    def: &EffectGraphDef,
    input: &GpuTexture,
    frames: u32,
) -> RenderTarget {
    use crate::node_graph::StateStore;
    let (w, h) = (input.width, input.height);
    let mut graph = def.clone().into_graph(registry).expect("graph builds");
    let plan = compile(&graph).expect("compiles");
    let src_res = resource_for_output(&plan, find_node(&graph, "system.source"), "out");
    let final_id = find_node(&graph, "system.final_output");
    let out_res = plan
        .steps()
        .iter()
        .find(|s| s.node == final_id)
        .and_then(|s| s.inputs.iter().find(|(n, _)| *n == "in").map(|(_, r)| *r))
        .expect("final_output consumes a texture");

    let src_rt = RenderTarget::new(device, w, h, FMT, "freeze-fb-src");
    {
        let mut e = device.create_encoder("freeze-fb-src-fill");
        e.copy_texture_to_texture(input, &src_rt.texture, w, h, 1);
        e.commit_and_wait_completed();
    }
    let out_rt = RenderTarget::new(device, w, h, FMT, "freeze-fb-out");
    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FMT);
    backend.pre_bind_texture_2d(src_res, src_rt);
    let out_slot = backend.pre_bind_texture_2d(out_res, out_rt);
    crate::node_graph::pre_allocate_resources(&graph, &plan, device, &mut backend)
        .expect("pre-allocate");

    let mut exec = Executor::new(Box::new(backend));
    let mut state = StateStore::new();
    for i in 0..frames {
        let ft = FrameTime {
            beats: Beats(f64::from(i) / 30.0),
            seconds: Seconds(f64::from(i) / 60.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: i64::from(i),
        };
        let mut enc = device.create_encoder("freeze-fb-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(&mut graph, &plan, ft, &mut gpu, &mut state, 0);
        }
        enc.commit_and_wait_completed();
    }

    let result = RenderTarget::new(device, w, h, FMT, "freeze-fb-result");
    let out_tex = exec.backend().texture_2d(out_slot).expect("output retained");
    {
        let mut e = device.create_encoder("freeze-fb-copy");
        e.copy_texture_to_texture(&out_tex.clone(), &result.texture, w, h, 1);
        e.commit_and_wait_completed();
    }
    result
}

/// BUG-175 (2026-07-16 FilmGrain stage freeze): absorbing the noise atom into
/// the soften blur's fetch inlined ~860 KB of WGSL (35 fetch sites × 4 corners
/// × ~6 KB noise body) and cost ~50 s of synchronous kernel compile on the
/// content thread per build — then once more for the specialized variant. The
/// `MAX_VIRTUAL_INLINE_BYTES` gate in `chain_is_absorbable` now refuses that
/// absorption; the region collapses below `MIN_REGION_LEN`, so FilmGrain
/// renders fully unfused (each node its own cheap dispatch). Watercolor's
/// warp-into-blur absorption (~75 KB) must keep fusing — proven by
/// `watercolor_inloop_chain_fusion_matches_unfused` below.
#[test]
fn filmgrain_noise_absorption_refused_by_inline_budget() {
    let registry = PrimitiveRegistry::with_builtin();
    let base = crate::node_graph::loaded_preset_view_by_id(&manifold_core::PresetTypeId::new(
        "FilmGrain",
    ))
    .expect("FilmGrain view");
    assert!(
        super::install::fuse_canonical_def(&base.canonical_def, &registry).is_none(),
        "FilmGrain must not fuse: its only region is the noise-into-blur \
         absorption the BUG-175 inline-size gate refuses"
    );
}

/// The REAL Watercolor preset, fused vs unfused, 8 feedback frames. The fused
/// def carries an IN-LOOP stencil region (the Linear diffuse blur with the
/// uv-displace warp absorbed into its fetch) plus two in-loop pointwise
/// regions (tier-A q16) — the whole wet path lives inside node.feedback's
/// cycle, so any per-frame rounding gap would compound visibly across frames.
/// Texel-exact taps + q16'd chain tail keep the loop bit-faithful by
/// induction; this is the live-path guarantee for the editor==stage line.
#[test]
fn watercolor_inloop_chain_fusion_matches_unfused() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = noise_input(&device, w, h);

    let base = crate::node_graph::loaded_preset_view_by_id(&manifold_core::PresetTypeId::new(
        "Watercolor",
    ))
    .expect("Watercolor view");
    let def = &*base.canonical_def;

    let fused = super::install::fuse_canonical_def(def, &registry)
        .expect("Watercolor fuses")
        .def;
    assert!(
        !fused.nodes.iter().any(|n| n.type_id == "node.uv_displace_by_flow"),
        "the warp must be absorbed into the diffuse blur's fetch"
    );

    let u_img = render_effect_frames_with_state(&device.arc(), &registry, def, &input, 8);
    let f_img = render_effect_frames_with_state(&device.arc(), &registry, &fused, &input, 8);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, 1.0e-3, 1.0e-2);
    eprintln!(
        "[watercolor in-loop] 8 frames fused vs unfused: max_abs={} over={}/{}",
        r.max_abs, r.over_count, r.total
    );
    assert!(
        r.passes(0.002) && r.over_count < 64,
        "in-loop stencil fusion must hold across feedback frames: \
         max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Look-equivalence for the Watercolor/Bloom preset swap: a legacy `node.blur`
/// (monolithic H+V with an internal f16 scratch) renders identically to an
/// explicit pair of `node.gaussian_blur` passes in `Linear` mode — the exact
/// port of blur.wgsl's loop, with the same f16 texture between the axes. Both
/// run unfused here; agreement is to f16-ulp scale (separate kernel
/// compilations may differ in FMA contraction). This is what licenses
/// rewriting the presets onto the fusable single-axis atom.
#[test]
fn linear_blur_pair_matches_legacy_blur_node() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = noise_input(&device, w, h);

    let legacy = r#"{
        "version": 1, "name": "legacy", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.blur", "nodeId": "blur",
              "params": { "radius": { "type": "Float", "value": 8.0 },
                          "mode": { "type": "Enum", "value": 0 } } },
            { "id": 2, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "source" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
        ]
    }"#;
    let pair = r#"{
        "version": 1, "name": "pair", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.gaussian_blur", "nodeId": "blur_h",
              "params": { "radius_mode": { "type": "Enum", "value": 2 },
                          "radius": { "type": "Float", "value": 8.0 },
                          "axis": { "type": "Enum", "value": 0 } } },
            { "id": 2, "typeId": "node.gaussian_blur", "nodeId": "blur_v",
              "params": { "radius_mode": { "type": "Enum", "value": 2 },
                          "radius": { "type": "Float", "value": 8.0 },
                          "axis": { "type": "Enum", "value": 1 } } },
            { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
        ]
    }"#;

    let l_def: EffectGraphDef = serde_json::from_str(legacy).unwrap();
    let mut l_graph = l_def.into_graph(&registry).expect("legacy graph");
    let l_plan = compile(&l_graph).expect("compile legacy");
    let l_src = resource_for_output(&l_plan, find_node(&l_graph, "system.source"), "out");
    let l_out = resource_for_output(&l_plan, find_node(&l_graph, "node.blur"), "out");
    let l_img = render_graph(&device.arc(), &mut l_graph, &l_plan, l_src, &input, l_out);

    let p_def: EffectGraphDef = serde_json::from_str(pair).unwrap();
    let mut p_graph = p_def.into_graph(&registry).expect("pair graph");
    let p_plan = compile(&p_graph).expect("compile pair");
    let p_src = resource_for_output(&p_plan, find_node(&p_graph, "system.source"), "out");
    let p_out = {
        let blur_v = p_graph
            .nodes()
            .filter(|n| n.node.type_id().as_str() == "node.gaussian_blur")
            .map(|n| n.id)
            .max_by_key(|id| id.0)
            .expect("blur_v present");
        resource_for_output(&p_plan, blur_v, "out")
    };
    let p_img = render_graph(&device.arc(), &mut p_graph, &p_plan, p_src, &input, p_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &l_img.texture, &p_img.texture, 1.0e-3, 1.0e-3);
    assert!(
        r.max_abs < 1.5e-3,
        "Linear pair must reproduce node.blur to f16-ulp scale: max_abs={}, over={}/{}",
        r.max_abs,
        r.over_count,
        r.total
    );
}

/// Coverage baseline — a regression guard on how much of the shipped library the
/// finder fuses. Walks every bundled preset (effect AND generator — P5/D4:
/// this test used to walk `PresetKind::Effect` only, silently excluding the
/// entire generator library from the ratchet), FLATTENS any node groups
/// first (P5/D4: it used to partition the raw `canonical_def` directly,
/// which `partition_regions` refuses outright the moment it sees a `group`
/// node — every grouped preset, effect or generator, silently contributed
/// zero to this floor), and tallies the presets that fuse + the total atoms
/// folded into kernels. A future change that silently turns the partition
/// conservative (everything a boundary) would drop these counts below the
/// floor and trip here. The floor is deliberately loose — it tracks "fusion
/// is broadly alive", not an exact number that churns as the atom
/// vocabulary lands. The exact counts are logged, never asserted.
///
/// Both blind spots were invisible before P5 because nothing this design's
/// earlier phases lifted lived in a generator or a grouped preset in a way
/// that mattered to this specific ratchet — P5's Vec3/Vec4/Color lift is the
/// first change whose real shipped-content proof (`node.shininess`/
/// `node.rim_light`/`node.matcap_two_tone` in OilyFluid, `node.brightness` in
/// MetallicGlass, `node.channel_mixer` in StarField — all THREE are
/// `generator-presets/*.json`, and OilyFluid/MetallicGlass are additionally
/// grouped) fell entirely inside them. Fixing the walk at its root (widen the
/// kind filter, flatten before partitioning) is what makes the floor able to
/// honestly move for this phase, instead of asserting a stale non-regression
/// number that never actually re-measures the thing D4 cares about.
#[test]
fn fusion_coverage_baseline() {
    let registry = PrimitiveRegistry::with_builtin();
    let mut fused_presets = 0usize;
    let mut total_fused_atoms = 0usize;
    let mut total_regions = 0usize;
    let mut detail: Vec<String> = Vec::new();

    for kind in [manifold_core::preset_def::PresetKind::Effect, manifold_core::preset_def::PresetKind::Generator]
    {
        for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(kind) {
            let Some(base) = crate::node_graph::loaded_preset_view_by_id(&type_id) else {
                continue;
            };
            let Ok(flat) = manifold_core::flatten::flatten_groups(&base.canonical_def) else {
                continue;
            };
            let regions = super::region::partition_regions(&flat, &registry);
            if regions.is_empty() {
                continue;
            }
            let atoms: usize = regions.iter().map(|r| r.members.len()).sum();
            fused_presets += 1;
            total_regions += regions.len();
            total_fused_atoms += atoms;
            detail.push(format!(
                "  {}: {} region(s), {atoms} atom(s)",
                type_id.as_str(),
                regions.len()
            ));
        }
    }
    detail.sort();
    eprintln!(
        "[freeze coverage] {fused_presets} preset(s) fuse, {total_regions} region(s), \
         {total_fused_atoms} atom(s) folded:\n{}",
        detail.join("\n")
    );

    // Floor LOWERED on the preset count, 2026-07-17 (BUG-183) — this is not a
    // partition regression, so lowering is the correct fix rather than the
    // backlog entry's default assumption. Root cause: commit `a065dec4`
    // (2026-07-16) unbundled eight 3D-infra presets out to
    // `assets/reference-presets/` (no longer part of the bundled set this
    // test walks); CinematicScene was one of them, and it used to fuse — its
    // fused-WGSL golden was deleted in the same commit. That alone drops the
    // bundled fused-preset count by one. Meanwhile regions/atoms RATCHETED
    // UP from unrelated post-P6 work landed since. Measured at tip `1a161d91`
    // (this session, via the test's own `eprintln!` above): 32 presets / 56
    // regions / 243 atoms — preset floor moves 33 → 32 (CinematicScene's
    // departure, verified not a regression elsewhere: every other preset
    // that fused before still fuses); regions floor moves 55 → 56 and atoms
    // floor moves 225 → 240 (measured 243, small churn headroom per this
    // test's own convention).
    assert!(
        fused_presets >= 32,
        "expected ≥32 bundled presets to fuse, got {fused_presets} — partition regressed?"
    );
    assert!(
        total_regions >= 56,
        "expected ≥56 regions library-wide, got {total_regions} — partition regressed?"
    );
    assert!(
        total_fused_atoms >= 240,
        "expected ≥240 atoms folded library-wide, got {total_fused_atoms} — partition regressed?"
    );
}

/// Grouped presets must fuse. The fuse entry (`fuse_canonical_def`) flattens its
/// input the way the live loader does — otherwise a preset organised into node
/// groups silently never fuses (`partition_regions` refuses any def still
/// carrying a group node). Glitch (a grouped EFFECT) and FluidSim2D (a
/// grouped GENERATOR) are the two shipped grouped presets whose flattened forms
/// have regions; both must produce a fused view/def through the real entry
/// points. Guards the flatten-before-fuse fix against regression.
#[test]
fn grouped_presets_fuse_through_entry_points() {
    use super::install::{fused_generator_def_by_id, fused_view_by_id};
    use manifold_core::PresetTypeId;

    assert!(
        fused_view_by_id(&PresetTypeId::new("Glitch")).is_some(),
        "Glitch is a grouped effect with fusable regions once flattened — \
         fuse_canonical_def must flatten before partitioning"
    );
    assert!(
        fused_generator_def_by_id(&PresetTypeId::new("FluidSim2D")).is_some(),
        "FluidSim2D is a grouped generator with a fusable region once flattened — \
         the generator fuse path must flatten too"
    );
}

/// Library-wide safety net for the LIVE generator fused path (the registry now
/// loads bundled generators through their fused def when the gate keeps it). Every
/// generator the finder fuses must build + render one frame through the real
/// [`JsonGraphGenerator`] path without panicking — the generator twin of
/// `every_fused_preset_executes_one_frame`. Renders only; per-generator numerical
/// agreement is `fused_generator_renders_like_unfused`'s job. This catches the
/// "does the live fused generator even run" class across the whole library.
#[test]
fn every_fused_generator_executes_one_frame() {
    use super::install::fused_generator_def_by_id;
    use crate::preset_context::PresetContext;
    use crate::preset_runtime::PresetRuntime;
    use std::panic::AssertUnwindSafe;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (192u32, 192u32);
    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let mut failures: Vec<String> = Vec::new();
    let mut fused_count = 0usize;

    for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Generator) {
        let Some(fused_def) = fused_generator_def_by_id(&type_id) else {
            continue;
        };
        fused_count += 1;
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut g = PresetRuntime::from_def_with_device(
                (*fused_def).clone(),
                &registry,
                device.arc(),
                w,
                h,
                FMT,
                None,
            )
            .expect("fused generator builds");
            let target = RenderTarget::new(&device, w, h, FMT, "fused-gen-smoke");
            let mut enc = device.create_encoder("fused-gen-smoke");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                g.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
            }
            enc.commit_and_wait_completed();
        }));
        if let Err(panic) = result {
            let msg = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&'static str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "<non-string panic>".to_string());
            failures.push(format!("{}: {msg}", type_id.as_str()));
        }
    }

    assert!(
        failures.is_empty(),
        "{fused_count} generators fuse; these panicked rendering their fused view:\n  - {}",
        failures.join("\n  - "),
    );
}

/// Generator fusion oracle — a generator renders identically fused vs unfused
/// through the REAL [`JsonGraphGenerator`] path, including a `preset_metadata`
/// binding driving a fused-away inner param. checkerboard (Source, non-black) →
/// gain → invert; the binding sets gain to 2.0 (≠ the atom default 1.0), so on the
/// non-black pattern the gain materially changes the pixels. Unfused applies the
/// binding to `gain.gain`; fused applies it to the re-anchored `n1_gain` on the
/// fused kernel. If the binding retarget were wrong, the fused gain would fall
/// back to its default and the frames would diverge. This drives the actual
/// generator render + binding-application path the live registry uses.
#[test]
fn fused_generator_renders_like_unfused() {
    use super::install::fuse_generator_def;
    use crate::preset_context::PresetContext;
    use crate::preset_runtime::PresetRuntime;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);

    let json = r#"{
        "version": 1, "name": "FuseGen",
        "presetMetadata": {
            "id": "FuseGen", "displayName": "Fuse Gen", "category": "Diagnostic",
            "oscPrefix": "fuse_gen",
            "params": [{ "id": "g", "name": "Gain", "min": 0.0, "max": 4.0, "defaultValue": 2.0 }],
            "bindings": [{ "id": "g", "label": "Gain", "defaultValue": 2.0,
                "target": { "kind": "node", "nodeId": "gain", "param": "gain" } }]
        },
        "nodes": [
            { "id": 0, "typeId": "system.generator_input", "nodeId": "gen_in" },
            { "id": 1, "typeId": "node.checkerboard", "nodeId": "checker" },
            { "id": 2, "typeId": "node.exposure", "nodeId": "gain" },
            { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
            { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
            { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
        ]
    }"#;
    let canonical: EffectGraphDef = serde_json::from_str(json).unwrap();
    let fused_def = fuse_generator_def(&canonical, &registry).expect("the generator fuses");

    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: w as f32 / h as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let render = |def: EffectGraphDef| -> RenderTarget {
        let mut g =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, FMT, None)
                .expect("generator builds");
        let target = RenderTarget::new(&device, w, h, FMT, "freeze-gen-out");
        let mut enc = device.create_encoder("freeze-gen");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            g.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
        target
    };

    let unfused = render(canonical);
    let fused = render(fused_def);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &unfused.texture, &fused.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005) && r.over_count < 64,
        "fused generator must match unfused (binding applied): max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// D4/P6 — MULTI-output texture atom fusion. `node.voronoi_2d` ("cells")
/// declares TWO texture outputs (`out`, `cell_id`); this graph wires only
/// `cell_id` into `node.hash_field_by_seed` (the real VoronoiPrism shape —
/// `docs/FUSION_SOTA_DESIGN.md` D4 names this exact pair as the family's
/// palette example). Before this phase, cut rule 6 (`tex_out != 1`) forced
/// voronoi to `Boundary` unconditionally, so this pair never fused. After the
/// narrowing (`tex_out == 0` boundary only) plus the struct-return texture
/// wrapper in `generate_fused` (the `N{i}BodyOutputs` struct + `InputSource::
/// NodeOutput` field pick), the two atoms must fuse into ONE region and the
/// fused kernel must render pixel-identical to the two-dispatch unfused graph
/// — proving the mechanism reads the RIGHT struct field (`cell_id`, not
/// `out`) through the register.
#[test]
fn voronoi_multi_output_fuses_with_pointwise_neighbor_and_matches_unfused() {
    use super::install::fuse_generator_def;
    use super::region::{NodeClass, classify_node, partition_regions};
    use crate::preset_context::PresetContext;
    use crate::preset_runtime::PresetRuntime;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (128u32, 128u32);

    let json = r#"{
        "version": 1, "name": "VoronoiMultiOutputFuse",
        "nodes": [
            { "id": 0, "typeId": "system.generator_input", "nodeId": "gen_in" },
            { "id": 1, "typeId": "node.voronoi_2d", "nodeId": "cells",
              "params": { "scale": { "type": "Float", "value": 6.0 },
                          "jitter": { "type": "Float", "value": 1.0 } } },
            { "id": 2, "typeId": "node.hash_field_by_seed", "nodeId": "hash",
              "params": { "seed": { "type": "Float", "value": 3.0 } } },
            { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 1, "fromPort": "cell_id", "toNode": 2, "toPort": "field" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
        ]
    }"#;
    let canonical: EffectGraphDef = serde_json::from_str(json).unwrap();

    // Structural claim first: voronoi (2 texture outputs) now classifies
    // Eligible, and the two atoms union into ONE region — not two boundaries.
    let cells_node = canonical.nodes.iter().find(|n| n.id == 1).unwrap();
    assert_eq!(
        classify_node(cells_node, &canonical, &registry),
        NodeClass::Eligible,
        "voronoi_2d must classify Eligible now that cut rule 6 admits tex_out >= 1"
    );
    let regions = partition_regions(&canonical, &registry);
    assert_eq!(regions.len(), 1, "cells + hash must union into one region");
    assert_eq!(
        regions[0].members.iter().map(|m| m.doc_id).collect::<Vec<_>>(),
        vec![1, 2],
        "voronoi (head) + hash_field_by_seed, in topo order"
    );

    let fused_def = fuse_generator_def(&canonical, &registry).expect("the pair fuses");

    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: w as f32 / h as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let render = |def: EffectGraphDef| -> RenderTarget {
        let mut g =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, FMT, None)
                .expect("generator builds");
        let target = RenderTarget::new(&device, w, h, FMT, "voronoi-multi-out");
        let mut enc = device.create_encoder("voronoi-multi-out");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            g.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
        target
    };

    let unfused = render(canonical);
    let fused = render(fused_def);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &unfused.texture, &fused.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005) && r.over_count < 64,
        "fused voronoi+hash must match unfused (right BodyOutputs field threaded): max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// D4/P6, real-preset half: `Glitch.json` (bundled, grouped) wires BOTH of
/// `node.block_displace_field`'s texture outputs (`offset` RG into the field
/// sum, `raw_hash` R into the invert-accent gate) to DIFFERENT downstream
/// consumers — the shipped preset the narrowed cut rule 6 actually promotes
/// from Boundary to Eligible, not just voronoi. Renders the real bundled def
/// (auto-fused via `fuse_canonical_def`, which flattens Glitch's groups first)
/// against the unfused canonical graph with the effect's master `amount`
/// cranked to 1.0 (the default 0.0 crossfades the whole effect out, which
/// would hide a wrong-field bug in the final pixels) — both BodyOutputs
/// fields must thread to their correct consumer or the block-tear / invert-
/// flash pattern diverges.
#[test]
fn glitch_block_displace_field_multi_output_matches_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input(&device, w, h);

    let json =
        crate::node_graph::bundled_presets::bundled_preset_json(&manifold_core::PresetTypeId::new(
            "Glitch",
        ))
        .expect("Glitch is a bundled preset");
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse Glitch.json");

    // ── Unfused: the shipped (grouped) preset graph, amount cranked on. ──
    let mut unfused_graph = def.clone().into_graph(&registry).expect("unfused graph");
    let set_by_handle = |g: &mut Graph, handle: &str, param: &str, v: f32| {
        let id = g
            .node_id_by_handle(handle)
            .unwrap_or_else(|| panic!("unfused graph missing handle `{handle}`"));
        g.set_param(id, param, ParamValue::Float(v))
            .unwrap_or_else(|e| panic!("set {handle}.{param}: {e:?}"));
    };
    set_by_handle(&mut unfused_graph, "amount_value", "value", 1.0);
    let unfused_plan = compile(&unfused_graph).expect("compile unfused");
    let u_src = resource_for_output(&unfused_plan, find_node(&unfused_graph, "system.source"), "out");
    let u_glitch = unfused_graph.node_id_by_handle("glitch").expect("unfused `glitch` mix node");
    let u_out = resource_for_output(&unfused_plan, u_glitch, "out");
    let unfused = render_graph(&device.arc(), &mut unfused_graph, &unfused_plan, u_src, &input, u_out);

    // ── Auto-fused: flatten groups, region-grow (now admits the multi-output
    // block_displace_field member), def-rewrite, run through the executor. ──
    let FusedDef { def: fused_def, retarget, .. } =
        fuse_canonical_def(&def, &registry).expect("Glitch is fusable once flattened");
    let mut fused_graph = fused_def.clone().into_graph(&registry).expect("fused graph builds");
    // `amount_value` fans out to FOUR different consumers (both fields, the
    // invert gain, the final crossfade) that land in DIFFERENT regions once
    // fused — a node can only ever be one region's member, so it survives as
    // its own free-standing node in both graphs rather than being absorbed
    // (retarget only covers params that DID move onto a fused kernel).
    // Resolve it directly, same as the unfused side.
    let (fused_amount_node, field) = match retarget
        .get(&("amount_value".to_string(), "value".to_string()))
    {
        // retarget maps (unfused handle, unfused param) -> (fused node's
        // STABLE node_id, field name) — resolve through `instance_by_node_id`
        // (the stable identity), never `node_id_by_handle` (a fused node's
        // handle is synthetic, `fused_region_<i>`, and irrelevant here).
        Some((target_node_id, field)) => (
            fused_graph
                .instance_by_node_id(target_node_id)
                .unwrap_or_else(|| panic!("fused graph missing retargeted amount node")),
            field.as_str(),
        ),
        None => (
            fused_graph
                .node_id_by_handle("amount_value")
                .unwrap_or_else(|| panic!("amount_value must survive if not retargeted")),
            "value",
        ),
    };
    fused_graph
        .set_param(fused_amount_node, field, ParamValue::Float(1.0))
        .unwrap_or_else(|e| panic!("set fused amount: {e:?}"));
    let fused_plan = compile(&fused_graph).expect("compile fused");
    let f_src = resource_for_output(&fused_plan, find_node(&fused_graph, "system.source"), "out");
    // Resolve the final output's producer STRUCTURALLY from the fused def's
    // own wiring (robust whether `glitch` survived as itself or fused away
    // into a `fused_region_<i>` kernel — never guess the handle).
    let fo_doc = fused_def
        .nodes
        .iter()
        .find(|n| n.type_id == "system.final_output")
        .expect("fused def has a final_output")
        .id;
    let out_wire = fused_def
        .wires
        .iter()
        .find(|w| w.to_node == fo_doc)
        .expect("final_output has a producer wire");
    let producer_doc = fused_def
        .nodes
        .iter()
        .find(|n| n.id == out_wire.from_node)
        .expect("producer node exists in fused def");
    let f_out_node = fused_graph
        .instance_by_node_id(&producer_doc.node_id)
        .unwrap_or_else(|| panic!("fused graph missing producer instance for final_output"));
    let f_out = resource_for_output(&fused_plan, f_out_node, &out_wire.from_port);
    let fused = render_graph(&device.arc(), &mut fused_graph, &fused_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &unfused.texture, &fused.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005) && r.over_count < 64,
        "auto-fused Glitch (block_displace_field's two outputs threaded to \
         different consumers) must match unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// BUFFER-domain fusion end-to-end parity: the real DigitalPlants generator —
/// whose GPU per-instance chain (instance_position_jitter → lerp_instance_fields
/// → instance_rotation_jitter) fuses into one `var<storage>` kernel writing back
/// to the aliased instance buffer in place — must render frame-for-frame like the
/// unfused preset. Drives the REAL JsonGraphGenerator path (CPU curve atoms,
/// particle/instance buffers, the fused kernel, the line renderer) with a short
/// warmup so the instance buffers populate, then compares the rendered frame. A
/// wrong buffer fuse (mis-threaded register, wrong alias target, corrupted
/// in-place write) diverges the geometry → the rendered lines move → fails.
/// This is the buffer analogue of `fused_generator_renders_like_unfused`.
///
/// Bit-exact: the write-only-output model fixed the execution-ordering bug and
/// the compute `arrayLength()` buffer-size-buffer index fix (manifold-gpu) closed
/// the residual — fused renders identically to unfused (0/160000 instance diffs).
#[test]
fn digitalplants_buffer_fusion_renders_like_unfused() {
    use super::install::fuse_generator_def;
    use crate::preset_context::PresetContext;
    use crate::preset_runtime::PresetRuntime;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);

    let json = crate::node_graph::bundled_presets::bundled_preset_json(&manifold_core::PresetTypeId::new("DigitalPlants"))
        .expect("DigitalPlants preset bundled");
    let canonical: EffectGraphDef = serde_json::from_str(&json).unwrap();
    // The whole point: DigitalPlants' GPU per-instance chain must fuse into a
    // buffer kernel that BUILDS (the aliased-output model). If this is None the
    // buffer-fusion activation regressed.
    let fused_def =
        fuse_generator_def(&canonical, &registry).expect("DigitalPlants buffer region fuses + builds");

    let ctx = |t: f64| PresetContext {
        time: t,
        beat: t * 2.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: w as f32 / h as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    // Warm up a few frames (instance/particle buffers populate), then capture.
    let render = |def: EffectGraphDef| -> RenderTarget {
        let mut g = PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, FMT, None)
            .expect("generator builds");
        let target = RenderTarget::new(&device, w, h, FMT, "freeze-dp-out");
        for i in 0..6u32 {
            let mut enc = device.create_encoder("freeze-dp");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                g.render(&mut gpu, &target.texture, &ctx(i as f64 / 60.0), &manifold_core::params::ParamManifest::default());
            }
            enc.commit_and_wait_completed();
        }
        target
    };

    let unfused = render(canonical);
    let fused = render(fused_def);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &unfused.texture, &fused.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.01) && r.over_count < 256,
        "fused DigitalPlants must render like unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// BUFFER-domain fusion with FRAME-DERIVED uniforms: the real FluidSim2D —
/// whose per-particle hot chain (noise force, euler integrate with `dt_scaled`,
/// wrap, anti-clump with `frame_count`, …) only fuses now that the codegen emits
/// each member's derived uniform as an `n{i}_<name>` field and
/// `node.wgsl_compute` recomputes its VALUE every frame via
/// `derived_uniform_registry::recompute` (D7/P0 — this test predates that
/// mechanism and originally asserted the install-time `system.generator_input`
/// control wire it replaced; see the marker check below) — must render
/// frame-for-frame like the unfused preset.
///
/// This is the test the whole buffer-chain-fusion build is gated on, and it only
/// became POSSIBLE after the determinism fix: a chaotic feedback sim amplifies any
/// divergence, so a non-deterministic render could never be its own oracle. Buffer
/// fusion threads f32 element registers (no f16 round-trip between atoms), so the
/// particle math is bit-identical and the chaotic trajectories stay locked — a
/// wrong derived-uniform wire (dt_scaled defaulting to 0 → frozen particles; a
/// frame_count off-by-one → decorrelated jitter) diverges the cloud and fails.
///
/// This was once blocked: FluidSim's particle buffer flows through array_feedback
/// IN PLACE, and a fused region writing a fresh `// @fused_output` buffer broke the
/// in==out aliasing (array_feedback fell to copy-delay = one extra frame of
/// latency, and the chaotic sim diverged ~15% at frame 1). The fix: the install
/// pass detects a feedback-loop region (`region_output_aliases_external` +
/// `external_is_inplace_loop`) and the codegen writes the result back to the
/// aliased `src_k` buffer in place, keeping array_feedback in-place. This test is
/// the proof that holds it correct.
///
/// FULL fusion — texture flow-field region AND the buffer particle region — and
/// it's bit-exact because the loop's texture INTERMEDIATES (grad, grad_scaled) are
/// declared rgba32float in the preset. At full precision the unfused chain stores
/// each intermediate exactly and the fused kernel keeps it in an f32 register
/// exactly, so there is NO rounding gap to amplify (the f16 gap that the chaotic
/// sim blew up). This is the edit-vs-perform guarantee: the editor renders the
/// region unfused, performance renders it fused, and at full precision they are
/// identical — the look can't shift when the editor closes. (The fused path keeps
/// those intermediates in registers, so the fp32 textures only exist while editing
/// — zero cost on stage.)
#[test]
fn fluidsim_buffer_fusion_renders_like_unfused() {
    use super::install::fuse_generator_def;
    use crate::preset_context::PresetContext;
    use crate::preset_runtime::PresetRuntime;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);

    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("FluidSim2D"),
    )
    .expect("FluidSim2D preset bundled");
    let canonical: EffectGraphDef = serde_json::from_str(&json).unwrap();
    let fused_def = fuse_generator_def(&canonical, &registry)
        .expect("FluidSim2D fuses + builds (derived-uniform buffer region)");

    // The build's whole point: a derived-uniform particle atom must actually have
    // fused. D7/P0 deleted the install-time `system.generator_input` control-wire
    // whitelist this check used to look for (`node.wgsl_compute` now recomputes
    // derived uniforms itself every frame via `derived_uniform_registry`) — the
    // non-vacuous proof is now the `// @derived_uniform_member:` marker
    // `emit_derived_uniform_markers` carries on any fused kernel with a
    // derived-uniform member (euler_step's `dt_scaled`, the diffuse/anti-clump
    // forces' `frame_count`). If no fused kernel carries the marker, the
    // derived-uniform region stayed unfused and this test would pass vacuously.
    let has_derived_uniform_member = fused_def.nodes.iter().any(|n| {
        n.type_id == "node.wgsl_compute"
            && n.wgsl_source.as_deref().is_some_and(|s| {
                s.lines()
                    .any(|l| matches!(Marker::parse(l), Some(Marker::DerivedUniformMember { .. })))
            })
    });
    assert!(
        has_derived_uniform_member,
        "FluidSim fusion must carry a @derived_uniform_member marker on its fused \
         kernel — no marker means the derived-uniform region never fused (vacuous pass)"
    );

    // The live-count dispatch cap must engage: euler+wrap agree on one
    // active_count producer, so the fused kernel carries the marker that lets
    // node.wgsl_compute dispatch live particles instead of pool capacity
    // (without it the fused kernel iterates the full pool — 2.69 ms vs the
    // standalone atoms' 1.37 at show scale). The render diff below then proves
    // the capped kernel leaves the pool tail bit-identical to unfused.
    assert!(
        fused_def.nodes.iter().any(|n| n.wgsl_source.as_deref().is_some_and(|s| {
            s.lines().any(|l| matches!(
                Marker::parse(l),
                Some(Marker::DispatchCountParam { field }) if field == "n0_active_count"
            ))
        })),
        "fused particle kernel must carry the live-count dispatch marker"
    );

    // The in-loop texture path must actually fire — at F16. The flow-field
    // atoms (grad → scale → rotate) fuse through the q16 f16-faithful tier:
    // an f16 `dst` plus the `q16(...)` register-rounding wrapper that
    // reproduces the unfused f16 store/load. f16 is the engine's texture
    // currency (2026-06-10 decision): the old rgba32float overrides existed
    // only as a pre-q16 parity workaround, and they doubled every downstream
    // consumer's bandwidth AND broke the gaussian blur's bilinear tap-pair
    // trick (fp32 textures aren't filterable on Apple GPUs). No rgba32float
    // dst may appear; the q16 wrapper must.
    let fused_texture_kernels: Vec<&str> = fused_def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.wgsl_compute")
        .filter_map(|n| n.wgsl_source.as_deref())
        .filter(|src| src.contains("texture_storage_2d<"))
        .collect();
    assert!(
        !fused_texture_kernels
            .iter()
            .any(|src| src.contains("texture_storage_2d<rgba32float, write>")),
        "no fused FluidSim texture kernel may declare an rgba32float dst — fp32 \
         textures are reserved for explicit data-texture opt-ins, not fusion policy"
    );
    // No in-loop texture fusion either: with the fp32 marks gone the flow
    // field is f16, and in-loop f16 texture atoms are boundaries (region.rs —
    // q16 reconciles store rounding but not cross-kernel body ULP noise,
    // which the feedback loop amplifies). The flow field renders unfused;
    // the bit-exact diff below holds because unfused IS the reference.
    let _ = &fused_texture_kernels;

    // The toroidal gradient (a `Gather` with wrap_mode=Repeat) must stay
    // UNFUSED now: in-loop gathers at f16 are boundaries (region.rs) because
    // the q16 register round-trip reproduces store rounding but not an f16
    // bilinear gather's interpolation, and the feedback loop amplifies that
    // gap (measured max_abs 0.73 / 31% of pixels, 2026-06-10). Gather fusion
    // with `// @sampler_address_mode` remains available to fp32-opt-in data
    // textures only. Assert the gradient produced NO fused repeat-sampler
    // kernel — if one appears, the boundary rule regressed.
    assert!(
        !fused_def.nodes.iter().any(|n| {
            n.type_id == "node.wgsl_compute"
                && n.wgsl_source
                    .as_deref()
                    .is_some_and(|src| src.contains("@sampler_address_mode: repeat"))
        }),
        "the f16 in-loop toroidal gradient must stay unfused (in-loop gather \
         boundary rule) — a fused repeat-sampler kernel means the rule regressed"
    );

    let ctx = |t: f64| PresetContext {
        time: t,
        beat: t * 2.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: w as f32 / h as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let render = |def: EffectGraphDef| -> RenderTarget {
        let mut g = PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, FMT, None)
            .expect("FluidSim2D builds");
        let target = RenderTarget::new(&device, w, h, FMT, "freeze-fluid-fusion");
        for i in 0..8u32 {
            let mut enc = device.create_encoder("freeze-fluid-fusion");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                g.render(&mut gpu, &target.texture, &ctx(i as f64 / 60.0), &manifold_core::params::ParamManifest::default());
            }
            enc.commit_and_wait_completed();
        }
        target
    };

    let unfused = render(canonical);
    let fused = render(fused_def);

    let differ = TextureDiff::new(&device);
    // Buffer fusion is bit-exact on the particle math (f32 registers, no f16
    // round-trip), so a chaotic sim only stays locked if the derived uniforms are
    // wired correctly. Tight bound — a 0-dt or wrong frame_count blows way past it.
    let r = differ.compare(&device, &unfused.texture, &fused.texture, 1.0e-3, 1.0e-2);
    assert!(
        r.passes(0.002) && r.over_count < 64,
        "fused FluidSim2D must render like unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// FluidSim3D's integrator must fuse WHOLE — including the 3D force sampler.
/// `sample_texture_3d_at_particles` was a deliberate fusion boundary while
/// `node.wgsl_compute` rejected sampled 3D textures at introspection; that
/// fragmented the 8-atom integrator and the fused build measured SLOWER than
/// unfused (0.84x on M4 Max — vetoed by the perf gate), while the original
/// fused `fluid_simulate_3d` kernel proved a single integrate kernel wins on
/// this hardware. With texture_3d introspection + the 3D external declaration
/// in the buffer codegen, the sampler joins its region: assert it's absorbed
/// (no standalone node survives in the fused def) and that a fused kernel
/// actually binds a `texture_3d<f32>` external — then prove render
/// equivalence frame-for-frame against the unfused preset, same oracle as
/// `fluidsim_buffer_fusion_renders_like_unfused` (f32 element registers
/// thread the force values the unfused chain stores in an f32 array, so the
/// chaotic sim only stays locked if the fused sample is the same sample).
#[test]
fn fluidsim3d_buffer_fusion_includes_3d_sampler_and_renders_like_unfused() {
    use super::install::fuse_generator_def;
    use crate::preset_context::PresetContext;
    use crate::preset_runtime::PresetRuntime;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);

    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("FluidSim3D"),
    )
    .expect("FluidSim3D preset bundled");
    let canonical: EffectGraphDef = serde_json::from_str(&json).unwrap();
    let fused_def = fuse_generator_def(&canonical, &registry)
        .expect("FluidSim3D fuses + builds (3D-sampler buffer region)");

    assert!(
        !fused_def.nodes.iter().any(|n| n.type_id == "node.sample_volume_at_particles"),
        "the 3D force sampler must be absorbed into a fused region — a surviving \
         standalone node means the Texture3D gate regressed and the integrator \
         is fragmented again"
    );
    assert!(
        fused_def.nodes.iter().any(|n| {
            n.type_id == "node.wgsl_compute"
                && n.wgsl_source.as_deref().is_some_and(|s| s.contains("texture_3d<f32>"))
        }),
        "a fused kernel must declare a texture_3d<f32> external (the volume \
         force field the integrator samples inline)"
    );

    let ctx = |t: f64| PresetContext {
        time: t,
        beat: t * 2.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: w as f32 / h as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let render = |def: EffectGraphDef| -> RenderTarget {
        let mut g = PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, FMT, None)
            .expect("FluidSim3D builds");
        let target = RenderTarget::new(&device, w, h, FMT, "freeze-fluid3d-fusion");
        for i in 0..8u32 {
            let mut enc = device.create_encoder("freeze-fluid3d-fusion");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                g.render(&mut gpu, &target.texture, &ctx(i as f64 / 60.0), &manifold_core::params::ParamManifest::default());
            }
            enc.commit_and_wait_completed();
        }
        target
    };

    let unfused = render(canonical);
    let fused = render(fused_def);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &unfused.texture, &fused.texture, 1.0e-3, 1.0e-2);
    assert!(
        r.passes(0.002) && r.over_count < 64,
        "fused FluidSim3D must render like unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Determinism guard for the FluidSim2D feedback sim. Rendering the SAME
/// canonical preset twice from fresh state, with an identical frame sequence,
/// must produce the SAME final image. It did NOT before the storage-layer
/// zero-init fix: scatter atomic-adds into a `u32` accumulator that
/// `node.resolve_scatter` clears *after* reading, so the accumulator must
/// start at zero — but the pool handed it freshly-`create_buffer_shared`'d VRAM,
/// which Metal does not zero. Frame 0 therefore resolved the splat ON TOP OF
/// uninitialized garbage into the density texture, which feeds back into
/// `node.anti_clump_particles.strength_modulator`; the chaotic sim then amplified
/// that frame-0 difference permanently, so two runs that allocated different VRAM
/// diverged (~14% of pixels). The fix zero-inits atomic-accumulator buffers at
/// allocation (graph_loader `pre_allocate_resources`), which is also what makes
/// the render-diff a VALID fusion oracle for the buffer-chain fusion work — a
/// non-deterministic render can't be its own ground truth.
///
/// This is show-correctness, not just a test fixture: a non-deterministic sim
/// means the same clip looks different every time it's triggered live.
#[test]
fn fluidsim_renders_deterministically_from_fresh_state() {
    use crate::preset_context::PresetContext;
    use crate::preset_runtime::PresetRuntime;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);

    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("FluidSim2D"),
    )
    .expect("FluidSim2D preset bundled");
    let canonical: EffectGraphDef = serde_json::from_str(&json).unwrap();

    let ctx = |t: f64| PresetContext {
        time: t,
        beat: t * 2.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: w as f32 / h as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    // Warm the feedback loop a handful of frames so any frame-0 divergence has
    // time to amplify through the density→force→position loop, then capture.
    let render = |def: EffectGraphDef| -> RenderTarget {
        let mut g = PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, FMT, None)
            .expect("FluidSim2D builds");
        let target = RenderTarget::new(&device, w, h, FMT, "freeze-fluid-determinism");
        for i in 0..8u32 {
            let mut enc = device.create_encoder("freeze-fluid");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                g.render(&mut gpu, &target.texture, &ctx(i as f64 / 60.0), &manifold_core::params::ParamManifest::default());
            }
            enc.commit_and_wait_completed();
        }
        target
    };

    let run_a = render(canonical.clone());
    let run_b = render(canonical);

    let differ = TextureDiff::new(&device);
    // Identical inputs → bit-exact output. Allow a hair of tolerance only for
    // f16 ULP noise, but the over_count must be ~0 — a garbage-seeded run blows
    // way past this (~14% of pixels diverge by up to 0.83).
    let r = differ.compare(&device, &run_a.texture, &run_b.texture, 1.0e-3, 1.0e-2);
    assert!(
        r.passes(0.002) && r.over_count < 64,
        "FluidSim2D must render deterministically from fresh state: \
         max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Warm a generator preset's feedback loop for 8 frames and capture the final.
fn render_generator_8_frames(
    def: EffectGraphDef,
    registry: &PrimitiveRegistry,
    device: &std::sync::Arc<GpuDevice>,
    w: u32,
    h: u32,
) -> RenderTarget {
    use crate::preset_context::PresetContext;
    use crate::preset_runtime::PresetRuntime;
    let ctx = |t: f64| PresetContext {
        time: t,
        beat: t * 2.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: w as f32 / h as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let mut g = PresetRuntime::from_def_with_device(def, registry, std::sync::Arc::clone(device), w, h, FMT, None)
        .expect("preset builds");
    let target = RenderTarget::new(device, w, h, FMT, "freeze-gen-fusion");
    for i in 0..8u32 {
        let mut enc = device.create_encoder("freeze-gen-fusion");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            g.render(&mut gpu, &target.texture, &ctx(i as f64 / 60.0), &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }
    target
}

/// Remove the `reset_trigger` wire feeding `seed_node_id` — a `@reset_gated`
/// kernel with that input unwired runs every frame (the gate is inert). Used to
/// build the "ungated" baseline for the seed-gate equivalence proofs. Addresses
/// the node by stable `node_id` (the def must be flattened first — grouping
/// nests the seed node and prefixes its handle, but `node_id` survives).
fn strip_reset_wire(def: &mut EffectGraphDef, seed_node_id: &str) {
    let Some(id) = def
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == seed_node_id)
        .map(|n| n.id)
    else {
        return;
    };
    def.wires.retain(|w| !(w.to_node == id && w.to_port == "reset_trigger"));
}

/// A reset-gated in-place buffer seed must render IDENTICALLY whether gated
/// (canonical) or ungated (seed runs every frame): the seed feeds array_feedback
/// only on reset, which both hit on frame 0, so skipping the redundant re-seeds
/// between resets changes nothing. Proves the gate is invisible AND that the
/// aliased seed skips WITHOUT tripping the executor's stale-output guard (a
/// regression there would panic this render). ParticleText: seed_alloc is
/// OnceOnReset, so the buffer persists and the skip relies on real retention.
#[test]
fn particletext_seed_gate_matches_ungated() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("ParticleText"),
    )
    .expect("ParticleText bundled");
    // Flatten so the grouped seed node lifts to the top level; address it by
    // stable node_id (grouping prefixes handles, node_id survives).
    let gated: EffectGraphDef =
        manifold_core::flatten::flatten_groups(&serde_json::from_str(&json).unwrap())
            .expect("flattens");
    let seed_id = gated
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "seed_pattern")
        .map(|n| n.id)
        .expect("seed_pattern node");
    assert!(
        gated.wires.iter().any(|w| w.to_node == seed_id && w.to_port == "reset_trigger"),
        "ParticleText seed_pattern must carry a reset_trigger wire (else gate is vacuous)"
    );
    let mut ungated = gated.clone();
    strip_reset_wire(&mut ungated, "seed_pattern");

    // Render through the RAW executor (unfused), not the fused PresetRuntime.
    // The reset gate is an executor/aliasing property, independent of fusion.
    // Rendering fused pollutes the A/B with the parked f16-seed fused
    // divergence ([[particletext_canonical_fused_diag]]): stripping the reset
    // wire changes the fusion topology, so the two sides fuse into different
    // f16 kernels and diverge for reasons unrelated to the gate. (Same reason
    // `oilyfluid_inloop_f16_fusion_matches_unfused` uses the raw harness.)
    let pick_final = |d: &EffectGraphDef| {
        let fo = d
            .nodes
            .iter()
            .find(|n| n.type_id == "system.final_output")
            .map(|n| n.id)
            .expect("final_output");
        d.wires
            .iter()
            .find(|w| w.to_node == fo)
            .map(|w| w.from_node)
            .expect("final_output fed")
    };
    let (g, gd) =
        render_def_capture_node_host(&gated, &registry, &device.arc(), w, h, 8, &pick_final, true)
            .expect("gated renders");
    let (u, ud) =
        render_def_capture_node_host(&ungated, &registry, &device.arc(), w, h, 8, &pick_final, true)
            .expect("ungated renders");
    assert_eq!(gd, ud, "gated/ungated dims match");
    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &g.texture, &u.texture, 1.0e-3, 1.0e-2);
    assert!(
        r.passes(0.002) && r.over_count < 64,
        "gated ParticleText seed must match ungated: max_abs={}, over={}/{} ({:.4})",
        r.max_abs,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Zero-copy feedback ping-pong equivalence: MetallicGlass (three
/// same-format `node.feedback` loops — the SWAP-eligible shape) rendered
/// with the ping-pong slot swap must match the bridge fallback BIT-EXACTLY
/// over 8 frames — a feedback loop amplifies any state error, and a swap
/// landing one frame off would diverge wildly, so this is the show-safety
/// oracle for the new path. (OilyFluid's fp32-state feedbacks take the
/// bridge on both settings — same copies as before, minus the prev
/// round-trip — so it wouldn't exercise the swap.) Env-var toggled; the
/// unfused def isolates the mechanism from fusion entirely.
#[test]
fn feedback_pingpong_matches_copy_path() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("MetallicGlass"),
    )
    .expect("MetallicGlass bundled");
    let def: EffectGraphDef = serde_json::from_str(&json).unwrap();
    let def = manifold_core::flatten::flatten_groups(&def).expect("flattens");

    let pick_tail = |d: &EffectGraphDef| {
        let fo = d
            .nodes
            .iter()
            .find(|n| n.type_id == "system.final_output")
            .map(|n| n.id)
            .expect("final_output");
        d.wires
            .iter()
            .find(|w| w.to_node == fo)
            .map(|w| w.from_node)
            .expect("final_output fed")
    };
    // SAFETY of the env toggle: this test renders both variants
    // sequentially within one thread; the env var is read per-frame by
    // `Feedback::run`, so each render sees a stable value.
    unsafe { std::env::set_var("MANIFOLD_FEEDBACK_PINGPONG", "0") };
    let copy = render_def_capture_node_host(&def, &registry, &device.arc(), w, h, 8, &pick_tail, true);
    unsafe { std::env::remove_var("MANIFOLD_FEEDBACK_PINGPONG") };
    let pp = render_def_capture_node_host(&def, &registry, &device.arc(), w, h, 8, &pick_tail, true);
    let (copy, cd) = copy.expect("copy path renders");
    let (pp, pd) = pp.expect("ping-pong renders");
    assert_eq!(cd, pd, "composite dims match");
    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &copy.texture, &pp.texture, 1.0e-7, 1.0e-6);
    assert!(
        r.over_count == 0,
        "ping-pong must be bit-exact vs the copy path: max_abs={}, over={}/{}",
        r.max_abs,
        r.over_count,
        r.total
    );
}

/// Stencil tier A proof on the real OilyFluid: its feedback-loop f16 chains
/// (previously hard boundaries) now fuse with `q16` register rounding, and
/// the fused render must match the unfused one BIT-EXACTLY through the raw
/// executor — the loop amplifies any rounding mismatch, so 8 frames at
/// 256² is a real drift test, not a smoke test. (Raw harness, not
/// PresetRuntime: the production path carries the parked
/// [[particletext_canonical_fused_diag]] f16-seed divergence which would
/// pollute this oracle.)
#[test]
fn oilyfluid_inloop_f16_fusion_matches_unfused() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("OilyFluid"),
    )
    .expect("OilyFluid bundled");
    let mut def: EffectGraphDef = serde_json::from_str(&json).unwrap();
    // Texture-domain oracle: shrink any particle pools so this test doesn't
    // starve the GPU when the suite runs it in parallel with the sim renders.
    shrink_particle_pool(&mut def, 100_000);
    // OilyFluid is GROUPED; the raw harness instantiates directly, so flatten
    // first (the live loader and the fuse entry both do).
    let def = manifold_core::flatten::flatten_groups(&def).expect("flattens");
    let fused =
        crate::node_graph::freeze::install::fuse_generator_def(&def, &registry).expect("fuses");

    // The tier actually engaged: the fused def must carry a q16-quantized
    // kernel (an in-loop f16 member fused) — else this oracle is vacuous.
    assert!(
        fused
            .nodes
            .iter()
            .any(|n| n.wgsl_source.as_deref().is_some_and(|s| s.contains("fn q16"))),
        "OilyFluid must fuse at least one in-loop f16 region under tier A"
    );

    let pick_tail = |d: &EffectGraphDef| {
        let fo = d
            .nodes
            .iter()
            .find(|n| n.type_id == "system.final_output")
            .map(|n| n.id)
            .expect("final_output");
        d.wires
            .iter()
            .find(|w| w.to_node == fo)
            .map(|w| w.from_node)
            .expect("final_output fed")
    };
    for frames in [1u32, 8] {
        let (u, ud) = render_def_capture_node_host(
            &def, &registry, &device.arc(), w, h, frames, &pick_tail, true,
        )
        .expect("unfused renders");
        let (f, fd) = render_def_capture_node_host(
            &fused, &registry, &device.arc(), w, h, frames, &pick_tail, true,
        )
        .expect("fused renders");
        assert_eq!(ud, fd, "composite dims match (frames={frames})");
        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &u.texture, &f.texture, 1.0e-7, 1.0e-6);
        assert!(
            r.over_count == 0,
            "in-loop f16 fusion must be bit-exact at frames={frames}: max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }
}

/// Optional-input fusion proof on the real MetallicGlass: its sobel tail
/// (sobel_x/sobel_y → pack_channels → length_vec2 → gain → clamp) only fuses
/// once an UNWIRED OPTIONAL texture input (pack_channels' b/a) is expressible
/// — the codegen passes a zero vector and folds the body's use flag to a
/// literal `0u`. Compared at the REGION OUTPUT (the fused kernel vs unfused
/// edge_clamp) under the established out-of-loop ulp tolerance — NOT at the
/// composite (the PBR render downstream amplifies sub-ulp register noise into
/// specular shimmer) and NOT bit-exact (out-of-loop fused regions carry
/// body-level FMA/inlining ULP noise across kernel contexts; the documented
/// contract is q16 bit-exactness inside loops, ≈ulp outside — see the
/// quantize_f16 comment in region.rs). A use-flag or argument-order bug fails
/// loudly here: the default fallback zeroes a sobel channel and the gradient
/// magnitude collapses, far beyond the tolerance.
#[test]
fn metallicglass_optional_input_fusion_matches_unfused() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("MetallicGlass"),
    )
    .expect("MetallicGlass bundled");
    let mut def: EffectGraphDef = serde_json::from_str(&json).unwrap();
    shrink_particle_pool(&mut def, 100_000); // no pools today; suite-parallelism hygiene
    let def = manifold_core::flatten::flatten_groups(&def).expect("flattens");
    let fused =
        crate::node_graph::freeze::install::fuse_generator_def(&def, &registry).expect("fuses");

    // Non-vacuous: pack_channels must be fused AWAY (it only fuses through the
    // unwired-optional path), and some fused kernel must carry the literal
    // unwired argument the new codegen emits.
    assert!(
        !fused.nodes.iter().any(|n| n.type_id == "node.pack_rgba"),
        "pack_channels must fold into the sobel-tail region"
    );
    assert!(
        fused
            .nodes
            .iter()
            .any(|n| n.wgsl_source.as_deref().is_some_and(|s| s.contains("vec4<f32>(0.0)"))),
        "a fused kernel must carry the unwired-optional zero argument"
    );

    // Unfused: edge_clamp (the region's tail member). Fused: the kernel that
    // carries pack_channels' default params — unique to the sobel-tail region.
    let pick_unfused = |d: &EffectGraphDef| {
        d.nodes
            .iter()
            .find(|n| n.node_id.as_str() == "edge_clamp")
            .map(|n| n.id)
            .expect("edge_clamp present")
    };
    let pick_fused = |d: &EffectGraphDef| {
        d.nodes
            .iter()
            .find(|n| n.wgsl_source.as_deref().is_some_and(|s| s.contains("default_r")))
            .map(|n| n.id)
            .expect("sobel-tail fused kernel present")
    };
    for frames in [1u32, 8] {
        let (u, ud) = render_def_capture_node_host(
            &def, &registry, &device.arc(), w, h, frames, &pick_unfused, true,
        )
        .expect("unfused renders");
        let (f, fd) = render_def_capture_node_host(
            &fused, &registry, &device.arc(), w, h, frames, &pick_fused, true,
        )
        .expect("fused renders");
        assert_eq!(ud, fd, "region output dims match (frames={frames})");
        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &u.texture, &f.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
        assert!(
            r.over_count == 0,
            "fused sobel tail must match unfused within ulp tolerance at frames={frames}: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }
}

/// Tier-6 proof on the real ParticleText: fp32-mark its flow-field atoms
/// (`grad` / `grad_scaled` / `grad_rotate` — the same marks FluidSim2D ships
/// in its grouped field), fuse, and require the fused render to match the
/// unfused one tight. Before element-space propagation this diverged ~0.43%
/// edge-localized — the fused region iterated a different grid than the
/// standalone atoms (the mixed-input canvas fallback). With the region's
/// space resolved from the unfused plan, stamped onto the fused node, and
/// verified by the install build-check, the fusion must now be coincident —
/// or be refused outright (also a pass: unfused is always correct).
#[test]
fn particletext_fp32_flow_field_fused_matches_unfused() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("ParticleText"),
    )
    .expect("ParticleText bundled");
    // Flatten so grouped nodes lift to the top level; address by stable node_id.
    let mut def: EffectGraphDef =
        manifold_core::flatten::flatten_groups(&serde_json::from_str(&json).unwrap())
            .expect("flattens");
    for node_id in ["grad", "grad_scaled", "grad_rotate"] {
        let node = def
            .nodes
            .iter_mut()
            .find(|n| n.node_id.as_str() == node_id)
            .unwrap_or_else(|| panic!("ParticleText carries node `{node_id}`"));
        node.output_formats.insert("out".to_string(), "rgba32float".to_string());
    }
    // Shrink the particle pool ~80×: the flow-field region under test is
    // texture-domain (doesn't depend on particle count), and the shipped 8M
    // pool (~512MB per Array) starves the GPU when the suite runs this test
    // in parallel with the other FluidSim renders.
    shrink_particle_pool(&mut def, 100_000);

    let Some(fused) = crate::node_graph::freeze::install::fuse_generator_def(&def, &registry)
    else {
        // The install verify refused the fusion (space drift it can't stamp
        // away). Refusal renders unfused — correct, just no speedup. Fail
        // here anyway so the refusal is VISIBLE: this preset is the tier-6
        // fixture, and a silent refusal would mean the stamp didn't land.
        panic!("ParticleText fp32 flow field should fuse under tier-6 space propagation");
    };
    // Sanity: the flow-field pointwise pair actually folded away.
    for node_id in ["grad_scaled", "grad_rotate"] {
        assert!(
            !fused.nodes.iter().any(|n| n.node_id.as_str() == node_id),
            "`{node_id}` should be fused away"
        );
    }

    // The tier-6 claim is about the GRID: the fused region must iterate the
    // same element space the standalone atoms did and produce the identical
    // field. Compare the region OUTPUT bitwise (the composite still carries a
    // pre-existing production-path divergence unrelated to this region — see
    // `particletext_canonical_fused_diag`).
    let by_unfused = |d: &EffectGraphDef| {
        d.nodes
            .iter()
            .find(|n| n.node_id.as_str() == "grad_rotate")
            .map(|n| n.id)
            .expect("grad_rotate")
    };
    let by_fused = |d: &EffectGraphDef| {
        d.nodes
            .iter()
            .find(|n| {
                n.type_id == "node.wgsl_compute" && n.params.keys().any(|k| k.ends_with("_angle"))
            })
            .map(|n| n.id)
            .expect("fused flow-field region")
    };
    for frames in [1u32, 8] {
        let (u, ud) =
            render_def_capture_node(&def, &registry, &device.arc(), w, h, frames, &by_unfused)
                .expect("unfused captures");
        let (f, fd) = render_def_capture_node(&fused, &registry, &device.arc(), w, h, frames, &by_fused)
            .expect("fused captures");
        assert_eq!(ud, fd, "fused region must resolve to the member's grid (frames={frames})");
        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &u.texture, &f.texture, 1.0e-7, 1.0e-6);
        assert!(
            r.over_count == 0,
            "fp32 flow-field region must be bit-exact at frames={frames}: max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }
}

/// Diagnostic for the ParticleText fp32 divergence — prints the flow-field
/// region's members/space and the unfused-vs-fused plan resolution of every
/// relevant output. Run with `-- --ignored --nocapture`.
#[test]
#[ignore]
fn particletext_fp32_flow_field_diag() {
    use crate::node_graph::freeze::space::resolve_output_spaces;
    let registry = PrimitiveRegistry::with_builtin();
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("ParticleText"),
    )
    .expect("ParticleText bundled");
    let mut def: EffectGraphDef = serde_json::from_str(&json).unwrap();
    for handle in ["grad", "grad_scaled", "grad_rotate"] {
        let node = def
            .nodes
            .iter_mut()
            .find(|n| n.handle.as_deref() == Some(handle))
            .unwrap();
        node.output_formats.insert("out".to_string(), "rgba32float".to_string());
    }

    let handle_of = |def: &EffectGraphDef, id: u32| -> String {
        def.nodes
            .iter()
            .find(|n| n.id == id)
            .and_then(|n| n.handle.clone())
            .unwrap_or_else(|| format!("#{id}"))
    };

    let regions = crate::node_graph::freeze::region::partition_regions(&def, &registry);
    println!("=== {} regions in fp32-marked ParticleText ===", regions.len());
    for (i, r) in regions.iter().enumerate() {
        let members: Vec<String> =
            r.members.iter().map(|m| handle_of(&def, m.doc_id)).collect();
        let outs: Vec<String> = r.outputs.iter().map(|(o, _)| handle_of(&def, *o)).collect();
        let exts: Vec<String> = r
            .externals
            .iter()
            .map(|e| format!("{}:{}", handle_of(&def, e.from_node), e.from_port))
            .collect();
        println!(
            "region {i}: space={:?}\n  members: {members:?}\n  outputs: {outs:?}\n  externals: {exts:?}",
            r.space
        );
    }

    let unfused_spaces = resolve_output_spaces(&def, &registry).expect("unfused resolves");
    for handle in ["grad", "grad_scaled", "grad_rotate", "field_mix", "density_downsample"] {
        if let Some(n) = def.nodes.iter().find(|n| n.handle.as_deref() == Some(handle)) {
            let s = unfused_spaces.get(&(n.id, "out".to_string()));
            println!("unfused {handle}: {s:?}");
        }
    }

    let fused =
        crate::node_graph::freeze::install::fuse_generator_def(&def, &registry).expect("fuses");
    let fused_spaces = resolve_output_spaces(&fused, &registry).expect("fused resolves");
    for n in &fused.nodes {
        if n.type_id == "node.wgsl_compute" && n.handle.as_deref().is_some_and(|h| h.starts_with("fused_region")) {
            for port in ["dst", "dst_0", "dst_1"] {
                if let Some(s) = fused_spaces.get(&(n.id, port.to_string())) {
                    println!(
                        "fused {}: {port} -> {s:?} (stamped: {:?})",
                        n.handle.as_deref().unwrap_or("?"),
                        n.output_canvas_scales
                    );
                }
            }
        }
    }
}

/// Cap every `max_capacity` / `active_count` param in `def` at `cap` — the
/// particle-pool shrink the FluidSim sweep uses, for tests whose subject is
/// texture-domain and doesn't depend on pool size.
fn shrink_particle_pool(def: &mut EffectGraphDef, cap: i32) {
    use manifold_core::effect_graph_def::SerializedParamValue;
    for node in &mut def.nodes {
        for key in ["max_capacity", "active_count"] {
            if node.params.contains_key(key) {
                node.params
                    .insert(key.to_string(), SerializedParamValue::Int { value: cap });
            }
        }
    }
}

/// Drive `def` through the RAW executor (standalone instantiate, identical on
/// both sides of an A/B — generator_input stays at defaults) for `frames`
/// frames, previewing the node `pick` selects so its output survives the last
/// frame. Returns the previewed texture copied out, plus its dims.
fn render_def_capture_node(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    device: &std::sync::Arc<GpuDevice>,
    w: u32,
    h: u32,
    frames: u32,
    pick: &dyn Fn(&EffectGraphDef) -> u32,
) -> Option<(RenderTarget, (u32, u32))> {
    render_def_capture_node_host(def, registry, device, w, h, frames, pick, false)
}

/// `host_params = true` additionally drives the `system.generator_input`
/// host params (time / beat / aspect / output dims) per frame the way the
/// production `PresetRuntime` path does — the discriminating variable
/// between the raw harness and production.
#[allow(clippy::too_many_arguments)]
fn render_def_capture_node_host(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    device: &std::sync::Arc<GpuDevice>,
    w: u32,
    h: u32,
    frames: u32,
    pick: &dyn Fn(&EffectGraphDef) -> u32,
    host_params: bool,
) -> Option<(RenderTarget, (u32, u32))> {
    use crate::node_graph::graph_loader::{
        BoundaryHandling, HandleScope, instantiate_def, pre_allocate_resources,
    };
    use crate::node_graph::parameters::ParamValue;
    use crate::node_graph::{Graph, StateStore};

    let mut graph = Graph::new();
    let inst = instantiate_def(
        &mut graph,
        def,
        registry,
        HandleScope::Global,
        BoundaryHandling::Standalone,
    )
    .ok()?;
    let plan = compile(&graph).ok()?;
    let target_inst = *inst.id_map.get(&pick(def))?;
    let gen_in = def
        .nodes
        .iter()
        .find(|n| n.type_id == "system.generator_input")
        .and_then(|n| inst.id_map.get(&n.id).copied());

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FMT);
    pre_allocate_resources(&graph, &plan, device, &mut backend).ok()?;
    let mut exec = Executor::new(Box::new(backend));
    exec.set_preview_target(Some(target_inst));
    let mut state = StateStore::new();
    for i in 0..frames {
        let t = f64::from(i) / 60.0;
        if host_params && let Some(gi) = gen_in {
            for (name, v) in [
                ("time", t as f32),
                ("beat", (t * 2.0) as f32),
                ("aspect", w as f32 / h as f32),
                ("output_width", w as f32),
                ("output_height", h as f32),
            ] {
                let _ = graph.set_param(gi, name, ParamValue::Float(v));
            }
        }
        let ft = FrameTime {
            seconds: Seconds(t),
            beats: Beats(t * 2.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: i64::from(i),
        };
        let mut enc = device.create_encoder("diag-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(&mut graph, &plan, ft, &mut gpu, &mut state, 0);
        }
        enc.commit_and_wait_completed();
    }
    let res = exec.preview_resource()?;
    let slot = exec.backend().slot_for(res)?;
    let tex = exec.backend().texture_2d(slot)?;
    let dims = (tex.width, tex.height);
    let out = RenderTarget::new(device, tex.width, tex.height, tex.format, "diag-capture");
    let mut enc = device.create_encoder("diag-copy");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
        gpu.copy_texture_to_texture(tex, &out.texture, tex.width, tex.height);
    }
    enc.commit_and_wait_completed();
    Some((out, dims))
}

/// Like [`render_def_capture_node`], but captures an ARRAY (particle buffer)
/// output of the picked node on the LAST frame via dump mode, read back to
/// host bytes.
fn render_def_capture_array(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    device: &std::sync::Arc<GpuDevice>,
    w: u32,
    h: u32,
    frames: u32,
    pick: &dyn Fn(&EffectGraphDef) -> u32,
) -> Option<Vec<f32>> {
    use crate::node_graph::graph_loader::{
        BoundaryHandling, HandleScope, instantiate_def, pre_allocate_resources,
    };
    use crate::node_graph::{Graph, StateStore};

    let mut graph = Graph::new();
    let inst = instantiate_def(
        &mut graph,
        def,
        registry,
        HandleScope::Global,
        BoundaryHandling::Standalone,
    )
    .ok()?;
    let plan = compile(&graph).ok()?;
    let target_inst = *inst.id_map.get(&pick(def))?;

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FMT);
    pre_allocate_resources(&graph, &plan, device, &mut backend).ok()?;
    let mut exec = Executor::new(Box::new(backend));
    let mut state = StateStore::new();
    for i in 0..frames {
        if i == frames - 1 {
            exec.set_dump_all(true);
        }
        let ft = FrameTime {
            seconds: Seconds(f64::from(i) / 60.0),
            beats: Beats(f64::from(i) / 30.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: i64::from(i),
        };
        let mut enc = device.create_encoder("diag-arr-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(&mut graph, &plan, ft, &mut gpu, &mut state, 0);
        }
        enc.commit_and_wait_completed();
    }
    let &(_, _, res) = exec
        .dump_array_resources()
        .iter()
        .find(|(n, _, _)| *n == target_inst)?;
    let slot = exec.backend().slot_for(res)?;
    let buf = exec.backend().array_buffer(slot)?;
    let staging = device.create_buffer_shared(buf.size);
    let mut enc = device.create_encoder("diag-arr-read");
    enc.copy_buffer_to_buffer(buf, &staging, buf.size);
    enc.commit_and_wait_completed();
    let ptr = staging.mapped_ptr()? as *const f32;
    let count = (buf.size / 4) as usize;
    Some(unsafe { std::slice::from_raw_parts(ptr, count) }.to_vec())
}

/// Sixth-stage diagnostic: bisect INSIDE the production path. Render the
/// canonical def fused and unfused through `PresetRuntime`, dump every
/// texture output on the last frame, and diff each node-id the two graphs
/// share (the surviving boundaries). The first divergent surviving node
/// localizes where the production-only divergence enters.
#[test]
#[ignore]
fn particletext_production_region_diag() {
    use crate::preset_context::PresetContext;
    use crate::preset_runtime::PresetRuntime;
    use std::collections::BTreeMap;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("ParticleText"),
    )
    .expect("ParticleText bundled");
    let base_def: EffectGraphDef = serde_json::from_str(&json).unwrap();
    let strip_arg = std::env::var("PT_STRIP_BINDINGS").is_ok();
    let mut def = base_def;
    if strip_arg && let Some(meta) = def.preset_metadata.as_mut() {
        meta.bindings.clear();
    }
    println!("bindings stripped: {strip_arg}");
    let fused =
        crate::node_graph::freeze::install::fuse_generator_def(&def, &registry).expect("fuses");

    let ctx = |t: f64| PresetContext {
        time: t,
        beat: t * 2.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: w as f32 / h as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let frames: u32 = std::env::var("PT_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    println!("frames: {frames}");
    let run = |d: EffectGraphDef| -> (PresetRuntime, RenderTarget) {
        let mut g = PresetRuntime::from_def_with_device(d, &registry, device.arc(), w, h, FMT, None)
            .expect("preset builds");
        let target = RenderTarget::new(&device, w, h, FMT, "prod-diag");
        for i in 0..frames {
            if i == frames - 1 {
                g.set_dump_all(true);
            }
            let mut enc = device.create_encoder("prod-diag");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                g.render(&mut gpu, &target.texture, &ctx(f64::from(i) / 60.0), &manifold_core::params::ParamManifest::default());
            }
            enc.commit_and_wait_completed();
        }
        (g, target)
    };
    let (u_rt, _ut) = run(def);
    let (f_rt, _ft) = run(fused);

    let collect = |rt: &PresetRuntime| -> BTreeMap<(String, String), (u32, u32)> {
        rt.dump_textures_all()
            .into_iter()
            .map(|(name, port, _ty, tex)| ((name, port), (tex.width, tex.height)))
            .collect()
    };
    let u_keys = collect(&u_rt);
    let f_keys = collect(&f_rt);

    let differ = TextureDiff::new(&device);
    let mut divergent = 0;
    for (key, &udims) in &u_keys {
        let Some(&fdims) = f_keys.get(key) else {
            continue; // fused-away node — not shared
        };
        if udims != fdims {
            println!("{key:?}: DIMS {udims:?} vs {fdims:?}");
            divergent += 1;
            continue;
        }
        let u_tex = u_rt
            .dump_textures_all()
            .into_iter()
            .find(|(n, p, _, _)| (&(n.clone(), p.clone())) == key)
            .map(|(_, _, _, t)| t as *const GpuTexture);
        let f_tex = f_rt
            .dump_textures_all()
            .into_iter()
            .find(|(n, p, _, _)| (&(n.clone(), p.clone())) == key)
            .map(|(_, _, _, t)| t as *const GpuTexture);
        let (Some(u_tex), Some(f_tex)) = (u_tex, f_tex) else {
            continue;
        };
        // Safety: the runtimes outlive this loop; raw pointers only dodge the
        // double-borrow of calling dump_textures_all twice above.
        let (u_tex, f_tex) = unsafe { (&*u_tex, &*f_tex) };
        let r = differ.compare(&device, u_tex, f_tex, 1.0e-7, 1.0e-6);
        if r.over_count > 0 {
            println!(
                "{key:?}: max_abs={} over={}/{}",
                r.max_abs, r.over_count, r.total
            );
            divergent += 1;
        }
    }
    println!("{divergent} divergent shared outputs of {} shared keys", u_keys.len());
}

/// Fifth-stage diagnostic: the COMPOSITE through the RAW executor (host
/// `time` param frozen at 0, frame clock advancing) vs the production
/// PresetRuntime path (host time advancing). Raw exact + production diverging
/// points at frame-context handling (derived-uniform sourcing), not kernels.
#[test]
#[ignore]
fn particletext_raw_composite_diag() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("ParticleText"),
    )
    .expect("ParticleText bundled");
    let def: EffectGraphDef = serde_json::from_str(&json).unwrap();
    let fused =
        crate::node_graph::freeze::install::fuse_generator_def(&def, &registry).expect("fuses");

    // The surviving node feeding final_output exists identically on both
    // sides — preview it as the composite.
    let pick_tail = |d: &EffectGraphDef| {
        let fo = d
            .nodes
            .iter()
            .find(|n| n.type_id == "system.final_output")
            .map(|n| n.id)
            .expect("final_output");
        d.wires
            .iter()
            .find(|w| w.to_node == fo)
            .map(|w| w.from_node)
            .expect("final_output fed")
    };
    let differ = TextureDiff::new(&device);
    for host_params in [false, true] {
        for frames in [1u32, 8] {
            let u = render_def_capture_node_host(
                &def, &registry, &device.arc(), w, h, frames, &pick_tail, host_params,
            );
            let f = render_def_capture_node_host(
                &fused, &registry, &device.arc(), w, h, frames, &pick_tail, host_params,
            );
            match (u, f) {
                (Some((u, ud)), Some((f, fd))) if ud == fd => {
                    let r = differ.compare(&device, &u.texture, &f.texture, 1.0e-7, 1.0e-6);
                    println!(
                        "raw composite host_params={host_params} frames={frames}: max_abs={} over={}/{}",
                        r.max_abs, r.over_count, r.total
                    );
                }
                (u, f) => println!(
                    "raw composite host_params={host_params} frames={frames}: capture mismatch (u={:?} f={:?})",
                    u.as_ref().map(|x| x.1),
                    f.as_ref().map(|x| x.1)
                ),
            }
        }
    }
}

/// Fourth-stage diagnostic: the particle BUFFER itself, fused in-place chain
/// vs unfused atoms, after 1 / 2 / 8 frames. Frame-1 divergence = codegen
/// semantics; later-only divergence = ordering / feedback interaction.
#[test]
#[ignore]
fn particletext_buffer_region_diag() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("ParticleText"),
    )
    .expect("ParticleText bundled");
    let def: EffectGraphDef = serde_json::from_str(&json).unwrap();
    let fused =
        crate::node_graph::freeze::install::fuse_generator_def(&def, &registry).expect("fuses");

    // Unfused: the chain tail (apply_inject) writes the loop buffer. Fused:
    // the in-place region writes the SAME loop buffer — its consumers read
    // the aliased src port, but dump records the array_feedback node's output
    // identically on both sides, so capture THAT (the loop's canonical home).
    let pick_loop = |d: &EffectGraphDef| {
        d.nodes
            .iter()
            .find(|n| n.type_id == "node.array_feedback")
            .map(|n| n.id)
            .expect("array_feedback")
    };
    for frames in [1u32, 2, 8] {
        let u = render_def_capture_array(&def, &registry, &device.arc(), w, h, frames, &pick_loop);
        let f = render_def_capture_array(&fused, &registry, &device.arc(), w, h, frames, &pick_loop);
        match (u, f) {
            (Some(u), Some(f)) => {
                if u.len() != f.len() {
                    println!("frames={frames}: LEN DIFFERS {} vs {}", u.len(), f.len());
                    continue;
                }
                let mut max_abs = 0.0_f32;
                let mut diff_count = 0usize;
                let mut first: Option<usize> = None;
                for (i, (a, b)) in u.iter().zip(&f).enumerate() {
                    let d = (a - b).abs();
                    if d > 0.0 {
                        diff_count += 1;
                        if first.is_none() {
                            first = Some(i);
                        }
                    }
                    if d > max_abs {
                        max_abs = d;
                    }
                }
                println!(
                    "frames={frames}: max_abs={max_abs} diffs={diff_count}/{} first={first:?}",
                    u.len()
                );
            }
            (u, f) => println!(
                "frames={frames}: capture failed (unfused={}, fused={})",
                u.is_some(),
                f.is_some()
            ),
        }
    }
}

/// Third-stage diagnostic: is the composite divergence PRE-EXISTING in the
/// canonical (unmarked) ParticleText? Its buffer region (euler/wrap/inject)
/// and text region fuse today without any fp32 marks — and no render-diff
/// has ever gated them. Also bitwise-compares the text region's output.
#[test]
#[ignore]
fn particletext_canonical_fused_diag() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("ParticleText"),
    )
    .expect("ParticleText bundled");
    let def: EffectGraphDef = serde_json::from_str(&json).unwrap();
    let fused =
        crate::node_graph::freeze::install::fuse_generator_def(&def, &registry).expect("fuses");

    // Composite, canonical def: fused vs unfused.
    let u = render_generator_8_frames(def.clone(), &registry, &device.arc(), w, h);
    let f = render_generator_8_frames(fused.clone(), &registry, &device.arc(), w, h);
    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u.texture, &f.texture, 1.0e-3, 1.0e-2);
    println!(
        "canonical composite: max_abs={} over={}/{} ({:.4})",
        r.max_abs,
        r.over_count,
        r.total,
        r.over_fraction()
    );

    // Discriminator: same comparison with the outer-card bindings STRIPPED
    // from both defs. The raw-executor composite (no bindings applied) is
    // bit-exact, so if removing bindings here kills the divergence, the bug
    // is the retargeted-binding application path on the fused def.
    let strip = |mut d: EffectGraphDef| {
        if let Some(meta) = d.preset_metadata.as_mut() {
            meta.bindings.clear();
        }
        d
    };
    let u2 = render_generator_8_frames(strip(def.clone()), &registry, &device.arc(), w, h);
    let f2 = render_generator_8_frames(strip(fused.clone()), &registry, &device.arc(), w, h);
    let r2 = differ.compare(&device, &u2.texture, &f2.texture, 1.0e-3, 1.0e-2);
    println!(
        "canonical composite, bindings stripped: max_abs={} over={}/{} ({:.4})",
        r2.max_abs,
        r2.over_count,
        r2.total,
        r2.over_fraction()
    );

    // Text region output, bitwise.
    let pick_unfused = |d: &EffectGraphDef| {
        d.nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("text_force_gain"))
            .map(|n| n.id)
            .expect("text_force_gain")
    };
    let pick_fused = |d: &EffectGraphDef| {
        d.nodes
            .iter()
            .find(|n| {
                n.type_id == "node.wgsl_compute"
                    && n.params.keys().any(|k| k.ends_with("_gain"))
                    && n.params.keys().any(|k| k.ends_with("_angle"))
            })
            .map(|n| n.id)
            .expect("fused text region")
    };
    for frames in [1u32, 8] {
        let Some((ut, ud)) =
            render_def_capture_node(&def, &registry, &device.arc(), w, h, frames, &pick_unfused)
        else {
            println!("frames={frames}: unfused text capture failed");
            continue;
        };
        let Some((ft, fd)) =
            render_def_capture_node(&fused, &registry, &device.arc(), w, h, frames, &pick_fused)
        else {
            println!("frames={frames}: fused text capture failed");
            continue;
        };
        println!("text region frames={frames}: unfused dims={ud:?} fused dims={fd:?}");
        if ud == fd {
            let r = differ.compare(&device, &ut.texture, &ft.texture, 1.0e-7, 1.0e-6);
            println!(
                "  text region diff: max_abs={} over={}/{}",
                r.max_abs, r.over_count, r.total
            );
        }
    }
}

/// Second-stage diagnostic: compare the flow-field REGION OUTPUT directly
/// (grad_rotate unfused vs fused_region dst), bitwise, at 1 and 8 frames —
/// the discriminator between "the fused field itself differs" and "the field
/// is exact but the divergence enters through the feedback/particle loop".
#[test]
#[ignore]
fn particletext_fp32_region_output_diag() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("ParticleText"),
    )
    .expect("ParticleText bundled");
    let mut def: EffectGraphDef = serde_json::from_str(&json).unwrap();
    for handle in ["grad", "grad_scaled", "grad_rotate"] {
        let node = def
            .nodes
            .iter_mut()
            .find(|n| n.handle.as_deref() == Some(handle))
            .unwrap();
        node.output_formats.insert("out".to_string(), "rgba32float".to_string());
    }
    let fused =
        crate::node_graph::freeze::install::fuse_generator_def(&def, &registry).expect("fuses");

    let by_handle = |h: &'static str| {
        move |d: &EffectGraphDef| {
            d.nodes
                .iter()
                .find(|n| n.handle.as_deref() == Some(h))
                .map(|n| n.id)
                .unwrap_or_else(|| panic!("node `{h}`"))
        }
    };

    for frames in [1u32, 8] {
        let (u, ud) = render_def_capture_node(
            &def, &registry, &device.arc(), w, h, frames, &by_handle("grad_rotate"),
        )
        .expect("unfused captures");
        // The flow-field region fused as the region containing grad_rotate —
        // its handle is `fused_region_<i>`; find the wgsl_compute whose params
        // carry the rotate angle shadow (n2_angle) to disambiguate.
        let pick_fused = |d: &EffectGraphDef| {
            d.nodes
                .iter()
                .find(|n| {
                    n.type_id == "node.wgsl_compute"
                        && n.params.keys().any(|k| k.ends_with("_angle"))
                })
                .map(|n| n.id)
                .expect("fused flow-field region")
        };
        let (f, fd) =
            render_def_capture_node(&fused, &registry, &device.arc(), w, h, frames, &pick_fused)
                .expect("fused captures");
        println!("frames={frames}: unfused dims={ud:?} fused dims={fd:?}");
        if ud != fd {
            println!("  DIMS DIFFER — grid mismatch survives, stamp didn't land at runtime");
            continue;
        }
        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &u.texture, &f.texture, 1.0e-7, 1.0e-6);
        println!(
            "  region output diff: max_abs={} max_rel={} over={}/{}",
            r.max_abs, r.max_rel, r.over_count, r.total
        );
    }
}

/// FluidSim3D twin of [`particletext_seed_gate_matches_ungated`]. Here seed_alloc
/// is EveryFrame, so the gated skip relies on the order (seed_alloc writes, the
/// gated seed_pattern re-dispatches only on reset) rather than buffer retention —
/// still invisible because array_feedback reads the seed only on reset.
#[test]
fn fluidsim3d_seed_gate_matches_ungated() {
    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("FluidSim3D"),
    )
    .expect("FluidSim3D bundled");
    // Flatten so the grouped seed node lifts to the top level; address it by
    // stable node_id (grouping prefixes handles, node_id survives).
    let gated: EffectGraphDef =
        manifold_core::flatten::flatten_groups(&serde_json::from_str(&json).unwrap())
            .expect("flattens");
    let seed_id = gated
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "seed_pattern")
        .map(|n| n.id)
        .expect("seed_pattern node");
    assert!(
        gated.wires.iter().any(|w| w.to_node == seed_id && w.to_port == "reset_trigger"),
        "FluidSim3D seed_pattern must carry a reset_trigger wire (else gate is vacuous)"
    );
    let mut ungated = gated.clone();
    strip_reset_wire(&mut ungated, "seed_pattern");

    let g = render_generator_8_frames(gated, &registry, &device.arc(), w, h);
    let u = render_generator_8_frames(ungated, &registry, &device.arc(), w, h);
    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &g.texture, &u.texture, 1.0e-3, 1.0e-2);
    assert!(
        r.passes(0.002) && r.over_count < 64,
        "gated FluidSim3D seed must match ungated: max_abs={}, over={}/{} ({:.4})",
        r.max_abs,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Render an effect graph whose bound output is at a REDUCED resolution
/// (`out_w` × `out_h`), for multi-resolution fusion proofs. Same shape as
/// [`render_graph`] but the output target — and the copy-out — are sized to the
/// element-space the producer actually writes (e.g. a quarter-res chain below a
/// downsample), so a fused node that failed to inherit that scale would mismatch.
fn render_graph_at(
    device: &std::sync::Arc<GpuDevice>,
    graph: &mut Graph,
    plan: &ExecutionPlan,
    source_res: ResourceId,
    input: &GpuTexture,
    output_res: ResourceId,
    out_w: u32,
    out_h: u32,
) -> RenderTarget {
    let (sw, sh) = (input.width, input.height);
    let src_rt = RenderTarget::new(device, sw, sh, FMT, "freeze-src");
    {
        let mut e = device.create_encoder("freeze-src-fill");
        e.copy_texture_to_texture(input, &src_rt.texture, sw, sh, 1);
        e.commit_and_wait_completed();
    }
    let out_rt = RenderTarget::new(device, out_w, out_h, FMT, "freeze-graph-out");

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), sw, sh, FMT);
    backend.pre_bind_texture_2d(source_res, src_rt);
    let out_slot = backend.pre_bind_texture_2d(output_res, out_rt);

    let mut enc = device.create_encoder("freeze-graph-exec");
    let mut exec = Executor::new(Box::new(backend));
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
        exec.execute_frame_with_gpu(graph, plan, frame_time(), &mut gpu);
    }
    enc.commit_and_wait_completed();

    let result = RenderTarget::new(device, out_w, out_h, FMT, "freeze-graph-result");
    let out_tex = exec.backend().texture_2d(out_slot).expect("graph output retained");
    {
        let mut e = device.create_encoder("freeze-graph-copy");
        e.copy_texture_to_texture(out_tex, &result.texture, out_w, out_h, 1);
        e.commit_and_wait_completed();
    }
    result
}

/// Multi-resolution oracle — a pixel-local chain BELOW a downsample fuses and
/// runs at the reduced (quarter-res) element space, matching the unfused chain.
/// source → downsample(boundary, 4x) → gain → invert → final: {gain, invert}
/// form one region whose input is canvas-scaled, so the executor's scale
/// propagation sizes the fused node's output at quarter-res. The fused node reads
/// the downsampled external via `textureLoad` at its own (quarter-res) coord —
/// correct precisely because producer and consumer share one element space. We
/// bind the output at quarter-res, so a fused node that wrongly ran at full canvas
/// would mismatch. (The downsample itself stays a boundary — folding a resample
/// INTO a region needs cross-scale sampler reads, a deferred marginal optimization
/// with no bundled-preset fixture yet: every shipped quarter-res chain is gated on
/// vocabulary the finder doesn't own, e.g. Bloom's unconverted threshold/blur.)
#[test]
fn fused_quarter_res_chain_matches_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input_varying_alpha(&device, w, h);
    let (qw, qh) = (w / 4, h / 4); // downsample default factor = 4x

    let json = r#"{
        "version": 1, "name": "multires", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.downsample", "nodeId": "down" },
            { "id": 2, "typeId": "node.exposure", "nodeId": "gain" },
            { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
            { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
            { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
        ]
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).unwrap();

    let mut unfused = def.clone().into_graph(&registry).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.invert"), "out");
    let u_img = render_graph_at(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out, qw, qh);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the quarter-res chain fuses");
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph_at(&device.arc(), &mut fused, &f_plan, f_src, &input, f_out, qw, qh);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005) && r.over_count < 16,
        "fused quarter-res chain must match unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Control-wire oracle — a param driven by a graph WIRE (not a slider) keeps
/// modulating after its atom folds into a fused kernel. texture_dimensions.aspect
/// drives gain.gain; the input is 256×128 so aspect = 2.0, materially different
/// from gain's default 1.0 — so if fusion dropped the wire (falling back to the
/// seeded default) the fused result would diverge. gain + invert fuse; the
/// producer survives and feeds the fused node's port-shadow n0_gain. Fused vs
/// unfused must agree, proving the re-anchored control wire actually drives the
/// kernel.
#[test]
fn fused_control_wired_param_matches_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 128u32); // non-square → aspect = 2.0 (≠ gain default 1.0)
    let input = gradient_input_varying_alpha(&device, w, h);

    let json = r#"{
        "version": 1, "name": "ctrl", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.texture_size", "nodeId": "dims" },
            { "id": 2, "typeId": "node.exposure", "nodeId": "gain" },
            { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
            { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 1, "fromPort": "aspect", "toNode": 2, "toPort": "gain" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
            { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
        ]
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).unwrap();

    let mut unfused = def.clone().into_graph(&registry).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.invert"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the control-wired region fuses");
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph(&device.arc(), &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005) && r.over_count < 16,
        "fused control-wired param must match unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Fan-out oracle — a region with TWO outputs renders identically fused vs
/// unfused, end-to-end through the executor. gain forks into invert and contrast;
/// each runs into its own `threshold` boundary; the two re-merge at a `mix`. The
/// fused def collapses {gain, invert, contrast} into ONE `node.wgsl_compute` that
/// writes `dst_0` (invert) and `dst_1` (contrast), each wired to its threshold —
/// so this exercises the multi-output codegen + the per-output executor
/// allocation (both outputs must be bound, or the whole dispatch early-returns).
/// Comparing the final `mix` output proves both branches are threaded correctly.
#[test]
fn fused_fanout_region_matches_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input_varying_alpha(&device, w, h);

    let json = r#"{
        "version": 1, "name": "fanout", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
            { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
            { "id": 3, "typeId": "node.contrast", "nodeId": "contrast" },
            { "id": 4, "typeId": "node.multi_blend", "nodeId": "thr_a" },
            { "id": 5, "typeId": "node.multi_blend", "nodeId": "thr_b" },
            { "id": 6, "typeId": "node.mix", "nodeId": "mix" },
            { "id": 7, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" },
            { "fromNode": 2, "fromPort": "out", "toNode": 4, "toPort": "in_0" },
            { "fromNode": 3, "fromPort": "out", "toNode": 5, "toPort": "in_0" },
            { "fromNode": 4, "fromPort": "out", "toNode": 6, "toPort": "a" },
            { "fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "b" },
            { "fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "in" }
        ]
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).unwrap();

    let mut unfused = def.clone().into_graph(&registry).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.mix"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the fan-out region fuses");
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, find_node(&fused, "node.mix"), "out");
    let f_img = render_graph(&device.arc(), &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.02),
        "fused fan-out region must match unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Checkpoint (wgsl_compute fusion contract): a FRAGMENT-form `node.wgsl_compute`
/// fuses between two atoms, and the fused single-kernel render matches the three
/// standalone dispatches within the f16-accumulation tolerance. The fragment is a
/// pointwise `c.rgb * scale`; `gain` and `invert` are real atoms. The unfused
/// side dispatches all three (the fragment running its synthesized standalone
/// kernel); the fused side collapses {gain, fragment, invert} into ONE
/// `node.wgsl_compute`. Proves the contract end-to-end: classify saw the fragment
/// (configured-construct), the codegen chained its body, and the result is
/// numerically faithful.
#[test]
fn fused_wgsl_compute_fragment_matches_unfused() {
    use super::install::{FusedDef, fuse_canonical_def};

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input_varying_alpha(&device, w, h);

    // The fragment's `wgslSource` needs a `@fusion: pointwise` marker — routed
    // through `Marker::emit` (a placeholder token, substituted below) rather
    // than a hand-typed literal, so this fixture stays on the single-sourced
    // grammar like every other marker-producing/consuming call site.
    let json = r#"{
        "version": 1, "name": "frag-parity", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.exposure", "nodeId": "gain",
              "params": { "gain": { "type": "Float", "value": 1.2 } } },
            { "id": 2, "typeId": "node.wgsl_compute", "nodeId": "frag",
              "wgslSource": "FUSION_MARKER\n// @in: src\n// @param: scale = 0.75 [0, 2]\nfn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, scale: f32) -> vec4<f32> {\n    return vec4<f32>(c.rgb * scale, c.a);\n}\n",
              "params": { "scale": { "type": "Float", "value": 0.75 } } },
            { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
            { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "src" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
            { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
        ]
    }"#
    .replacen("FUSION_MARKER", &Marker::Fusion { kind: "pointwise".to_string() }.emit(), 1);
    let def: EffectGraphDef = serde_json::from_str(&json).unwrap();

    // Unfused: all three atoms dispatch; the fragment runs its synthesized kernel.
    let mut unfused = def.clone().into_graph(&registry).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.invert"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    // Fused: {gain, fragment, invert} collapse into one node.wgsl_compute.
    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the fragment region fuses");
    assert!(
        !fdef.nodes.iter().any(|n| n.type_id == "node.exposure"),
        "gain must be absorbed into the fused kernel"
    );
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, find_node(&fused, "node.wgsl_compute"), "dst");
    let f_img = render_graph(&device.arc(), &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005),
        "fused fragment region must match unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}
