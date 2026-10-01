//! Look gates A1 and A3, and free-flight momentum
//! (`docs/GPU_MPM_SOLVER_DESIGN.md` section 7 (look gates) and section 12
//! (invariants)). A2, A4 and A6 were water look targets, withdrawn with the
//! MPM water look goal; A5 compared splash against the CPU FLIP engine and was
//! dropped with routine FLIP comparisons. Every metric comes from
//! `matter::look` over seam particle frames. The Dam Break scene matches
//! `WaterDamBreakMatter.json`: 4 m domain, 64 cells, 0.16 m pool, the
//! preset's column, closed walls. Gates are evaluated at Liveliness 0;
//! Liveliness 0.9 numbers are printed.

use std::sync::OnceLock;

use manifold_renderer::node_graph::Transform;
use manifold_renderer::node_graph::fluid::domain_layout;
use manifold_renderer::node_graph::fluid_particles::FluidParticle;
use manifold_renderer::node_graph::matter::look::{ALIGNMENT_TICKS, Cells, LookRecorder};

use crate::matter_scene::{MatterScene, SceneSettings};

const DOMAIN_SIZE: f32 = 4.0;
const RESOLUTION: u32 = 64;
const POOL: f32 = 0.16;
/// Point spacing at 8 points per cell: half a cell.
const SPACING: f32 = DOMAIN_SIZE / RESOLUTION as f32 * 0.5;
const DAM_BREAK_TICKS: u32 = 720;

fn column() -> Transform {
    Transform { pos: [-1.25, 1.12, 0.0], scale: [1.18, 1.92, 3.5], ..Transform::default() }
}

fn cells() -> Cells {
    Cells::from_layout(&domain_layout(None, DOMAIN_SIZE, RESOLUTION).expect("domain"))
}

fn dam_break(liveliness: f32) -> SceneSettings {
    SceneSettings {
        domain_size: DOMAIN_SIZE,
        resolution: RESOLUTION,
        fill_height: POOL,
        column: Some(column()),
        liveliness,
        ..SceneSettings::default()
    }
}

/// One Dam Break run: the shared look readings plus what only matter reports.
struct Series {
    look: LookRecorder,
    /// (time, |Σ V0·J − Σ V0| / Σ V0); matter only.
    volume_error: Vec<(f32, f32)>,
    /// Smallest and largest J seen; matter only.
    j_range: (f32, f32),
}

impl std::ops::Deref for Series {
    type Target = LookRecorder;

    fn deref(&self) -> &LookRecorder {
        &self.look
    }
}

impl Series {
    fn new() -> Self {
        Self {
            look: LookRecorder::new(cells(), SPACING),
            volume_error: Vec::new(),
            j_range: (f32::INFINITY, f32::NEG_INFINITY),
        }
    }

    fn observe(&mut self, tick: u32, frame: &[FluidParticle]) {
        self.look.observe(tick as f32 / 60.0, frame);
    }
}

fn run_matter(liveliness: f32) -> Series {
    let mut scene = MatterScene::new(&dam_break(liveliness));
    let mut series = Series::new();
    let mut rest_volume = None;
    for tick in 1..=DAM_BREAK_TICKS {
        scene.tick();
        let frame = scene.frame();
        series.observe(tick, &frame);
        let rest = *rest_volume.get_or_insert_with(|| {
            scene.points().iter().filter(|p| p.id != 0).map(|p| f64::from(p.affine_y[3])).sum::<f64>()
        });
        let stats = scene.stats();
        assert_eq!(stats.nonfinite, 0, "non-finite state at tick {tick}");
        // The first frame restarts the domain and runs no tick; its stats are empty.
        if stats.live > 0 {
            series.j_range = (series.j_range.0.min(stats.min_j), series.j_range.1.max(stats.max_j));
            series.volume_error.push((tick as f32 / 60.0, ((f64::from(stats.volume) - rest).abs() / rest) as f32));
        }
    }
    series
}

fn matter(liveliness: f32) -> &'static Series {
    static ZERO: OnceLock<Series> = OnceLock::new();
    static LIVELY: OnceLock<Series> = OnceLock::new();
    if liveliness == 0.0 {
        ZERO.get_or_init(|| run_matter(0.0))
    } else {
        LIVELY.get_or_init(|| run_matter(0.9))
    }
}

fn describe(name: &str, s: &Series) {
    let settle = s.settling();
    eprintln!(
        "  {name}: settle {:?} s, ringing {:.4} m, peak volume error {:.4}, J {:?}, splash {:.4}, sheets {:.4}, alignment {:?}",
        settle.settle_time,
        settle.ringing,
        s.volume_error.iter().map(|v| v.1).fold(0.0, f32::max),
        s.j_range,
        s.max_splash(),
        s.max_sheets(),
        s.alignment
    );
}

/// A1: no grid-aligned ridges at 1.5 s and 8 s.
#[test]
fn matter_look_lattice_alignment() {
    let gate = matter(0.0);
    eprintln!("matter_look_lattice_alignment (largest 16-bin histogram bin over mean; fails above 1.5)");
    describe("matter L0", gate);
    describe("matter L0.9", matter(0.9));
    assert_eq!(gate.alignment.len(), ALIGNMENT_TICKS.len());
    for (t, ratio, interior) in &gate.alignment {
        assert!(*interior > 10_000, "too few interior points at {t} s: {interior}");
        assert!(ratio.iter().all(|&r| r <= 1.5), "grid-aligned ridges at {t} s: {ratio:?}");
    }
}

/// A3: Σ V0·J stays within 3% of the rest volume at every Dam Break tick and
/// within 1% after settling, and within 1% over 60 s of still pool.
#[test]
fn matter_look_volume_drift() {
    let gate = matter(0.0);
    let settle = gate.settling().settle_time.unwrap_or(f32::INFINITY);
    let peak = gate.volume_error.iter().map(|v| v.1).fold(0.0, f32::max);
    let settled = gate.volume_error.iter().filter(|v| v.0 >= settle).map(|v| v.1).fold(0.0, f32::max);
    let lively = matter(0.9).volume_error.iter().map(|v| v.1).fold(0.0, f32::max);

    let mut pool = MatterScene::new(&SceneSettings {
        domain_size: DOMAIN_SIZE,
        resolution: RESOLUTION,
        fill_height: 0.5,
        ..SceneSettings::default()
    });
    pool.tick();
    let rest: f64 = pool.points().iter().filter(|p| p.id != 0).map(|p| f64::from(p.affine_y[3])).sum();
    let mut pool_error = 0.0f64;
    for _ in 1..3600 {
        pool.tick();
        pool_error = pool_error.max((f64::from(pool.stats().volume) - rest).abs() / rest);
    }
    eprintln!(
        "matter_look_volume_drift: Dam Break peak {peak:.4}, after settling ({settle} s) {settled:.4}, L0.9 peak {lively:.4}; still pool 60 s peak {pool_error:.4}"
    );
    assert!(peak <= 0.03, "Dam Break volume error {peak}");
    assert!(settled <= 0.01, "settled volume error {settled}");
    assert!(pool_error <= 0.01, "still pool volume error {pool_error}");
}

/// Zero gravity, a blob in the middle of the domain moving at constant
/// velocity: total momentum changes by at most 1e-4 of itself over 60 ticks.
#[test]
fn matter_momentum_conserved_free_blob() {
    let mut scene = MatterScene::new(&SceneSettings {
        domain_size: DOMAIN_SIZE,
        resolution: RESOLUTION,
        fill_height: 0.0,
        column: Some(Transform { pos: [-0.5, 1.5, 0.0], scale: [1.0, 1.0, 1.0], ..Transform::default() }),
        gravity: [0.0; 3],
        ..SceneSettings::default()
    });
    scene.tick();
    scene.set_velocity([1.0, 0.5, -0.25]);
    scene.tick();
    let start = scene.stats().momentum;
    let norm = |m: [f32; 3]| (m[0] * m[0] + m[1] * m[1] + m[2] * m[2]).sqrt();
    let mut worst = 0.0f32;
    for tick in 0..60 {
        scene.tick();
        let s = scene.stats();
        if std::env::var_os("MATTER_DIAG").is_some() && tick % 5 == 0 {
            let pts = scene.points();
            let live: Vec<_> = pts.iter().filter(|p| p.id != 0).collect();
            let lo = live.iter().fold([f32::MAX; 3], |a, p| std::array::from_fn(|i| a[i].min(p.position[i])));
            let hi = live.iter().fold([f32::MIN; 3], |a, p| std::array::from_fn(|i| a[i].max(p.position[i])));
            let cpu: [f64; 3] = std::array::from_fn(|i| live.iter().map(|p| 1000.0 * f64::from(p.affine_y[3]) * f64::from(p.velocity[i])).sum());
            eprintln!(
                "tick {tick}: live {} mass {:.2} momentum {:?} cpu {cpu:?} J [{:.5},{:.5}] max speed {:.4} clamped {} bounds {lo:?}..{hi:?}",
                s.live, s.mass, s.momentum, s.min_j, s.max_j, s.max_speed, s.clamped
            );
        }
        let m = s.momentum;
        let change = [m[0] - start[0], m[1] - start[1], m[2] - start[2]];
        worst = worst.max(norm(change) / norm(start));
    }
    eprintln!("matter_momentum_conserved_free_blob: start {start:?}, worst relative change {worst:.2e}");
    assert!(norm(start) > 0.0);
    assert!(worst <= 1e-4, "momentum changed by {worst}");
}
