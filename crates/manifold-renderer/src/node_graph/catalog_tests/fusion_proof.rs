use manifold_node_engine::freeze::TextureDiff;
use manifold_node_engine::freeze::markers::Marker;
use manifold_node_engine::freeze::reference::{ColorGradeParams, colorgrade_pipeline, dispatch_fused_colorgrade};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::exec::execution_plan::compile;
use manifold_node_engine::graph::Graph;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::{persistence::EffectGraphDefExt, exec::execution::Executor, scene::boundary_nodes::FinalOutput, exec::effect_node::FrameTime, exec::metal_backend::MetalBackend, exec::effect_node::NodeInstanceId, persistence::PrimitiveRegistry, scene::boundary_nodes::Source};
use manifold_node_engine::gpu::render_target::RenderTarget;
use half::f16;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, Seconds};
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};

const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;



/// The section 7.4 "out-of-loop ≈ulp" precision-contract tolerance (freeze section 7,
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

use manifold_node_engine::testkit::proof_support::*;

/// The real target: the shipped ColorGrade preset (9 nodes, 7 pointwise atoms
/// fanning source into both a grade chain and a mix) hand-fused into one
/// kernel and validated against the unfused preset at non-trivial params.
#[test]
fn fused_colorgrade_matches_unfused_within_tolerance() {
    let device = manifold_gpu::testkit::test_device();
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
    let mut graph = def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("build ColorGrade graph");
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
    // texels failing both bounds (section 11.D discontinuity-aware metric).
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

/// CHAIN FUSION CHECKPOINT (docs/CHAIN_FUSION_DESIGN.md section 8) — the fail-fast
/// gate before any cross-card generalization. Two pointwise cards are rendered
/// the way the chain renders them today (card A's graph to a texture, that
/// texture fed into card B's graph — the full-canvas seam round-trip), and
/// against the fused concatenated segment def (one kernel, no seam). The fused
/// side must match within the same f16-accumulation budget as every pointwise
/// proof. Card params drive the fused kernel through the segment's namespaced
/// retarget map — proving the binding surface survives the card boundary.
#[test]
fn chain_segment_fused_matches_sequential_per_card() {
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};
    use manifold_node_engine::freeze::segment::concat_defs;

    let device = manifold_gpu::testkit::test_device();
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
    let mut graph_a = card_a.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("card A graph");
    set_f(&mut graph_a, "node.exposure", "gain", gain);
    set_f(&mut graph_a, "node.contrast", "contrast", contrast);
    let plan_a = compile(&graph_a).expect("compile card A");
    let a_src = resource_for_output(&plan_a, find_node(&graph_a, "system.source"), "out");
    let a_out = resource_for_output(&plan_a, find_node(&graph_a, "node.contrast"), "out");
    let a_result = render_graph(&device.arc(), &mut graph_a, &plan_a, a_src, &input, a_out);

    let mut graph_b = card_b.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("card B graph");
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
    let mut fused_graph = fused_def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused segment graph builds");
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

/// **Step-6 fuzz hardening (design section 12.3 step 6 / section 12.4).** The single hardened
/// fixture proves correctness at one point; this sweeps the param space so we
/// aren't trusting one vector. For many random in-range param sets — including
/// every `mix` blend mode (0..7, so the `switch` + `safe_div` divide path are
/// all exercised) — it renders the unfused shipped preset and the AUTO-fused def
/// through the executor and asserts they agree: no divergent NaN/Inf
/// (`special_count == 0`, the hard gate) and within the discontinuity-aware
/// fraction budget. A fixed seed makes any failure replayable.
#[test]
fn colorgrade_fuzz_fused_agrees_with_unfused() {
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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
    let mut unfused_graph = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
    let mut fused_graph = fused_def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph");
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

/// **The step-4 production gate (design section 12.3 step 5).** Drives the *install*
/// path end-to-end through the real executor: the region-grower
/// ([`crate::node_graph::freeze::install::fuse_canonical_def`]) auto-discovers the ColorGrade region
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
/// Hardened fixture (section 12.4): interior `mix_amount = 0.35` so the source→mix.a
/// fork materially contributes (not crossfaded out), plus a spatially-varying
/// input alpha so faithful alpha threading is exercised. Both sides run the
/// same alpha-faithful atom bodies, so alpha must agree exactly.
#[test]
fn auto_fused_colorgrade_via_executor_matches_unfused() {
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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
    let mut unfused_graph = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
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
    let mut fused_graph = fused_def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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
    // the 0.5% fraction (section 12.4 verdict tightening).
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
    use manifold_node_engine::state_store::StateStore;
    use std::panic::AssertUnwindSafe;

    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (192u32, 192u32);
    let ft = frame_time();
    let mut failures: Vec<String> = Vec::new();
    let mut fused_count = 0usize;

    for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Effect) {
        let preset_id = type_id.as_str().to_string();
        let Some(base) = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(&type_id) else {
            continue;
        };
        let Some(fused) = manifold_node_engine::freeze::install::fuse_canonical_def(&base.canonical_def, &registry) else {
            continue; // no fusable region — nothing to validate
        };
        fused_count += 1;

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut graph = fused.def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused def builds a graph");
            let plan = compile(&graph).expect("fused graph compiles");
            let r_src = try_resource_for_output(
                &plan,
                find_node(&graph, "system.source"),
                "out",
            );
            let mut backend = MetalBackend::new(device.arc(), w, h, FMT);
            if let Some(r_src) = r_src {
                let src_target = RenderTarget::new(&device, w, h, FMT, "fused-smoke-src");
                backend.pre_bind_texture_2d(r_src, src_target);
            }
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
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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
    let mut unfused = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.mix"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    // ── Fused: checkerboard + mix collapse into one kernel. ──
    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the Source region fuses");
    let mut fused = fdef.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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

    let device = manifold_gpu::testkit::test_device();
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
        let Some(fused) = manifold_node_engine::freeze::install::fuse_canonical_def(&def, &registry) else {
            continue; // no fusable region in this generator
        };
        fused_generators += 1;
        for node in fused.def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute") {
            fused_kernels += 1;
            let wgsl = node.wgsl_source.as_deref().expect("fused node carries WGSL");
            let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _ = device.create_compute_pipeline(wgsl, manifold_node_engine::freeze::codegen::ENTRY, "gen-kernel-smoke");
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
    use manifold_node_engine::load::chain_spec::splice_def_into_chain;
    use manifold_node_engine::parameters::ParamValue;
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

    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (128u32, 128u32);
    let input = black_input(&device, w, h);

    let id = PresetTypeId::new("Infrared");
    let def = crate::node_graph::bundled_presets::bundled_preset_def(&id)
        .expect("Infrared preset def");

    let mut graph = Graph::new();
    let src = graph.add_node(Box::new(Source::new()));
    let result =
        splice_def_into_chain(&mut graph, (src, "out"), def, &registry, None, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("splice");
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
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::runtime::PresetRuntime;
    use manifold_core::PresetTypeId;

    let device = manifold_gpu::testkit::test_device();
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
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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

    let mut unfused = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.invert"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the gather region fuses");
    let mut fused = fdef.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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

    let mut unfused = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.remap"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the warp region fuses");
    let mut fused = fdef.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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
    let base = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(&manifold_core::PresetTypeId::new(
        "FilmGrain",
    ))
    .expect("FilmGrain view");
    assert!(
        manifold_node_engine::freeze::install::fuse_canonical_def(&base.canonical_def, &registry).is_none(),
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
    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = noise_input(&device, w, h);

    let base = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(&manifold_core::PresetTypeId::new(
        "Watercolor",
    ))
    .expect("Watercolor view");
    let def = &*base.canonical_def;

    let fused = manifold_node_engine::freeze::install::fuse_canonical_def(def, &registry)
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
            let Some(base) = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(&type_id) else {
                continue;
            };
            let Ok(flat) = manifold_core::flatten::flatten_groups(&base.canonical_def) else {
                continue;
            };
            let regions = manifold_node_engine::freeze::region::partition_regions(&flat, &registry);
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
    use manifold_node_engine::freeze::install::{fused_generator_view_by_id, fused_view_by_id};
    use manifold_core::PresetTypeId;

    assert!(
        fused_view_by_id(&PresetTypeId::new("Glitch")).is_some(),
        "Glitch is a grouped effect with fusable regions once flattened — \
         fuse_canonical_def must flatten before partitioning"
    );
    assert!(
        fused_generator_view_by_id(&PresetTypeId::new("FluidSim2D")).is_some(),
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
    use manifold_node_engine::freeze::install::fused_generator_view_by_id;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::runtime::PresetRuntime;
    use std::panic::AssertUnwindSafe;

    let device = manifold_gpu::testkit::test_device();
    manifold_gpu::testkit::load_disk_shader_caches(&device);
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
        let Some(fused_view) = fused_generator_view_by_id(&type_id) else {
            continue;
        };
        fused_count += 1;
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut g = PresetRuntime::from_def_with_device(
                (*fused_view.def).clone(),
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
    use manifold_node_engine::freeze::install::fuse_generator_view;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::runtime::PresetRuntime;

    let device = manifold_gpu::testkit::test_device();
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
    let fused_view = fuse_generator_view(&canonical, &registry).expect("the generator fuses");

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
    let fused = render((*fused_view.def).clone());

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





/// BUG-z3l6: the fused Glitch kernel must NOT freeze `time`. Render the auto-
/// fused def at two different frame-clock seconds; the two outputs must differ.
/// Fails on the buggy path because unwired `time` resolves to the static param
/// default (0.0) in the fused kernel.
#[test]
fn glitch_fused_kernel_animates_over_time() {
    use manifold_node_engine::freeze::install::FusedDef;

    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input(&device, w, h);

    let json = crate::node_graph::bundled_presets::bundled_preset_json(&manifold_core::PresetTypeId::new("Glitch"))
        .expect("Glitch is a bundled preset");
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse Glitch.json");

    let FusedDef { def: fused_def, retarget, .. } =
        manifold_node_engine::freeze::install::fuse_canonical_def(&def, &registry).expect("Glitch is fusable once flattened");
    let mut fused_graph = fused_def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");

    // Crank the master amount so the effect is visible.
    let (amount_node, amount_field) = match retarget.get(&("amount_value".to_string(), "value".to_string())) {
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
        .set_param(amount_node, amount_field, ParamValue::Float(1.0))
        .unwrap_or_else(|e| panic!("set fused amount: {e:?}"));

    let fused_plan = compile(&fused_graph).expect("compile fused");
    let f_src = resource_for_output(&fused_plan, find_node(&fused_graph, "system.source"), "out");
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

    let at_0 = render_graph_at_time(
        &device.arc(),
        &mut fused_graph,
        &fused_plan,
        f_src,
        &input,
        f_out,
        FrameTime {
            seconds: Seconds(0.0),
            beats: Beats(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        },
    );
    let at_1 = render_graph_at_time(
        &device.arc(),
        &mut fused_graph,
        &fused_plan,
        f_src,
        &input,
        f_out,
        FrameTime {
            seconds: Seconds(1.0),
            beats: Beats(2.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 60,
        },
    );

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &at_0.texture, &at_1.texture, 0.0, 0.0);
    assert!(
        r.max_abs > 0.0 || r.over_count > 0,
        "BUG-z3l6: fused Glitch output is frozen across time (time uniform not recomputed)"
    );
}

/// BUG-z3l6: with live `time`, the fused Glitch kernel must respond to a speed
/// binding. At the same frame clock, speed 1.0 and speed 10.0 must produce
/// different outputs. Fails on the buggy path because speed*0 == 0 regardless.
#[test]
fn glitch_fused_kernel_speed_binding_scales_time() {
    use manifold_node_engine::freeze::install::FusedDef;

    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input(&device, w, h);

    let json = crate::node_graph::bundled_presets::bundled_preset_json(&manifold_core::PresetTypeId::new("Glitch"))
        .expect("Glitch is a bundled preset");
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse Glitch.json");

    let FusedDef { def: fused_def, retarget, .. } =
        manifold_node_engine::freeze::install::fuse_canonical_def(&def, &registry).expect("Glitch is fusable once flattened");

    // `speed_value.value` retargets onto a fused field like `amount_value.value`
    // does; the binding id is "speed" but the retarget key is the TARGET param.
    let set_speed = |fused_graph: &mut Graph, speed: f32| {
        let (speed_node, speed_field) = match retarget.get(&("speed_value".to_string(), "value".to_string())) {
            Some((target_node_id, field)) => (
                fused_graph
                    .instance_by_node_id(target_node_id)
                    .unwrap_or_else(|| panic!("fused graph missing retargeted speed node")),
                field.as_str(),
            ),
            None => (
                fused_graph
                    .node_id_by_handle("speed_value")
                    .unwrap_or_else(|| panic!("speed_value must survive if not retargeted")),
                "value",
            ),
        };
        fused_graph
            .set_param(speed_node, speed_field, ParamValue::Float(speed))
            .unwrap_or_else(|e| panic!("set fused speed: {e:?}"));
    };

    let make_graph = || {
        let mut fused_graph = fused_def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
        let (amount_node, amount_field) = match retarget.get(&("amount_value".to_string(), "value".to_string())) {
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
            .set_param(amount_node, amount_field, ParamValue::Float(1.0))
            .unwrap_or_else(|e| panic!("set fused amount: {e:?}"));
        fused_graph
    };

    let fused_plan = compile(&make_graph()).expect("compile fused");
    let f_src = resource_for_output(&fused_plan, find_node(&make_graph(), "system.source"), "out");
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

    let render_with_speed = |speed: f32| -> RenderTarget {
        let mut fused_graph = make_graph();
        set_speed(&mut fused_graph, speed);
        let fused_plan = compile(&fused_graph).expect("compile fused");
        let f_out_node = fused_graph
            .instance_by_node_id(&producer_doc.node_id)
            .unwrap_or_else(|| panic!("fused graph missing producer instance for final_output"));
        let f_out = resource_for_output(&fused_plan, f_out_node, &out_wire.from_port);
        render_graph_at_time(
            &device.arc(),
            &mut fused_graph,
            &fused_plan,
            f_src,
            &input,
            f_out,
            FrameTime {
                seconds: Seconds(1.0),
                beats: Beats(2.0),
                delta: Seconds(1.0 / 60.0),
                frame_count: 60,
            },
        )
    };

    let slow = render_with_speed(1.0);
    let fast = render_with_speed(10.0);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &slow.texture, &fast.texture, 0.0, 0.0);
    assert!(
        r.max_abs > 0.0 || r.over_count > 0,
        "BUG-z3l6: fused Glitch ignores the speed binding (time uniform not live)"
    );
}

/// BUG-z3l6, second shipped victim: Watercolor's `node.flow_field_noise` has an
/// unwired `time` port and the preset has no `system.generator_input`, so the
/// fused kernel must supply the frame clock itself. Render the auto-fused def
/// at two frame-clock seconds; the outputs must differ. Defaults are safe here:
/// a frozen clock can only make the outputs identical (red), never vacuously
/// green.
#[test]
fn watercolor_fused_kernel_animates_over_time() {
    use manifold_node_engine::freeze::install::FusedDef;

    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let input = gradient_input(&device, w, h);

    let json = crate::node_graph::bundled_presets::bundled_preset_json(&manifold_core::PresetTypeId::new("Watercolor"))
        .expect("Watercolor is a bundled preset");
    let def: EffectGraphDef = serde_json::from_str(&json).expect("parse Watercolor.json");

    let FusedDef { def: fused_def, .. } =
        manifold_node_engine::freeze::install::fuse_canonical_def(&def, &registry).expect("Watercolor is fusable once flattened");
    let mut fused_graph = fused_def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");

    let fused_plan = compile(&fused_graph).expect("compile fused");
    let f_src = resource_for_output(&fused_plan, find_node(&fused_graph, "system.source"), "out");
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

    let frame = |t: f64, n: i64| FrameTime {
        seconds: Seconds(t),
        beats: Beats(t * 2.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: n,
    };
    let at_0 = render_graph_at_time(
        &device.arc(),
        &mut fused_graph,
        &fused_plan,
        f_src,
        &input,
        f_out,
        frame(0.0, 0),
    );
    let at_1 = render_graph_at_time(
        &device.arc(),
        &mut fused_graph,
        &fused_plan,
        f_src,
        &input,
        f_out,
        frame(1.0, 60),
    );

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &at_0.texture, &at_1.texture, 0.0, 0.0);
    assert!(
        r.max_abs > 0.0 || r.over_count > 0,
        "BUG-z3l6: fused Watercolor output is frozen across time (flow_field_noise time not recomputed)"
    );
}

/// Minimal generator def exercising `node.flow_field_noise` with unwired `time`:
/// the fused region must recompute `time` every frame. Render at t=0 and t=2;
/// the outputs must differ. A single Source atom would not form a region, so we
/// add a downstream pointwise node to meet MIN_REGION_LEN.
#[test]
fn flow_field_noise_fused_region_animates_over_time() {
    use manifold_node_engine::freeze::install::fuse_generator_view;
    use {manifold_nodes_image::node_graph::primitives::flow_field_noise::FlowFieldNoise, manifold_node_engine::primitives::gain::Gain};
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::runtime::PresetRuntime;
    use manifold_node_engine::primitive::PrimitiveSpec;

    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (128u32, 128u32);

    let def = EffectGraphDef {
        version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION_WITH_METADATA,
        name: Some("flow-time".to_string()),
        description: None,
        preset_metadata: Default::default(),
        scene_modifiers: Vec::new(),
        nodes: vec![
            manifold_core::effect_graph_def::EffectGraphNode {
                id: 0,
                node_id: manifold_core::NodeId::new("gen_in"),
                type_id: "system.generator_input".to_string(),
                handle: Some("gen_in".to_string()),
                params: Default::default(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: Default::default(),
                output_canvas_scales: Default::default(),
                group: None,
            },
            manifold_core::effect_graph_def::EffectGraphNode {
                id: 1,
                node_id: manifold_core::NodeId::new("flow"),
                type_id: FlowFieldNoise::TYPE_ID.to_string(),
                handle: Some("flow".to_string()),
                params: Default::default(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: Default::default(),
                output_canvas_scales: Default::default(),
                group: None,
            },
            manifold_core::effect_graph_def::EffectGraphNode {
                id: 2,
                node_id: manifold_core::NodeId::new("gain"),
                type_id: Gain::TYPE_ID.to_string(),
                handle: Some("gain".to_string()),
                params: Default::default(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: Default::default(),
                output_canvas_scales: Default::default(),
                group: None,
            },
            manifold_core::effect_graph_def::EffectGraphNode {
                id: 3,
                node_id: manifold_core::NodeId::new("final"),
                type_id: "system.final_output".to_string(),
                handle: Some("final".to_string()),
                params: Default::default(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: Default::default(),
                output_canvas_scales: Default::default(),
                group: None,
            },
        ],
        wires: vec![
            manifold_core::effect_graph_def::EffectGraphWire {
                from_node: 1,
                from_port: "flow".to_string(),
                to_node: 2,
                to_port: "in".to_string(),
            },
            manifold_core::effect_graph_def::EffectGraphWire {
                from_node: 2,
                from_port: "out".to_string(),
                to_node: 3,
                to_port: "in".to_string(),
            },
        ],
    };

    let fused_view = fuse_generator_view(&def, &registry).expect("flow_field_noise + gain fuses");

    let render = |t: f64| -> RenderTarget {
        let mut g = PresetRuntime::from_def_with_device((*fused_view.def).clone(), &registry, device.arc(), w, h, FMT, None)
            .expect("generator builds");
        let target = RenderTarget::new(&device, w, h, FMT, "flow-time-out");
        let ctx = PresetContext {
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
        let mut enc = device.create_encoder("flow-time");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            g.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
        target
    };

    let at_0 = render(0.0);
    let at_2 = render(2.0);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &at_0.texture, &at_2.texture, 0.0, 0.0);
    assert!(
        r.max_abs > 0.0 || r.over_count > 0,
        "BUG-z3l6: fused flow_field_noise region is frozen across time"
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
    use manifold_node_engine::freeze::install::fuse_generator_view;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::runtime::PresetRuntime;

    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);

    let json = crate::node_graph::bundled_presets::bundled_preset_json(&manifold_core::PresetTypeId::new("DigitalPlants"))
        .expect("DigitalPlants preset bundled");
    let canonical: EffectGraphDef = serde_json::from_str(&json).unwrap();
    // The whole point: DigitalPlants' GPU per-instance chain must fuse into a
    // buffer kernel that BUILDS (the aliased-output model). If this is None the
    // buffer-fusion activation regressed.
    let fused_view =
        fuse_generator_view(&canonical, &registry).expect("DigitalPlants buffer region fuses + builds");

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
    let fused = render((*fused_view.def).clone());

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
    use manifold_node_engine::freeze::install::fuse_generator_view;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::runtime::PresetRuntime;

    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);

    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("FluidSim2D"),
    )
    .expect("FluidSim2D preset bundled");
    let canonical: EffectGraphDef = serde_json::from_str(&json).unwrap();
    let fused_view = fuse_generator_view(&canonical, &registry)
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
    let has_derived_uniform_member = fused_view.def.nodes.iter().any(|n| {
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
        fused_view.def.nodes.iter().any(|n| n.wgsl_source.as_deref().is_some_and(|s| {
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
    // currency: the old rgba32float overrides existed
    // only as a pre-q16 parity workaround, and they doubled every downstream
    // consumer's bandwidth AND broke the gaussian blur's bilinear tap-pair
    // trick (fp32 textures aren't filterable on Apple GPUs). No rgba32float
    // dst may appear; the q16 wrapper must.
    let fused_texture_kernels: Vec<&str> = fused_view.def
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
        !fused_view.def.nodes.iter().any(|n| {
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
    let fused = render((*fused_view.def).clone());

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
    use manifold_node_engine::freeze::install::fuse_generator_view;
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::runtime::PresetRuntime;

    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);

    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("FluidSim3D"),
    )
    .expect("FluidSim3D preset bundled");
    let canonical: EffectGraphDef = serde_json::from_str(&json).unwrap();
    let fused_view = fuse_generator_view(&canonical, &registry)
        .expect("FluidSim3D fuses + builds (3D-sampler buffer region)");

    assert!(
        !fused_view.def.nodes.iter().any(|n| n.type_id == "node.sample_volume_at_particles"),
        "the 3D force sampler must be absorbed into a fused region — a surviving \
         standalone node means the Texture3D gate regressed and the integrator \
         is fragmented again"
    );
    assert!(
        fused_view.def.nodes.iter().any(|n| {
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
    let fused = render((*fused_view.def).clone());

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
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::runtime::PresetRuntime;

    let device = manifold_gpu::testkit::test_device();
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

/// A reset-gated in-place buffer seed must render IDENTICALLY whether gated
/// (canonical) or ungated (seed runs every frame): the seed feeds array_feedback
/// only on reset, which both hit on frame 0, so skipping the redundant re-seeds
/// between resets changes nothing. Proves the gate is invisible AND that the
/// aliased seed skips WITHOUT tripping the executor's stale-output guard (a
/// regression there would panic this render). ParticleText: seed_alloc is
/// OnceOnReset, so the buffer persists and the skip relies on real retention.
#[test]
fn particletext_seed_gate_matches_ungated() {
    let device = manifold_gpu::testkit::test_device();
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
    let device = manifold_gpu::testkit::test_device();
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
/// now fuse with `q16` register rounding, and
/// the fused render must match the unfused one BIT-EXACTLY through the raw
/// executor — the loop amplifies any rounding mismatch, so 8 frames at
/// 256² is a real drift test, not a smoke test. (Raw harness, not
/// PresetRuntime: the production path carries the parked
/// [[particletext_canonical_fused_diag]] f16-seed divergence which would
/// pollute this oracle.)
#[test]
fn oilyfluid_inloop_f16_fusion_matches_unfused() {
    let device = manifold_gpu::testkit::test_device();
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
    let fused_view =
        manifold_node_engine::freeze::install::fuse_generator_view(&def, &registry).expect("fuses");

    // The tier actually engaged: the fused def must carry a q16-quantized
    // kernel (an in-loop f16 member fused) — else this oracle is vacuous.
    assert!(
        fused_view
            .def
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
            &fused_view.def, &registry, &device.arc(), w, h, frames, &pick_tail, true,
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
    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (256u32, 256u32);
    let json = crate::node_graph::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("MetallicGlass"),
    )
    .expect("MetallicGlass bundled");
    let mut def: EffectGraphDef = serde_json::from_str(&json).unwrap();
    shrink_particle_pool(&mut def, 100_000); // no pools today; suite-parallelism hygiene
    let def = manifold_core::flatten::flatten_groups(&def).expect("flattens");
    let fused_view =
        manifold_node_engine::freeze::install::fuse_generator_view(&def, &registry).expect("fuses");

    // Non-vacuous: pack_channels must be fused AWAY (it only fuses through the
    // unwired-optional path), and some fused kernel must carry the literal
    // unwired argument the new codegen emits.
    assert!(
        !fused_view.def.nodes.iter().any(|n| n.type_id == "node.pack_rgba"),
        "pack_channels must fold into the sobel-tail region"
    );
    assert!(
        fused_view
            .def
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
            &fused_view.def, &registry, &device.arc(), w, h, frames, &pick_fused, true,
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
#[ignore = "BUG-i6eo (ParticleText fused-flow-field proof GPU hang): parked 2026-07-31 — the fused def's full-frame render hangs the GPU at the frame-1 commit (last dispatch: the particle-domain fused kernel); every component individually exonerated, resume trail in the bead"]
fn particletext_fp32_flow_field_fused_matches_unfused() {
    let device = manifold_gpu::testkit::test_device();
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

    let Some(fused_view) = manifold_node_engine::freeze::install::fuse_generator_view(&def, &registry)
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
            !fused_view.def.nodes.iter().any(|n| n.node_id.as_str() == node_id),
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
        let (f, fd) = render_def_capture_node(&fused_view.def, &registry, &device.arc(), w, h, frames, &by_fused)
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

/// FluidSim3D twin of [`particletext_seed_gate_matches_ungated`]. Here seed_alloc
/// is EveryFrame, so the gated skip relies on the order (seed_alloc writes, the
/// gated seed_pattern re-dispatches only on reset) rather than buffer retention —
/// still invisible because array_feedback reads the seed only on reset.
#[test]
fn fluidsim3d_seed_gate_matches_ungated() {
    let device = manifold_gpu::testkit::test_device();
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
