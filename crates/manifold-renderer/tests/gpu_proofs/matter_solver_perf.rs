//! The P1b solver budget report (`docs/GPU_MPM_SOLVER_DESIGN.md` P1b, D20,
//! section 8 (speed)): the Dam Break at 64³ (4 m, the preset's column) with
//! the pool deepened until at least 500,000 points are live; 16 warm-up and
//! 120 measured frames of production GPU time, p50/p95 against D20's 6 ms;
//! a profiled pass for per-kernel time and the dispatch count; both P2G paths
//! (the L1 before/after); and the 128³ / 30 Hz stretch. It reports; P4 is the
//! gate. Opt in with `--features matter-perf-proofs`.

use std::collections::BTreeMap;

use manifold_renderer::node_graph::Transform;

use crate::harness;
use crate::matter_scene::{MatterScene, SceneSettings};

const WARMUP_FRAMES: u32 = 16;
const MEASURED_FRAMES: u32 = 120;
const PROFILED_FRAMES: u32 = 20;
const BUDGET_MS: f64 = 6.0;

fn dam_break(resolution: u32, block_p2g: bool) -> SceneSettings {
    SceneSettings {
        domain_size: 4.0,
        resolution,
        fill_height: 0.625,
        column: Some(Transform { pos: [-1.25, 1.12, 0.0], scale: [1.18, 1.92, 3.5], ..Transform::default() }),
        block_p2g,
        ..SceneSettings::default()
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Kernel family of a dispatch label, as the cost probe groups them.
fn family(label: &str) -> &'static str {
    match label {
        "node.zero_array" => "clear",
        "node.matter_to_grid" => "P2G",
        "node.matter_grid_update" => "grid update",
        "node.grid_to_matter" => "G2P",
        l if l.starts_with("node.matter_stats") => "stats",
        "node.matter_frame" => "frame",
        l if l == "node.matter_to_particles"
            || l.starts_with("node.sort_particles_into_cells")
            || l.starts_with("prefix_scan") => "sort",
        _ => "other",
    }
}

struct Report {
    points: usize,
    substeps: f32,
    frame_ms: Vec<f64>,
    kernels: BTreeMap<&'static str, f64>,
    dispatches: usize,
}

fn measure(settings: &SceneSettings, frame_interval: f64, warmup: u32, measured: u32, profiled: u32) -> Report {
    let mut scene = MatterScene::new(settings);
    scene.set_frame_interval(frame_interval);
    for _ in 0..warmup {
        scene.tick();
    }
    let mut frame_ms: Vec<f64> = (0..measured).map(|_| scene.tick_timed(None).total_ms).collect();
    frame_ms.sort_by(f64::total_cmp);
    let sampler = harness::shared()
        .device
        .create_timestamp_sampler(4096)
        .expect("timestamp counters on this GPU");
    let mut totals: BTreeMap<&'static str, f64> = BTreeMap::new();
    let mut dispatches = 0;
    for _ in 0..profiled {
        let profile = scene.tick_timed(Some(&sampler));
        assert_eq!(profile.overflow, 0, "the sampler is too small for one frame");
        dispatches = dispatches.max(profile.spans.len());
        for span in &profile.spans {
            *totals.entry(family(&span.label)).or_default() += span.millis;
        }
    }
    let kernels = totals.into_iter().map(|(k, v)| (k, v / f64::from(profiled.max(1)))).collect();
    Report {
        points: scene.points().iter().filter(|p| p.id != 0).count(),
        substeps: scene.state_input("substeps_per_tick"),
        frame_ms,
        kernels,
        dispatches,
    }
}

fn print(name: &str, report: &Report) {
    let k = |kernel: &str| report.kernels.get(kernel).copied().unwrap_or(0.0);
    eprintln!(
        "  {name}: {} points, n {} | p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms (budget {BUDGET_MS} ms) | {} dispatches | clear {:.2} sort {:.2} P2G {:.2} grid {:.2} G2P {:.2} stats {:.2} frame {:.2} other {:.2} (profiled means)",
        report.points,
        report.substeps,
        percentile(&report.frame_ms, 0.5),
        percentile(&report.frame_ms, 0.95),
        report.frame_ms.last().copied().unwrap_or(0.0),
        report.dispatches,
        k("clear"),
        k("sort"),
        k("P2G"),
        k("grid update"),
        k("G2P"),
        k("stats"),
        k("frame"),
        k("other"),
    );
}

#[test]
fn matter_solver_perf() {
    let load = std::process::Command::new("uptime")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    eprintln!("matter_solver_perf: Dam Break at 64³ with a 0.625 m pool; {load}");
    eprintln!("  out-of-tile fraction: not measured; the cell sort runs every substep until it can gate on the tick start");
    let point = measure(&dam_break(64, false), 1.0 / 60.0, WARMUP_FRAMES, MEASURED_FRAMES, PROFILED_FRAMES);
    print("per-point P2G", &point);
    let block = measure(&dam_break(64, true), 1.0 / 60.0, WARMUP_FRAMES, MEASURED_FRAMES, PROFILED_FRAMES);
    print("block P2G (L1)", &block);
    let stretch = measure(&dam_break(128, true), 1.0 / 30.0, 2, 8, 2);
    print("128³ at 30 Hz, block P2G (reported, not gated)", &stretch);
    assert!(point.points >= 500_000, "the deepened Dam Break has {} points", point.points);
    assert!(point.frame_ms.iter().chain(&block.frame_ms).all(|t| *t > 0.0), "the report measured nothing");
}
