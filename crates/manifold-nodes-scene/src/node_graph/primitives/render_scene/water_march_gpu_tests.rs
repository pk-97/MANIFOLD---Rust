//! GPU proofs for the water level-set march: a sphere level set on a
//! non-cubic lattice under a rotated, scaled, translated model, against
//! analytic ray-sphere answers.
use super::water_march::{FieldDraw, FieldView, WaterMarch};
use super::{mat4_inverse, model_matrix};
use half::f16;
use manifold_gpu::*;

pub(super) type M4 = [[f32; 4]; 4];

pub(super) const W: u32 = 96;
pub(super) const H: u32 = 72;
pub(super) const NODES: [u32; 3] = [37, 33, 41];
pub(super) const SIZE: [f32; 3] = [1.8, 1.6, 2.0];
pub(super) const CENTER: [f32; 3] = [0.1, -0.05, 0.2];
const SCALE: f32 = 1.25;
const RADIUS: f32 = 0.5;

fn mul(a: M4, b: M4) -> M4 {
    let mut out = [[0.0f32; 4]; 4];
    for c in 0..4 {
        for r in 0..4 {
            out[c][r] = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

pub(super) fn mul_point(m: M4, p: [f32; 3], w: f32) -> [f32; 4] {
    let v = [p[0], p[1], p[2], w];
    let mut out = [0.0f32; 4];
    for (r, o) in out.iter_mut().enumerate() {
        *o = (0..4).map(|k| m[k][r] * v[k]).sum();
    }
    out
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt();
    [a[0] / l, a[1] / l, a[2] / l]
}

/// Reverse-Z perspective (infinite far, raw depth = near / distance), looking
/// from `eye` at `target`, column-major.
fn camera(eye: [f32; 3], target: [f32; 3], fov_y_deg: f32, aspect: f32) -> M4 {
    let near = 0.1f32;
    let f = norm(sub(target, eye));
    let r = norm(cross(f, [0.0, 1.0, 0.0]));
    let u = cross(r, f);
    let view: M4 = [
        [r[0], u[0], -f[0], 0.0],
        [r[1], u[1], -f[1], 0.0],
        [r[2], u[2], -f[2], 0.0],
        [-dot(r, eye), -dot(u, eye), dot(f, eye), 1.0],
    ];
    let t = 1.0 / (fov_y_deg.to_radians() * 0.5).tan();
    let proj: M4 = [
        [t / aspect, 0.0, 0.0, 0.0],
        [0.0, t, 0.0, 0.0],
        [0.0, 0.0, 0.0, -1.0],
        [0.0, 0.0, near, 0.0],
    ];
    mul(proj, view)
}

pub(super) fn model() -> M4 {
    model_matrix(
        [0.3, 0.2, -0.4],
        [0.0, 20.0f32.to_radians(), 0.0],
        [SCALE; 3],
    )
}

pub(super) fn cell() -> f32 {
    (SIZE[0] / (NODES[0] - 1) as f32)
        .min(SIZE[1] / (NODES[1] - 1) as f32)
        .min(SIZE[2] / (NODES[2] - 1) as f32)
}

/// Sphere level set at `CENTER + offset`: distance minus radius, clamped to +-4 cells.
fn sphere_levelset(offset: [f32; 3], radius: f32) -> Vec<f32> {
    let c = [
        CENTER[0] + offset[0],
        CENTER[1] + offset[1],
        CENTER[2] + offset[2],
    ];
    let spacing = [
        SIZE[0] / (NODES[0] - 1) as f32,
        SIZE[1] / (NODES[1] - 1) as f32,
        SIZE[2] / (NODES[2] - 1) as f32,
    ];
    let min = [
        CENTER[0] - SIZE[0] * 0.5,
        CENTER[1] - SIZE[1] * 0.5,
        CENTER[2] - SIZE[2] * 0.5,
    ];
    let cell = cell();
    let mut out = Vec::with_capacity((NODES[0] * NODES[1] * NODES[2]) as usize);
    for z in 0..NODES[2] {
        for y in 0..NODES[1] {
            for x in 0..NODES[0] {
                let p = [
                    min[0] + x as f32 * spacing[0],
                    min[1] + y as f32 * spacing[1],
                    min[2] + z as f32 * spacing[2],
                ];
                let d = dot(sub(p, c), sub(p, c)).sqrt() - radius;
                out.push(d.clamp(-4.0 * cell, 4.0 * cell));
            }
        }
    }
    out
}

pub(super) fn shared_buffer(device: &GpuDevice, data: &[f32]) -> GpuBuffer {
    let buf = device.create_buffer_shared((data.len() * 4) as u64);
    unsafe { buf.write(0, bytemuck::cast_slice(data)) };
    buf
}

struct Hit {
    depth: f32,
    normal: [f32; 3],
    thickness: f32,
    slot_plus_one: u32,
}

fn oct_decode(e: [f32; 2]) -> [f32; 3] {
    let mut n = [e[0], e[1], 1.0 - e[0].abs() - e[1].abs()];
    let t = (-n[2]).max(0.0);
    n[0] += if n[0] >= 0.0 { -t } else { t };
    n[1] += if n[1] >= 0.0 { -t } else { t };
    norm(n)
}

fn unpack_hit(words: [u32; 3]) -> Hit {
    let snorm = |bits: u16| (f32::from(bits as i16) / 32767.0).max(-1.0);
    let nx = snorm((words[1] & 0xffff) as u16);
    let ny = snorm((words[1] >> 16) as u16);
    Hit {
        depth: f32::from_bits(words[0]),
        normal: oct_decode([nx, ny]),
        thickness: f16::from_bits((words[2] & 0xffff) as u16).to_f32(),
        slot_plus_one: f16::from_bits((words[2] >> 16) as u16).to_f32() as u32,
    }
}

pub(super) struct Scene {
    pub(super) view_proj: M4,
    pub(super) inv: M4,
}

pub(super) fn scene() -> Scene {
    let m = model();
    let sphere_local = [CENTER[0] + 0.05, CENTER[1], CENTER[2] - 0.1];
    let c = mul_point(m, sphere_local, 1.0);
    let view_proj = camera(
        [0.3, 0.3, 4.0],
        [c[0], c[1], c[2]],
        40.0,
        W as f32 / H as f32,
    );
    Scene {
        view_proj,
        inv: mat4_inverse(view_proj).expect("camera is invertible"),
    }
}

fn world_sphere_center() -> [f32; 3] {
    let c = mul_point(model(), [CENTER[0] + 0.05, CENTER[1], CENTER[2] - 0.1], 1.0);
    [c[0], c[1], c[2]]
}

fn unproject(inv: M4, ndc: [f32; 2], depth: f32) -> [f32; 3] {
    let w = mul_point(inv, [ndc[0], ndc[1], depth], 1.0);
    [w[0] / w[3], w[1] / w[3], w[2] / w[3]]
}

fn pixel_ndc(x: u32, y: u32) -> [f32; 2] {
    let u = (x as f32 + 0.5) / W as f32;
    let v = (y as f32 + 0.5) / H as f32;
    [u * 2.0 - 1.0, 1.0 - v * 2.0]
}

/// The same ray the shader builds: two finite points of the pixel's clip line.
pub(super) fn pixel_ray(inv: M4, x: u32, y: u32) -> ([f32; 3], [f32; 3]) {
    let ndc = pixel_ndc(x, y);
    let o = unproject(inv, ndc, 1.0);
    let far = unproject(inv, ndc, 0.5);
    (o, norm(sub(far, o)))
}

/// Closest-approach distance to the sphere and, when it is crossed, the near
/// and far ray parameters.
fn analytic(o: [f32; 3], d: [f32; 3], c: [f32; 3], r: f32) -> (f32, Option<(f32, f32)>) {
    let oc = sub(o, c);
    let b = dot(oc, d);
    let dmin2 = dot(oc, oc) - b * b;
    let dmin = dmin2.max(0.0).sqrt();
    if dmin < r {
        let half = (r * r - dmin2).sqrt();
        (dmin, Some((-b - half, -b + half)))
    } else {
        (dmin, None)
    }
}

fn ndc_of(view_proj: M4, p: [f32; 3]) -> [f32; 2] {
    let c = mul_point(view_proj, p, 1.0);
    [c[0] / c[3], c[1] / c[3]]
}

pub(super) fn depth_target(device: &GpuDevice, enc: &mut GpuEncoder) -> GpuTexture {
    let tex = device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        mip_levels: 1,
        format: GpuTextureFormat::Depth32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
        label: "water march test opaque depth",
    });
    // Reverse-Z: a Greater state clears depth to 0, which means no geometry.
    let state = device.create_depth_stencil_state(&GpuDepthStencilDesc {
        compare: GpuCompareFunction::Greater,
        write_enabled: true,
    });
    enc.draw_instanced_depth_only_batch(&tex, &state, &[], "water march test clear depth");
    tex
}

struct Run {
    hits: Vec<Hit>,
    motion: Option<Vec<[f32; 2]>>,
}

struct TestField {
    levelset: Vec<f32>,
    velocity: Option<Vec<f32>>,
    slot: u32,
}

fn run(fields: &[TestField], wants_velocity: bool, frame_seconds: f32) -> Run {
    let device = manifold_gpu::testkit::test_device();
    let sc = scene();
    let mut enc = device.create_encoder("water march test");
    let opaque = depth_target(&device, &mut enc);
    let velocity_out = wants_velocity.then(|| {
        device.create_texture(&GpuTextureDesc {
            width: W,
            height: H,
            depth: 1,
            mip_levels: 1,
            format: GpuTextureFormat::Rg16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::SHADER_WRITE
                | GpuTextureUsage::COPY_SRC,
            label: "water march test velocity out",
        })
    });
    let view = FieldView {
        view_proj: sc.view_proj,
        prev_view_proj: sc.view_proj,
        jitter_ndc: [0.0; 2],
        prev_jitter_ndc: [0.0; 2],
        frame_seconds,
        width: W,
        height: H,
    };
    let mut march = WaterMarch::default();
    march.begin(&device, &mut enc, W, H);
    let mut keep_alive = Vec::new();
    for f in fields {
        let levelset = shared_buffer(&device, &f.levelset);
        let velocity = f.velocity.as_ref().map(|v| shared_buffer(&device, v));
        let draw = FieldDraw {
            levelset: &levelset,
            velocity: velocity.as_ref(),
            mist: None,
            center: CENTER,
            size: SIZE,
            nodes: NODES,
            model: model(),
            prev_model: model(),
            slot: f.slot,
        };
        march
            .encode(&mut enc, &view, &draw, &opaque, None, velocity_out.as_ref())
            .expect("march encodes");
        keep_alive.push(levelset);
        keep_alive.extend(velocity);
    }
    let hit_bytes = u64::from(W * H) * 12;
    let hits_read = device.create_buffer_shared(hit_bytes);
    enc.copy_buffer_to_buffer(march.hits().expect("begin ran"), &hits_read, hit_bytes);
    let motion_read = velocity_out.as_ref().map(|t| {
        let b = device.create_buffer_shared(u64::from(W * H * 4));
        enc.copy_texture_to_buffer(t, &b, W, H, W * 4);
        b
    });
    enc.commit_and_wait_completed();
    let words: &[u32] = unsafe {
        std::slice::from_raw_parts(
            hits_read.mapped_ptr().expect("shared").cast::<u32>(),
            (W * H * 3) as usize,
        )
    };
    let hits = words
        .chunks_exact(3)
        .map(|c| unpack_hit([c[0], c[1], c[2]]))
        .collect();
    let motion = motion_read.map(|b| {
        let halves: &[u16] = unsafe {
            std::slice::from_raw_parts(
                b.mapped_ptr().expect("shared").cast::<u16>(),
                (W * H * 2) as usize,
            )
        };
        halves
            .chunks_exact(2)
            .map(|c| [f16::from_bits(c[0]).to_f32(), f16::from_bits(c[1]).to_f32()])
            .collect()
    });
    Run { hits, motion }
}

fn first_sphere() -> TestField {
    TestField {
        levelset: sphere_levelset([0.05, 0.0, -0.1], RADIUS),
        velocity: None,
        slot: 2,
    }
}

#[test]
fn sphere_field_is_hit_at_its_surface() {
    let sc = scene();
    let result = run(&[first_sphere()], false, 1.0 / 60.0);
    let c = world_sphere_center();
    let r = RADIUS * SCALE;
    let margin = 1.5 * cell() * SCALE;
    let world_cell = cell() * SCALE;
    let mut checked = 0;
    for y in 0..H {
        for x in 0..W {
            let hit = &result.hits[(y * W + x) as usize];
            let (o, d) = pixel_ray(sc.inv, x, y);
            let (dmin, chord) = analytic(o, d, c, r);
            if dmin <= r - margin {
                let (t_near, t_far) = chord.expect("inside silhouette");
                assert!(hit.depth > 0.0, "pixel ({x},{y}) missed the sphere");
                let world = unproject(sc.inv, pixel_ndc(x, y), hit.depth);
                let t = dot(sub(world, o), d);
                let hit_point = [
                    o[0] + d[0] * t_near,
                    o[1] + d[1] * t_near,
                    o[2] + d[2] * t_near,
                ];
                let n = norm(sub(hit_point, c));
                // The march finds the zero of the trilinear field, which sits
                // off the true sphere by up to h^2 / 4 times the curvature
                // trace 2 / rho (rho >= r - h near the surface); along the ray
                // that offset grows by 1 / cos(view angle).
                let interpolation =
                    world_cell * world_cell / (4.0 * (r - world_cell)) / dot(n, d).abs();
                assert!(
                    (t - t_near).abs() <= 0.02 * world_cell + interpolation,
                    "pixel ({x},{y}) hit at {t}, analytic {t_near}"
                );
                let cos = dot(n, hit.normal).clamp(-1.0, 1.0);
                assert!(
                    cos >= 2.0f32.to_radians().cos(),
                    "pixel ({x},{y}) normal off by {} degrees",
                    cos.acos().to_degrees()
                );
                // The exit walk overshoots by at most its 0.1-cell smallest
                // step; the field's offset applies at both ends of the chord.
                assert!(
                    (hit.thickness - (t_far - t_near)).abs()
                        <= 0.1 * world_cell + 2.0 * interpolation,
                    "pixel ({x},{y}) thickness {} vs chord {}",
                    hit.thickness,
                    t_far - t_near
                );
                assert_eq!(hit.slot_plus_one, 3);
                checked += 1;
            } else if dmin >= r + margin {
                assert_eq!(hit.depth, 0.0, "pixel ({x},{y}) hit outside the silhouette");
            }
        }
    }
    // The sphere covers about 640 pixels of the 96x72 target; the margin ring
    // removes the rest.
    assert!(checked >= 300, "only {checked} hit pixels were checked");
}

#[test]
fn velocity_lattice_moves_the_motion_vector() {
    let sc = scene();
    let node_count = (NODES[0] * NODES[1] * NODES[2]) as usize;
    let velocity: Vec<f32> = (0..node_count).flat_map(|_| [0.5, 0.0, 0.0]).collect();
    let mut field = first_sphere();
    field.velocity = Some(velocity);
    let frame = 1.0f32 / 60.0;
    let result = run(&[field], true, frame);
    let motion = result.motion.expect("velocity output requested");
    let m = model();
    let v_world = mul_point(m, [0.5, 0.0, 0.0], 0.0);
    let c = world_sphere_center();
    let r = RADIUS * SCALE;
    let margin = 1.5 * cell() * SCALE;
    let mut checked = 0;
    for y in 0..H {
        for x in 0..W {
            let (o, d) = pixel_ray(sc.inv, x, y);
            let (dmin, chord) = analytic(o, d, c, r);
            if dmin > r - margin {
                continue;
            }
            let (t_near, _) = chord.expect("inside silhouette");
            let hit = [
                o[0] + d[0] * t_near,
                o[1] + d[1] * t_near,
                o[2] + d[2] * t_near,
            ];
            let prev = [
                hit[0] - v_world[0] * frame,
                hit[1] - v_world[1] * frame,
                hit[2] - v_world[2] * frame,
            ];
            let now = ndc_of(sc.view_proj, hit);
            let before = ndc_of(sc.view_proj, prev);
            let expected = [now[0] - before[0], now[1] - before[1]];
            let got = motion[(y * W + x) as usize];
            assert!(
                (got[0] - expected[0]).abs() < 2e-3 && (got[1] - expected[1]).abs() < 2e-3,
                "pixel ({x},{y}) motion {got:?}, expected {expected:?}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 300, "only {checked} motion pixels were checked");
}

#[test]
fn nearer_field_already_drawn_is_kept() {
    let sc = scene();
    let behind = TestField {
        levelset: sphere_levelset([0.05, 0.0, -0.6], 0.3),
        velocity: None,
        slot: 6,
    };
    let result = run(&[first_sphere(), behind], false, 1.0 / 60.0);
    let c = world_sphere_center();
    let r = RADIUS * SCALE;
    let margin = 1.5 * cell() * SCALE;
    let mut covered = 0;
    for y in 0..H {
        for x in 0..W {
            let (o, d) = pixel_ray(sc.inv, x, y);
            let (dmin, _) = analytic(o, d, c, r);
            if dmin <= r - margin {
                let hit = &result.hits[(y * W + x) as usize];
                assert_eq!(
                    hit.slot_plus_one, 3,
                    "pixel ({x},{y}) lost the nearer field"
                );
                covered += 1;
            }
        }
    }
    assert!(covered >= 300, "only {covered} covered pixels were checked");
}
