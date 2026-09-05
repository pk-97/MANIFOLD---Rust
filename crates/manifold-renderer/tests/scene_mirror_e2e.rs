//! SCENE_MIRROR_DESIGN P2 end-to-end gate (the `scene_loop_e2e_import.rs`
//! pattern for the mirror kind): import DamagedHelmet, apply Scene Loop →
//! Scene Mirror through the REAL descriptor plan builders + the REAL
//! generic editing commands, and prove the mirrored copy RENDERS — a
//! computed region probe only, no human-look gate.
//!
//! Probe design: the mirrored copies hang below the floor plane (axis +Y,
//! plane_offset = scene_bounds min-Y, D8). The performer's framing gesture
//! (the loop_camera Height/Pitch/FOV rows — all whitelisted) puts the
//! camera above the path pitched down, so the reflection is spatially
//! separate, unoccluded, and in the lower half of the frame (verified
//! empirically 2026-09-05; at the level camera the reflection maps below
//! the frame edge, and a mirror plane through the object z-fights it —
//! neither says anything about the splice). Three renders:
//!   A. mirror enabled (applied state) — the show look
//!   B. `enabled = 0` (the INV-MR1 off write, one param write, no rebuild)
//!   C. after a save/reload, `plane_offset` written through the row-write
//!      command (the performer gesture) — the reflected region moves
//! Assertions (thresholds measured on an M-series device, margins ≥2×
//! stated): A≠B with the diff concentrated in the lower half and the
//! reflected region non-background (differs from the corner-sampled
//! void), and A≠C the same way.

use std::path::Path;

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_editing::command::Command;
use manifold_editing::commands::graph::{ApplySceneModifierCommand, SetGraphNodeParamCommand};
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::scene_modifier::{LOOP_KIND_ID, MIRROR_KIND_ID, build_plan};
use manifold_renderer::node_graph::scene_vm::RENDER_SCENE_TYPE_ID;
use manifold_renderer::node_graph::{PrimitiveRegistry, render_viewport_frame};
use manifold_renderer::preset_context::PresetContext;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/khronos/DamagedHelmet.glb"
);

const W: u32 = 256;
const H: u32 = 256;

fn render(def: &manifold_core::effect_graph_def::EffectGraphDef) -> Vec<u8> {
    let device = manifold_gpu::GpuDevice::new();
    let registry = PrimitiveRegistry::with_builtin();
    let ctx = PresetContext {
        time: 2.0,
        beat: 4.0,
        dt: 0.016,
        width: W,
        height: H,
        output_width: W,
        output_height: H,
        aspect: W as f32 / H as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let (rgba, _, _) = render_viewport_frame(
        def.clone(),
        &registry,
        std::sync::Arc::new(device),
        W,
        H,
        &ctx,
    )
    .expect("render_viewport_frame");
    rgba
}

fn dump_png(name: &str, rgba: &[u8]) {
    let Ok(out_dir) = std::env::var("MIRROR_E2E_OUT") else { return };
    let png = format!("{out_dir}/{name}.png");
    image::save_buffer(&png, rgba, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("mirror e2e frame → {png}");
}

/// Per-channel mean over a region, for region-mean probes.
fn region_mean(rgba: &[u8], x0: u32, y0: u32, x1: u32, y1: u32) -> [f32; 3] {
    let mut sum = [0f64; 3];
    let mut n = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * W + x) * 4) as usize;
            sum[0] += rgba[i] as f64;
            sum[1] += rgba[i + 1] as f64;
            sum[2] += rgba[i + 2] as f64;
            n += 1;
        }
    }
    [
        (sum[0] / n as f64) as f32,
        (sum[1] / n as f64) as f32,
        (sum[2] / n as f64) as f32,
    ]
}

/// Pixels where the two frames differ by more than a just-noticeable step.
fn diff_mask(a: &[u8], b: &[u8], threshold: u8) -> Vec<bool> {
    a.chunks(4)
        .zip(b.chunks(4))
        .map(|(pa, pb)| {
            pa.iter()
                .zip(pb.iter())
                .take(3)
                .map(|(x, y)| x.abs_diff(*y))
                .max()
                .unwrap_or(0)
                > threshold
        })
        .collect()
}

fn mask_stats(mask: &[bool]) -> (usize, usize) {
    let total = mask.iter().filter(|&&m| m).count();
    let lower = mask
        .iter()
        .enumerate()
        .filter(|(i, m)| **m && *i >= (W * H / 2) as usize)
        .count();
    (total, lower)
}

/// Write one node param through the SAME command the bound-row write path
/// uses at the def level (the fog INV-M7 test pattern).
fn write_param(project: &mut Project, idx: usize, doc: u32, param: &str, value: f32) {
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let mut cmd = SetGraphNodeParamCommand::new(
        manifold_core::GraphTarget::Generator(layer_id),
        doc,
        param.to_string(),
        SerializedParamValue::Float { value },
        manifold_core::effect_graph_def::EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        },
    );
    cmd.execute(project);
}

fn node_doc(project: &Project, idx: usize, node_id: &str) -> u32 {
    project.timeline.layers[idx]
        .generator_graph()
        .expect("graph")
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == node_id)
        .unwrap_or_else(|| panic!("{node_id} minted"))
        .id
}

/// Apply loop → mirror to a fresh project carrying the imported `def`;
/// returns (project, layer index).
fn apply_loop_and_mirror(
    def: manifold_core::effect_graph_def::EffectGraphDef,
) -> (Project, usize) {
    let mut project = Project::default();
    let idx = project.timeline.add_layer(
        "Mirror E2E",
        LayerType::Generator,
        PresetTypeId::from_string("MirrorE2E".to_string()),
    );
    project.timeline.layers[idx].gen_params_or_init().graph = Some(def);
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = || manifold_core::GraphTarget::Generator(layer_id.clone());
    let catalog = manifold_core::effect_graph_def::EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: None,
        nodes: Vec::new(),
        wires: Vec::new(),
    };

    for kind_id in [LOOP_KIND_ID, MIRROR_KIND_ID] {
        let render_scene_id = project.timeline.layers[idx]
            .generator_graph()
            .expect("graph")
            .nodes
            .iter()
            .find(|n| n.type_id == RENDER_SCENE_TYPE_ID)
            .expect("render_scene")
            .id;
        let plan = build_plan(
            kind_id,
            project.timeline.layers[idx].generator_graph().expect("graph"),
            render_scene_id,
        )
        .unwrap_or_else(|| panic!("{kind_id} plan builder succeeds on the import"));
        let mut cmd = ApplySceneModifierCommand::new(target(), Vec::new(), plan, catalog.clone());
        cmd.execute(&mut project);
    }
    (project, idx)
}

#[test]
fn mirror_apply_on_import_renders_reflected_copy() {
    let (def, report) = assemble_import_graph(Path::new(FIXTURE))
        .unwrap_or_else(|e| panic!("assemble_import_graph({FIXTURE}) failed: {e}"));
    assert!(
        report.object_count > 0,
        "fixture must import at least one object group"
    );
    let bounds = def
        .preset_metadata
        .as_ref()
        .and_then(|m| m.scene_bounds)
        .expect("the import stamps scene_bounds (D8 derivation source)");

    let (mut project, idx) = apply_loop_and_mirror(def);

    // The structural gate: the mirror took the instances port over from
    // the loop's scene_array through the flattened interface.
    let applied = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph")
        .clone();
    let flat = manifold_core::flatten::flatten_groups(&applied).expect("flat applied");
    let reflect_id = flat
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "mirror_reflect")
        .expect("mirror_reflect minted")
        .id;
    for so in flat.nodes.iter().filter(|n| n.type_id == "node.scene_object") {
        assert!(
            flat.wires
                .iter()
                .any(|w| w.from_node == reflect_id && w.to_node == so.id && w.to_port == "instances"),
            "every scene_object must be fed by mirror_reflect through the interface"
        );
    }

    // The performer's framing gesture (the loop_camera Height/Pitch/FOV
    // rows — all whitelisted): camera above the path pitched down so the
    // below-floor reflection is separate and in frame.
    let cam = node_doc(&project, idx, "loop_camera");
    for (param, value) in [("height", 1.2f32), ("pitch", -0.45), ("fov_y", 1.2)] {
        write_param(&mut project, idx, cam, param, value);
    }

    let doc = node_doc(&project, idx, "mirror_reflect");
    let framed = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph")
        .clone();

    // Frame A: the applied (enabled, floor-offset) state.
    let frame_a = render(&framed);
    dump_png("a_enabled", &frame_a);

    // Frame B: the INV-MR1 off write — one param write, no rebuild.
    write_param(&mut project, idx, doc, "enabled", 0.0);
    let off_def = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph")
        .clone();
    let frame_b = render(&off_def);
    dump_png("b_disabled", &frame_b);

    // Measured on this device: 2231 diff pixels, 2217 in the lower half,
    // region-delta 26. Thresholds carry ≥2× margin.
    let (total, lower) = mask_stats(&diff_mask(&frame_a, &frame_b, 12));
    eprintln!("on/off diff = {total}, lower-half = {lower}");
    assert!(total >= 800, "the mirror must change the frame, got {total} diff pixels");
    assert!(
        lower * 10 >= total * 9,
        "the reflected copy must render below the scene centre: {lower}/{total} diff pixels in the lower half"
    );

    // Non-background: the reflected region in frame A must differ from
    // the corner-sampled void by a visible margin.
    let bg = region_mean(&frame_a, 0, 0, 32, 32);
    let reflected = region_mean(&frame_a, 64, 160, 192, 256);
    let delta = bg
        .iter()
        .zip(reflected.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    eprintln!("bg mean = {bg:?}, reflected region = {reflected:?}, delta = {delta:.1}");
    assert!(
        delta >= 12.0,
        "the reflected region must not be background (delta {delta})"
    );

    // Performer gesture: save → reload → ride Plane Offset through the
    // row-write command → the reflected region must move.
    let path = std::env::temp_dir().join(format!(
        "manifold_mirror_e2e_{}_{}.manifold",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    manifold_io::saver::save_project_v1(&project, &path).expect("save v1");
    let mut reloaded = manifold_io::loader::load_project(&path).expect("load v1");
    let _ = std::fs::remove_file(&path);

    let ridx = reloaded
        .timeline
        .layers
        .iter()
        .position(|l| l.layer_type == LayerType::Generator)
        .expect("generator layer survived reload");
    let rdoc = node_doc(&reloaded, ridx, "mirror_reflect");
    let stamped = reloaded.timeline.layers[ridx]
        .generator_graph()
        .expect("graph")
        .nodes
        .iter()
        .find(|n| n.id == rdoc)
        .and_then(|n| match n.params.get("plane_offset") {
            Some(SerializedParamValue::Float { value }) => Some(*value),
            _ => None,
        })
        .expect("plane_offset survives the round trip");
    eprintln!("reloaded plane_offset = {stamped}");

    // Ride the offset a quarter of the scene Y-extent below the stamped
    // floor — the reflection visibly drops, staying inside the row's
    // curated band by construction.
    let y_extent = (bounds.1[1] - bounds.0[1]).abs();
    let ridden = stamped - y_extent * 0.25;
    write_param(&mut reloaded, ridx, rdoc, "plane_offset", ridden);
    let ridden_def = reloaded.timeline.layers[ridx]
        .generator_graph()
        .expect("graph")
        .clone();
    let frame_c = render(&ridden_def);
    dump_png("c_offset_ridden", &frame_c);

    let (total_c, lower_c) = mask_stats(&diff_mask(&frame_a, &frame_c, 12));
    eprintln!("offset-ride diff = {total_c}, lower-half = {lower_c}");
    assert!(
        total_c >= 800,
        "riding Plane Offset after reload must move the reflected region, got {total_c} diff pixels"
    );
    assert!(
        lower_c * 10 >= total_c * 9,
        "the ridden reflection must stay below the scene centre: {lower_c}/{total_c}"
    );
}
