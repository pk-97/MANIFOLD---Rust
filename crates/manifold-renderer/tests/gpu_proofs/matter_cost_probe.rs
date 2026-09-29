//! The P1 cost probe and kill check (`docs/GPU_MPM_SOLVER_DESIGN.md` P1,
//! section 8 (performance budget)): per-kernel GPU time per 60 Hz frame at
//! 64³ (4 m domain, resolution 64) with pools of about 125k, 250k and 500k
//! points, from per-dispatch timestamps. Profiled mode gives every dispatch
//! its own encoder, so absolute times run a little high and the shares are
//! what to read. It reports; it asserts only that the probe measured
//! something. The kill check projects the Dam Break solver time as ns per
//! point-substep × 500,000 × 34 plus the lattice term, against 12 ms.

use std::collections::BTreeMap;

use crate::harness;
use crate::matter_scene::{MatterScene, SceneSettings};

const WARMUP_TICKS: u32 = 16;
const MEASURED_TICKS: u32 = 30;

/// Kernel family of a dispatch label.
fn family(label: &str) -> &'static str {
    match label {
        "node.zero_array" => "clear",
        "node.matter_to_grid" => "P2G",
        "node.matter_grid_update" => "grid update",
        "node.grid_to_matter" => "G2P",
        l if l.starts_with("node.matter_stats") => "stats",
        "node.matter_frame" => "frame",
        "node.matter_fill" => "fill",
        _ => "other",
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).expect("finite times"));
    v[v.len() / 2]
}

struct Row {
    points: usize,
    substeps: f64,
    kernels: BTreeMap<&'static str, f64>,
    attributed: f64,
    total: f64,
    /// Whole-buffer GPU time of unprofiled frames: the production encoder.
    production: f64,
    labels: BTreeMap<String, usize>,
}

impl Row {
    fn kernel(&self, name: &str) -> f64 {
        self.kernels.get(name).copied().unwrap_or(0.0)
    }

    /// The per-point kernels, P2G and G2P, per point per substep.
    fn ns_per_point_substep(&self) -> f64 {
        (self.kernel("P2G") + self.kernel("G2P")) * 1e6 / (self.points as f64 * self.substeps)
    }
}

fn probe(fill_height: f32) -> Row {
    let settings = SceneSettings {
        domain_size: 4.0,
        resolution: 64,
        fill_height,
        ..SceneSettings::default()
    };
    let mut scene = MatterScene::new(&settings);
    for _ in 0..WARMUP_TICKS {
        scene.tick();
    }
    let sampler = harness::shared()
        .device
        .create_timestamp_sampler(1024)
        .expect("timestamp counters on this GPU");
    let mut per_kernel: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    let mut attributed = Vec::new();
    let mut totals = Vec::new();
    let mut labels = BTreeMap::new();
    let mut production = Vec::new();
    for _ in 0..MEASURED_TICKS {
        production.push(scene.tick_timed(None).total_ms);
    }
    for _ in 0..MEASURED_TICKS {
        let profile = scene.tick_timed(Some(&sampler));
        assert_eq!(profile.overflow, 0, "the sampler is too small for one frame");
        let mut frame: BTreeMap<&'static str, f64> = BTreeMap::new();
        for span in &profile.spans {
            *frame.entry(family(&span.label)).or_default() += span.millis;
            *labels.entry(span.label.clone()).or_default() += 1;
        }
        for (k, v) in frame {
            per_kernel.entry(k).or_default().push(v);
        }
        attributed.push(profile.attributed_ms());
        totals.push(profile.total_ms);
    }
    Row {
        points: scene.points().iter().filter(|p| p.id != 0).count(),
        substeps: f64::from(scene.state_input("substeps_per_tick")),
        kernels: per_kernel.into_iter().map(|(k, v)| (k, median(v))).collect(),
        attributed: median(attributed),
        total: median(totals),
        production: median(production),
        labels,
    }
}

fn command(program: &str, args: &[&str]) -> String {
    std::process::Command::new(program)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("{program} failed: {e}"))
}

#[test]
fn matter_cost_probe() {
    eprintln!(
        "matter_cost_probe: {}; {}",
        command("sysctl", &["-n", "machdep.cpu.brand_string"]),
        command("uptime", &[])
    );
    eprintln!("  64³ cells (71³ nodes), 4 m domain, Stiffness 1; medians of {MEASURED_TICKS} frames, one tick each; ms per frame");
    eprintln!("  kernels, spans and buffer: profiled frames (one encoder per dispatch); production: unprofiled whole-buffer GPU time");
    eprintln!(
        "  {:>7} {:>3} | {:>6} {:>5} {:>7} {:>6} {:>7} {:>6} {:>6} {:>6} | {:>7} {:>7} | {:>10} | {:>6}",
        "points", "n", "clear", "sort", "P2G", "grid", "G2P", "stats", "frame", "other", "spans", "buffer", "production", "ns/p·s"
    );
    let mut rows = Vec::new();
    for height in [0.25, 0.5, 1.0] {
        let row = probe(height);
        eprintln!(
            "  {:>7} {:>3} | {:>6.3} {:>5} {:>7.3} {:>6.3} {:>7.3} {:>6.3} {:>6.3} {:>6.3} | {:>7.3} {:>7.3} | {:>10.3} | {:>6.4}",
            row.points, row.substeps, row.kernel("clear"), "n/a", row.kernel("P2G"), row.kernel("grid update"),
            row.kernel("G2P"), row.kernel("stats"), row.kernel("frame"), row.kernel("other"), row.attributed,
            row.total, row.production, row.ns_per_point_substep()
        );
        rows.push(row);
    }
    let largest = rows.last().expect("three probe rows");
    let other: Vec<_> = largest.labels.iter().filter(|(l, _)| family(l) == "other").collect();
    if !other.is_empty() {
        eprintln!("  other spans over {MEASURED_TICKS} frames: {other:?}");
    }
    let ns = largest.ns_per_point_substep();
    let lattice = (largest.kernel("clear") + largest.kernel("grid update")) * 34.0 / largest.substeps;
    let stats = largest.kernel("stats");
    let projected = ns * 500_000.0 * 34.0 / 1e6 + lattice + stats;
    eprintln!(
        "  kill check: {ns:.4} ns × 500,000 × 34 = {:.2} ms, + lattice {lattice:.2} ms + stats {stats:.2} ms = {projected:.2} ms against 12 ms: {}",
        ns * 500_000.0 * 34.0 / 1e6,
        if projected > 12.0 { "FIRES" } else { "passes" }
    );
    assert!(rows.iter().all(|r| r.attributed > 0.0 && r.points > 0), "the probe measured nothing");
}
