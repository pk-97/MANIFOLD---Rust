//! DIAG PROBE (diag/gpu-flip-feel, not for landing): the FLIP Fluids engine's
//! side of `gpu_flip_feel_probe`, solver only, meshed only on the still frames.

use std::io::Write as _;

use manifold_core::Seconds;
use manifold_fluids::{CaptureError, ParticleRecord, SurfaceOptions, WhitewaterOptions};

use super::native::seeded_world;
use super::{FluidSettings, Transform};
use crate::node_graph::primitives::gpu_flip_feel_probe::{HEADER, OUT, STILLS, measure_row, side_still};
use crate::node_graph::primitives::gpu_flip_still::write_still;

#[test]
fn gpu_flip_feel_engine() {
    engine(false, 300);
}

/// Meshing is fixed at the first step: a meshed run for the mesh stills only.
#[test]
fn gpu_flip_feel_engine_meshed() {
    engine(true, 151);
}

fn engine(meshed: bool, frames: usize) {
    // SAFETY: set before any thread reads it; this test runs alone.
    unsafe { std::env::set_var("GPU_FLIP_STILLS", OUT) };
    std::fs::create_dir_all(OUT).expect("out dir");
    let settings = FluidSettings {
        seed: 0,
        resolution: 64,
        domain_size: 4.0,
        fill_height: 0.16,
        initial_volume: Some(Transform { pos: [-1.25, 1.12, 0.0], scale: [1.18, 1.92, 3.5], ..Transform::default() }),
        surface_subdivisions: 1,
        surface: SurfaceOptions { particle_scale: 2.2, smoothing: 0.35, smoothing_iterations: 2 },
        whitewater: WhitewaterOptions { enabled: false, ..WhitewaterOptions::default() },
        ..FluidSettings::default()
    };
    println!("FEEL engine settings: {:?} {:?} apic {} boundary {:?}", settings.liquid, settings.time_steps, settings.apic, settings.boundary_collisions);
    let domain = settings.domain_layout().expect("domain");
    println!("FEEL engine domain: min {:?} cells {:?} cell {} native origin {:?}", domain.min, domain.cells, domain.cell_size, domain.native_origin());
    let mut world = seeded_world(settings, domain, meshed).expect("world");
    world.set_gravity([0.0, -9.81, 0.0]).expect("gravity");
    let offset = domain.to_scene([0.0; 3]);
    let floor = f64::from(domain.min[1]);
    let (mut records, mut solid, mut surface) = (Vec::new(), Vec::new(), Vec::new());
    let mut csv = std::fs::File::create(format!("{OUT}/engine{}.csv", if meshed { "_meshed" } else { "" })).expect("csv");
    writeln!(csv, "{HEADER},substeps").unwrap();
    let start = std::time::Instant::now();
    for frame in 0..frames {
        let mesh = STILLS.contains(&frame);
        let stats = world.step(Seconds(1.0 / 60.0)).expect("step");
        let info = loop {
            match world.capture_particle_frame(offset, &mut records, &mut solid) {
                Ok(info) => break info,
                Err(CaptureError::Capacity { particles, solid: nodes }) => {
                    records.resize(particles as usize, ParticleRecord::default());
                    solid.resize(nodes, 0.0);
                }
                Err(CaptureError::Fluid(e)) => panic!("capture: {e}"),
            }
        };
        let live: Vec<_> = records[..info.count as usize].iter().map(|p| (p.position_radius, p.velocity)).collect();
        writeln!(csv, "{},{}", measure_row(frame, &live, floor), stats.substeps).unwrap();
        if mesh {
            if !meshed {
                side_still(&format!("engine_side_f{frame}"), &live, floor);
                continue;
            }
            world.surface(&mut surface).expect("surface");
            let tris = surface.chunks_exact(3).map(|t| [0, 1, 2].map(|i| domain.to_scene(t[i].position)));
            write_still(&format!("engine_mesh_f{frame}"), tris);
        }
        if frame % 30 == 29 {
            println!("FEEL engine frame {frame}: {:.1} s elapsed, {} substeps, {} particles", start.elapsed().as_secs_f64(), stats.substeps, info.count);
        }
    }
}
