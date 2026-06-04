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
    device: &GpuDevice,
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

    let mut backend = MetalBackend::new(device, w, h, FMT);
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
fn render_unfused_two_gain(device: &GpuDevice, input: &GpuTexture, g1: f32, g2: f32) -> RenderTarget {
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

    let unfused = render_unfused_two_gain(&device, &input, g1, g2);
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

    let unfused = render_unfused_two_gain(&device, &input, g1, g2);
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
    set_f(&mut graph, "node.gain", "gain", params.gain);
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
    let out_res = resource_for_output(&plan, find_node(&graph, "node.clamp_texture"), "out");
    let unfused = render_graph(&device, &mut graph, &plan, src_res, &input, out_res);

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
    let r = differ.compare(&device, &unfused.texture, &fused.texture, 1.0e-2, 3.0e-2);
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

/// CPU-built gradient with spatially-varying alpha (A ramps in x), so the
/// faithful per-atom alpha threading (mix lerps a.a→b.a) is observable in the
/// diff — the §12.4 hardened-fixture alpha axis. R/G/B as in `gradient_input`.
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
        resource_for_output(&unfused_plan, find_node(&unfused_graph, "node.clamp_texture"), "out");
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

        let unfused = render_graph(&device, &mut unfused_graph, &unfused_plan, u_src, &input, u_out);
        let fused = render_graph(&device, &mut fused_graph, &fused_plan, f_src, &input, f_out);

        let r = differ.compare(&device, &unfused.texture, &fused.texture, 1.0e-2, 3.0e-2);
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
        resource_for_output(&unfused_plan, find_node(&unfused_graph, "node.clamp_texture"), "out");
    let unfused = render_graph(&device, &mut unfused_graph, &unfused_plan, u_src, &input, u_out);

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
    let fused = render_graph(&device, &mut fused_graph, &fused_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    // Same discontinuity-aware budget as the hand-kernel test, plus an absolute
    // cap on the failing-texel count so a contiguous failure band can't hide in
    // the 0.5% fraction (§12.4 verdict tightening).
    let r = differ.compare(&device, &unfused.texture, &fused.texture, 1.0e-2, 3.0e-2);
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

    for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids() {
        let preset_id = type_id.as_str().to_string();
        // WireframeDepthGraph carries a documented, fusion-unrelated pre-existing
        // bug (a 42×42 vs 256×256 same-size-blit panic in its depth path); it
        // would panic with or without fusion, so testing it here measures noise.
        if preset_id == "WireframeDepthGraph" {
            continue;
        }
        let Some(base) = crate::node_graph::loaded_preset_view_by_id(&type_id) else {
            continue;
        };
        let Some(fused) = super::install::fuse_canonical_def(base.canonical_def, &registry) else {
            continue; // no fusable region — nothing to validate
        };
        fused_count += 1;

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut graph = fused.def.into_graph(&registry).expect("fused def builds a graph");
            let plan = compile(&graph).expect("fused graph compiles");
            let r_src = resource_for_output(&plan, find_node(&graph, "system.source"), "out");
            let src_target = RenderTarget::new(&device, w, h, FMT, "fused-smoke-src");
            let mut backend = MetalBackend::new(&device, w, h, FMT);
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
