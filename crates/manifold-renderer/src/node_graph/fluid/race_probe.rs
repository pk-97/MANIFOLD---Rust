//! The FLIP Fluids engine's side of the FFT water race
//! (docs/FFT_WATER_SOLVER_DESIGN.md P3): the shipped Dam Break
//! (`WaterDamBreak.json`) with its obstacle unwired, built by the production
//! world setup and stepped as the worker steps it. It reports the wall clock
//! per tick, the engine's own counters and substeps, and the volume its
//! surface mesh encloses per frame, measured as `swash_scene_tests` measures
//! SWASH's. CPU only, and minutes long, so it sits with the opt-in probes.

use std::time::Instant;

use manifold_core::Seconds;
use manifold_fluids::{SurfaceOptions, SurfaceVertex, WhitewaterOptions};

use super::native::seeded_world;
use super::{FluidSettings, Transform};

/// `WaterDamBreak.json`'s `node.fluid_surface` params, as that node builds
/// its settings.
fn dam_break(resolution: u32, whitewater: bool) -> FluidSettings {
    FluidSettings {
        seed: 0,
        resolution,
        domain_size: 4.0,
        fill_height: 0.16,
        initial_volume: Some(Transform {
            pos: [-1.25, 1.12, 0.0],
            scale: [1.18, 1.92, 3.5],
            ..Transform::default()
        }),
        surface_subdivisions: 1,
        surface: SurfaceOptions { particle_scale: 2.2, smoothing: 0.35, smoothing_iterations: 2 },
        whitewater: WhitewaterOptions {
            enabled: whitewater,
            max_particles: 100_000,
            wavecrest_rate: 175.0,
            turbulence_rate: 175.0,
            min_energy: 0.1,
            max_energy: 60.0,
        },
        ..FluidSettings::default()
    }
}

/// The volume the mesh encloses with the tank: the flux of (0, y − floor, 0)
/// through it, which the open wall and floor pieces do not carry.
fn enclosed_volume(vertices: &[SurfaceVertex], floor: f64) -> f64 {
    vertices
        .chunks_exact(3)
        .map(|t| {
            let [a, b, c] = [0, 1, 2].map(|i| t[i].position.map(f64::from));
            let (u, v) = ([b[0] - a[0], b[2] - a[2]], [c[0] - a[0], c[2] - a[2]]);
            let area_y = 0.5 * (u[1] * v[0] - u[0] * v[1]);
            ((a[1] + b[1] + c[1]) / 3.0 - floor) * area_y
        })
        .sum()
}

fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn race(resolution: u32, whitewater: bool, frames: u32) {
    let settings = dam_break(resolution, whitewater);
    let domain = settings.domain_layout().expect("dam break domain");
    let mut world = seeded_world(settings, domain, true).expect("dam break world");
    world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
    // The floor in native coordinates: the scene floor moved by the padding.
    let floor = f64::from(domain.to_native(domain.min)[1]);
    let mut surface = Vec::new();
    let (mut wall, mut reported, mut substeps, mut drift) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut first = None;
    let mut particles = 0;
    for frame in 0..frames {
        let start = Instant::now();
        let stats = world.step(Seconds(1.0 / 60.0)).expect("engine step");
        wall.push(start.elapsed().as_secs_f64() * 1000.0);
        reported.push(stats.simulation_ms);
        substeps.push(f64::from(stats.substeps));
        world.surface(&mut surface).expect("engine surface");
        let volume = enclosed_volume(&surface, floor);
        let v0 = *first.get_or_insert(volume);
        drift.push((volume / v0 - 1.0).abs());
        if frame == 0 {
            particles = stats.particles;
        }
        if frame % 30 == 29 {
            println!(
                "ENGINE dam break {resolution}³ whitewater {whitewater} frame {frame:3}: {:.1} ms wall, {:.1} ms reported, {} substeps, {} particles, volume {volume:.4} m³ ({:+.2}% of frame 0)",
                wall.last().unwrap(),
                stats.simulation_ms,
                stats.substeps,
                stats.particles,
                100.0 * (volume / v0 - 1.0)
            );
        }
    }
    // Short lines: the tool output around these probes cuts long ones.
    println!("ENGINE {resolution}³ settings: Detail 1, particle scale 2.2, smoothing 0.35 × 2, substeps 1–6 at CFL 5, whitewater {whitewater}");
    println!(
        "ENGINE {resolution}³ over {frames} frames: {:.1} ms wall median, {:.1} ms reported median, {:.2} substeps mean, {particles} particles",
        median(&wall),
        median(&reported),
        substeps.iter().sum::<f64>() / substeps.len() as f64,
    );
    println!(
        "ENGINE {resolution}³ volume: frame 0 {:.4} m³, drift max {:.2}%, at the last frame {:.2}%",
        first.unwrap_or(0.0),
        100.0 * drift.iter().copied().fold(0.0, f64::max),
        100.0 * drift.last().copied().unwrap_or(0.0)
    );
}

#[test]
fn fft_water_engine_race() {
    race(64, false, 300);
    race(64, true, 120);
}

#[test]
fn fft_water_engine_race_refined() {
    race(128, false, 300);
}
