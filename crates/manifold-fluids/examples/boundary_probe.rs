//! Bounded CPU diagnostic for fluid that may disappear near a domain boundary
//! at low resolution. It records particle and surface evidence without making
//! assumptions about which fixtures should retain particles.
//! Usage: boundary_probe <output.csv>

use std::error::Error;
use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

use manifold_fluids::{Bounds, Config, FluidWorld, SurfaceOptions, SurfaceVertex};
use manifold_foundation::Seconds;

struct MeshMetrics {
    min: Option<[f32; 3]>,
    max: Option<[f32; 3]>,
    finite_vertices: usize,
}

fn measure_mesh(vertices: &[SurfaceVertex], offset: f32) -> MeshMetrics {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut finite_vertices = 0;
    for vertex in vertices {
        let position = [
            vertex.position[0] - offset,
            vertex.position[1] - offset,
            vertex.position[2] - offset,
        ];
        if !position
            .iter()
            .chain(&vertex.normal)
            .all(|value| value.is_finite())
        {
            continue;
        }
        finite_vertices += 1;
        for axis in 0..3 {
            min[axis] = min[axis].min(position[axis]);
            max[axis] = max[axis].max(position[axis]);
        }
    }
    let bounds = (finite_vertices > 0).then_some((min, max));
    MeshMetrics {
        min: bounds.map(|(min, _)| min),
        max: bounds.map(|(_, max)| max),
        finite_vertices,
    }
}

struct ProbeRow<'a> {
    fixture: &'a str,
    resolution: u32,
    phase: &'a str,
    tick: u32,
    particles: u32,
    triangles: u32,
    metrics: &'a MeshMetrics,
    elapsed_ms: f64,
}

fn write_row(csv: &mut File, row: ProbeRow<'_>) -> Result<(), Box<dyn Error>> {
    let ProbeRow {
        fixture,
        resolution,
        phase,
        tick,
        particles,
        triangles,
        metrics,
        elapsed_ms,
    } = row;
    if let (Some(min), Some(max)) = (metrics.min, metrics.max) {
        writeln!(
            csv,
            "{fixture},{resolution},{phase},{tick},{particles},{triangles},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{},{elapsed_ms:.3}",
            min[0], min[1], min[2], max[0], max[1], max[2], metrics.finite_vertices
        )?;
    } else {
        writeln!(
            csv,
            "{fixture},{resolution},{phase},{tick},{particles},{triangles},,,,,,,{},{elapsed_ms:.3}",
            metrics.finite_vertices
        )?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 2 {
        return Err("usage: boundary_probe <output.csv>".into());
    }

    let mut csv = File::create(&args[1])?;
    writeln!(
        csv,
        "fixture,resolution,phase,tick,particles,triangles,min_x,min_y,min_z,max_x,max_y,max_z,finite_vertices,elapsed_ms"
    )?;

    let global_start = Instant::now();
    let options = SurfaceOptions {
        particle_scale: 2.2,
        smoothing: 0.35,
        smoothing_iterations: 2,
    };
    let fixtures = [
        ("shallow_pool", [0.0, 0.0, 0.0], [4.0, 0.16, 4.0]),
        ("left_wall_cube", [0.0, 0.8, 1.0], [1.0, 1.8, 2.0]),
    ];

    for resolution in [8, 16, 32] {
        let dx = 4.0 / f64::from(resolution);
        let offset = 1.5 * dx as f32;
        for &(fixture, min, max) in &fixtures {
            let authored_bounds = |bounds_min: [f32; 3], bounds_max: [f32; 3]| Bounds {
                min: bounds_min.map(|value| value + offset),
                max: bounds_max.map(|value| value + offset),
            };
            let mut world = FluidWorld::new(Config {
                cells: [resolution + 3; 3],
                cell_size: dx,
                surface_subdivisions: 0,
                apic: false,
            })?;
            world.set_surface_options(options)?;
            world.set_gravity([0.0, -9.81, 0.0])?;
            world.add_fluid_box(authored_bounds(min, max), [0.0; 3])?;

            let run_start = Instant::now();
            let mut surface = Vec::new();
            let mut final_particles = 0;
            for tick in 1..=180 {
                let stats = world.step(Seconds(1.0 / 60.0))?;
                final_particles = stats.particles;
                if global_start.elapsed() > Duration::from_secs(60) {
                    return Err(format!(
                        "boundary probe exceeded the 60-second deadline at {fixture}, resolution {resolution}, tick {tick}"
                    )
                    .into());
                }
                if matches!(tick, 1 | 30 | 90 | 180) {
                    world.surface(&mut surface)?;
                    let metrics = measure_mesh(&surface, offset);
                    write_row(
                        &mut csv,
                        ProbeRow {
                            fixture,
                            resolution,
                            phase: "tick",
                            tick,
                            particles: stats.particles,
                            triangles: stats.triangles,
                            metrics: &metrics,
                            elapsed_ms: run_start.elapsed().as_secs_f64() * 1000.0,
                        },
                    )?;
                }
            }

            let remesh_start = Instant::now();
            let mut frame = world.capture_surface_frame()?;
            drop(world);
            let mut remeshed = Vec::new();
            frame.reconstruct(1, options, &mut remeshed)?;
            let metrics = measure_mesh(&remeshed, offset);
            write_row(
                &mut csv,
                ProbeRow {
                    fixture,
                    resolution,
                    phase: "remesh_final",
                    tick: 180,
                    particles: final_particles,
                    triangles: (remeshed.len() / 3) as u32,
                    metrics: &metrics,
                    elapsed_ms: remesh_start.elapsed().as_secs_f64() * 1000.0,
                },
            )?;
            csv.flush()?;
        }
    }
    Ok(())
}
