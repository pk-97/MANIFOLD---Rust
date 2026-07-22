//! RT P0 prototype harness. See BRIEF.md. Standalone measurement binary —
//! not wired into manifold-renderer.

mod accel;
mod gbuffer;
mod gpu;
mod scene;
mod tonemap;
mod trace;
mod types;

use std::path::PathBuf;

use manifold_gpu::metalfx::MetalFxSpatialScaler;
use manifold_gpu::{GpuBuffer, GpuTexture, GpuTextureFormat, GpuTextureUsage};
use objc2_metal::{MTLBlitCommandEncoder, MTLBuffer, MTLCommandBuffer, MTLCommandEncoder};

use gpu::Gpu;
use types::{GpuMaterial, PaddedVec3, RtMaterial, TraceParams};

struct Args {
    scan: PathBuf,
    out: PathBuf,
    width: u32,
    height: u32,
    frames: u32,
    sun_only: bool,
}

fn parse_args() -> Args {
    let mut scan = None;
    let mut out = None;
    let mut width = 3840u32;
    let mut height = 2160u32;
    let mut frames = 120u32;
    let mut sun_only = false;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--scan" => scan = Some(PathBuf::from(it.next().expect("--scan needs a path"))),
            "--out" => out = Some(PathBuf::from(it.next().expect("--out needs a path"))),
            "--size" => {
                let s = it.next().expect("--size needs WxH");
                let (w, h) = s.split_once('x').expect("--size format WxH");
                width = w.parse().expect("bad width");
                height = h.parse().expect("bad height");
            }
            "--frames" => frames = it.next().expect("--frames needs a number").parse().expect("bad frames"),
            "--sun-only" => sun_only = true,
            other => panic!("unknown arg: {other}"),
        }
    }
    Args {
        scan: scan.expect("--scan is required"),
        out: out.expect("--out is required"),
        width,
        height,
        frames,
        sun_only,
    }
}

const WARMUP_FRAMES: u32 = 10;
const REFIT_FRAMES: u32 = 60;

fn main() {
    let args = parse_args();
    std::fs::create_dir_all(&args.out).expect("create out dir");

    let scn = scene::load(&args.scan);
    let gpu = Gpu::new();

    // --- CPU-side GPU buffers shared across modes ---
    let positions_flat: Vec<[f32; 3]> = scn.positions.clone();
    let normals_flat: Vec<[f32; 3]> = scn.normals.clone();
    let material_ids: Vec<u32> = scn.material_ids.clone();
    let gpu_materials: Vec<GpuMaterial> = scn
        .materials
        .iter()
        .map(|m| GpuMaterial {
            albedo: m.albedo,
            _p0: 0.0,
            metallic: m.metallic,
            roughness: m.roughness,
            _p1: [0.0, 0.0],
            emissive: m.emissive,
            _p2: 0.0,
        })
        .collect();
    let rt_materials: Vec<RtMaterial> = scn
        .materials
        .iter()
        .map(|m| RtMaterial { albedo: m.albedo, _p0: 0.0, emissive: m.emissive, _p1: 0.0 })
        .collect();

    let positions_buf = gpu.buffer_with_data(&positions_flat);
    let normals_buf = gpu.buffer_with_data(&normals_flat);
    let material_ids_buf = gpu.buffer_with_data(&material_ids);
    let gpu_materials_buf = gpu.buffer_with_data(&gpu_materials);
    let rt_materials_buf = gpu.buffer_with_data(&rt_materials);
    let indices_buf = gpu.buffer_with_data(&scn.indices);
    // mat_index[primitive_id] for the RT kernel — one entry per triangle,
    // matching the geometry's index-buffer triangle order.
    let mat_index_per_tri: Vec<u32> = scn
        .indices
        .chunks_exact(3)
        .map(|tri| scn.material_ids[tri[0] as usize])
        .collect();
    let mat_index_buf = gpu.buffer_with_data(&mat_index_per_tri);

    let tri_count = (scn.indices.len() / 3) as u32;

    // --- Acceleration structure: build once, benchmark refit once ---
    let (accel_obj, bvh_build_cpu_ms, bvh_build_gpu_ms) =
        accel::build(&gpu, &positions_buf, &indices_buf, tri_count);
    println!(
        "[accel] build: cpu={bvh_build_cpu_ms:.3}ms gpu={bvh_build_gpu_ms:.3}ms ({tri_count} tris)"
    );

    let bvh_refit_ms_avg = {
        // Scratch copy of the position buffer so the refit benchmark's
        // sine-displacement doesn't perturb the geometry used for the A/B/C
        // renders below.
        let refit_positions = gpu.buffer_with_data(&positions_flat);
        let (refit_accel, _, _) = accel::build(&gpu, &refit_positions, &indices_buf, tri_count);
        let ptr = refit_positions.raw().contents().as_ptr() as *mut f32;
        let mut total = 0f64;
        for frame in 0..REFIT_FRAMES {
            let t = frame as f32 * 0.05;
            for (i, p) in positions_flat.iter().enumerate() {
                let r = scn.radius;
                let y = p[1] + 0.05 * r * (3.0 * t + p[0]).sin();
                unsafe { *ptr.add(i * 3 + 1) = y };
            }
            total += accel::refit(&gpu, &refit_accel);
        }
        total / REFIT_FRAMES as f64
    };
    println!("[accel] refit avg over {REFIT_FRAMES} frames: {bvh_refit_ms_avg:.3}ms");

    // --- Camera + shared uniform buffers ---
    let aspect = args.width as f32 / args.height as f32;
    let (cam, eye) = gbuffer::build_camera(scn.center, scn.radius, aspect);
    let camera_buf = gpu.buffer_with_data(&[cam]);
    let cam_pos_buf = gpu.buffer_with_data(&[PaddedVec3 { xyz: eye.into(), _pad: 0.0 }]);

    // --- Compile libraries ---
    let gbuffer_src = std::fs::read_to_string("tools/rt_prototype/shaders/gbuffer.metal")
        .or_else(|_| std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/gbuffer.metal")))
        .expect("read gbuffer.metal");
    let trace_src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/rt_trace.metal"))
        .expect("read rt_trace.metal");
    let gbuffer_lib = gpu.compile_library(&gbuffer_src, "gbuffer");
    let trace_lib = gpu.compile_library(&trace_src, "rt_trace");
    let gbuffer_pipeline = gbuffer::GBufferPipeline::new(&gpu, &gbuffer_lib);
    let trace_pipelines = trace::TracePipelines::new(&gpu, &trace_lib);

    let sun_dir = glam::Vec3::new(0.4, 0.8, 0.3).normalize();
    let base_params = TraceParams {
        sun_dir: sun_dir.into(),
        sun_cone: 0.0,
        sun_color: [8.0, 7.6, 7.0],
        ao_radius: 0.3 * scn.radius,
        env_zenith: if args.sun_only { [0.0; 3] } else { [0.35, 0.45, 0.7] },
        shadow_spp: 0,
        env_horizon: if args.sun_only { [0.0; 3] } else { [0.12, 0.10, 0.09] },
        ao_spp: 0,
        gi_spp: 0,
        frame_index: 0,
        trace_size: [args.width, args.height],
        gbuffer_size: [args.width, args.height],
        _pad0: 0,
        _pad1: 0,
    };

    // --- raster_only.png: shadow_spp=ao_spp=gi_spp=0, flat ambient ---
    render_mode(
        &gpu,
        &gbuffer_pipeline,
        &trace_pipelines,
        &positions_buf,
        &normals_buf,
        &material_ids_buf,
        &gpu_materials_buf,
        &rt_materials_buf,
        &mat_index_buf,
        &camera_buf,
        &cam_pos_buf,
        &accel_obj.structure,
        &indices_buf,
        scn.indices.len() as u32,
        args.width,
        args.height,
        args.width,
        args.height,
        &base_params,
        1,
        &args.out.join("raster_only.png"),
        None,
    );

    // --- Modes A / B / C ---
    // (name, gbuf_w, gbuf_h, trace_w, trace_h, shadow_spp, sun_cone, ao_spp, gi_spp)
    struct ModeSpec {
        name: char,
        gbuf_w: u32,
        gbuf_h: u32,
        trace_w: u32,
        trace_h: u32,
        shadow_spp: u32,
        sun_cone: f32,
        ao_spp: u32,
        gi_spp: u32,
    }
    let modes = [
        ModeSpec { name: 'A', gbuf_w: args.width, gbuf_h: args.height, trace_w: args.width, trace_h: args.height, shadow_spp: 1, sun_cone: 0.0, ao_spp: 0, gi_spp: 0 },
        ModeSpec { name: 'B', gbuf_w: args.width, gbuf_h: args.height, trace_w: args.width / 2, trace_h: args.height / 2, shadow_spp: 4, sun_cone: 0.015, ao_spp: 4, gi_spp: 4 },
        ModeSpec { name: 'C', gbuf_w: 2560, gbuf_h: 1440, trace_w: 2560, trace_h: 1440, shadow_spp: 4, sun_cone: 0.015, ao_spp: 4, gi_spp: 4 },
    ];

    for ModeSpec { name, gbuf_w, gbuf_h, trace_w, trace_h, shadow_spp, sun_cone, ao_spp, gi_spp } in modes {
        let mut params = base_params;
        params.trace_size = [trace_w, trace_h];
        params.gbuffer_size = [gbuf_w, gbuf_h];
        params.shadow_spp = shadow_spp;
        params.sun_cone = sun_cone;
        params.ao_spp = ao_spp;
        params.gi_spp = gi_spp;

        let metalfx_target = if name == 'C' { Some((args.width, args.height)) } else { None };
        let (aspect_mode, _) = (gbuf_w as f32 / gbuf_h as f32, ());
        let (cam_mode, eye_mode) = gbuffer::build_camera(scn.center, scn.radius, aspect_mode);
        let camera_buf_mode = gpu.buffer_with_data(&[cam_mode]);
        let cam_pos_buf_mode = gpu.buffer_with_data(&[PaddedVec3 { xyz: eye_mode.into(), _pad: 0.0 }]);

        let timing = render_mode(
            &gpu,
            &gbuffer_pipeline,
            &trace_pipelines,
            &positions_buf,
            &normals_buf,
            &material_ids_buf,
            &gpu_materials_buf,
            &rt_materials_buf,
            &mat_index_buf,
            &camera_buf_mode,
            &cam_pos_buf_mode,
            &accel_obj.structure,
            &indices_buf,
            scn.indices.len() as u32,
            gbuf_w,
            gbuf_h,
            trace_w,
            trace_h,
            &params,
            args.frames,
            &args.out.join(format!("mode_{name}.png")),
            metalfx_target,
        );

        println!(
            "MODE {name} size={}x{} trace={}x{}\nbvh_build_ms={:.3} bvh_refit_ms_avg={:.3}\ngbuffer_ms={:.3} trace_ms={:.3} upsample_ms={:.3} combine_ms={:.3} metalfx_ms={:.3}\nframe_ms_avg={:.3} fps={:.2}",
            if metalfx_target.is_some() { args.width } else { gbuf_w },
            if metalfx_target.is_some() { args.height } else { gbuf_h },
            trace_w,
            trace_h,
            bvh_build_gpu_ms,
            bvh_refit_ms_avg,
            timing.gbuffer_ms,
            timing.trace_ms,
            timing.upsample_ms,
            timing.combine_ms,
            timing.metalfx_ms,
            timing.frame_ms_avg,
            1000.0 / timing.frame_ms_avg,
        );
    }
}

#[derive(Default, Clone, Copy)]
struct ModeTiming {
    gbuffer_ms: f64,
    trace_ms: f64,
    upsample_ms: f64,
    combine_ms: f64,
    metalfx_ms: f64,
    frame_ms_avg: f64,
}

#[allow(clippy::too_many_arguments)]
fn render_mode(
    gpu: &Gpu,
    gbuffer_pipeline: &gbuffer::GBufferPipeline,
    trace_pipelines: &trace::TracePipelines,
    positions_buf: &GpuBuffer,
    normals_buf: &GpuBuffer,
    material_ids_buf: &GpuBuffer,
    gpu_materials_buf: &GpuBuffer,
    rt_materials_buf: &GpuBuffer,
    mat_index_buf: &GpuBuffer,
    camera_buf: &GpuBuffer,
    cam_pos_buf: &GpuBuffer,
    accel_structure: &objc2::runtime::ProtocolObject<dyn objc2_metal::MTLAccelerationStructure>,
    indices_buf: &GpuBuffer,
    index_count: u32,
    gbuf_w: u32,
    gbuf_h: u32,
    trace_w: u32,
    trace_h: u32,
    params_template: &TraceParams,
    frames: u32,
    png_path: &std::path::Path,
    metalfx_target: Option<(u32, u32)>,
) -> ModeTiming {
    let targets = gbuffer::GBufferTargets::new(gpu, gbuf_w, gbuf_h);

    let rw_usage = GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE;
    let sv = gpu.texture(GpuTextureFormat::Rgba16Float, trace_w, trace_h, rw_usage, false, "sv");
    let gi = gpu.texture(GpuTextureFormat::Rgba16Float, trace_w, trace_h, rw_usage, false, "gi");
    let hi_sv = gpu.texture(GpuTextureFormat::Rgba16Float, gbuf_w, gbuf_h, rw_usage, false, "hi_sv");
    let hi_gi = gpu.texture(GpuTextureFormat::Rgba16Float, gbuf_w, gbuf_h, rw_usage, false, "hi_gi");
    // CPU-readable: this is the texture the PNG writer reads back from
    // when there's no MetalFX pass (modes A/B, raster_only).
    let out_hdr = gpu.texture(GpuTextureFormat::Rgba16Float, gbuf_w, gbuf_h, rw_usage, true, "out_hdr");

    let needs_upsample = trace_w != gbuf_w || trace_h != gbuf_h;

    // MTLFXSpatialScaler requires its output texture to have PRIVATE storage
    // mode (asserts otherwise) — so `final_tex` stays private (cpu_readable:
    // false) and a separate CPU-readable staging texture is blitted from it
    // once, after the timed loop, purely for the PNG readback.
    let final_tex = if let Some((fw, fh)) = metalfx_target {
        // MTLFXSpatialScaler's internal upscale write needs RenderTarget
        // usage on the output texture, not just ShaderWrite — caught by
        // MTL_SHADER_VALIDATION's RenderPass Descriptor Validation
        // ("doesn't specify MTLTextureUsageRenderTarget"), matching
        // Apple's own MetalFX sample code (RenderTarget | ShaderWrite on
        // the output texture).
        let metalfx_out_usage = rw_usage | GpuTextureUsage::RENDER_TARGET;
        Some(gpu.texture(GpuTextureFormat::Rgba16Float, fw, fh, metalfx_out_usage, false, "metalfx_out"))
    } else {
        None
    };
    let staging_tex = metalfx_target.map(|(fw, fh)| {
        gpu.texture(GpuTextureFormat::Rgba16Float, fw, fh, GpuTextureUsage::SHADER_READ, true, "metalfx_staging")
    });
    let scaler = metalfx_target.map(|(fw, fh)| {
        MetalFxSpatialScaler::new(
            gpu.device.raw_device(),
            gbuf_w,
            gbuf_h,
            fw,
            fh,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
        )
        .expect("MetalFX unavailable")
    });

    let mut acc = ModeTiming::default();
    let total_iters = WARMUP_FRAMES + frames;
    for i in 0..total_iters {
        let mut params = *params_template;
        params.frame_index = i;
        let params_buf = gpu.buffer_with_data(&[params]);

        let gbuffer_ms = gbuffer_pipeline.render(
            gpu, &targets, positions_buf, normals_buf, material_ids_buf, gpu_materials_buf, camera_buf,
            indices_buf, index_count,
        );

        let trace_ms = trace::dispatch_trace_lighting(
            gpu, &trace_pipelines.trace_lighting, accel_structure, &params_buf, rt_materials_buf, mat_index_buf,
            &targets.g_wpos, &targets.g_nrm, &sv, &gi, trace_w, trace_h,
        );

        let (sv_final, gi_final, upsample_ms) = if needs_upsample {
            let ms = trace::dispatch_upsample_lighting(
                gpu, &trace_pipelines.upsample_lighting, &params_buf, &targets.g_wpos, &targets.g_nrm,
                &sv, &gi, &hi_sv, &hi_gi, gbuf_w, gbuf_h,
            );
            (&hi_sv, &hi_gi, ms)
        } else {
            (&sv, &gi, 0.0)
        };

        let combine_ms = trace::dispatch_shade_combine(
            gpu, &trace_pipelines.shade_combine, &params_buf, &targets.g_wpos, &targets.g_nrm, &targets.g_alb,
            &targets.g_mat, sv_final, gi_final, &out_hdr, cam_pos_buf, gbuf_w, gbuf_h,
        );

        let metalfx_ms = if let (Some(scaler), Some(final_tex)) = (&scaler, &final_tex) {
            let cb = gpu.command_buffer("metalfx");
            scaler.encode(&cb, &out_hdr, final_tex);
            gpu::Gpu::commit_and_time(&cb)
        } else {
            0.0
        };

        if i >= WARMUP_FRAMES {
            acc.gbuffer_ms += gbuffer_ms;
            acc.trace_ms += trace_ms;
            acc.upsample_ms += upsample_ms;
            acc.combine_ms += combine_ms;
            acc.metalfx_ms += metalfx_ms;
        }
    }

    let n = frames as f64;
    acc.gbuffer_ms /= n;
    acc.trace_ms /= n;
    acc.upsample_ms /= n;
    acc.combine_ms /= n;
    acc.metalfx_ms /= n;
    acc.frame_ms_avg = acc.gbuffer_ms + acc.trace_ms + acc.upsample_ms + acc.combine_ms + acc.metalfx_ms;

    let (readback_tex, rw, rh): (&GpuTexture, u32, u32) = match (&final_tex, &staging_tex) {
        (Some(final_tex), Some(staging_tex)) => {
            // One untimed blit — private MetalFX output -> shared staging —
            // purely so the PNG writer can read it back. Not part of the
            // per-frame timed loop or any reported timing bucket.
            let cb = gpu.command_buffer("metalfx-staging-blit");
            let blit = cb.blitCommandEncoder().expect("blitCommandEncoder failed");
            unsafe { blit.copyFromTexture_toTexture(final_tex.raw(), staging_tex.raw()) };
            blit.endEncoding();
            gpu::Gpu::commit_and_time(&cb);
            (staging_tex, metalfx_target.unwrap().0, metalfx_target.unwrap().1)
        }
        _ => (&out_hdr, gbuf_w, gbuf_h),
    };
    let pixels = gpu::Gpu::read_rgba_f32(readback_tex.raw(), rw, rh, 2);
    let mean = tonemap::write_png(png_path, &pixels, rw, rh);
    println!("[png] {png_path:?} mean={mean:.2}");

    acc
}
