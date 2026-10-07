use crate::node_graph::freeze::TextureDiff;
use crate::node_graph::freeze::markers::Marker;
use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use crate::node_graph::execution_plan::compile;
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::{
    EffectGraphDefExt, Executor, FrameTime, MetalBackend, NodeInstanceId,
    PrimitiveRegistry, StateStore,
};
use crate::render_target::RenderTarget;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;

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

use crate::testkit::proof_support::*;

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
/// the ColorGrade proof above (freeze section 7 tier 4, "out-of-loop texture
/// regions: ≈1 ulp, not bit-exact, and cannot be" — the unfused chain
/// round-trips the intermediate through an actual rgba16float texture
/// between the two members; the fused kernel keeps it in an f32 register).
/// Measured gap: max_abs ≈ 1/1024 (one f16 ULP at this value range) — the
/// SAME tolerance band `auto_fused_colorgrade_via_executor_matches_unfused`
/// uses.
#[test]
fn camera_derived_pointwise_atom_fuses_and_matches_unfused() {
    use crate::node_graph::freeze::install::{FusedDef, fuse_canonical_def};
    use crate::testkit::test_camera_pointwise_fixture::TestCameraPointwise;

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
    let mut unfused_graph = def.clone().into_graph(&registry, &crate::node_graph::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
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

    let mut fused_graph = fused_def.into_graph(&registry, &crate::node_graph::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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
    // Out-of-loop texture tier (freeze section 7.4): ≈1 f16 ULP, same tolerance band
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
    use crate::node_graph::freeze::install::{FusedDef, fuse_canonical_def};

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
    let mut unfused = def.clone().into_graph(&registry, &crate::node_graph::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
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
    let mut fused = fdef.into_graph(&registry, &crate::node_graph::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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

/// BUG-agfh (Codegen: buffer atom with several outputs, one atomic): an atom
/// with an aliased pointwise particle output AND an atomic fixed-point side
/// output (`test.multi_output_atomic`, the `grid_to_matter` + reaction shape)
/// sits between two fusable particle regions. Fusion must cut at it — both
/// neighbouring regions fuse, the atom itself stays a standalone node — and
/// the fused graph must match the unfused one exactly: the atomic momentum
/// sums word for word, and the rendered density bit for bit.
///
/// Exactness is owed, not hoped for: the upstream region is a fused buffer
/// region (bit-exact to unfused by the precision contract's tier 3), so the
/// atom sees identical particles either way, and integer `atomicAdd` is
/// order-independent.
#[test]
fn atomic_side_output_atom_cuts_fusion_and_matches_unfused() {
    use crate::node_graph::freeze::install::fuse_generator_view;
    use crate::node_graph::graph_loader::{
        BoundaryHandling, HandleScope, instantiate_def, pre_allocate_resources,
    };
    use crate::node_graph::mesh_change::PreparedMeshRules;
    use crate::testkit::test_multi_output_atomic_fixture::{
        MOMENTUM_WORDS, TYPE_ID as FIXTURE, TestMultiOutputAtomic,
    };

    let test_device = crate::test_device();
    let device = test_device.arc();
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register(FIXTURE, || Box::new(TestMultiOutputAtomic::new()));
    let (w, h) = (128u32, 128u32);

    let def: EffectGraphDef = serde_json::from_str(&format!(
        r#"{{
        "version": 1, "name": "AtomicSideOutputCut", "nodes": [
            {{ "id": 0, "typeId": "system.generator_input", "nodeId": "input" }},
            {{ "id": 1, "typeId": "node.spawn_particles", "nodeId": "spawn", "params": {{
                "max_capacity": {{ "type": "Int", "value": 4096 }},
                "active_count": {{ "type": "Int", "value": 4096 }} }} }},
            {{ "id": 2, "typeId": "node.spread_out", "nodeId": "kick_a", "params": {{
                "diffusion": {{ "type": "Float", "value": 0.05 }},
                "active_count": {{ "type": "Int", "value": 4096 }} }} }},
            {{ "id": 3, "typeId": "node.wrap_around", "nodeId": "wrap_a", "params": {{
                "active_count": {{ "type": "Int", "value": 4096 }} }} }},
            {{ "id": 4, "typeId": "{FIXTURE}", "nodeId": "drag", "params": {{
                "drag": {{ "type": "Float", "value": 0.5 }},
                "fixed_point_scale": {{ "type": "Float", "value": 65536.0 }} }} }},
            {{ "id": 5, "typeId": "node.spread_out", "nodeId": "kick_b", "params": {{
                "diffusion": {{ "type": "Float", "value": 0.03 }},
                "active_count": {{ "type": "Int", "value": 4096 }} }} }},
            {{ "id": 6, "typeId": "node.wrap_around", "nodeId": "wrap_b", "params": {{
                "active_count": {{ "type": "Int", "value": 4096 }} }} }},
            {{ "id": 7, "typeId": "node.draw_particles", "nodeId": "splat", "params": {{
                "active_count": {{ "type": "Int", "value": 4096 }},
                "scaled_energy": {{ "type": "Int", "value": 4096 }} }} }},
            {{ "id": 8, "typeId": "node.resolve_scatter", "nodeId": "resolve", "params": {{
                "fixed_point_scale": {{ "type": "Float", "value": 4096.0 }} }} }},
            {{ "id": 9, "typeId": "system.final_output", "nodeId": "final_output" }}
        ], "wires": [
            {{ "fromNode": 1, "fromPort": "particles", "toNode": 2, "toPort": "in" }},
            {{ "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }},
            {{ "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "points" }},
            {{ "fromNode": 4, "fromPort": "points_out", "toNode": 5, "toPort": "in" }},
            {{ "fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in" }},
            {{ "fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "particles" }},
            {{ "fromNode": 0, "fromPort": "output_width", "toNode": 7, "toPort": "width" }},
            {{ "fromNode": 0, "fromPort": "output_height", "toNode": 7, "toPort": "height" }},
            {{ "fromNode": 7, "fromPort": "accum", "toNode": 8, "toPort": "accum" }},
            {{ "fromNode": 8, "fromPort": "density", "toNode": 9, "toPort": "in" }}
        ]
    }}"#
    ))
    .expect("parse the atomic-side-output def");

    let fused_view = fuse_generator_view(&def, &registry).expect("the particle regions fuse");
    let fused_def: &EffectGraphDef = &fused_view.def;
    // Non-vacuous: both neighbouring regions fused, the atom did not.
    let fused_kernels = fused_def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").count();
    assert_eq!(fused_kernels, 2, "kick+wrap on each side of the atom must fuse into its own kernel");
    for gone in ["node.spread_out", "node.wrap_around"] {
        assert!(
            !fused_def.nodes.iter().any(|n| n.type_id == gone),
            "{gone} must be absorbed into a fused region"
        );
    }
    assert_eq!(
        fused_def.nodes.iter().filter(|n| n.type_id == FIXTURE).count(),
        1,
        "the atomic-side-output atom must stay a standalone node (region cut)"
    );

    // Render `frames` frames; return the atom's momentum words and the
    // resolved density copied out.
    let render = |def: &EffectGraphDef, rules: &PreparedMeshRules| -> (Vec<i32>, RenderTarget) {
        let mut graph = Graph::new();
        let inst = instantiate_def(
            &mut graph,
            def,
            &registry,
            HandleScope::Global,
            BoundaryHandling::Standalone,
            rules,
        )
        .expect("instantiate");
        let node_of = |type_id: &str| -> NodeInstanceId {
            let id = def.nodes.iter().find(|n| n.type_id == type_id).map(|n| n.id).unwrap();
            *inst.id_map.get(&id).unwrap()
        };
        let atom = node_of(FIXTURE);
        // No atom on main reads an i32 array yet; the test is the reader, so
        // the side output is an external output (else it gets no resource).
        graph.add_external_output(atom, "momentum").expect("momentum is an output");
        let plan = compile(&graph).expect("compile");
        let resolve = node_of("node.resolve_scatter");
        let gen_in = node_of("system.generator_input");
        let momentum_res = resource_for_output(&plan, atom, "momentum");

        let mut backend = MetalBackend::new(std::sync::Arc::clone(&device), w, h, FMT);
        pre_allocate_resources(&mut graph, &plan, &device, &mut backend).expect("pre-allocate");
        let mut exec = Executor::new(Box::new(backend));
        exec.set_preview_target(Some(resolve));
        let mut state = StateStore::new();
        for i in 0..2u32 {
            let t = f64::from(i) / 60.0;
            for (name, v) in [("output_width", w as f32), ("output_height", h as f32)] {
                graph.set_param(gen_in, name, ParamValue::Float(v)).expect("host param");
            }
            let ft = FrameTime {
                seconds: Seconds(t),
                beats: Beats(t * 2.0),
                delta: Seconds(1.0 / 60.0),
                frame_count: i64::from(i),
            };
            let mut enc = device.create_encoder("agfh-frame");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                exec.execute_frame_with_state(&mut graph, &plan, ft, &mut gpu, &mut state, 0);
            }
            enc.commit_and_wait_completed();
        }

        let slot = exec.backend().slot_for(momentum_res).expect("momentum slot");
        let buf = exec.backend().array_buffer(slot).expect("momentum buffer");
        let momentum = unsafe {
            std::slice::from_raw_parts(
                buf.mapped_ptr().expect("shared momentum buffer").cast::<i32>(),
                MOMENTUM_WORDS as usize,
            )
        }
        .to_vec();

        let res = exec.preview_resource().expect("preview resource");
        let tex = exec.backend().texture_2d(exec.backend().slot_for(res).unwrap()).unwrap();
        let out = RenderTarget::new(&device, tex.width, tex.height, tex.format, "agfh-capture");
        let mut enc = device.create_encoder("agfh-copy");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            gpu.copy_texture_to_texture(tex, &out.texture, tex.width, tex.height);
        }
        enc.commit_and_wait_completed();
        (momentum, out)
    };

    let (u_momentum, u_img) = render(&def, &PreparedMeshRules::default());
    let (f_momentum, f_img) = render(fused_def, &fused_view.mesh_rules);

    assert!(
        u_momentum.iter().any(|&m| m != 0),
        "the atom must accumulate non-zero momentum (non-vacuous): {u_momentum:?}"
    );
    assert_eq!(f_momentum, u_momentum, "atomic side output must match unfused word for word");

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &u_img.texture, &f_img.texture, 0.0, 0.0);
    assert!(
        r.over_count == 0 && r.max_abs == 0.0,
        "fused density must match unfused bit for bit: max_abs={}, over={}/{}",
        r.max_abs,
        r.over_count,
        r.total
    );
}
