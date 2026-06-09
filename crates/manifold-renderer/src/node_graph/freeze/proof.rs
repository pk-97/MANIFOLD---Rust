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

    for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Effect) {
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
    let u_img = render_graph(&device, &mut unfused, &u_plan, u_src, &input, u_out);

    // ── Fused: checkerboard + mix collapse into one kernel. ──
    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the Source region fuses");
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph(&device, &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, 1.0e-2, 3.0e-2);
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
    let u_img = render_graph(&device, &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the gather region fuses");
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph(&device, &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, 1.0e-2, 3.0e-2);
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
    let u_img = render_graph(&device, &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the warp region fuses");
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph(&device, &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, 1.0e-2, 3.0e-2);
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

/// Coverage baseline — a regression guard on how much of the shipped library the
/// finder fuses. Walks every bundled effect preset, partitions it, and tallies
/// the presets that fuse + the total atoms folded into kernels. A future change
/// that silently turns the partition conservative (everything a boundary) would
/// drop these counts below the floor and trip here. The floor is deliberately
/// loose — it tracks "fusion is broadly alive", not an exact number that churns
/// as the atom vocabulary lands. The exact counts are logged, never asserted.
#[test]
fn fusion_coverage_baseline() {
    let registry = PrimitiveRegistry::with_builtin();
    let mut fused_presets = 0usize;
    let mut total_fused_atoms = 0usize;
    let mut total_regions = 0usize;
    let mut detail: Vec<String> = Vec::new();

    for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Effect) {
        let Some(base) = crate::node_graph::loaded_preset_view_by_id(&type_id) else {
            continue;
        };
        let regions = super::region::partition_regions(base.canonical_def, &registry);
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
    detail.sort();
    eprintln!(
        "[freeze coverage] {fused_presets} preset(s) fuse, {total_regions} region(s), \
         {total_fused_atoms} atom(s) folded:\n{}",
        detail.join("\n")
    );

    // Loose floors: fusion must stay broadly alive across the library. (At the
    // time of writing: well above these — fan-out, gather, source, and
    // control-wire coverage all contribute.)
    assert!(
        fused_presets >= 8,
        "expected ≥8 bundled presets to fuse, got {fused_presets} — partition regressed?"
    );
    assert!(
        total_fused_atoms >= 30,
        "expected ≥30 atoms folded library-wide, got {total_fused_atoms} — partition regressed?"
    );
}

/// Grouped presets must fuse. The fuse entry (`fuse_canonical_def`) flattens its
/// input the way the live loader does — otherwise a preset organised into node
/// groups silently never fuses (`partition_regions` refuses any def still
/// carrying a group node). Glitch (a grouped EFFECT) and FluidSimulation (a
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
        fused_generator_def_by_id(&PresetTypeId::new("FluidSimulation")).is_some(),
        "FluidSimulation is a grouped generator with a fusable region once flattened — \
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
    use crate::preset_context::{MAX_GEN_PARAMS, PresetContext};
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
        params: [0.0; MAX_GEN_PARAMS],
        param_count: 0,
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
                fused_def.clone(),
                &registry,
                &device,
                w,
                h,
                FMT,
            )
            .expect("fused generator builds");
            let target = RenderTarget::new(&device, w, h, FMT, "fused-gen-smoke");
            let mut enc = device.create_encoder("fused-gen-smoke");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                g.render(&mut gpu, &target.texture, &ctx);
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
    use crate::preset_context::{MAX_GEN_PARAMS, PresetContext};
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
            { "id": 2, "typeId": "node.gain", "nodeId": "gain" },
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
        params: [0.0; MAX_GEN_PARAMS],
        param_count: 0,
    };
    let render = |def: EffectGraphDef| -> RenderTarget {
        let mut g =
            PresetRuntime::from_def_with_device(def, &registry, &device, w, h, FMT)
                .expect("generator builds");
        let target = RenderTarget::new(&device, w, h, FMT, "freeze-gen-out");
        let mut enc = device.create_encoder("freeze-gen");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            g.render(&mut gpu, &target.texture, &ctx);
        }
        enc.commit_and_wait_completed();
        target
    };

    let unfused = render(canonical);
    let fused = render(fused_def);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &unfused.texture, &fused.texture, 1.0e-2, 3.0e-2);
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
    use crate::preset_context::{MAX_GEN_PARAMS, PresetContext};
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
        params: [0.0; MAX_GEN_PARAMS],
        param_count: 0,
    };
    // Warm up a few frames (instance/particle buffers populate), then capture.
    let render = |def: EffectGraphDef| -> RenderTarget {
        let mut g = PresetRuntime::from_def_with_device(def, &registry, &device, w, h, FMT)
            .expect("generator builds");
        let target = RenderTarget::new(&device, w, h, FMT, "freeze-dp-out");
        for i in 0..6u32 {
            let mut enc = device.create_encoder("freeze-dp");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                g.render(&mut gpu, &target.texture, &ctx(i as f64 / 60.0));
            }
            enc.commit_and_wait_completed();
        }
        target
    };

    let unfused = render(canonical);
    let fused = render(fused_def);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &unfused.texture, &fused.texture, 1.0e-2, 3.0e-2);
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

/// BUFFER-domain fusion with FRAME-DERIVED uniforms: the real FluidSimulation —
/// whose per-particle hot chain (noise force, euler integrate with `dt_scaled`,
/// wrap, anti-clump with `frame_count`, …) only fuses now that the codegen emits
/// each member's derived uniform as an `n{i}_<name>` field and the install pass
/// wires it from `system.generator_input` (frame_delta / frame_count) — must
/// render frame-for-frame like the unfused preset.
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
    use crate::preset_context::{MAX_GEN_PARAMS, PresetContext};
    use crate::preset_runtime::PresetRuntime;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);

    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("FluidSimulation"),
    )
    .expect("FluidSimulation preset bundled");
    let canonical: EffectGraphDef = serde_json::from_str(&json).unwrap();
    let fused_def = fuse_generator_def(&canonical, &registry)
        .expect("FluidSimulation fuses + builds (derived-uniform buffer region)");

    // The build's whole point: a derived-uniform particle atom must actually have
    // fused. Assert the fused def added a generator_input → fused frame wire — if
    // it didn't, the region stayed unfused and this test would pass vacuously.
    let has_frame_wire = fused_def.nodes.iter().any(|n| n.type_id == "node.wgsl_compute")
        && fused_def.wires.iter().any(|wire| {
            fused_def
                .nodes
                .iter()
                .any(|n| n.id == wire.from_node && n.type_id == "system.generator_input")
                && (wire.from_port == "frame_delta" || wire.from_port == "frame_count")
        });
    assert!(
        has_frame_wire,
        "FluidSim fusion must wire a frame-derived uniform from generator_input — \
         no such wire means the derived-uniform region never fused (vacuous pass)"
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
        params: [0.0; MAX_GEN_PARAMS],
        param_count: 0,
    };
    let render = |def: EffectGraphDef| -> RenderTarget {
        let mut g = PresetRuntime::from_def_with_device(def, &registry, &device, w, h, FMT)
            .expect("FluidSimulation builds");
        let target = RenderTarget::new(&device, w, h, FMT, "freeze-fluid-fusion");
        for i in 0..8u32 {
            let mut enc = device.create_encoder("freeze-fluid-fusion");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                g.render(&mut gpu, &target.texture, &ctx(i as f64 / 60.0));
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
        "fused FluidSimulation must render like unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// Determinism guard for the FluidSimulation feedback sim. Rendering the SAME
/// canonical preset twice from fresh state, with an identical frame sequence,
/// must produce the SAME final image. It did NOT before the storage-layer
/// zero-init fix: scatter atomic-adds into a `u32` accumulator that
/// `node.resolve_accumulator` clears *after* reading, so the accumulator must
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
    use crate::preset_context::{MAX_GEN_PARAMS, PresetContext};
    use crate::preset_runtime::PresetRuntime;

    let device = crate::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);

    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("FluidSimulation"),
    )
    .expect("FluidSimulation preset bundled");
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
        params: [0.0; MAX_GEN_PARAMS],
        param_count: 0,
    };
    // Warm the feedback loop a handful of frames so any frame-0 divergence has
    // time to amplify through the density→force→position loop, then capture.
    let render = |def: EffectGraphDef| -> RenderTarget {
        let mut g = PresetRuntime::from_def_with_device(def, &registry, &device, w, h, FMT)
            .expect("FluidSimulation builds");
        let target = RenderTarget::new(&device, w, h, FMT, "freeze-fluid-determinism");
        for i in 0..8u32 {
            let mut enc = device.create_encoder("freeze-fluid");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                g.render(&mut gpu, &target.texture, &ctx(i as f64 / 60.0));
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
        "FluidSimulation must render deterministically from fresh state: \
         max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
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
    device: &GpuDevice,
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

    let mut backend = MetalBackend::new(device, sw, sh, FMT);
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
            { "id": 2, "typeId": "node.gain", "nodeId": "gain" },
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
    let u_img = render_graph_at(&device, &mut unfused, &u_plan, u_src, &input, u_out, qw, qh);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the quarter-res chain fuses");
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph_at(&device, &mut fused, &f_plan, f_src, &input, f_out, qw, qh);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, 1.0e-2, 3.0e-2);
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
            { "id": 1, "typeId": "node.texture_dimensions", "nodeId": "dims" },
            { "id": 2, "typeId": "node.gain", "nodeId": "gain" },
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
    let u_img = render_graph(&device, &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the control-wired region fuses");
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_node = find_node(&fused, "node.wgsl_compute");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, f_node, "dst");
    let f_img = render_graph(&device, &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, 1.0e-2, 3.0e-2);
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
            { "id": 1, "typeId": "node.gain", "nodeId": "gain" },
            { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
            { "id": 3, "typeId": "node.contrast", "nodeId": "contrast" },
            { "id": 4, "typeId": "node.threshold", "nodeId": "thr_a" },
            { "id": 5, "typeId": "node.threshold", "nodeId": "thr_b" },
            { "id": 6, "typeId": "node.mix", "nodeId": "mix" },
            { "id": 7, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" },
            { "fromNode": 2, "fromPort": "out", "toNode": 4, "toPort": "source" },
            { "fromNode": 3, "fromPort": "out", "toNode": 5, "toPort": "source" },
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
    let u_img = render_graph(&device, &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the fan-out region fuses");
    let mut fused = fdef.into_graph(&registry).expect("fused graph builds");
    let f_plan = compile(&fused).expect("compile fused");
    let f_src = resource_for_output(&f_plan, find_node(&fused, "system.source"), "out");
    let f_out = resource_for_output(&f_plan, find_node(&fused, "node.mix"), "out");
    let f_img = render_graph(&device, &mut fused, &f_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, 1.0e-2, 3.0e-2);
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
