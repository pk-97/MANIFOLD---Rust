//! Bounded CPU cost comparison: simulate once, destroy the world, remesh six
//! variants. This is a native dam-break fixture, not a project-render substitute.
//! Pass `--meshes` to export each remeshed variant as a sibling GLB for viewing.
//! `--defer-mesh` skips unused per-step meshes; the final snapshot is still meshed.
//! Usage: surface_tuning <output.csv> [resolution=64] [steps=90] [--meshes] [--defer-mesh]

use std::error::Error;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use manifold_fluids::{Bounds, Config, FluidWorld, SurfaceOptions, SurfaceVertex};
use manifold_foundation::Seconds;

fn write_mesh_glb(
    path: &Path,
    vertices: &[SurfaceVertex],
    scene_offset: f32,
) -> Result<(), Box<dyn Error>> {
    if vertices.is_empty() {
        return Err("cannot export an empty mesh".into());
    }
    if !vertices.len().is_multiple_of(3) {
        return Err("surface mesh vertex count is not a triangle list".into());
    }

    let vertex_count = u32::try_from(vertices.len())?;
    let binary_len = vertices
        .len()
        .checked_mul(24)
        .ok_or("GLB vertex buffer length overflow")?;
    let mut binary = Vec::with_capacity(binary_len);
    let scene_origin = [2.0 + scene_offset, scene_offset, 2.0 + scene_offset];
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in vertices {
        let position = [
            vertex.position[0] - scene_origin[0],
            vertex.position[1] - scene_origin[1],
            vertex.position[2] - scene_origin[2],
        ];
        if position
            .iter()
            .chain(vertex.normal.iter())
            .any(|value| !value.is_finite())
        {
            return Err("surface mesh contains a non-finite value".into());
        }
        for axis in 0..3 {
            min[axis] = min[axis].min(position[axis]);
            max[axis] = max[axis].max(position[axis]);
        }
        for value in position.into_iter().chain(vertex.normal) {
            binary.extend_from_slice(&value.to_le_bytes());
        }
    }
    debug_assert_eq!(binary.len(), binary_len);

    let json = format!(
        "{{\"asset\":{{\"version\":\"2.0\"}},\"scene\":0,\"scenes\":[{{\"nodes\":[0]}}],\"nodes\":[{{\"mesh\":0}}],\"meshes\":[{{\"primitives\":[{{\"attributes\":{{\"POSITION\":0,\"NORMAL\":1}},\"mode\":4}}]}}],\"buffers\":[{{\"byteLength\":{binary_len}}}],\"bufferViews\":[{{\"buffer\":0,\"byteOffset\":0,\"byteLength\":{binary_len},\"byteStride\":24,\"target\":34962}}],\"accessors\":[{{\"bufferView\":0,\"byteOffset\":0,\"componentType\":5126,\"count\":{vertex_count},\"type\":\"VEC3\",\"min\":[{:.9},{:.9},{:.9}],\"max\":[{:.9},{:.9},{:.9}]}},{{\"bufferView\":0,\"byteOffset\":12,\"componentType\":5126,\"count\":{vertex_count},\"type\":\"VEC3\"}}]}}",
        min[0], min[1], min[2], max[0], max[1], max[2]
    );
    let mut json = json.into_bytes();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let total_len = 12usize
        .checked_add(8)
        .and_then(|length| length.checked_add(json.len()))
        .and_then(|length| length.checked_add(8))
        .and_then(|length| length.checked_add(binary.len()))
        .ok_or("GLB length overflow")?;
    let total_len = u32::try_from(total_len)?;
    let json_len = u32::try_from(json.len())?;
    let binary_len = u32::try_from(binary.len())?;

    let mut glb = Vec::with_capacity(total_len as usize);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&total_len.to_le_bytes());
    glb.extend_from_slice(&json_len.to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json);
    glb.extend_from_slice(&binary_len.to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&binary);
    debug_assert_eq!(glb.len(), total_len as usize);

    File::create(path)?.write_all(&glb)?;
    Ok(())
}

fn mesh_output_path(csv_path: &Path, detail: u32, isolated_scale: f64) -> PathBuf {
    let stem = csv_path.file_stem().unwrap_or_default().to_string_lossy();
    let filename = format!("{stem}-detail{detail}-isolated{isolated_scale}.glb");
    csv_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(filename)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args: Vec<_> = std::env::args().collect();
    let mut export_meshes = false;
    let mut defer_mesh = false;
    while let Some(flag) = args.last() {
        match flag.as_str() {
            "--meshes" => export_meshes = true,
            "--defer-mesh" => defer_mesh = true,
            _ => break,
        }
        args.pop();
    }
    let positional_args = args.len();
    if !(2..=4).contains(&positional_args) {
        return Err(
            "usage: surface_tuning <output.csv> [resolution=64] [steps=90] [--meshes] [--defer-mesh]".into(),
        );
    }
    let resolution: u32 = if positional_args >= 3 {
        args[2].parse()?
    } else {
        64
    };
    let steps: u32 = if positional_args >= 4 {
        args[3].parse()?
    } else {
        90
    };
    if ![32, 64, 96].contains(&resolution) || !(1..=120).contains(&steps) {
        return Err("resolution must be 32, 64 or 96; steps must be in 1..=120".into());
    }
    let mut csv = File::create(&args[1])?;
    writeln!(
        csv,
        "phase,resolution,steps,detail,isolated_scale,vertices,elapsed_ms"
    )?;
    let dx = 4.0 / f64::from(resolution);
    let offset = 1.5 * dx as f32;
    let bounds = |min: [f32; 3], max: [f32; 3]| Bounds {
        min: min.map(|v| v + offset),
        max: max.map(|v| v + offset),
    };
    let mut world = FluidWorld::new(Config {
        cells: [resolution + 3; 3],
        cell_size: dx,
        surface_subdivisions: 0,
        apic: false,
    })?;
    world.set_surface_reconstruction_enabled(!defer_mesh)?;
    let options = SurfaceOptions {
        particle_scale: 2.2,
        smoothing: 0.35,
        smoothing_iterations: 2,
    };
    world.set_surface_options(options)?;
    world.set_gravity([0.0, -9.81, 0.0])?;
    world.add_fluid_box(bounds([0.0; 3], [4.0, 0.16, 4.0]), [0.0; 3])?;
    world.add_fluid_box(bounds([0.16, 0.16, 0.25], [1.34, 2.08, 3.75]), [0.0; 3])?;
    let obstacle = bounds([2.05, 0.0, 1.475], [2.65, 1.16, 2.325]);
    world.set_obstacle(obstacle, obstacle, obstacle)?;
    let start = Instant::now();
    for _ in 0..steps {
        world.step(Seconds(1.0 / 60.0))?;
        if start.elapsed() > Duration::from_secs(120) {
            return Err("simulation exceeded the 120-second probe budget".into());
        }
    }
    let simulation_phase = if defer_mesh {
        "simulate_deferred"
    } else {
        "simulate"
    };
    writeln!(
        csv,
        "{simulation_phase},{resolution},{steps},0,1,0,{:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    )?;
    let start = Instant::now();
    let mut frame = world.capture_surface_frame()?;
    let capture_ms = start.elapsed().as_secs_f64() * 1000.0;
    drop(world);
    writeln!(csv, "capture,{resolution},{steps},0,1,0,{:.3}", capture_ms)?;
    let mut vertices = Vec::new();
    for detail in 0..=2 {
        for isolated_scale in [1.0, 0.65] {
            let start = Instant::now();
            frame.reconstruct_with_isolated_scale(
                detail,
                options,
                isolated_scale,
                &mut vertices,
            )?;
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            writeln!(
                csv,
                "remesh,{resolution},{steps},{detail},{isolated_scale},{},{elapsed_ms:.3}",
                vertices.len()
            )?;
            println!(
                "detail={detail} isolated={isolated_scale} vertices={} elapsed_ms={elapsed_ms:.3}",
                vertices.len()
            );
            csv.flush()?;
            if export_meshes {
                let path = mesh_output_path(Path::new(&args[1]), detail, isolated_scale);
                write_mesh_glb(&path, &vertices, offset)?;
                println!("wrote {}", path.display());
            }
        }
    }
    Ok(())
}
