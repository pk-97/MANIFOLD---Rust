//! Checked against FLIP Fluids the engine's Dam Break race (MIT); see THIRD_PARTY_NOTICES.md.
//! The FLIP Fluids engine's side of the water race
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md section 6 (measures)): the shipped Dam Break
//! (`WaterDamBreak.json`) with its obstacle unwired, built by the production
//! world setup and stepped as the worker steps it. It reports the wall clock
//! per tick, the engine's own counters and substeps, and the volume its
//! surface mesh holds and how its particles move per frame, measured as
//! `gpu_flip_race_tests` measures GPU FLIP's. CPU only and minutes long: opt in
//! with `--features water-race-probes`.

use std::time::Instant;

use manifold_core::Seconds;
use manifold_fluids::{CaptureError, ParticleRecord, SurfaceOptions, SurfaceVertex, WhitewaterOptions};

use super::native::seeded_world;
use super::{FluidSettings, Transform};
use crate::node_graph::primitives::gpu_flip_race_tests::{
    Breakup, Motion, Packing, Splash, breakup, motion, packing, print_height, print_lid_layer, print_side_sheet, print_splash,
    report_breakup, report_motion, report_water, splash,
};
use crate::node_graph::primitives::gpu_flip_still::write_still;
use crate::node_graph::primitives::gpu_flip_volume::{VolumeDrift, volume_and_area};

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

fn triangles(vertices: &[SurfaceVertex]) -> impl Iterator<Item = [[f32; 3]; 3]> + '_ {
    vertices.chunks_exact(3).map(|t| [0, 1, 2].map(|i| t[i].position))
}

/// The marker particles' motion after a step, read through the particle-frame
/// seam in scene coordinates, their packing on the engine's own grid (GPU FLIP's
/// water measure) and how high they throw. The buffers grow to what the
/// capture asks for.
fn engine_motion(
    world: &mut manifold_fluids::FluidWorld,
    domain: super::FluidDomainLayout,
    records: &mut Vec<ParticleRecord>,
    solid: &mut Vec<f32>,
) -> (Motion, Packing, Splash, Breakup, usize) {
    let offset = domain.to_scene([0.0; 3]);
    let info = loop {
        match world.capture_particle_frame(offset, records, solid) {
            Ok(info) => break info,
            Err(CaptureError::Capacity { particles, solid: nodes }) => {
                records.resize(particles as usize, ParticleRecord::default());
                solid.resize(nodes, 0.0);
            }
            Err(CaptureError::Fluid(e)) => panic!("engine particle frame: {e}"),
        }
    };
    let live = &records[..info.count as usize];
    let (origin, cells) = engine_grid(domain);
    let pack = packing(live.iter().map(|p| p.position_radius), origin, cells, domain.cell_size);
    let floor = f64::from(domain.min[1]);
    let thrown = splash(live.iter().map(|p| p.position_radius), floor);
    // Breakup on the tank's own lattice, as GPU FLIP measures it.
    let broken =
        breakup(live.iter().map(|p| p.position_radius), domain.min.map(f64::from), domain.cells.map(|n| n as usize), domain.cell_size);
    (motion(live.iter().map(|p| (p.position_radius, p.velocity)), floor), pack, thrown, broken, live.len())
}

/// The engine's own cells in scene coordinates: its native grid, 1.5 cells
/// of solid padding past the authored box on every side.
fn engine_grid(domain: super::FluidDomainLayout) -> ([f64; 3], [usize; 3]) {
    (domain.native_origin().map(f64::from), domain.config(FluidSettings::default()).cells.map(|n| n as usize))
}

fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn race(resolution: u32, whitewater: bool, frames: u32) {
    let settings = dam_break(resolution, whitewater);
    // The particles' own volume: the seeding puts 8 in a cell.
    let cell = f64::from(settings.domain_size) / f64::from(resolution);
    let domain = settings.domain_layout().expect("dam break domain");
    let mut world = seeded_world(settings, domain, true).expect("dam break world");
    world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
    // The tank in native coordinates: the scene box moved by the padding.
    let tank_min = domain.to_native(domain.min).map(f64::from);
    let tank_size = f64::from(domain.cells[0]) * domain.cell_size;
    let mut surface = Vec::new();
    let (mut wall, mut reported, mut substeps, mut drift, mut raw) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut oracle = None;
    let mut particles = 0;
    let (mut records, mut solid, mut motions, mut packed, mut breaks) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for frame in 0..frames {
        let start = Instant::now();
        let stats = world.step(Seconds(1.0 / 60.0)).expect("engine step");
        wall.push(start.elapsed().as_secs_f64() * 1000.0);
        reported.push(stats.simulation_ms);
        substeps.push(f64::from(stats.substeps));
        let (m, pack, thrown, broken, count) = engine_motion(&mut world, domain, &mut records, &mut solid);
        motions.push(m);
        packed.push(pack);
        breaks.push(broken);
        let sheet = frame % 5 == 4 && frame < 90;
        if frame % 15 == 14 || sheet {
            let label = format!("ENGINE {resolution}³");
            let live: Vec<_> = records[..count].iter().map(|p| (p.position_radius, p.velocity)).collect();
            let floor = f64::from(domain.min[1]);
            if sheet {
                print_side_sheet(&label, frame as usize, &live, floor);
                print_height(&label, frame as usize, &m);
            }
            if frame % 15 == 14 {
                print_splash(&label, frame as usize, &thrown);
                let (origin, cells) = engine_grid(domain);
                print_lid_layer(&label, frame as usize, &live, origin, cells, domain.cell_size, floor);
            }
        }
        world.surface(&mut surface).expect("engine surface");
        let measure = volume_and_area(triangles(&surface), tank_min, tank_size);
        raw.push(measure.0);
        if frame == 0 {
            particles = stats.particles;
        }
        let oracle = oracle.get_or_insert_with(|| VolumeDrift::new(measure, particles as f64 * cell.powi(3) / 8.0));
        drift.push(oracle.drift(measure));
        if !whitewater && (frame == 90 || frame == 240) {
            let scene = triangles(&surface).map(|t| t.map(|p| domain.to_scene(p)));
            write_still(&format!("ENGINE{resolution}_frame{frame}"), scene);
        }
        if frame % 30 == 29 {
            println!(
                "ENGINE {resolution}³ whitewater {whitewater} frame {frame:3}: {:.1} ms wall, {:.1} ms reported, {} substeps, {} particles",
                wall.last().unwrap(),
                stats.simulation_ms,
                stats.substeps,
                stats.particles,
            );
            println!(
                "ENGINE {resolution}³ frame {frame:3}: speed mean {:.2} p99 {:.2} top {:.2} m/s, highest {:.2} m",
                m.mean, m.p99, m.fastest, m.highest
            );
            println!(
                "ENGINE {resolution}³ frame {frame:3}: water volume {:+.2}%, raw mesh {:+.2}%",
                100.0 * drift[frame as usize],
                100.0 * (raw[frame as usize] / raw[0] - 1.0),
            );
            println!(
                "ENGINE {resolution}³ frame {frame:3}: particles past rest {:.1}%, missing inside {:.1}%",
                100.0 * pack.crowded,
                100.0 * pack.hollow
            );
        }
    }
    report_water(&format!("ENGINE {resolution}³"), &packed);
    report_breakup(&format!("ENGINE {resolution}³"), &breaks);
    report_motion(&format!("ENGINE {resolution}³"), &motions);
    // Short lines: the tool output around these probes cuts long ones.
    println!("ENGINE {resolution}³ settings: Detail 1, particle scale 2.2, smoothing 0.35 × 2, substeps 1–6 at CFL 5, whitewater {whitewater}");
    println!(
        "ENGINE {resolution}³ over {frames} frames: {:.1} ms wall median, {:.1} ms reported median, {:.2} substeps mean, {particles} particles",
        median(&wall),
        median(&reported),
        substeps.iter().sum::<f64>() / substeps.len() as f64,
    );
    if let Some(oracle) = oracle {
        println!(
            "ENGINE {resolution}³: particles hold {:.4} m³, mesh {:.4} m³ at frame 0, skin {:.2} mm",
            particles as f64 * cell.powi(3) / 8.0,
            raw[0],
            1000.0 * oracle.skin()
        );
    }
    println!(
        "ENGINE {resolution}³: water volume drift max {:.2}%, at the last frame {:+.2}%",
        100.0 * drift.iter().map(|d| d.abs()).fold(0.0, f64::max),
        100.0 * drift.last().copied().unwrap_or(0.0)
    );
}

#[test]
fn gpu_flip_engine_race() {
    race(64, false, 300);
    race(64, true, 120);
}

/// The engine's side of `gpu_flip_splash_causes_64`: 150 frames at 64³,
/// whitewater off.
#[test]
fn gpu_flip_engine_splash_64() {
    race(64, false, 150);
}

/// The engine's side of the settle check: 15 s at 64³, whitewater off.
#[test]
fn gpu_flip_engine_settles() {
    race(64, false, 900);
}

#[test]
fn gpu_flip_engine_race_refined() {
    race(128, false, 300);
}

/// The engine's side of `gpu_flip_still_pool_rests_round_a_static_obstacle`:
/// the pool 1 m deep round the Dam Break's box at 64³ for 120 frames, the
/// fastest particles in the engine's own cells.
#[test]
fn gpu_flip_engine_still_pool_round_a_box() {
    use crate::node_graph::primitives::gpu_flip_preset::DAM_OBSTACLE;
    let settings = FluidSettings { initial_volume: None, fill_height: 1.0, ..dam_break(64, false) };
    let domain = settings.domain_layout().expect("pool domain");
    let mut world = seeded_world(settings, domain, false).expect("pool world");
    world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
    let [pos, scale] = DAM_OBSTACLE;
    let bounds = domain.bounds(Transform { pos: pos.map(|v| v as f32), scale: scale.map(|v| v as f32), ..Transform::default() });
    let (origin, _) = engine_grid(domain);
    let h = domain.cell_size;
    let offset = domain.to_scene([0.0; 3]);
    let (mut records, mut solid) = (Vec::new(), Vec::new());
    let mut peak = 0.0f64;
    for frame in 0..120 {
        world.set_obstacle(bounds, bounds, bounds).expect("obstacle");
        world.step(Seconds(1.0 / 60.0)).expect("engine step");
        let info = loop {
            match world.capture_particle_frame(offset, &mut records, &mut solid) {
                Ok(info) => break info,
                Err(CaptureError::Capacity { particles, solid: nodes }) => {
                    records.resize(particles as usize, ParticleRecord::default());
                    solid.resize(nodes, 0.0);
                }
                Err(CaptureError::Fluid(e)) => panic!("engine particle frame: {e}"),
            }
        };
        let mut fast: Vec<(f64, [f64; 3], [f32; 3])> = records[..info.count as usize]
            .iter()
            .map(|p| {
                let speed = p.velocity.iter().map(|c| f64::from(*c).powi(2)).sum::<f64>().sqrt();
                let at = std::array::from_fn(|a| (f64::from(p.position_radius[a]) - origin[a]) / h);
                (speed, at, [p.velocity[0], p.velocity[1], p.velocity[2]])
            })
            .collect();
        fast.sort_by(|a, b| b.0.total_cmp(&a.0));
        peak = peak.max(fast[0].0);
        if [0, 1, 2, 5, 19, 39, 59, 79, 99, 119].contains(&frame) {
            println!("ENGINE pool round a box frame {frame}: {} live", info.count);
            for (s, at, v) in fast.iter().take(5) {
                println!("ENGINE   {s:.4} m/s at [{:.3}, {:.3}, {:.3}] v {v:?}", at[0], at[1], at[2]);
            }
        }
    }
    println!("ENGINE pool round a box: peak {peak:.4} m/s");
}

/// The engine's side of `gpu_flip_replay_changes_nothing`'s scene: the Dam
/// Break with its box at 64³, the substeps and fastest particle per frame.
#[test]
fn gpu_flip_engine_dam_break_with_box_substeps() {
    use crate::node_graph::primitives::gpu_flip_preset::DAM_OBSTACLE;
    let settings = dam_break(64, false);
    let domain = settings.domain_layout().expect("dam break domain");
    let mut world = seeded_world(settings, domain, false).expect("dam break world");
    world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
    let [pos, scale] = DAM_OBSTACLE;
    let bounds = domain.bounds(Transform { pos: pos.map(|v| v as f32), scale: scale.map(|v| v as f32), ..Transform::default() });
    let offset = domain.to_scene([0.0; 3]);
    let (mut records, mut solid) = (Vec::new(), Vec::new());
    for frame in 0..90 {
        world.set_obstacle(bounds, bounds, bounds).expect("obstacle");
        let stats = world.step(Seconds(1.0 / 60.0)).expect("engine step");
        let info = loop {
            match world.capture_particle_frame(offset, &mut records, &mut solid) {
                Ok(info) => break info,
                Err(CaptureError::Capacity { particles, solid: nodes }) => {
                    records.resize(particles as usize, ParticleRecord::default());
                    solid.resize(nodes, 0.0);
                }
                Err(CaptureError::Fluid(e)) => panic!("engine particle frame: {e}"),
            }
        };
        let mut fast: Vec<(f64, [f32; 3], [f32; 3])> = records[..info.count as usize]
            .iter()
            .map(|p| {
                let s = p.velocity.iter().map(|c| f64::from(*c).powi(2)).sum::<f64>().sqrt();
                (s, [p.position_radius[0], p.position_radius[1], p.position_radius[2]], [p.velocity[0], p.velocity[1], p.velocity[2]])
            })
            .collect();
        fast.sort_by(|a, b| b.0.total_cmp(&a.0));
        println!("ENGINE dam break with box frame {frame}: {} substeps, fastest {:.2} m/s", stats.substeps, fast[0].0);
        if (27..40).contains(&frame) {
            for (s, at, v) in fast.iter().take(3) {
                println!("ENGINE   {s:.2} m/s at {at:?} v {v:?}");
            }
        }
    }
}

/// Probe (floor-jet junction): the engine's `_solidSDF` nodes for the Dam
/// Break with its box after one frame, raw f32, i fastest, written to
/// `$JUNCTION_DUMP_DIR/native_solid.f32` for comparison with GPU FLIP's
/// (`gpu_flip_scene_tests::gpu_flip_junction_solid_dump`).
#[test]
fn gpu_flip_engine_junction_solid_dump() {
    use crate::node_graph::primitives::gpu_flip_preset::DAM_OBSTACLE;
    let settings = dam_break(64, false);
    let domain = settings.domain_layout().expect("dam break domain");
    let mut world = seeded_world(settings, domain, false).expect("dam break world");
    world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
    let [pos, scale] = DAM_OBSTACLE;
    let bounds = domain.bounds(Transform { pos: pos.map(|v| v as f32), scale: scale.map(|v| v as f32), ..Transform::default() });
    let offset = domain.to_scene([0.0; 3]);
    let (mut records, mut solid) = (Vec::new(), Vec::new());
    world.set_obstacle(bounds, bounds, bounds).expect("obstacle");
    world.step(Seconds(1.0 / 60.0)).expect("engine step");
    let info = loop {
        match world.capture_particle_frame(offset, &mut records, &mut solid) {
            Ok(info) => break info,
            Err(CaptureError::Capacity { particles, solid: nodes }) => {
                records.resize(particles as usize, ParticleRecord::default());
                solid.resize(nodes, 0.0);
            }
            Err(CaptureError::Fluid(e)) => panic!("engine particle frame: {e}"),
        }
    };
    let dir = std::env::var_os("JUNCTION_DUMP_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let path = dir.join("native_solid.f32");
    std::fs::write(&path, bytemuck::cast_slice::<f32, u8>(&solid)).expect("dump");
    println!(
        "JUNCTION native nodes {:?} origin {:?} h {} bounds {bounds:?} -> {}",
        info.solid_nodes,
        domain.native_origin(),
        domain.cell_size,
        path.display()
    );
}
