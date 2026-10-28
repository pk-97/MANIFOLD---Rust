//! The cinematic import tail's frame cost (dof + motion_blur, CINEMATIC_SCENE_TAIL D5 and P4) at 1920x1080
//! and 3840x2160: the same import-assembled scene with the tail present and surgically stripped, each drained
//! per frame so wall time is the true GPU cost. Prints the min-of-reps steady-state delta and warns past the
//! targets (3 ms mean at 1080p, 20 ms at 4K).
//!
//! A report, not a gate: the tail's contract, a byte-clean passthrough at a neutral lens, is the
//! `cinematic_scene_tail` GPU proof. Run it deliberately, on an idle machine:
//!
//! ```text
//! cargo run --release -p manifold-nodes --features gpu-proofs --example cinematic_tail_cost
//! ```

use manifold_core::effect_graph_def::EffectGraphWire;
use manifold_core::flatten::flatten_groups;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::testkit::gpu_harness::{self, ParityHarness};
use manifold_nodes as _;
use manifold_nodes_scene::node_graph::gltf_import::compile_model_scene;

const WARMUP: u64 = 12;
const STEADY: u64 = 40;

/// Build one variant (tail present or stripped), warm it, then return (max, mean) steady-state frame ms.
fn measure_tail(
    h: &ParityHarness,
    glb: &std::path::Path,
    strip_tail: bool,
    w: u32,
    hh: u32,
) -> (f64, f64) {
    let (def, _report) = compile_model_scene(glb).expect("import must succeed");
    let mut flat = flatten_groups(&def).expect("flatten_groups must succeed");
    if strip_tail {
        const TAIL_TYPES: [&str; 4] = [
            "node.coc_from_depth",
            "node.coc_dilate",
            "node.bokeh_gather",
            "node.motion_blur",
        ];
        let keep: Vec<u32> = flat
            .nodes
            .iter()
            .filter(|n| !TAIL_TYPES.contains(&n.type_id.as_str()))
            .map(|n| n.id)
            .collect();
        flat.nodes.retain(|n| keep.contains(&n.id));
        flat.wires
            .retain(|w| keep.contains(&w.from_node) && keep.contains(&w.to_node));
        // The scene's colour before the tail: the render's own colour output.
        let render = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.render_scene")
            .expect("render present")
            .id;
        let final_id = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "system.final_output")
            .expect("final output")
            .id;
        flat.wires.push(EffectGraphWire {
            from_node: render,
            from_port: "color".to_string(),
            to_node: final_id,
            to_port: "in".to_string(),
        });
    }
    let registry = PrimitiveRegistry::with_builtin();
    let manifest = ParamManifest::from_params(
        def.preset_metadata
            .as_ref()
            .map(|m| m.params.iter().cloned().map(Param::bundled).collect())
            .unwrap_or_default(),
    );
    let mut runtime = PresetRuntime::from_def_with_device(
        flat,
        &registry,
        std::sync::Arc::clone(&h.device),
        w,
        hh,
        GpuTextureFormat::Rgba16Float,
        Some(&manifest),
    )
    .expect("variant must build");
    let target = h.make_target("cinematic-tail-cost");

    let mut max_steady_ms = 0.0f64;
    let mut total_steady_ms = 0.0f64;
    for frame in 0..(WARMUP + STEADY) {
        let t = std::time::Instant::now();
        let c = PresetContext {
            time: frame as f64 / 60.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: w,
            height: hh,
            output_width: w,
            output_height: hh,
            aspect: w as f32 / hh as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame as i64,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        gpu_harness::retry_on_gpu_commit_error(|| {
            let mut enc = h.device.create_encoder("cinematic-tail-cost-frame");
            {
                let mut gpu = GpuEncoder::new(&mut enc, &h.device);
                runtime.render(&mut gpu, &target.texture, &c, &manifest);
            }
            enc.commit_and_wait().expect("GPU work completes");
        });
        // An empty commit waits every earlier buffer on the single queue, so wall time is the frame's cost.
        h.device
            .create_encoder("cinematic-tail-cost-drain")
            .commit_and_wait()
            .expect("GPU work completes");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if frame >= WARMUP {
            max_steady_ms = max_steady_ms.max(ms);
            total_steady_ms += ms;
        }
    }
    (max_steady_ms, total_steady_ms / STEADY as f64)
}

/// Contention only adds wall time, so the rep with the smallest mean delta is the tail's cost. `any_frame`
/// also holds the max delta to the target (the 4K target is per frame, the 1080p one a mean).
fn report(
    h: &ParityHarness,
    glb: &std::path::Path,
    w: u32,
    hh: u32,
    reps: u32,
    target_ms: f64,
    any_frame: bool,
) {
    let mut best: Option<(f64, f64)> = None;
    for _ in 0..reps {
        let (tail_max, tail_mean) = measure_tail(h, glb, false, w, hh);
        let (base_max, base_mean) = measure_tail(h, glb, true, w, hh);
        let delta = tail_mean - base_mean;
        println!(
            "{w}x{hh} steady-state max/mean ms: with-tail {tail_max:.2}/{tail_mean:.2}, stripped {base_max:.2}/{base_mean:.2}, \
             tail delta {:+.2}/{delta:+.2}",
            tail_max - base_max
        );
        if best.is_none_or(|(_, d)| delta < d) {
            best = Some((tail_max - base_max, delta));
        }
    }
    let (delta_max, delta_mean) = best.expect("at least one rep ran");
    println!(
        "{w}x{hh} best tail delta: max {delta_max:+.2} ms, mean {delta_mean:+.2} ms (target {target_ms} ms)"
    );
    if delta_mean > target_ms || (any_frame && delta_max > target_ms) {
        println!("WARNING: {w}x{hh} tail exceeds the {target_ms} ms target");
    }
}

fn main() {
    let h = gpu_harness::shared();
    let glb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/apricot_tl05.glb");
    assert!(glb.exists(), "fixture missing: {glb:?}");
    report(h, &glb, 1920, 1080, 3, 3.0, false);
    report(h, &glb, 3840, 2160, 2, 20.0, true);
}
