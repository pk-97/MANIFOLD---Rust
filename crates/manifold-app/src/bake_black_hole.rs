// Headless bake tool for the Black Hole deflection cache.
//
// Runs the existing deflection compute shader at every grid point in a
// (cam_dist, tilt) lattice and writes the resulting RGBA16Float textures to
// a single packed `.bhcache` file with LZ4-compressed entries.
//
// Invoked via `manifold bake-black-hole`. Does not create a window — uses a
// standalone GpuDevice. Designed to run in 20-30 seconds for the default
// 10×10 grid at 2048×2048.

use manifold_gpu::{
    GpuBinding, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};
use manifold_renderer::generators::bh_cache::{
    BhCacheHeader, BhCacheWriter, grid_cam_dist_values, grid_tilt_values,
};
use std::path::PathBuf;

const DEFLECTION_SHADER: &str =
    include_str!("../../manifold-renderer/src/generators/shaders/black_hole_deflection.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DeflectionUniforms {
    aspect: f32,
    cam_dist: f32,
    tilt_rad: f32,
    rotate_rad: f32,
    steps: f32,
    uv_scale: f32,
    spin: f32,
    _pad0: f32,
}

pub struct BakeArgs {
    pub output: PathBuf,
    pub resolution: u32,
    pub spin: f32,
    pub steps: f32,
    pub grid_size: u32,
}

pub fn run(args: BakeArgs) -> Result<(), String> {
    let start = std::time::Instant::now();

    eprintln!(
        "manifold bake-black-hole — resolution={}, grid={}x{}, spin={}, steps={}",
        args.resolution, args.grid_size, args.grid_size, args.spin, args.steps,
    );
    eprintln!("output: {}", args.output.display());

    // ── Create standalone GPU device ──
    let device = GpuDevice::new();
    let pipeline =
        device.create_compute_pipeline(DEFLECTION_SHADER, "cs_main", "BlackHole Bake Deflection");

    let cam_dist_values = grid_cam_dist_values(args.grid_size);
    let tilt_values = grid_tilt_values(args.grid_size);

    let header = BhCacheHeader {
        grid_rows: args.grid_size,
        grid_cols: args.grid_size,
        tex_width: args.resolution,
        tex_height: args.resolution,
        tex_count: 3,
        spin: args.spin,
        steps: args.steps,
        cam_dist_values: cam_dist_values.clone(),
        tilt_values: tilt_values.clone(),
    };

    if let Some(parent) = args.output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| format!("create output dir: {e}"))?;
    }

    let mut writer = BhCacheWriter::create(&args.output, header.clone())
        .map_err(|e| format!("create cache file: {e}"))?;

    // ── Allocate output textures ──
    let make_tex = |label: &str| -> GpuTexture {
        device.create_texture(&GpuTextureDesc {
            width: args.resolution,
            height: args.resolution,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::SHADER_WRITE
                | GpuTextureUsage::COPY_SRC
                | GpuTextureUsage::COPY_DST,
            label,
        })
    };
    let tex1 = make_tex("BHBake Deflection1");
    let tex2 = make_tex("BHBake Deflection2");
    let tex3 = make_tex("BHBake SkyDir");

    // ── Allocate readback buffers ──
    // Metal requires bytes_per_row aligned to 256 bytes for blit copies.
    let bpp = 8u32; // RGBA16Float
    let bytes_per_row = align_to_256(args.resolution * bpp);
    let buffer_size = (bytes_per_row * args.resolution) as u64;
    let buf1 = device.create_buffer_shared(buffer_size);
    let buf2 = device.create_buffer_shared(buffer_size);
    let buf3 = device.create_buffer_shared(buffer_size);

    let buf1_ptr = buf1
        .mapped_ptr()
        .ok_or("readback buffer 1 has no mapped pointer")?
        as *const u8;
    let buf2_ptr = buf2
        .mapped_ptr()
        .ok_or("readback buffer 2 has no mapped pointer")?
        as *const u8;
    let buf3_ptr = buf3
        .mapped_ptr()
        .ok_or("readback buffer 3 has no mapped pointer")?
        as *const u8;

    let event = device.create_event();
    let total = (args.grid_size * args.grid_size) as usize;
    let mut entry_idx = 0usize;

    // Pre-allocated tight scratch for the assembled entry data.
    let entry_bytes = header.entry_bytes();
    let mut entry_scratch = vec![0u8; entry_bytes];
    let tex_bytes = header.texture_bytes();

    // ── Iterate grid ──
    for (di, &cam_dist) in cam_dist_values.iter().enumerate() {
        for (ti, &tilt_deg) in tilt_values.iter().enumerate() {
            let tilt_rad = tilt_deg.to_radians();

            let uniforms = DeflectionUniforms {
                aspect: 1.0,    // square bake
                cam_dist,
                tilt_rad,
                rotate_rad: 0.0,
                steps: args.steps,
                uv_scale: 1.0,
                spin: args.spin,
                _pad0: 0.0,
            };

            let mut enc = device.create_encoder("BHBake Frame");

            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Texture {
                        binding: 1,
                        texture: &tex1,
                    },
                    GpuBinding::Texture {
                        binding: 2,
                        texture: &tex2,
                    },
                    GpuBinding::Texture {
                        binding: 3,
                        texture: &tex3,
                    },
                ],
                [
                    args.resolution.div_ceil(16),
                    args.resolution.div_ceil(16),
                    1,
                ],
                "BHBake Deflection",
            );

            enc.copy_texture_to_buffer(&tex1, &buf1, args.resolution, args.resolution, bytes_per_row);
            enc.copy_texture_to_buffer(&tex2, &buf2, args.resolution, args.resolution, bytes_per_row);
            enc.copy_texture_to_buffer(&tex3, &buf3, args.resolution, args.resolution, bytes_per_row);

            enc.signal_event(&event);
            let signaled_value = event.current_value();
            enc.commit();

            event.wait_until_done(signaled_value);

            // Tightly pack each row (strip 256-byte alignment padding).
            copy_tight(buf1_ptr, &mut entry_scratch[0..tex_bytes], args.resolution, bytes_per_row);
            copy_tight(
                buf2_ptr,
                &mut entry_scratch[tex_bytes..2 * tex_bytes],
                args.resolution,
                bytes_per_row,
            );
            copy_tight(
                buf3_ptr,
                &mut entry_scratch[2 * tex_bytes..3 * tex_bytes],
                args.resolution,
                bytes_per_row,
            );

            writer
                .write_entry(&entry_scratch)
                .map_err(|e| format!("write_entry: {e}"))?;

            entry_idx += 1;
            eprintln!(
                "  [{:3}/{}] di={di} ti={ti} cam_dist={:.2} tilt={:.1}°",
                entry_idx, total, cam_dist, tilt_deg,
            );
        }
    }

    writer.finish().map_err(|e| format!("finish writer: {e}"))?;

    let elapsed = start.elapsed();
    let file_size = std::fs::metadata(&args.output)
        .map(|m| m.len())
        .unwrap_or(0);
    eprintln!(
        "done in {:.1}s — wrote {} ({:.1} MB)",
        elapsed.as_secs_f32(),
        args.output.display(),
        file_size as f64 / (1024.0 * 1024.0),
    );

    Ok(())
}

/// Copy a 2D buffer with row stride > tight row bytes into a tight destination.
fn copy_tight(src: *const u8, dst: &mut [u8], height: u32, src_stride: u32) {
    let row_bytes = dst.len() / height as usize;
    for row in 0..height as usize {
        let src_row_start = row * src_stride as usize;
        let dst_row_start = row * row_bytes;
        unsafe {
            std::ptr::copy_nonoverlapping(
                src.add(src_row_start),
                dst[dst_row_start..dst_row_start + row_bytes].as_mut_ptr(),
                row_bytes,
            );
        }
    }
}

fn align_to_256(n: u32) -> u32 {
    (n + 255) & !255
}
