//! GPU proofs for the water level-set march: two spheres in a field over 16
//! world tiles, a layer of unheld tiles between them, under a rotated, scaled,
//! translated model, against analytic ray-sphere answers; the extent as the
//! ray span; and the step over an unheld tile.
use super::water_march::{
    FieldDraw, FieldGeometry, FieldView, MarchUniforms, WaterMarch, march_uniforms,
};
use super::{FIELD_TILES_WGSL, mat4_inverse, model_matrix};
use crate::testkit::field_tiles::FieldTable;
use half::f16;
use manifold_gpu::BufferUse;
use manifold_gpu::*;
use manifold_node_engine::scene::scene_object::FIELD_FAR_CELLS;
use manifold_node_engine::scene::world_tiles::READ_SIDE;

pub(super) type M4 = [[f32; 4]; 4];

pub(super) const W: u32 = 96;
pub(super) const H: u32 = 72;
/// The display cell in metres: a tile is 0.5 m.
pub(super) const CELL: f32 = 0.0625;
const SCALE: f32 = 1.25;

/// A sphere of the field, in the object's space.
#[derive(Clone, Copy)]
struct Ball {
    center: [f32; 3],
    radius: f32,
}

/// Centred on the corner the eight tiles around the origin share (node
/// coordinate 8 * tile - 1/2 is a tile face), so it is split over all eight.
const FRONT: Ball = Ball {
    center: [-0.5 * CELL; 3],
    radius: 6.0 * CELL,
};

/// Centred on the corner shared by the eight tiles x 1..2, y 0..1, z -4..-3,
/// behind the front sphere along -z: the layer of tiles z = -2 between them
/// is unheld.
const BACK: Ball = Ball {
    center: [15.5 * CELL, 7.5 * CELL, -24.5 * CELL],
    radius: 6.0 * CELL,
};

/// Inside the front sphere, behind its centre as the camera sees it.
const INSIDE: Ball = Ball {
    center: [-0.5 * CELL, -0.5 * CELL, -2.5 * CELL],
    radius: 3.0 * CELL,
};

/// Tiles of the front sphere (2 x 2 x 2 around the origin) and of the back
/// one; tiles z = -2 lie between them and are not in the table.
fn table() -> FieldTable {
    let mut held = FieldTable::tiles_between([-1, -1, -1], [0, 0, 0]);
    held.extend(FieldTable::tiles_between([1, 0, -4], [2, 1, -3]));
    FieldTable::new(CELL, held.len() as u32, &held)
}

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

fn world_center(ball: &Ball) -> [f32; 3] {
    let c = mul_point(model(), ball.center, 1.0);
    [c[0], c[1], c[2]]
}

/// Level set of `balls` at every node of the pool under `table`: distance
/// minus radius, clamped to the canonical far value.
fn sphere_levelset(table: &FieldTable, balls: &[Ball]) -> Vec<f32> {
    let far = FIELD_FAR_CELLS * CELL;
    table.pool(far, |p| {
        let x = table.position(p);
        balls
            .iter()
            .map(|b| dot(sub(x, b.center), sub(x, b.center)).sqrt() - b.radius)
            .fold(f32::MAX, f32::min)
            .clamp(-far, far)
    })
}

pub(super) fn shared_buffer<T: bytemuck::Pod>(device: &GpuDevice, data: &[T]) -> GpuBuffer {
    let bytes: &[u8] = bytemuck::cast_slice(data);
    let buf = device.create_buffer(bytes.len() as u64, BufferUse::Upload);
    unsafe { buf.write(0, bytes) };
    buf
}

/// The GPU buffers of one test field under `table`.
pub(super) struct FieldBuffers {
    pub(super) levelset: GpuBuffer,
    pub(super) velocity: Option<GpuBuffer>,
    pub(super) mist: Option<GpuBuffer>,
    pub(super) tiles: GpuBuffer,
    pub(super) extent: GpuBuffer,
    pub(super) pool_tiles: u32,
}

impl FieldBuffers {
    pub(super) fn new(
        device: &GpuDevice,
        table: &FieldTable,
        extent: [f32; 8],
        levelset: &[f32],
        velocity: Option<&[[f32; 3]]>,
        mist: Option<&[f32]>,
    ) -> Self {
        Self {
            levelset: shared_buffer(device, levelset),
            velocity: velocity.map(|v| shared_buffer(device, v)),
            mist: mist.map(|m| shared_buffer(device, m)),
            tiles: shared_buffer(device, &table.words),
            extent: shared_buffer(device, &extent),
            pool_tiles: table.pool_tiles,
        }
    }

    pub(super) fn draw(&self, slot: u32) -> FieldDraw<'_> {
        FieldDraw {
            levelset: &self.levelset,
            velocity: self.velocity.as_ref(),
            mist: self.mist.as_ref(),
            tiles: &self.tiles,
            extent: &self.extent,
            pool_tiles: self.pool_tiles,
            cell_size: CELL,
            model: model(),
            prev_model: model(),
            slot,
        }
    }
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

/// The camera looks at the front sphere from four metres away.
pub(super) fn scene() -> Scene {
    let c = world_center(&FRONT);
    let view_proj = camera([0.3, 0.3, 4.0], c, 40.0, W as f32 / H as f32);
    Scene {
        view_proj,
        inv: mat4_inverse(view_proj).expect("camera is invertible"),
    }
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
    velocity: Option<Vec<[f32; 3]>>,
    extent: [f32; 8],
    slot: u32,
}

/// A field of `balls` over the test table, its extent the held tiles'.
fn test_field(table: &FieldTable, balls: &[Ball], slot: u32) -> TestField {
    TestField {
        levelset: sphere_levelset(table, balls),
        velocity: None,
        extent: table.extent(),
        slot,
    }
}

fn run(fields: &[TestField], wants_velocity: bool, frame_seconds: f32) -> Run {
    let device = manifold_gpu::testkit::test_device();
    let table = table();
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
    let buffers: Vec<FieldBuffers> = fields
        .iter()
        .map(|f| {
            FieldBuffers::new(
                &device,
                &table,
                f.extent,
                &f.levelset,
                f.velocity.as_deref(),
                None,
            )
        })
        .collect();
    for (f, buffers) in fields.iter().zip(&buffers) {
        march
            .encode(
                &device,
                &mut enc,
                &view,
                &buffers.draw(f.slot),
                &opaque,
                None,
                None,
                velocity_out.as_ref(),
            )
            .expect("march encodes");
    }
    let hit_bytes = u64::from(W * H) * 12;
    let hits_read = device.create_buffer(hit_bytes, BufferUse::Upload);
    enc.copy_buffer_to_buffer(march.hits().expect("begin ran"), &hits_read, hit_bytes);
    let motion_read = velocity_out.as_ref().map(|t| {
        let b = device.create_buffer(u64::from(W * H * 4), BufferUse::Upload);
        enc.copy_texture_to_buffer(t, &b, W, H, W * 4);
        b
    });
    enc.commit_and_wait().expect("GPU work completes");
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

/// Every pixel whose ray clearly meets exactly one ball (inside its silhouette
/// by a margin, and outside every other ball's by one) must hit it where the
/// analytic sphere is, with its normal and chord; every pixel clear of all
/// the balls must hit nothing. Returns the pixels checked inside each ball.
fn check_hits(hits: &[Hit], inv: M4, balls: &[Ball], slot_plus_one: u32) -> Vec<u32> {
    let n = balls.len();
    let centers: Vec<[f32; 3]> = balls.iter().map(world_center).collect();
    let radii: Vec<f32> = balls.iter().map(|b| b.radius * SCALE).collect();
    let world_cell = CELL * SCALE;
    let margin = 1.5 * world_cell;
    let mut checked = vec![0u32; n];
    for y in 0..H {
        for x in 0..W {
            let hit = &hits[(y * W + x) as usize];
            let (o, d) = pixel_ray(inv, x, y);
            let seen: Vec<_> = (0..n)
                .map(|i| analytic(o, d, centers[i], radii[i]))
                .collect();
            let clear_of = |i: usize| seen[i].0 >= radii[i] + margin;
            let inside: Vec<usize> = (0..n).filter(|&i| seen[i].0 <= radii[i] - margin).collect();
            if let [i] = inside[..]
                && (0..n).all(|j| j == i || clear_of(j))
            {
                let (r, c) = (radii[i], centers[i]);
                let (t_near, t_far) = seen[i].1.expect("inside silhouette");
                assert!(hit.depth > 0.0, "pixel ({x},{y}) missed ball {i}");
                let world = unproject(inv, pixel_ndc(x, y), hit.depth);
                let t = dot(sub(world, o), d);
                let hit_point = [
                    o[0] + d[0] * t_near,
                    o[1] + d[1] * t_near,
                    o[2] + d[2] * t_near,
                ];
                let normal = norm(sub(hit_point, c));
                // The march finds the zero of the trilinear field, which sits
                // off the true sphere by up to h^2 / 4 times the curvature
                // trace 2 / rho (rho >= r - h near the surface); along the ray
                // that offset grows by 1 / cos(view angle).
                let interpolation =
                    world_cell * world_cell / (4.0 * (r - world_cell)) / dot(normal, d).abs();
                assert!(
                    (t - t_near).abs() <= 0.02 * world_cell + interpolation,
                    "pixel ({x},{y}) hit ball {i} at {t}, analytic {t_near}"
                );
                let cos = dot(normal, hit.normal).clamp(-1.0, 1.0);
                assert!(
                    cos >= 2.0f32.to_radians().cos(),
                    "pixel ({x},{y}) ball {i} normal off by {} degrees",
                    cos.acos().to_degrees()
                );
                // The exit walk overshoots by at most its 0.1-cell smallest
                // step; the field's offset applies at both ends of the chord.
                assert!(
                    (hit.thickness - (t_far - t_near)).abs()
                        <= 0.1 * world_cell + 2.0 * interpolation,
                    "pixel ({x},{y}) ball {i} thickness {} vs chord {}",
                    hit.thickness,
                    t_far - t_near
                );
                assert_eq!(hit.slot_plus_one, slot_plus_one);
                checked[i] += 1;
            } else if (0..n).all(clear_of) {
                assert_eq!(hit.depth, 0.0, "pixel ({x},{y}) hit outside every ball");
            }
        }
    }
    checked
}

#[test]
fn spheres_across_tiles_are_hit_at_their_surface_over_an_unheld_layer() {
    let sc = scene();
    let table = table();
    let balls = [FRONT, BACK];
    let result = run(&[test_field(&table, &balls, 2)], false, 1.0 / 60.0);
    let checked = check_hits(&result.hits, sc.inv, &balls, 3);
    // The front sphere covers about 350 pixels of the 96x72 target and the
    // back one, further and partly behind it, about 150; the margin ring
    // removes the rest.
    assert!(
        checked[0] >= 120,
        "only {} front pixels checked",
        checked[0]
    );
    assert!(checked[1] >= 10, "only {} back pixels checked", checked[1]);
}

#[test]
fn velocity_array_moves_the_motion_vector() {
    let sc = scene();
    let table = table();
    let mut field = test_field(&table, &[FRONT], 2);
    field.velocity = Some(table.pool([0.0; 3], |_| [0.5, 0.0, 0.0]));
    let frame = 1.0f32 / 60.0;
    let result = run(&[field], true, frame);
    let motion = result.motion.expect("velocity output requested");
    let m = model();
    let v_world = mul_point(m, [0.5, 0.0, 0.0], 0.0);
    let c = world_center(&FRONT);
    let r = FRONT.radius * SCALE;
    let margin = 1.5 * CELL * SCALE;
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
    assert!(checked >= 120, "only {checked} motion pixels were checked");
}

#[test]
fn nearer_field_already_drawn_is_kept() {
    let sc = scene();
    let table = table();
    // The second field's sphere lies inside the first's, behind its front.
    let result = run(
        &[
            test_field(&table, &[FRONT, BACK], 2),
            test_field(&table, &[INSIDE], 6),
        ],
        false,
        1.0 / 60.0,
    );
    let c = world_center(&FRONT);
    let r = FRONT.radius * SCALE;
    let margin = 1.5 * CELL * SCALE;
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
    assert!(covered >= 120, "only {covered} covered pixels were checked");
}

/// The extent is the ray's span: an empty extent draws nothing even though
/// tiles are held, and one that holds only the front sphere's tiles draws it
/// and not the back sphere.
#[test]
fn the_extent_is_the_ray_span() {
    let sc = scene();
    let table = table();
    let balls = [FRONT, BACK];

    let mut empty = test_field(&table, &balls, 2);
    empty.extent = FieldTable::new(CELL, 1, &[]).extent();
    let result = run(&[empty], false, 1.0 / 60.0);
    assert!(
        result.hits.iter().all(|hit| hit.depth == 0.0),
        "an empty extent drew water"
    );

    let mut front_only = test_field(&table, &balls, 2);
    front_only.extent =
        FieldTable::new(CELL, 8, &FieldTable::tiles_between([-1, -1, -1], [0, 0, 0])).extent();
    let result = run(&[front_only], false, 1.0 / 60.0);
    let (mut front, mut back) = (0, 0);
    for y in 0..H {
        for x in 0..W {
            let (o, d) = pixel_ray(sc.inv, x, y);
            let hit = &result.hits[(y * W + x) as usize];
            let near_front = analytic(o, d, world_center(&FRONT), FRONT.radius * SCALE).0;
            let near_back = analytic(o, d, world_center(&BACK), BACK.radius * SCALE).0;
            let margin = 1.5 * CELL * SCALE;
            if near_front <= FRONT.radius * SCALE - margin {
                assert!(hit.depth > 0.0, "pixel ({x},{y}) lost the front sphere");
                front += 1;
            } else if near_back <= BACK.radius * SCALE - margin
                && near_front >= FRONT.radius * SCALE + margin
            {
                assert_eq!(hit.depth, 0.0, "pixel ({x},{y}) drew past the extent");
                back += 1;
            }
        }
    }
    assert!(front >= 120 && back >= 10, "{front} front, {back} back");
}

/// Probes `field_ahead` and `march_surface` of the shader at rays given as
/// (origin, direction) pairs.
const PROBE_KERNEL: &str = "\n\
@group(0) @binding(90) var<storage, read_write> probe_out: array<vec4<f32>>;\n\
fn field_tile_dry(slot: u32) -> bool { return false; }\n\
@group(0) @binding(91) var<storage, read> probe_in: array<vec4<f32>>;\n\
@compute @workgroup_size(1)\n\
fn cs_probe(@builtin(global_invocation_id) id: vec3<u32>) {\n\
    field_pool = u.tiles.xy;\n\
    let o = probe_in[2u * id.x].xyz;\n\
    let d = normalize(probe_in[2u * id.x + 1u].xyz);\n\
    let ahead = field_ahead(o, d);\n\
    probe_out[id.x] = vec4<f32>(ahead.phi, ahead.skip, march_surface(o, d, 0.0, 10.0), 0.0);\n\
}\n";

/// A ray in an unheld tile steps to the tile's far face at once, and a march
/// through the gap between two held tiles finds the surface beyond it. Tiles
/// -1 and 1 along x are held, tile 0 between them is not; tile 1 holds a
/// level set 0.6 - x, so the surface is the plane x = 0.6.
#[test]
fn a_ray_steps_over_an_unheld_tile_in_one_step() {
    let device = manifold_gpu::testkit::test_device();
    let table = FieldTable::new(CELL, 2, &[[-1, 0, 0], [1, 0, 0]]);
    let far = FIELD_FAR_CELLS * CELL;
    let levelset: Vec<f32> = table.pool(far, |p| {
        if p[0] >= 8 {
            (0.6 - table.position(p)[0]).clamp(-far, far)
        } else {
            far
        }
    });
    let identity = model_matrix([0.0; 3], [0.0; 3], [1.0; 3]);
    let view = FieldView {
        view_proj: identity,
        prev_view_proj: identity,
        jitter_ndc: [0.0; 2],
        prev_jitter_ndc: [0.0; 2],
        frame_seconds: 1.0 / 60.0,
        width: W,
        height: H,
    };
    let uniforms: MarchUniforms = march_uniforms(
        &view,
        &FieldGeometry {
            pool_tiles: table.pool_tiles,
            cell_size: CELL,
            model: identity,
            prev_model: identity,
            slot: 0,
            has_velocity: false,
        },
    )
    .expect("a real model inverts");

    // (origin, direction) pairs; all at y = z = 0.2, inside tile row 0.
    let rays: [([f32; 3], [f32; 3]); 5] = [
        // Held tile -1 toward tile 1 across the gap.
        ([-0.4, 0.2, 0.2], [1.0, 0.0, 0.0]),
        // In the gap, toward tile 1 and toward tile -1.
        ([0.1, 0.2, 0.2], [1.0, 0.0, 0.0]),
        ([0.1, 0.2, 0.2], [-1.0, 0.0, 0.0]),
        // In the gap, up and across: the tile's top face is nearer than its far x face.
        ([0.1, 0.2, 0.2], [0.6, 0.8, 0.0]),
        // Inside the water of tile 1.
        ([0.7, 0.2, 0.2], [1.0, 0.0, 0.0]),
    ];
    let input: Vec<[f32; 4]> = rays
        .iter()
        .flat_map(|(o, d)| [[o[0], o[1], o[2], 0.0], [d[0], d[1], d[2], 0.0]])
        .collect();
    let wgsl = format!(
        "{READ_SIDE}{FIELD_TILES_WGSL}{}{PROBE_KERNEL}",
        include_str!("../shaders/water_march_core.wgsl"),
    );
    let pipeline = device.create_compute_pipeline(&wgsl, "cs_probe", "march probe");
    let levelset = shared_buffer(&device, &levelset);
    let tiles = shared_buffer(&device, &table.words);
    let extent = shared_buffer(&device, &table.extent());
    let probe_in = shared_buffer(&device, &input);
    let probe_out = device.create_buffer((rays.len() * 16) as u64, BufferUse::Upload);
    let mut enc = device.create_encoder("march probe");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &levelset,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 70,
                buffer: &tiles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 71,
                buffer: &extent,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 90,
                buffer: &probe_out,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 91,
                buffer: &probe_in,
                offset: 0,
            },
        ],
        [rays.len() as u32, 1, 1],
        "march probe",
    );
    enc.commit_and_wait().expect("GPU work completes");
    let out: &[[f32; 4]] = unsafe {
        std::slice::from_raw_parts(
            probe_out.mapped_ptr().expect("shared").cast::<[f32; 4]>(),
            rays.len(),
        )
    };

    // The step lands a hundredth of a cell past the face.
    let nudge = 0.01 * CELL;
    let face = 8.0 * CELL - 0.5 * CELL;
    let near = |got: f32, want: f32, what: &str| {
        assert!((got - want).abs() < 1e-4, "{what}: {got} against {want}");
    };
    // The march refines its last step by false position, well inside this.
    let march = |got: f32, want: f32, what: &str| {
        assert!((got - want).abs() < 2e-3, "{what}: {got} against {want}");
    };
    // Held tile -1: the far value, no skip, then the plane at x = 0.6 once
    // across the gap.
    near(out[0][0], far, "held far value");
    assert_eq!(out[0][1], 0.0, "a held sample does not skip");
    march(out[0][2], 1.0, "march across the gap");
    // In the gap: the far value, and the distance to the face along the ray.
    near(out[1][0], far, "unheld value");
    near(out[1][1], face - 0.1 + nudge, "skip to the +x face");
    march(out[1][2], 0.5, "march from inside the gap");
    near(out[2][1], 0.1 + 0.5 * CELL + nudge, "skip to the -x face");
    assert_eq!(out[2][2], -1.0, "nothing to the -x side");
    near(out[3][1], (face - 0.2) / 0.8 + nudge, "skip to the +y face");
    // Held tile 1: inside the water, the level set is exact.
    near(out[4][0], -0.1, "level set inside tile 1");
    assert_eq!(out[4][1], 0.0);
    assert!(
        out[4][2] >= 0.0 && out[4][2] < 1e-4,
        "starts inside the water"
    );
}

/// The tile flags over a table holding a layer of water one node thick, a tile
/// of air past it, and a 2 x 2 x 2 block of water: a tile's flags cover the
/// nodes of every cell whose lower corner it holds, so its far faces' too (the
/// canonical element where unheld), and a free slot has none.
#[test]
fn tile_flags_cover_the_far_faces_of_a_tiles_cells() {
    // The TILE_ bits of water_march_core.wgsl.
    const WATER: u32 = 1;
    const AIR: u32 = 2;
    const MIST: u32 = 4;
    let device = manifold_gpu::testkit::test_device();
    let mut held = vec![[0, 0, 0], [1, 0, 0], [2, 0, 0]];
    held.extend(FieldTable::tiles_between([5, 5, 5], [6, 6, 6]));
    let table = FieldTable::new(CELL, held.len() as u32 + 1, &held);
    let far = FIELD_FAR_CELLS * CELL;
    let in_block = |p: [i32; 3]| p.iter().all(|c| (40..56).contains(c));
    // Water on the first node layer of tile 1 and through the block.
    let levelset = table.pool(far, |p| if p[0] == 8 || in_block(p) { -far } else { far });
    let mist = table.pool(0.0, |p| if p == [8, 0, 0] { 0.5 } else { 0.0 });
    let buffers = FieldBuffers::new(
        &device,
        &table,
        table.extent(),
        &levelset,
        None,
        Some(&mist),
    );
    let sc = scene();
    let view = FieldView {
        view_proj: sc.view_proj,
        prev_view_proj: sc.view_proj,
        jitter_ndc: [0.0; 2],
        prev_jitter_ndc: [0.0; 2],
        frame_seconds: 1.0 / 60.0,
        width: W,
        height: H,
    };
    let mut enc = device.create_encoder("water tile flags test");
    let opaque = depth_target(&device, &mut enc);
    let mut march = WaterMarch::default();
    march.begin(&device, &mut enc, W, H);
    march
        .encode(
            &device,
            &mut enc,
            &view,
            &buffers.draw(0),
            &opaque,
            None,
            None,
            None,
        )
        .expect("march encodes");
    let bytes = u64::from(table.pool_tiles) * 4;
    let read = device.create_buffer(bytes, BufferUse::Upload);
    enc.copy_buffer_to_buffer(march.tile_flags(0).expect("marched"), &read, bytes);
    enc.commit_and_wait().expect("GPU work completes");
    let flags: &[u32] = unsafe {
        std::slice::from_raw_parts(
            read.mapped_ptr().expect("shared").cast::<u32>(),
            table.pool_tiles as usize,
        )
    };
    let slot = |t: [i32; 3]| table.held.iter().position(|&h| h == t).expect("held");
    assert_eq!(
        flags[slot([0, 0, 0])],
        WATER | AIR | MIST,
        "water and mist on its +x face"
    );
    assert_eq!(flags[slot([1, 0, 0])], WATER | AIR | MIST);
    assert_eq!(
        flags[slot([2, 0, 0])],
        AIR,
        "only air: the march steps over it"
    );
    assert_eq!(
        flags[slot([5, 5, 5])],
        WATER,
        "its far faces are in the block"
    );
    assert_eq!(
        flags[slot([6, 6, 6])],
        WATER | AIR,
        "its far faces read the canonical air"
    );
    assert_eq!(flags[held.len()], 0, "a free slot");
}
