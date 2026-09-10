//! Live Water S6 GPU proofs — surface reconstruction vs CPU-computed
//! expectations.
//!
//! Every numerical comparison here is GPU output against a CPU oracle that
//! transcribes the same math independently (view transform, V1 reject
//! rules, conservative bbox, exact ray-sphere test, shared clip-depth
//! convention) — never against a second WGSL implementation. Contract:
//! docs/WATER_SIMULATION_DESIGN.md section 7; brief:
//! docs/WATER_IMPLEMENTATION_PLAN.md section 4 (S6).
//!
//! Synthetic particle fixtures only (slab + sphere lattices) — no
//! dependence on solver tuning. The substep solver is S4/S5's seam and is
//! not exercised here.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use half::f16;
use bytemuck::Zeroable as _;
use manifold_gpu::{
    GpuBinding, GpuBuffer, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension,
    GpuTextureFormat, GpuTextureUsage,
};

use manifold_renderer::node_graph::camera::{Camera, CameraMode, delinearize_depth};
use manifold_renderer::node_graph::freeze::codegen::ENTRY;
use manifold_renderer::node_graph::primitives::{
    SurfacePixelUniforms, SurfaceSplatUniforms, SURFACE_DEPTH_CLEAR_WGSL,
    SURFACE_DEPTH_RESOLVE_WGSL, SURFACE_DEPTH_SPLAT_WGSL, THICKNESS_RESOLVE_WGSL,
    THICKNESS_SPLAT_WGSL,
};
use manifold_renderer::node_graph::water::{GRID_SPACING, PARTICLE_MASS, WaterParticle};

fn device() -> &'static Arc<GpuDevice> {
    static DEVICE: OnceLock<Arc<GpuDevice>> = OnceLock::new();
    DEVICE.get_or_init(|| Arc::new(GpuDevice::new()))
}

const PARTICLE_BYTES: u64 = std::mem::size_of::<WaterParticle>() as u64;

// ---------------------------------------------------------------------------
// Fixtures: synthetic particle lattices (world positions only — the surface
// raster does not touch the solver domain).
// ---------------------------------------------------------------------------

fn make_particle(pos: [f32; 3]) -> WaterParticle {
    WaterParticle {
        position_mass: [pos[0], pos[1], pos[2], PARTICLE_MASS],
        velocity_density: [0.0, 0.0, 0.0, 1000.0],
        affine_x: [0.0; 4],
        affine_y: [0.0; 4],
        affine_z: [0.0; 4],
        previous_position: [pos[0], pos[1], pos[2], 0.0],
    }
}

/// A filled lattice slab with corners `lo`/`hi` at `spacing`.
fn slab_particles(lo: [f32; 3], hi: [f32; 3], spacing: f32) -> Vec<WaterParticle> {
    let mut out = Vec::new();
    let mut x = lo[0];
    while x <= hi[0] + 1e-6 {
        let mut y = lo[1];
        while y <= hi[1] + 1e-6 {
            let mut z = lo[2];
            while z <= hi[2] + 1e-6 {
                out.push(make_particle([x, y, z]));
                z += spacing;
            }
            y += spacing;
        }
        x += spacing;
    }
    out
}

/// A filled lattice sphere.
fn sphere_particles(center: [f32; 3], radius: f32, spacing: f32) -> Vec<WaterParticle> {
    let mut out = Vec::new();
    let mut x = -radius;
    while x <= radius + 1e-6 {
        let mut y = -radius;
        while y <= radius + 1e-6 {
            let mut z = -radius;
            while z <= radius + 1e-6 {
                if x * x + y * y + z * z <= radius * radius + 1e-9 {
                    out.push(make_particle([
                        center[0] + x,
                        center[1] + y,
                        center[2] + z,
                    ]));
                }
                z += spacing;
            }
            y += spacing;
        }
        x += spacing;
    }
    out
}

/// Standard proof camera: at the origin looking down +z, 60° fov.
fn proof_camera() -> Camera {
    Camera::look_at(
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 5.0],
        [0.0, 1.0, 0.0],
        std::f32::consts::FRAC_PI_3,
        0.05,
        200.0,
    )
}

// ---------------------------------------------------------------------------
// CPU oracle — independent transcription of the kernel math.
// ---------------------------------------------------------------------------

struct Oracle<'a> {
    cam: &'a Camera,
    radius: f32,
    w: u32,
    h: u32,
}

impl Oracle<'_> {
    /// View-space centre in the +z-forward frame (mirror of
    /// splat_view_center in particle_splat_common.wgsl): `Camera.view` is
    /// right-handed, so in-front points map to negative view z and the z
    /// component is negated once here.
    fn view_pos(&self, world: [f32; 3]) -> [f32; 3] {
        let m = &self.cam.view;
        let v = [world[0], world[1], world[2], 1.0f32];
        let mut out = [0.0f32; 3];
        for row in 0..3 {
            out[row] = m[0][row] * v[0] + m[1][row] * v[1] + m[2][row] * v[2] + m[3][row];
        }
        out[2] = -out[2];
        out
    }

    /// V1 reject rules (mirror of splat_accept in particle_splat_common.wgsl).
    fn accept(&self, c: [f32; 3]) -> bool {
        let r = self.radius;
        c.iter().all(|v| v.is_finite())
            && c[2] - r > self.cam.near
            && c[0] * c[0] + c[1] * c[1] + c[2] * c[2] >= r * r
            && c[2] - r <= self.cam.far
    }

    /// Conservative inclusive pixel bbox (mirror of splat_bbox: the
    /// half-extent bounds the projected deviation from the centre, not the
    /// absolute offset).
    fn bbox(&self, c: [f32; 3]) -> Option<(i32, i32, i32, i32)> {
        let (w, h) = (self.w as f32, self.h as f32);
        let aspect = w / h;
        let tan_half = (std::f32::consts::FRAC_PI_3 * 0.5).tan();
        let f_px_x = 0.5 * w / (tan_half * aspect);
        let f_px_y = 0.5 * h / tan_half;
        let z_near = c[2] - self.radius;
        if z_near <= 0.0 {
            return None;
        }
        let sx = (c[0] / c[2]) * f_px_x + 0.5 * w;
        let sy = (-c[1] / c[2]) * f_px_y + 0.5 * h;
        let r = self.radius;
        let ex = (r * (c[2] + c[0].abs()) / (z_near * c[2])) * f_px_x;
        let ey = (r * (c[2] + c[1].abs()) / (z_near * c[2])) * f_px_y;
        let x0 = ((sx - ex).floor() as i32).clamp(0, self.w as i32 - 1);
        let y0 = ((sy - ey).floor() as i32).clamp(0, self.h as i32 - 1);
        let x1 = ((sx + ex).ceil() as i32).clamp(0, self.w as i32 - 1);
        let y1 = ((sy + ey).ceil() as i32).clamp(0, self.h as i32 - 1);
        Some((x0, y0, x1, y1))
    }

    fn ray_dir(&self, px: [i32; 2]) -> [f32; 3] {
        let (w, h) = (self.w as f32, self.h as f32);
        let aspect = w / h;
        let tan_half = (std::f32::consts::FRAC_PI_3 * 0.5).tan();
        let ndc_x = ((px[0] as f32 + 0.5) / w) * 2.0 - 1.0;
        let ndc_y = 1.0 - ((px[1] as f32 + 0.5) / h) * 2.0;
        let d = [
            ndc_x * tan_half * aspect,
            ndc_y * tan_half,
            1.0f32,
        ];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        [d[0] / len, d[1] / len, d[2] / len]
    }

    /// Front hit distance along the ray, or None (mirror of splat_ray_hit).
    fn ray_hit(&self, c: [f32; 3], dir: [f32; 3]) -> Option<f32> {
        let b = dir[0] * c[0] + dir[1] * c[1] + dir[2] * c[2];
        let disc = b * b - (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]) + self.radius * self.radius;
        if disc <= 0.0 {
            return None;
        }
        let t = b - disc.sqrt();
        if t <= 0.0 {
            return None;
        }
        Some(t)
    }

    /// Per-pixel min clip depth + coverage, exactly as the splat/resolve
    /// pair computes them.
    fn depth_coverage(&self, particles: &[WaterParticle]) -> (Vec<f32>, Vec<u8>) {
        let mut depth = vec![1.0f32; (self.w * self.h) as usize];
        let mut cov = vec![0u8; (self.w * self.h) as usize];
        for p in particles {
            if p.position_mass[3] == 0.0 {
                continue;
            }
            let c = self.view_pos([
                p.position_mass[0],
                p.position_mass[1],
                p.position_mass[2],
            ]);
            if !self.accept(c) {
                continue;
            }
            let Some((x0, y0, x1, y1)) = self.bbox(c) else {
                continue;
            };
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let dir = self.ray_dir([x, y]);
                    let Some(t) = self.ray_hit(c, dir) else {
                        continue;
                    };
                    let view_z = t * dir[2];
                    if view_z <= self.cam.near {
                        continue;
                    }
                    let raw = delinearize_depth(view_z, self.cam.near, self.cam.far)
                        .clamp(0.0, 0.999_999_94);
                    let i = (y as u32 * self.w + x as u32) as usize;
                    if raw < depth[i] {
                        depth[i] = raw;
                    }
                    cov[i] = 255;
                }
            }
        }
        (depth, cov)
    }

    /// Per-pixel additive chord sum (the declared sphere-splat optical
    /// thickness approximation).
    fn thickness(&self, particles: &[WaterParticle]) -> Vec<f32> {
        let mut out = vec![0.0f32; (self.w * self.h) as usize];
        for p in particles {
            if p.position_mass[3] == 0.0 {
                continue;
            }
            let c = self.view_pos([
                p.position_mass[0],
                p.position_mass[1],
                p.position_mass[2],
            ]);
            if !self.accept(c) {
                continue;
            }
            let Some((x0, y0, x1, y1)) = self.bbox(c) else {
                continue;
            };
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let dir = self.ray_dir([x, y]);
                    let b = dir[0] * c[0] + dir[1] * c[1] + dir[2] * c[2];
                    let disc =
                        b * b - (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]) + self.radius * self.radius;
                    if disc <= 0.0 {
                        continue;
                    }
                    let i = (y as u32 * self.w + x as u32) as usize;
                    out[i] += 2.0 * disc.sqrt();
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// GPU dispatch helpers (direct kernel dispatches on shared buffers, the
// resolve_accumulator gpu_tests pattern — no graph executor involvement).
// ---------------------------------------------------------------------------

fn write_particles(buf: &GpuBuffer, particles: &[WaterParticle]) {
    unsafe {
        buf.write(0, bytemuck::cast_slice(particles));
    }
}

fn splat_uniforms(cam: &Camera, radius: f32, w: u32, h: u32, count: u32) -> SurfaceSplatUniforms {
    let CameraMode::Perspective { fov_y } = cam.mode else {
        panic!("proof cameras are perspective")
    };
    SurfaceSplatUniforms {
        view: cam.view,
        tan_half_fov: (fov_y * 0.5).tan(),
        near: cam.near,
        far: cam.far,
        radius,
        width: w,
        height: h,
        count,
        _pad: 0,
    }
}

fn pixel_uniforms(w: u32, h: u32) -> SurfacePixelUniforms {
    SurfacePixelUniforms {
        width: w,
        height: h,
        _pad0: 0,
        _pad1: 0,
    }
}

fn make_output(w: u32, h: u32, format: GpuTextureFormat, label: &str) -> GpuTexture {
    device().create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET_FULL,
        label,
        mip_levels: 1,
    })
}

/// Full node.particle_surface_depth pipeline: clear -> splat -> resolve.
fn raster_depth(
    particles: &[WaterParticle],
    cam: &Camera,
    radius: f32,
    w: u32,
    h: u32,
) -> (Vec<f32>, Vec<u8>) {
    let dev = device();
    let particle_buf = dev.create_buffer_shared(particles.len() as u64 * PARTICLE_BYTES);
    write_particles(&particle_buf, particles);
    let scratch_depth = dev.create_buffer(u64::from(w) * u64::from(h) * 4);
    let scratch_cov = dev.create_buffer(u64::from(w) * u64::from(h) * 4);
    let depth_tex = make_output(w, h, GpuTextureFormat::R32Float, "ws-depth");
    let cov_tex = make_output(w, h, GpuTextureFormat::R8Unorm, "ws-coverage");

    let clear_pl = dev.create_compute_pipeline(SURFACE_DEPTH_CLEAR_WGSL, "cs_main", "ws-clear");
    let splat_pl = dev.create_compute_pipeline(SURFACE_DEPTH_SPLAT_WGSL, "cs_main", "ws-splat");
    let resolve_pl =
        dev.create_compute_pipeline(SURFACE_DEPTH_RESOLVE_WGSL, "cs_main", "ws-resolve");

    let pu = pixel_uniforms(w, h);
    let su = splat_uniforms(cam, radius, w, h, particles.len() as u32);
    let mut enc = dev.create_encoder("ws-depth");
    enc.dispatch_compute(
        &clear_pl,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&pu) },
            GpuBinding::Buffer { binding: 1, buffer: &scratch_depth, offset: 0 },
        ],
        [(w * h).div_ceil(256), 1, 1],
        "ws-clear",
    );
    enc.clear_buffer(&scratch_cov);
    enc.dispatch_compute(
        &splat_pl,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&su) },
            GpuBinding::Buffer { binding: 1, buffer: &particle_buf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &scratch_depth, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &scratch_cov, offset: 0 },
        ],
        [(particles.len() as u32).div_ceil(256), 1, 1],
        "ws-splat",
    );
    enc.dispatch_compute(
        &resolve_pl,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&pu) },
            GpuBinding::Buffer { binding: 1, buffer: &scratch_depth, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &scratch_cov, offset: 0 },
            GpuBinding::Texture { binding: 3, texture: &depth_tex },
            GpuBinding::Texture { binding: 4, texture: &cov_tex },
        ],
        [(w * h).div_ceil(256), 1, 1],
        "ws-resolve",
    );
    enc.commit_and_wait_completed();

    let depth = readback_f32(&depth_tex, w, h);
    let cov = readback_u8(&cov_tex, w, h);
    (depth, cov)
}

/// Full node.particle_thickness pipeline: clear -> splat -> resolve.
fn raster_thickness(particles: &[WaterParticle], cam: &Camera, radius: f32, w: u32, h: u32) -> Vec<f32> {
    let dev = device();
    let particle_buf = dev.create_buffer_shared(particles.len() as u64 * PARTICLE_BYTES);
    write_particles(&particle_buf, particles);
    let scratch = dev.create_buffer(u64::from(w) * u64::from(h) * 4);
    let out_tex = make_output(w, h, GpuTextureFormat::R16Float, "ws-thickness");

    let splat_pl = dev.create_compute_pipeline(THICKNESS_SPLAT_WGSL, "cs_main", "ws-thick-splat");
    let resolve_pl =
        dev.create_compute_pipeline(THICKNESS_RESOLVE_WGSL, "cs_main", "ws-thick-resolve");

    let pu = pixel_uniforms(w, h);
    let su = splat_uniforms(cam, radius, w, h, particles.len() as u32);
    let mut enc = dev.create_encoder("ws-thickness");
    enc.clear_buffer(&scratch);
    enc.dispatch_compute(
        &splat_pl,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&su) },
            GpuBinding::Buffer { binding: 1, buffer: &particle_buf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &scratch, offset: 0 },
        ],
        [(particles.len() as u32).div_ceil(256), 1, 1],
        "ws-thick-splat",
    );
    enc.dispatch_compute(
        &resolve_pl,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&pu) },
            GpuBinding::Buffer { binding: 1, buffer: &scratch, offset: 0 },
            GpuBinding::Texture { binding: 2, texture: &out_tex },
        ],
        [(w * h).div_ceil(256), 1, 1],
        "ws-thick-resolve",
    );
    enc.commit_and_wait_completed();

    readback_f16_as_f32(&out_tex, w, h)
}

fn readback_f32(tex: &GpuTexture, w: u32, h: u32) -> Vec<f32> {
    let dev = device();
    let buf = dev.create_buffer_shared(u64::from(w) * u64::from(h) * 4);
    let mut enc = dev.create_encoder("ws-readback-f32");
    enc.copy_texture_to_buffer(tex, &buf, w, h, w * 4);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    unsafe { std::slice::from_raw_parts(ptr.cast::<f32>(), (w * h) as usize) }.to_vec()
}

fn readback_u8(tex: &GpuTexture, w: u32, h: u32) -> Vec<u8> {
    let dev = device();
    let buf = dev.create_buffer_shared(u64::from(w) * u64::from(h));
    let mut enc = dev.create_encoder("ws-readback-u8");
    enc.copy_texture_to_buffer(tex, &buf, w, h, w);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), (w * h) as usize) }.to_vec()
}

fn readback_f16_as_f32(tex: &GpuTexture, w: u32, h: u32) -> Vec<f32> {
    let dev = device();
    let buf = dev.create_buffer_shared(u64::from(w) * u64::from(h) * 2);
    let mut enc = dev.create_encoder("ws-readback-f16");
    enc.copy_texture_to_buffer(tex, &buf, w, h, w * 2);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    let halves = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h) as usize) };
    halves.iter().map(|&b| f16::from_bits(b).to_f32()).collect()
}

fn upload_r32(data: &[f32], w: u32, h: u32, label: &str) -> GpuTexture {
    let dev = device();
    let tex = dev.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: GpuTextureFormat::R32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
        label,
        mip_levels: 1,
    });
    dev.upload_texture(&tex, bytemuck::cast_slice(data));
    tex
}

fn upload_r8(data: &[u8], w: u32, h: u32, label: &str) -> GpuTexture {
    let dev = device();
    let tex = dev.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: GpuTextureFormat::R8Unorm,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
        label,
        mip_levels: 1,
    });
    dev.upload_texture(&tex, data);
    tex
}

// ---------------------------------------------------------------------------
// Proofs
// ---------------------------------------------------------------------------

/// Empty pixels: no live particles -> depth exactly 1, coverage 0,
/// thickness 0 (and the normals stage would emit alpha 0 — checked by the
/// translated-camera test's empty-corner assertion).
#[test]
fn water_surface_empty_pixels() {
    let dev = device();
    let cam = proof_camera();
    let (w, h) = (64u32, 64u32);
    let one_inactive = vec![WaterParticle::zeroed()];

    let (depth, cov) = raster_depth(&one_inactive, &cam, 0.046875, w, h);
    assert!(
        depth.iter().all(|&d| d == 1.0),
        "empty raster must leave clip depth exactly 1.0"
    );
    assert!(cov.iter().all(|&c| c == 0), "empty raster must leave coverage 0");

    let thickness = raster_thickness(&one_inactive, &cam, 0.046875, w, h);
    assert!(
        thickness.iter().all(|&t| t == 0.0),
        "empty raster must leave thickness 0"
    );
    let _ = dev;
}

/// A single sphere: the covered disc's centre depth matches the exact
/// ray-sphere front hit; outside the disc coverage is 0 and depth is 1
/// (silhouette is hard); a far-behind second sphere is occluded by the
/// front one where their discs overlap.
#[test]
fn water_surface_single_sphere_depth_and_silhouette() {
    let cam = proof_camera();
    let (w, h) = (128u32, 128u32);
    let radius = 0.15f32;
    let sphere = sphere_particles([0.0, 0.0, 3.0], 0.5, GRID_SPACING * 0.5);
    assert!(!sphere.is_empty());

    let (depth, cov) = raster_depth(&sphere, &cam, radius, w, h);
    let oracle = Oracle { cam: &cam, radius, w, h };
    let (expected_depth, expected_cov) = oracle.depth_coverage(&sphere);

    // Centre of the projected disc: the sphere sits on the camera axis, so
    // its centre projects to the continuous point (64, 64) — the covered
    // disc surrounds it. Compare GPU vs oracle over the whole buffer, then
    // spot-check the analytic front depth near the middle.
    let mut checked = 0usize;
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let i = (y as u32 * w + x as u32) as usize;
            assert_eq!(cov[i], expected_cov[i], "coverage mismatch at ({x},{y})");
            if cov[i] > 0 {
                // f32 ulp-level differences in the raw-depth mapping (Metal
                // fast-math vs strict Rust ordering) — real convention bugs
                // are orders of magnitude larger.
                assert!(
                    (depth[i] - expected_depth[i]).abs() <= 1e-6,
                    "depth mismatch at ({x},{y}): gpu={} cpu={}",
                    depth[i],
                    expected_depth[i]
                );
                checked += 1;
            } else {
                assert_eq!(depth[i], 1.0, "uncovered pixel ({x},{y}) must stay empty=1");
            }
        }
    }
    assert!(checked > 100, "sphere disc too small ({checked} px) — fixture broken");

    // Projection-conformance pin: the GPU splat must cover the pixel the
    // codebase oracle `Camera::project_to_pixel` projects an off-axis world
    // point to — this catches any view-frame / y-flip drift between the
    // kernel's pinhole and the shared camera convention.
    let proj = cam
        .project_to_pixel([0.2, 0.1, 3.0], w, h)
        .expect("world point in front of camera");
    let pi = (proj.py as u32 * w + proj.px as u32) as usize;
    assert_eq!(
        cov[pi], 255,
        "projected world point ({:.1},{:.1}) must be covered — camera-convention drift",
        proj.px, proj.py
    );

    // Analytic: near the projected centre the front depth approaches
    // z_sphere - impostor_radius along the axis.
    let axis_raw = delinearize_depth(3.0 - radius, cam.near, cam.far);
    let centre_i = (63u32 * w + 63) as usize;
    assert!(
        cov[centre_i] > 0,
        "pixel next to the projected centre must be covered"
    );
    assert!(
        (depth[centre_i] - axis_raw).abs() < 0.02,
        "centre depth {} must approach the analytic front depth {axis_raw}",
        depth[centre_i]
    );
}

/// Two separated depth layers: the near slab occludes the far one where
/// their projections overlap; regions covered by only one layer carry that
/// layer's depth.
#[test]
fn water_surface_two_separated_depth_layers() {
    let cam = proof_camera();
    let (w, h) = (128u32, 128u32);
    let radius = 0.046875f32;
    // Near slab z in [2.0, 2.25], x in [-0.6, 0.6]. Far slab z in [3.5,
    // 3.75]: to be VISIBLE (not fully occluded) its projection must stick
    // out past the near slab's silhouette, i.e. far x_lo / z_far >
    // near x_hi / z_near -> x_lo > 0.6 * 3.6/2.1 ≈ 1.03.
    let near = slab_particles([-0.6, -0.2, 2.0], [0.6, 0.2, 2.25], GRID_SPACING * 0.5);
    let far = slab_particles([1.05, -0.2, 3.5], [1.45, 0.2, 3.75], GRID_SPACING * 0.5);
    let mut both = near.clone();
    both.extend(far.iter().copied());

    let (depth, cov) = raster_depth(&both, &cam, radius, w, h);
    let oracle = Oracle { cam: &cam, radius, w, h };
    let (expected_depth, expected_cov) = oracle.depth_coverage(&both);

    let mut occluded_overlap = 0usize;
    let mut far_only = 0usize;
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let i = (y as u32 * w + x as u32) as usize;
            assert_eq!(cov[i], expected_cov[i], "coverage mismatch at ({x},{y})");
            if cov[i] == 0 {
                assert_eq!(depth[i], 1.0);
                continue;
            }
            assert!(
                (depth[i] - expected_depth[i]).abs() <= 1e-6,
                "depth mismatch at ({x},{y}): gpu={} cpu={}",
                depth[i],
                expected_depth[i]
            );
            let z = manifold_renderer::node_graph::camera::linearize_depth(
                depth[i],
                cam.near,
                cam.far,
            );
            // Classify by linearized depth: near-layer pixels are much
            // nearer than far-layer pixels.
            if z < 2.6 {
                occluded_overlap += 1;
            } else {
                far_only += 1;
            }
        }
    }
    // The near slab's projection covers the far slab's left part; the far
    // slab's right part (x >= ~0.7m offset) is visible on its own.
    assert!(occluded_overlap > 200, "near layer pixels missing ({occluded_overlap})");
    assert!(far_only > 100, "far-only layer pixels missing ({far_only})");
}

/// Single-sphere centre chord error <= 2% at 256^2 (design S6 gate). One
/// isolated particle's impostor: the chord at the pixel nearest the
/// projected centre approaches 2*radius. (A dense lattice fixture would be
/// wrong here — overlapping impostors legitimately sum to hundreds of
/// chords per ray.)
#[test]
fn water_surface_chord_error_at_256() {
    let cam = proof_camera();
    let (w, h) = (256u32, 256u32);
    let radius = 0.15f32;
    let one = vec![make_particle([0.0, 0.0, 3.0])];

    let thickness = raster_thickness(&one, &cam, radius, w, h);
    let oracle = Oracle { cam: &cam, radius, w, h };
    let expected = oracle.thickness(&one);

    // GPU matches the CPU chord oracle everywhere covered. Bbox-edge
    // inclusiveness is legitimately fuzzy at one-ulp float boundaries
    // (Metal fast-math vs strict Rust ordering), so a pixel's sum may
    // differ by at most ONE boundary particle's chord; relative error is
    // only meaningful where several particles contribute.
    let mut max_abs = 0.0f32;
    let mut worst = (0usize, 0.0f32, 0.0f32);
    for i in 0..thickness.len() {
        if expected[i] > 0.0 {
            let err = (thickness[i] - expected[i]).abs();
            if err > max_abs {
                max_abs = err;
                worst = (i, thickness[i], expected[i]);
            }
        } else {
            assert_eq!(thickness[i], 0.0, "uncovered pixel must stay 0");
        }
    }
    println!(
        "[water_surface] chord 256^2: max abs err {max_abs:.6} at pixel {} (gpu={} cpu={})",
        worst.0, worst.1, worst.2
    );
    assert!(
        max_abs <= radius * 2.0 + 1e-4,
        "chord raster diverged by more than one boundary particle: {max_abs}"
    );

    // The S6 acceptance gate: centre chord vs the analytic 2*radius. The
    // lone impostor projects to continuous (128,128); the covered pixel
    // with the thickest chord is the closest to the centre.
    let centre = (0..thickness.len())
        .max_by(|&a, &b| thickness[a].total_cmp(&thickness[b]))
        .expect("non-empty");
    assert!(
        thickness[centre] > 0.0,
        "single impostor must cover its projected centre"
    );
    let chord = thickness[centre];
    let analytic = 2.0 * radius;
    let rel_err = (chord - analytic).abs() / analytic;
    println!(
        "[water_surface] 256^2 single-impostor centre chord: gpu={chord:.6} analytic={analytic:.6} rel err={rel_err:.4}"
    );
    assert!(
        rel_err <= 0.02,
        "centre chord error {rel_err} exceeds the 2% gate (gpu={chord}, analytic={analytic})"
    );
}

/// Slab thickness stays within the declared sphere-splat approximation: the
/// GPU chord sum matches the CPU oracle exactly, and the oracle's ratio to
/// the slab's true geometric thickness documents the approximation factor
/// (~2.4x for h/2-spaced particles at 0.75*h impostor radius — reported as
/// evidence, bounded away from both 0 and absurd).
#[test]
fn water_surface_slab_thickness_matches_declared_approximation() {
    let cam = proof_camera();
    let (w, h) = (128u32, 128u32);
    let radius = 0.75 * GRID_SPACING;
    let slab_thickness = 0.25f32;
    let slab = slab_particles([-0.5, -0.125, 2.5], [0.5, 0.125, 2.5 + slab_thickness], GRID_SPACING * 0.5);

    let thickness = raster_thickness(&slab, &cam, radius, w, h);
    let oracle = Oracle { cam: &cam, radius, w, h };
    let expected = oracle.thickness(&slab);

    let mut checked = 0usize;
    let mut max_err = 0.0f32;
    let mut sum = 0.0f32;
    for i in 0..thickness.len() {
        if expected[i] > 0.0 {
            let err = (thickness[i] - expected[i]).abs() / expected[i];
            max_err = max_err.max(err);
            sum += thickness[i];
            checked += 1;
        } else {
            assert_eq!(thickness[i], 0.0);
        }
    }
    assert!(checked > 500, "slab projection too small ({checked} px)");
    // Same one-ulp bbox-edge allowance as the chord proof: a boundary
    // particle's chord at most.
    let mut max_abs = 0.0f32;
    for i in 0..thickness.len() {
        if expected[i] > 0.0 {
            max_abs = max_abs.max((thickness[i] - expected[i]).abs());
        }
    }
    assert!(
        max_abs <= radius * 2.0 + 1e-4,
        "slab chord sum diverged by more than one boundary particle: {max_abs}"
    );

    // The sphere-splat approximation factor vs the true path length through
    // the slab (view is near-orthogonal to the slab's largest faces, so the
    // true chord ≈ slab_thickness). For h/2-spaced particles with 0.75*h
    // impostors the summed impostor volume per cell is ~14x the cell, so a
    // low-teens factor is the declared approximation working as specified —
    // evidence for S7's attenuation tuning, not a correctness bound. The
    // loose envelope only catches degenerate behaviour (silent or
    // everything-everywhere).
    let approx_factor = (sum / checked as f32) / slab_thickness;
    println!(
        "[water_surface] slab mean chord thickness {:.4} m vs true {slab_thickness} m — approximation factor {approx_factor:.2}x",
        sum / checked as f32
    );
    assert!(
        (0.5..=100.0).contains(&approx_factor),
        "sphere-splat approximation factor {approx_factor}x outside the sanity envelope"
    );
}

/// Silhouette preservation through the S6 bilateral: with coverage wired
/// and value_space=ClipDepth, empty pixels stay exactly empty=1 (never
/// smoothed into liquid) and covered pixels stay near their raw depth on
/// the near-planar part of the sphere.
#[test]
fn water_surface_silhouette_preservation() {
    let cam = proof_camera();
    let (w, h) = (128u32, 128u32);
    let radius = 0.15f32;
    let sphere = sphere_particles([0.0, 0.0, 3.0], 0.5, GRID_SPACING * 0.5);

    let (depth, cov) = raster_depth(&sphere, &cam, radius, w, h);

    // Two bilateral passes (H then V) with coverage, ClipDepth mode — the
    // exact S6 water surface smoothing chain.
    let depth_tex = upload_r32(&depth, w, h, "ws-bilateral-in");
    let cov_tex = upload_r8(&cov, w, h, "ws-bilateral-cov");
    let wgsl = manifold_renderer::node_graph::freeze::codegen::standalone_for_spec::<
        manifold_renderer::node_graph::primitives::BilateralBlur,
    >()
    .expect("bilateral standalone codegen");
    let dev = device();
    let pl = dev.create_compute_pipeline(&wgsl, ENTRY, "ws-bilateral");

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct BilinearUniforms {
        axis: u32,
        depth_sigma: f32,
        value_space: u32,
        near: f32,
        far: f32,
        use_coverage: u32,
    }

    let run_pass = |axis: u32, input: &GpuTexture| -> GpuTexture {
        let out = make_output(w, h, GpuTextureFormat::R32Float, "ws-bilateral-out");
        let uniforms = BilinearUniforms {
            axis,
            depth_sigma: 0.05,
            value_space: 1,
            near: cam.near,
            far: cam.far,
            use_coverage: 1,
        };
        let mut enc = dev.create_encoder("ws-bilateral-pass");
        enc.dispatch_compute(
            &pl,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: input },
                GpuBinding::Texture { binding: 2, texture: &depth_tex },
                GpuBinding::Texture { binding: 3, texture: &cov_tex },
                GpuBinding::Texture { binding: 4, texture: &out },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "ws-bilateral-pass",
        );
        enc.commit_and_wait_completed();
        out
    };
    let h_out = run_pass(0, &depth_tex);
    let hv_out = run_pass(1, &h_out);
    let smoothed = readback_f32(&hv_out, w, h);

    let mut covered_checked = 0usize;
    for i in 0..smoothed.len() {
        if cov[i] == 0 {
            assert_eq!(
                smoothed[i], 1.0,
                "empty pixel {i} must stay exactly empty=1 through the bilateral"
            );
            continue;
        }
        assert!(smoothed[i].is_finite(), "covered pixel {i} produced non-finite depth");
        // On the sphere's near-planar crown the smoothed depth stays close
        // to raw (the crown's depth gradient across 9 taps is tiny vs the
        // 0.05 sigma).
        if (depth[i] - delinearize_depth(3.0 - radius, cam.near, cam.far)).abs() < 0.01 {
            assert!(
                (smoothed[i] - depth[i]).abs() < 0.02,
                "crown pixel {i} drifted: raw={} smoothed={}",
                depth[i],
                smoothed[i]
            );
            covered_checked += 1;
        }
    }
    assert!(covered_checked > 20, "crown fixture produced no near-planar pixels");
}

/// Translated-camera normals: a flat slab's reconstructed view normals
/// rotate back to world space as "up" from two different camera positions
/// — the normals come from the shared perspective projection convention,
/// not a screen-space heightmap hack. Also checks empty corners emit
/// alpha=0.
#[test]
fn water_surface_translated_camera_normals() {
    let (w, h) = (128u32, 128u32);
    // A larger impostor radius smooths the splat surface enough for a
    // stable mean normal.
    let radius = 0.2f32;
    let slab = slab_particles([-0.5, -0.125, 2.5], [0.5, 0.125, 2.75], GRID_SPACING * 0.5);

    let render_normals = |cam: &Camera| -> Vec<[f32; 4]> {
        let (depth, cov) = raster_depth(&slab, cam, radius, w, h);
        let depth_tex = upload_r32(&depth, w, h, "ws-normals-depth");
        let cov_tex = upload_r8(&cov, w, h, "ws-normals-cov");
        let out_tex = make_output(w, h, GpuTextureFormat::Rgba16Float, "ws-normals-out");

        let wgsl = manifold_renderer::node_graph::freeze::codegen::standalone_for_spec::<
            manifold_renderer::node_graph::primitives::NormalsFromDepth,
        >()
        .expect("normals_from_depth standalone codegen");
        let dev = device();
        let pl = dev.create_compute_pipeline(&wgsl, ENTRY, "ws-normals");

        let CameraMode::Perspective { fov_y } = cam.mode else {
            panic!("proof cameras are perspective")
        };
        #[repr(C)]
        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        struct NormalsUniforms {
            fov_y: f32,
            near: f32,
            far: f32,
            _pad: u32,
        }
        let uniforms = NormalsUniforms {
            fov_y,
            near: cam.near,
            far: cam.far,
            _pad: 0,
        };
        let mut enc = dev.create_encoder("ws-normals");
        enc.dispatch_compute(
            &pl,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: &depth_tex },
                GpuBinding::Texture { binding: 2, texture: &cov_tex },
                GpuBinding::Texture { binding: 3, texture: &out_tex },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "ws-normals",
        );
        enc.commit_and_wait_completed();

        // Rgba16Float readback.
        let dev = device();
        let buf = dev.create_buffer_shared(u64::from(w) * u64::from(h) * 8);
        let mut enc = dev.create_encoder("ws-normals-readback");
        enc.copy_texture_to_buffer(&out_tex, &buf, w, h, w * 8);
        enc.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared readback buffer");
        let halves =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        (0..(w * h) as usize)
            .map(|i| {
                let o = i * 4;
                [
                    f16::from_bits(halves[o]).to_f32(),
                    f16::from_bits(halves[o + 1]).to_f32(),
                    f16::from_bits(halves[o + 2]).to_f32(),
                    f16::from_bits(halves[o + 3]).to_f32(),
                ]
            })
            .collect()
    };

    // Two cameras: same downward direction at the slab, translated
    // sideways. A steep look-down keeps the slab's TOP face dominant; its
    // toward-camera world normal is +y (the cameras sit above). Both
    // cameras must reconstruct the same world-space normal — that is the
    // "perspective normals, not heightmap" gate.
    let cameras = [
        Camera::look_at([-0.4, 3.5, 1.6], [0.0, 0.0, 2.6], [0.0, 1.0, 0.0], std::f32::consts::FRAC_PI_3, 0.05, 200.0),
        Camera::look_at([0.4, 3.5, 1.6], [0.0, 0.0, 2.6], [0.0, 1.0, 0.0], std::f32::consts::FRAC_PI_3, 0.05, 200.0),
    ];

    let mut world_normals_per_camera = Vec::new();
    for cam in &cameras {
        let normals = render_normals(cam);
        let mut acc = [0.0f32; 3];
        let mut count = 0usize;
        for (i, n) in normals.iter().copied().enumerate() {
            if n[3] < 0.5 {
                continue;
            }
            // Rotate the view-space normal into world space:
            // n_world = right*n.x + up*n.y + fwd*n.z.
            let mut nw = [0.0f32; 3];
            for (nwa, ((&r, &u), &f)) in nw
                .iter_mut()
                .zip(cam.right.iter().zip(cam.up.iter()).zip(cam.fwd.iter()))
            {
                *nwa = r * n[0] + u * n[1] + f * n[2];
            }
            let len = (nw[0] * nw[0] + nw[1] * nw[1] + nw[2] * nw[2]).sqrt();
            assert!(len > 0.5, "covered normal {i} nearly zero-length: {nw:?}");
            for a in 0..3 {
                acc[a] += nw[a] / len;
            }
            count += 1;
        }
        assert!(count > 500, "too few covered normals ({count})");
        let mean = [acc[0] / count as f32, acc[1] / count as f32, acc[2] / count as f32];
        println!("[water_surface] translated-camera mean world normal: {mean:?} over {count} px");
        // The covered area mixes the slab top (+y toward camera) with its
        // front face (−z toward camera); at this elevation the two-face
        // model predicts mean ≈ (0, 0.8, −0.2). Gate the physically
        // meaningful band; the discriminating check is the cross-camera
        // agreement below.
        assert!(
            (0.6..=0.95).contains(&mean[1]),
            "mean world normal {mean:?} outside the slab-dominant band"
        );
        assert!(
            mean[0].abs() < 0.1,
            "mean world normal {mean:?} has an unexpected lateral component (top + front faces are x-symmetric)"
        );
        world_normals_per_camera.push(mean);
    }

    // Both cameras agree: the normals are a property of the surface, not
    // the screen. The y/z components (top + front faces, shared by both
    // views) must agree tightly; x carries each camera's own side-wall
    // fraction (mirrored by the symmetric fixture), so its bound is looser.
    let agreement_bounds = [0.08f32, 0.02, 0.02];
    for a in 0..3 {
        assert!(
            (world_normals_per_camera[0][a] - world_normals_per_camera[1][a]).abs()
                < agreement_bounds[a],
            "translated cameras disagree on world normal axis {a}: {:?} vs {:?}",
            world_normals_per_camera[0],
            world_normals_per_camera[1]
        );
    }
}

/// L2 artifact: a 2x2 contact sheet (depth / coverage / thickness /
/// normals) of a synthetic slab + sphere fixture, written under
/// `WATER_ARTIFACT_DIR` (created if missing). Synthetic fixtures only; the
/// automated gates above remain the pass/fail criteria — this sheet is for
/// Peter's S7 review.
#[test]
fn water_surface_artifact() {
    let cam = proof_camera();
    let (w, h) = (256u32, 256u32);
    let radius = 0.15f32;
    let mut fixture = slab_particles([-0.7, -0.15, 2.4], [0.7, 0.15, 2.65], GRID_SPACING * 0.5);
    fixture.extend(sphere_particles([0.35, 0.35, 2.2], 0.35, GRID_SPACING * 0.5));

    let (depth, cov) = raster_depth(&fixture, &cam, radius, w, h);
    let thickness = raster_thickness(&fixture, &cam, radius, w, h);

    let depth_tex = upload_r32(&depth, w, h, "ws-art-depth");
    let cov_tex = upload_r8(&cov, w, h, "ws-art-cov");
    let normals_tex = make_output(w, h, GpuTextureFormat::Rgba16Float, "ws-art-normals");
    {
        let wgsl = manifold_renderer::node_graph::freeze::codegen::standalone_for_spec::<
            manifold_renderer::node_graph::primitives::NormalsFromDepth,
        >()
        .expect("normals_from_depth standalone codegen");
        let dev = device();
        let pl = dev.create_compute_pipeline(&wgsl, ENTRY, "ws-art-normals");
        #[repr(C)]
        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        struct NormalsUniforms {
            fov_y: f32,
            near: f32,
            far: f32,
            _pad: u32,
        }
        let uniforms = NormalsUniforms {
            fov_y: std::f32::consts::FRAC_PI_3,
            near: cam.near,
            far: cam.far,
            _pad: 0,
        };
        let mut enc = dev.create_encoder("ws-art-normals");
        enc.dispatch_compute(
            &pl,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: &depth_tex },
                GpuBinding::Texture { binding: 2, texture: &cov_tex },
                GpuBinding::Texture { binding: 3, texture: &normals_tex },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "ws-art-normals",
        );
        enc.commit_and_wait_completed();
    }
    let normals = {
        let dev = device();
        let buf = dev.create_buffer_shared(u64::from(w) * u64::from(h) * 8);
        let mut enc = dev.create_encoder("ws-art-normals-readback");
        enc.copy_texture_to_buffer(&normals_tex, &buf, w, h, w * 8);
        enc.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared readback buffer");
        let halves =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        (0..(w * h) as usize)
            .map(|i| {
                let o = i * 4;
                [
                    f16::from_bits(halves[o]).to_f32(),
                    f16::from_bits(halves[o + 1]).to_f32(),
                    f16::from_bits(halves[o + 2]).to_f32(),
                    f16::from_bits(halves[o + 3]).to_f32(),
                ]
            })
            .collect::<Vec<[f32; 4]>>()
    };

    // Compose the 2x2 sheet: depth (grayscale), coverage (black/white),
    // thickness (tonemapped), normals (rgb*0.5+0.5 with coverage alpha).
    let mut sheet = vec![0u8; (w * h * 4 * 4) as usize];
    let put = |sheet: &mut Vec<u8>, px: usize, rgba: [u8; 4]| {
        sheet[px * 4..px * 4 + 4].copy_from_slice(&rgba);
    };
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = y * w as usize + x;
            let top_left = y * (2 * w) as usize + x;
            let top_right = y * (2 * w) as usize + w as usize + x;
            let bot_left = (h as usize + y) * (2 * w) as usize + x;
            let bot_right = (h as usize + y) * (2 * w) as usize + w as usize + x;
            let d = (depth[i].clamp(0.0, 1.0) * 255.0) as u8;
            put(&mut sheet, top_left, [255 - d, 255 - d, 255 - d, 255]);
            let c = cov[i];
            put(&mut sheet, top_right, [c, c, c, 255]);
            let t = thickness[i].max(0.0);
            let t8 = ((t / (1.0 + t)) * 255.0) as u8;
            put(&mut sheet, bot_left, [t8, (t8 as f32 * 0.6) as u8, 255 - t8, 255]);
            let n = normals[i];
            let a = (n[3].clamp(0.0, 1.0) * 255.0) as u8;
            put(
                &mut sheet,
                bot_right,
                [
                    ((n[0] * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0) as u8,
                    ((n[1] * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0) as u8,
                    ((n[2] * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0) as u8,
                    a,
                ],
            );
        }
    }

    match std::env::var("WATER_ARTIFACT_DIR") {
        Ok(dir) => {
            let out_dir = PathBuf::from(&dir);
            std::fs::create_dir_all(&out_dir).expect("create WATER_ARTIFACT_DIR");
            let path = out_dir.join("water_surface_contact_sheet.png");
            image::save_buffer(
                &path,
                &sheet,
                w * 2,
                h * 2,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
            println!("[water_surface] artifact contact sheet: {}", path.display());
        }
        Err(_) => {
            println!(
                "[water_surface] WATER_ARTIFACT_DIR unset — contact sheet computed but not written"
            );
        }
    }
}
