//! The P1b solver budget report (`docs/GPU_MPM_SOLVER_DESIGN.md` P1b, D20,
//! section 8 (speed)): the Dam Break at 64³ (4 m, the preset's column) at
//! two operating points, the column alone (241,920 points) and the pool
//! deepened until at least 500,000 points are live, each at Stiffness 1 and
//! 0.5 on the block path; 16 warm-up and 120 measured frames of production
//! GPU time, p50/p95 against D20's 6 ms; a profiled pass for per-kernel time
//! and the dispatch count; how many points leave their sorted block's tile; the
//! per-point P2G baseline; and the 128³ / 30 Hz stretch. It reports; P4 is
//! the gate. Opt in with `--features matter-perf-proofs`.

use std::collections::BTreeMap;

use manifold_node_engine::scene::transform::Transform;

use crate::harness;
use crate::matter_scene::{MatterScene, SceneSettings};

const WARMUP_FRAMES: u32 = 16;
const MEASURED_FRAMES: u32 = 120;
const PROFILED_FRAMES: u32 = 20;
const BUDGET_MS: f64 = 6.0;
/// Pool depths (m): none, leaving the column's 241,920 points, and deep
/// enough for at least 500k.
const SMALL_POOL: f32 = 0.0;
const DEEP_POOL: f32 = 0.625;

fn dam_break(resolution: u32, fill_height: f32, stiffness: f32, block_p2g: bool) -> SceneSettings {
    SceneSettings {
        domain_size: 4.0,
        resolution,
        fill_height,
        stiffness,
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
        l if l.starts_with("node.sort_particles_into_cells") || l.starts_with("prefix_scan") => "sort",
        _ => "other",
    }
}

struct Report {
    points: usize,
    substeps: f32,
    frame_ms: Vec<f64>,
    kernels: BTreeMap<&'static str, f64>,
    dispatches: usize,
    /// Mean over the measured frames of `MatterScene::tile_drift`, on the
    /// block path.
    drift: Option<f64>,
}

fn measure(settings: &SceneSettings, frame_interval: f64, warmup: u32, measured: u32, profiled: u32) -> Report {
    let mut scene = MatterScene::new(settings);
    scene.set_frame_interval(frame_interval);
    for _ in 0..warmup {
        scene.tick();
    }
    let mut drift = 0.0;
    let mut frame_ms: Vec<f64> = (0..measured)
        .map(|_| {
            let ms = scene.tick_timed(None).total_ms;
            if settings.block_p2g {
                drift += scene.tile_drift();
            }
            ms
        })
        .collect();
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
    let n = f64::from(measured.max(1));
    Report {
        points: scene.points().iter().filter(|p| p.id != 0).count(),
        substeps: scene.state_input("substeps_per_tick"),
        frame_ms,
        kernels,
        dispatches,
        drift: settings.block_p2g.then_some(drift / n),
    }
}

fn print(name: &str, report: &Report) {
    let k = |kernel: &str| report.kernels.get(kernel).copied().unwrap_or(0.0);
    let p95 = percentile(&report.frame_ms, 0.95);
    // Whole-frame p95 per point per substep, one tick per frame at 60 Hz.
    let ns = p95 * 1e6 / (report.points as f64 * f64::from(report.substeps));
    let drift = report
        .drift
        .map_or("-".to_string(), |tile| format!("left tile {tile:.4}"));
    eprintln!(
        "  {name}: {} points, n {} | p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms (budget {BUDGET_MS} ms) | {ns:.2} ns/point-substep | {drift} | {} dispatches | clear {:.2} sort {:.2} P2G {:.2} grid {:.2} G2P {:.2} stats {:.2} frame {:.2} other {:.2} (profiled means)",
        report.points,
        report.substeps,
        percentile(&report.frame_ms, 0.5),
        p95,
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
    let load = || {
        std::process::Command::new("uptime")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    eprintln!("matter_solver_perf: Dam Break at 64³; {}", load());
    eprintln!("  drift: mean over measured frames, at tick end, of the points out of the tile of the block the tick's sort put them in");
    let mut rows = Vec::new();
    for (label, fill_height) in [("242k", SMALL_POOL), ("524k", DEEP_POOL)] {
        for stiffness in [1.0, 0.5] {
            let report = measure(&dam_break(64, fill_height, stiffness, true), 1.0 / 60.0, WARMUP_FRAMES, MEASURED_FRAMES, PROFILED_FRAMES);
            print(&format!("{label}, Stiffness {stiffness}, block P2G"), &report);
            rows.push(report);
        }
    }
    let point = measure(&dam_break(64, DEEP_POOL, 1.0, false), 1.0 / 60.0, WARMUP_FRAMES, MEASURED_FRAMES, PROFILED_FRAMES);
    print("524k, Stiffness 1, per-point P2G (baseline)", &point);
    let stretch = measure(&dam_break(128, DEEP_POOL, 1.0, true), 1.0 / 30.0, 2, 8, 2);
    print("128³ at 30 Hz, block P2G (reported, not gated)", &stretch);
    eprintln!("  load after: {}", load());
    assert!(rows[2].points >= 500_000, "the deepened Dam Break has {} points", rows[2].points);
    assert!(rows.iter().chain([&point]).all(|r| r.frame_ms.iter().all(|t| *t > 0.0)), "the report measured nothing");
}
