use manifold_node_engine::freeze::TextureDiff;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::exec::execution_plan::compile;
use manifold_node_engine::graph::Graph;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::persistence::{EffectGraphDefExt, PrimitiveRegistry};
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_core::effect_graph_def::EffectGraphDef;
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

use manifold_node_engine::testkit::proof_support::*;

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
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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
    let mut unfused_graph = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
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
        fused_wgsl.contains("(near + raw * (far - near))"),
        "the fused camera-depth helper must retain the reversed-Z inverse, rather than a stale forward-depth formula:\n{}",
        fused_wgsl
    );
    assert!(
        fused_wgsl.contains("@camera_external: camera_ext_0")
            && fused_wgsl.contains("@derived_uniform_member:"),
        "the fused kernel must carry both D7/P0 markers (camera_ext port + \
         derived-uniform recompute):\n{}",
        fused_wgsl
    );

    let mut fused_graph = fused_def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
    set_by_node_id(&mut fused_graph, "lens", "focus_distance", focus_distance);
    set_by_node_id(&mut fused_graph, "lens", "f_stop", f_stop);
    let fused_node = find_node(&fused_graph, "node.wgsl_compute");
    // coc_from_depth has two params (max_radius, world_to_mm) but this
    // fixture leaves them at their defaults on both sides, so no retarget
    // lookup is needed here beyond confirming the field exists (parity with
    // I6's pattern of driving the fused node's port-shadow through
    // `retarget`).
    let _ = retarget;
    let fused_plan = compile(&fused_graph).expect("compile fused");
    let f_src = resource_for_output(&fused_plan, find_node(&fused_graph, "system.source"), "out");
    let f_out = resource_for_output(&fused_plan, fused_node, "dst");
    let fused = render_graph(&device.arc(), &mut fused_graph, &fused_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    // Out-of-loop texture tier (freeze section 7.4): ≈1 f16 ULP, same tolerance band
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

/// The Ocean backdrop region (docs/OCEAN_SURFACE_DESIGN.md): `node.camera_sky`
/// (camera-derived, Gather on the sky) feeding `node.over`'s bottom, with a
/// varying-alpha top, fuses into one kernel and matches the unfused pair.
#[test]
fn camera_sky_over_fuses_and_matches_unfused() {
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (64u32, 64u32);
    let input = gradient_input_varying_alpha(&device, w, h);

    let json = r#"{
        "version": 1, "name": "CameraSkyOverFusion", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.free_camera", "nodeId": "cam" },
            { "id": 2, "typeId": "node.camera_sky", "nodeId": "sky" },
            { "id": 3, "typeId": "node.over", "nodeId": "over" },
            { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "sky" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "camera" },
            { "fromNode": 0, "fromPort": "out", "toNode": 3, "toPort": "top" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "bottom" },
            { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
        ]
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).expect("parse fixture graph");
    let set_by_node_id = |g: &mut Graph, node_id: &str, param: &str, v: f32| {
        let id = g
            .node_id_by_handle(node_id)
            .or_else(|| g.instance_by_node_id(&manifold_core::NodeId::new(node_id)))
            .unwrap_or_else(|| panic!("graph missing node `{node_id}`"));
        g.set_param(id, param, ParamValue::Float(v))
            .unwrap_or_else(|e| panic!("set {node_id}.{param}: {e:?}"));
    };
    let aim = |g: &mut Graph| {
        set_by_node_id(g, "cam", "yaw", 0.7);
        set_by_node_id(g, "cam", "pitch", 0.3);
    };

    let mut unfused_graph = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
    aim(&mut unfused_graph);
    let unfused_plan = compile(&unfused_graph).expect("compile unfused");
    let u_src = resource_for_output(&unfused_plan, find_node(&unfused_graph, "system.source"), "out");
    let u_out = resource_for_output(&unfused_plan, find_node(&unfused_graph, "node.over"), "out");
    let unfused = render_graph(&device.arc(), &mut unfused_graph, &unfused_plan, u_src, &input, u_out);

    let FusedDef { def: fused_def, .. } =
        fuse_canonical_def(&def, &registry).expect("camera_sky + over is one fusable region");
    assert_eq!(
        fused_def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").count(),
        1,
        "camera_sky and over must collapse to exactly one fused node"
    );
    assert!(
        fused_def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.wgsl_compute")
            .and_then(|n| n.wgsl_source.as_deref())
            .is_some_and(|s| s.contains("@camera_external: camera_ext_0")
                && s.contains("@derived_uniform_member:")),
        "the fused kernel must read the camera through the derived-uniform recompute"
    );
    let mut fused_graph = fused_def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
    aim(&mut fused_graph);
    let fused_node = find_node(&fused_graph, "node.wgsl_compute");
    let fused_plan = compile(&fused_graph).expect("compile fused");
    let f_src = resource_for_output(&fused_plan, find_node(&fused_graph, "system.source"), "out");
    let f_out = resource_for_output(&fused_plan, fused_node, "dst");
    let fused = render_graph(&device.arc(), &mut fused_graph, &fused_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &unfused.texture, &fused.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005) && r.over_count < 64,
        "camera_sky + over fusion must match unfused within the out-of-loop \
         tolerance: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
}

/// `node.sea_horizon_env` (Gather on the sky) fused with a Pointwise
/// neighbour matches the unfused pair.
#[test]
fn sea_horizon_env_fuses_and_matches_unfused() {
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (64u32, 64u32);
    let input = gradient_input(&device, w, h);
    let json = r#"{
        "version": 1, "name": "SeaHorizonFusion", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.sea_horizon_env", "nodeId": "sea" },
            { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
            { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "sky" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
        ]
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).expect("parse fixture graph");

    let mut unfused_graph = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
    let unfused_plan = compile(&unfused_graph).expect("compile unfused");
    let u_src = resource_for_output(&unfused_plan, find_node(&unfused_graph, "system.source"), "out");
    let u_out = resource_for_output(&unfused_plan, find_node(&unfused_graph, "node.invert"), "out");
    let unfused = render_graph(&device.arc(), &mut unfused_graph, &unfused_plan, u_src, &input, u_out);

    let FusedDef { def: fused_def, .. } =
        fuse_canonical_def(&def, &registry).expect("sea_horizon_env + invert is one fusable region");
    assert_eq!(
        fused_def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").count(),
        1,
        "sea_horizon_env and invert must collapse to exactly one fused node"
    );
    let mut fused_graph = fused_def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
    let fused_node = find_node(&fused_graph, "node.wgsl_compute");
    let fused_plan = compile(&fused_graph).expect("compile fused");
    let f_src = resource_for_output(&fused_plan, find_node(&fused_graph, "system.source"), "out");
    let f_out = resource_for_output(&fused_plan, fused_node, "dst");
    let fused = render_graph(&device.arc(), &mut fused_graph, &fused_plan, f_src, &input, f_out);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &unfused.texture, &fused.texture, OUT_OF_LOOP_ULP_ABS_TOL, OUT_OF_LOOP_ULP_REL_TOL);
    assert!(
        r.passes(0.005) && r.over_count < 64,
        "sea_horizon_env + invert fusion must match unfused: max_abs={}, max_rel={}, over={}/{} ({:.4})",
        r.max_abs,
        r.max_rel,
        r.over_count,
        r.total,
        r.over_fraction()
    );
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
) -> manifold_node_engine::freeze::DiffResult {
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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

    let mut unfused = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
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
    let mut fused = fdef.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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

    let mut unfused = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
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
    let mut fused = fdef.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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
    let regions = manifold_node_engine::freeze::region::partition_regions(&def, &registry);
    assert_eq!(regions.len(), 1, "blur + absorbed warp form one region");
    assert_eq!(regions[0].virtual_chains.len(), 1, "the warp is a virtual chain");
    assert_eq!(regions[0].virtual_chains[0].members[0].doc_id, 2);

    let mut unfused = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
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
    let mut fused = fdef.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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

/// Look-equivalence for the Watercolor/Bloom preset swap: a legacy `node.blur`
/// (monolithic H+V with an internal f16 scratch) renders identically to an
/// explicit pair of `node.gaussian_blur` passes in `Linear` mode — the exact
/// port of blur.wgsl's loop, with the same f16 texture between the axes. Both
/// run unfused here; agreement is to f16-ulp scale (separate kernel
/// compilations may differ in FMA contraction). This is what licenses
/// rewriting the presets onto the fusable single-axis atom.
#[test]
fn linear_blur_pair_matches_legacy_blur_node() {
    let device = manifold_gpu::testkit::test_device();
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
    let mut l_graph = l_def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("legacy graph");
    let l_plan = compile(&l_graph).expect("compile legacy");
    let l_src = resource_for_output(&l_plan, find_node(&l_graph, "system.source"), "out");
    let l_out = resource_for_output(&l_plan, find_node(&l_graph, "node.blur"), "out");
    let l_img = render_graph(&device.arc(), &mut l_graph, &l_plan, l_src, &input, l_out);

    let p_def: EffectGraphDef = serde_json::from_str(pair).unwrap();
    let mut p_graph = p_def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("pair graph");
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
    use manifold_node_engine::freeze::install::fuse_generator_view;
    use manifold_node_engine::freeze::region::{NodeClass, classify_node, partition_regions};
    use manifold_node_engine::runtime::preset_context::PresetContext;
    use manifold_node_engine::runtime::PresetRuntime;

    let device = manifold_gpu::testkit::test_device();
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

    let fused_view = fuse_generator_view(&canonical, &registry).expect("the pair fuses");

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
    let fused = render((*fused_view.def).clone());

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
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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
    let mut unfused_graph = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
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
    let mut fused_graph = fused_def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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

    let mut unfused = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.invert"), "out");
    let u_img = render_graph_at(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out, qw, qh);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the quarter-res chain fuses");
    let mut fused = fdef.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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

    let mut unfused = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.invert"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the control-wired region fuses");
    let mut fused = fdef.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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
    use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};

    let device = manifold_gpu::testkit::test_device();
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

    let mut unfused = def.clone().into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("unfused graph");
    let u_plan = compile(&unfused).expect("compile unfused");
    let u_src = resource_for_output(&u_plan, find_node(&unfused, "system.source"), "out");
    let u_out = resource_for_output(&u_plan, find_node(&unfused, "node.mix"), "out");
    let u_img = render_graph(&device.arc(), &mut unfused, &u_plan, u_src, &input, u_out);

    let FusedDef { def: fdef, .. } =
        fuse_canonical_def(&def, &registry).expect("the fan-out region fuses");
    let mut fused = fdef.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("fused graph builds");
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
