//! Frame CPU and GPU ms for the bundled Dam Break, encode replay off against on (`docs/ENCODE_REPLAY_DESIGN.md`),
//! alternating runs so drift hits both sides, and the CPU of the frame whose region recorded first.
//!
//! A report, not a gate: the byte and conservation parity of replay are the `encode_replay` GPU proofs. Run it
//! deliberately, in release, on an idle machine:
//!
//! ```text
//! cargo run --release -p manifold-nodes --features gpu-proofs --example encode_replay_probe
//! ```

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::GpuReplayStats;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::testkit::gpu_harness;

const DAM_BREAK: &str = "WaterDamBreakMatter";
const WARM: usize = 8;
const MEASURED: u32 = 48;

struct Timings {
    stats_by_frame: Vec<GpuReplayStats>,
    cpu_ms: Vec<f64>,
    gpu_ms: Vec<f64>,
}

fn run_preset(replay: bool, frames: u32) -> Timings {
    let h = gpu_harness::shared();
    let json = manifold_nodes::engine_scenes::scene_json(&PresetTypeId::new(DAM_BREAK))
        .expect("bundled preset");
    let def: EffectGraphDef = serde_json::from_str(&json).expect("preset parses");
    let manifest = ParamManifest::from_params(
        def.preset_metadata
            .as_ref()
            .map(|m| m.params.iter().cloned().map(Param::bundled).collect())
            .unwrap_or_default(),
    );
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        h.format,
        None,
    )
    .expect("preset builds");
    runtime.set_encode_replay(replay);
    let target = h.make_target("encode-replay-probe");
    let mut out = Timings {
        stats_by_frame: Vec::new(),
        cpu_ms: Vec::new(),
        gpu_ms: Vec::new(),
    };
    for frame in 0..frames {
        let ctx = PresetContext {
            time: f64::from(frame) / 60.0,
            beat: f64::from(frame) / 30.0,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: i64::from(frame),
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("encode-replay-probe");
        let start = std::time::Instant::now();
        {
            let mut gpu = GpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifest);
        }
        out.cpu_ms.push(start.elapsed().as_secs_f64() * 1e3);
        let profile = enc.commit_and_wait_profiled(&h.device);
        assert_eq!(
            profile.failed_command_buffers, 0,
            "{DAM_BREAK} frame {frame} failed on the GPU"
        );
        out.gpu_ms.push(profile.total_ms);
        out.stats_by_frame.push(runtime.replay_stats());
    }
    out
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn main() {
    let mut rows: [Vec<(f64, f64)>; 2] = [Vec::new(), Vec::new()];
    // The frame whose region recorded first, and its CPU on each side.
    let mut first_visit = (0usize, [0.0; 2]);
    // A cold first run renders its early frames while pipelines still compile.
    run_preset(true, 16);
    for round in 0..3 {
        let runs = [false, true].map(|replay| run_preset(replay, WARM as u32 + MEASURED));
        if round == 0 {
            let k = runs[1]
                .stats_by_frame
                .iter()
                .position(|s| s.recorded > 0)
                .expect("the region recorded");
            first_visit = (k, [runs[0].cpu_ms[k], runs[1].cpu_ms[k]]);
        }
        for (side, run) in runs.iter().enumerate() {
            rows[side].push((mean(&run.cpu_ms[WARM..]), mean(&run.gpu_ms[WARM..])));
        }
    }
    for (side, name) in ["replay off", "replay on"].into_iter().enumerate() {
        let cpu: Vec<String> = rows[side].iter().map(|r| format!("{:.3}", r.0)).collect();
        let gpu: Vec<String> = rows[side].iter().map(|r| format!("{:.3}", r.1)).collect();
        println!(
            "ENCODE REPLAY PROBE {DAM_BREAK} {name}: frame CPU ms [{}], GPU ms [{}], recording frame {} CPU {:.2} ms",
            cpu.join(", "),
            gpu.join(", "),
            first_visit.0,
            first_visit.1[side],
        );
    }
}
