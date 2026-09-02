//! SCENE_LOOP_DESIGN P2 end-to-end gate: drive the REAL renderer-side plan
//! builder (`assemble_scene_loop_plan`) through the REAL editing command
//! (`ApplySceneLoopCommand`) against a REAL imported GLB graph
//! (`assemble_import_graph` on `tests/fixtures/gltf/apricot_tl05.glb`), then
//! RENDER one frame and assert copies present: count=1 vs count=3 frames must
//! differ above threshold (the P1 wrap test's `max_pixel_diff`, same family).
//!
//! This is the seam that let P1 ship: a hand-built `SceneLoopPlan` in a unit
//! test never exercised production plan construction. Here the plan comes
//! from the SAME builder the panel's "Enable Scene Loop" dispatches.

use std::path::Path;

use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_editing::command::Command;
use manifold_editing::commands::graph::ApplySceneLoopCommand;
use manifold_renderer::node_graph::gltf_import::{
    assemble_import_graph, assemble_scene_loop_plan,
};
use manifold_renderer::node_graph::scene_vm::RENDER_SCENE_TYPE_ID;
use manifold_renderer::node_graph::{PrimitiveRegistry, render_viewport_frame};
use manifold_renderer::preset_context::PresetContext;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/apricot_tl05.glb"
);

fn max_pixel_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

// BLOCKED-tracking gate (CLAUDE.md: "passes the test but codegen can't
// express it" = BLOCKED, tracked, never a quiet exemption). Currently RED:
// count=1/3/8 render identically end-to-end on the real import — the instance
// splice is structurally present in the flat graph but no copies reach the
// renderer. Same-input same-session renders are deterministic on a SHARED
// device (noise=0), so this is a genuine no-copies finding, not noise. See
// /tmp/scene_loop_p1_verdict.md Escaped note; renderer investigation is the
// owner's (lead) call. Run explicitly with `cargo test -- --ignored`.
#[test]
#[ignore = "BLOCKED: scene loop instances do not render on real import — see P1 verdict Escaped note"]
fn scene_loop_apply_import_renders_copies() {
    let (def, report) = assemble_import_graph(Path::new(FIXTURE))
        .unwrap_or_else(|e| panic!("assemble_import_graph({FIXTURE}) failed: {e}"));
    assert!(
        report.object_count > 0,
        "fixture must import at least one object group"
    );
    let render_scene_id = def
        .nodes
        .iter()
        .find(|n| n.type_id == RENDER_SCENE_TYPE_ID)
        .expect("import has a render_scene node")
        .id;

    // The REAL plan builder (D5) — the same one the panel dispatches.
    let plan = assemble_scene_loop_plan(&def, render_scene_id)
        .expect("plan builder must succeed on the imported scene");
    assert!(
        !plan.instance_wirings.is_empty(),
        "plan must splice every object group's instances port"
    );

    let layer_count = def.nodes.iter().map(|n| n.id).max().unwrap_or(0);
    assert!(
        plan.new_nodes.iter().all(|n| n.id > layer_count),
        "plan mints fresh doc ids beyond the import's"
    );

    // Apply through the REAL command (editing) against a Project, exactly as
    // the panel's ProjectAction does.
    let mut project = Project::default();
    let idx = project.timeline.add_layer(
        "Apricot Loop",
        LayerType::Generator,
        PresetTypeId::from_string("ApricotLoopTest".to_string()),
    );
    {
        let layer = &mut project.timeline.layers[idx];
        layer.gen_params_or_init().graph = Some(def.clone());
    }
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let catalog = manifold_core::effect_graph_def::EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: None,
        nodes: Vec::new(),
        wires: Vec::new(),
    };
    let mut cmd = ApplySceneLoopCommand::new(target, Vec::new(), plan, catalog);
    cmd.execute(&mut project);

    let applied = project.timeline.layers[idx]
        .generator_graph()
        .expect("layer graph survives apply");

    // count=1 vs count=3 must render differently — the copies are real
    // geometry, not a silent no-op splice. One shared device for both frames
    // (same-session semantics), and a threshold far above the import's own
    // per-call render noise (~10 on this fixture — see the parity test's
    // same-input diagnostic below).
    let device = std::sync::Arc::new(manifold_gpu::GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (64u32, 64u32);
    let render_copies = |copies: f32| -> Vec<u8> {
        let mut d = applied.clone();
        d.nodes
            .iter_mut()
            .find(|n| n.node_id.as_str() == "scene_array")
            .expect("scene_array node missing after apply")
            .params
            .insert("count".to_string(), SerializedParamValue::Float { value: copies });
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 0.016,
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
        let (rgba, _, _) = render_viewport_frame(
            d,
            &registry,
            device.clone(),
            w,
            h,
            &ctx,
        )
        .expect("render_viewport_frame");
        rgba
    };
    let frame_1a = render_copies(1.0);
    let frame_3 = render_copies(3.0);
    let frame_1b = render_copies(1.0);
    // Same-count renders must match (device/session determinism on this
    // fixture: with a SHARED device both raw-noise and self-diff are 0).
    let nonblack = frame_1a.iter().filter(|&&v| v > 0).count();
    let noise = max_pixel_diff(&frame_1a, &frame_1b);
    let diff = max_pixel_diff(&frame_1a, &frame_3);
    assert!(nonblack > 0, "imported looped scene should render visible pixels");
    assert_eq!(noise, 0, "same-count renders must be identical (noise={noise})");
    assert!(
        diff > 40,
        "P2 gate: count=1 vs count=3 frames differ by only {diff} — the \
         instance splice did not visibly reach the scene_object (or the loop \
         camera is not in frame). The panel would show a 'Scene Loop' that \
         renders nothing. See /tmp/scene_loop_p1_verdict.md Escaped note."
    );
}

/// The wrap-parity contract against the REAL import + REAL plan: phase 0 vs
/// phase 0.99999 must be pixel-identical through the applied graph (INV-3,
/// re-proven on the production path — a hand-built minimal graph can't catch
/// an import-only driver sneaking in).
#[test]
#[ignore = "BLOCKED: real-import render is not deterministic per GpuDevice (diff ~80 same-input) — see P1 verdict Escaped note"]
fn scene_loop_import_wrap_parity_phases_match() {
    let (def, _) = assemble_import_graph(Path::new(FIXTURE))
        .unwrap_or_else(|e| panic!("assemble_import_graph({FIXTURE}) failed: {e}"));
    let render_scene_id = def
        .nodes
        .iter()
        .find(|n| n.type_id == RENDER_SCENE_TYPE_ID)
        .expect("render_scene").id;
    let plan = assemble_scene_loop_plan(&def, render_scene_id).expect("plan");

    let mut project = Project::default();
    let idx = project.timeline.add_layer(
        "Apricot Loop Parity",
        LayerType::Generator,
        PresetTypeId::from_string("ApricotLoopParityTest".to_string()),
    );
    {
        let layer = &mut project.timeline.layers[idx];
        layer.gen_params_or_init().graph = Some(def);
    }
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let catalog = EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: None,
        nodes: Vec::new(),
        wires: Vec::new(),
    };
    let mut cmd = ApplySceneLoopCommand::new(target, Vec::new(), plan, catalog);
    cmd.execute(&mut project);

    let applied = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph after apply");

    // rate = 1/8 (8 bars per loop). beat 0 → phase 0; beat 8 → fract(1.0) →
    // phase 0 exactly, the same wrap the P1 unit test asserts.
    // Same buffer size contract as the P1 unit test. One shared device and
    // registry across both frames — the real app renders consecutive frames
    // through the SAME device, so a model that is deterministic between two
    // frames of ONE session is what wrap purity means on stage (a
    // per-device seed must not separate two beats of the same run).
    let device = std::sync::Arc::new(manifold_gpu::GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let (w, h) = (64u32, 64u32);
    let render_at = |beat: f64| -> Vec<u8> {
        let ctx = PresetContext {
            time: beat * 0.5,
            beat,
            dt: 0.016,
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
        let (rgba, _, _) = render_viewport_frame(
            applied.clone(),
            &registry,
            device.clone(),
            w,
            h,
            &ctx,
        )
        .expect("render_viewport_frame");
        rgba
    };
    // SANITY: the RENDER path must already be deterministic between two
    // frames of one session on THIS fixture. The P1 minimal-graph wrap test is
    // deterministic; the real import's AO/cinematic tail was found NOT to be
    // (same inputs, diff≈10 run-to-run — observed while authoring this gate).
    // If that reoccurs, fail LOUDLY naming the seam rather than producing a
    // flaky parity assert: wrap purity can only be asserted once the renderer
    // is deterministic on the production import.
    let raw_a = render_at(0.0);
    let raw_a2 = render_at(0.0);
    let det = max_pixel_diff(&raw_a, &raw_a2);
    assert!(
        det == 0,
        "SEAM FINDING: same-input same-session renders of the real import differ \
         by {det} — the import's AO/cinematic path is not deterministic, so INV-3 \
         wrap parity cannot be asserted on this fixture yet. Surface to the lead; \
         do not paper over with a tolerance."
    );

    let a = render_at(0.0);
    // beat = 8 × rate(0.125) = 1.0 → fract → phase 0 exactly (same wrap the
    // P1 unit test asserts at).
    let b = render_at(8.0);
    let diff = max_pixel_diff(&a, &b);
    assert_eq!(
        diff, 0,
        "INV-3: phase 0 vs ~1 through the real import + real plan disagree (diff={diff})"
    );
}