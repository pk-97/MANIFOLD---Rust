//! RT_INSTANCING_DESIGN.md P2 — value-level proofs for the RT instancing
//! path (D10: synthetic buffers only, CPU oracle from the SAME TRS/mirror
//! math the raster uses — render_scene.wgsl:781-813 semantics). Covers
//! INV-RTI1 (RT sees the raster's instance set), INV-RTI2 (mirror exactness
//! in RT — D5's VERIFY-AT-IMPL arbiter), D2 (dead slots never intersect),
//! INV-RTI5/BUG-757c (capacity traced, not count), and INV-RTI4 (static
//! instances cost zero per-frame descriptor dispatches).
//!
//! Probe mechanism (same discipline as `rt_p1_shadow.rs`): the dispatch
//! reconstructs each texel's world position from a hand-written depth
//! texture with `inv_view_proj = IDENTITY`, so `world = (ndc_x, ndc_y,
//! depth)` and the PRIMARY ray (camera at the origin) is aimed exactly by
//! choosing the texel's depth/pixel. A texel whose ray passes through a
//! copy's expected center must produce `out_n.w == object_index` (a hit)
//! with `out_n.xyz` equal to the CPU-computed world normal; a texel aimed
//! at empty space (or a dead slot's would-be position) must produce
//! `out_n.w == -1` (primary miss). Because every probe ray is pre-aimed at
//! a CPU-computed 3D position, "hit vs miss" IS the position/distance
//! assertion to ray precision — the kernel exposes no primary-hit distance
//! channel, and the pre-aimed normal check carries the orientation
//! evidence (including the D3 instance fold, exercised because the hit
//! row is a wired SLOT row).

use std::ffi::c_void;
use std::slice;

use manifold_gpu::raytrace::{
    ensure_normal_sources, GiMaterial, MetalShadowRayTracer, RtCasterParams, RtObjectGeometry,
    ShadowRayParams, ShadowRayTracer,
};
use manifold_gpu::{
    GpuBuffer, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};
use manifold_renderer::generators::mesh_common::InstanceTransform;

use crate::harness;

/// `pos` (12 bytes) + `normal` (12 bytes) interleaved vertex — the RT
/// normal fetch reads `normal_offset = 12` within this stride-24 record.
#[repr(C)]
#[derive(Clone, Copy)]
struct PackedVertexN {
    pos: [f32; 3],
    normal: [f32; 3],
}

/// The quad every fixture instances: two triangles in the LOCAL z=0 plane,
/// x/y in [-0.05, 0.05], all normals (0,0,-1). Local origin = quad center,
/// so a slot's stored translation IS its copy's expected world center
/// (model = identity). Half-extent 0.05 keeps neighboring column probes
/// (0.1875 apart in x at z = 0.5) clear of the wrong quad.
const QUAD: [PackedVertexN; 6] = [
    PackedVertexN { pos: [-0.05, -0.05, 0.0], normal: [0.0, 0.0, -1.0] },
    PackedVertexN { pos: [ 0.05, -0.05, 0.0], normal: [0.0, 0.0, -1.0] },
    PackedVertexN { pos: [ 0.05,  0.05, 0.0], normal: [0.0, 0.0, -1.0] },
    PackedVertexN { pos: [-0.05, -0.05, 0.0], normal: [0.0, 0.0, -1.0] },
    PackedVertexN { pos: [ 0.05,  0.05, 0.0], normal: [0.0, 0.0, -1.0] },
    PackedVertexN { pos: [-0.05,  0.05, 0.0], normal: [0.0, 0.0, -1.0] },
];

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

const _: () = assert!(std::mem::size_of::<InstanceTransform>() == 32);

// ─── CPU oracle: the raster's TRS/mirror math (render_scene.wgsl:781-813) ──

/// CPU port of `render_scene.wgsl`'s `euler_xyz` (Rz·Ry·Rx, COLUMN
/// construction — the MSL/WGSL matrix constructors take columns in the
/// same order, so this is the same math the descriptor kernel and the
/// normal fold evaluate on the GPU). Returns column-major 3x3.
fn euler_xyz(angles: [f32; 3]) -> [[f32; 3]; 3] {
    let (sx, cx) = angles[0].sin_cos();
    let (sy, cy) = angles[1].sin_cos();
    let (sz, cz) = angles[2].sin_cos();
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, sx], [0.0, -sx, cx]];
    let ry = [[cy, 0.0, -sy], [0.0, 1.0, 0.0], [sy, 0.0, cy]];
    let rz = [[cz, sz, 0.0], [-sz, cz, 0.0], [0.0, 0.0, 1.0]];
    mat3_mul(rz, mat3_mul(ry, rx))
}

fn mat3_mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut r = [[0.0f32; 3]; 3];
    for (j, col) in r.iter_mut().enumerate() {
        *col = mat3_vec(a, b[j]);
    }
    r
}

fn mat3_vec(m: [[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
    ]
}

/// vs_main's msign decode: marker k>0 = mirrored across the plane
/// perpendicular to component k-1 (SCENE_MIRROR_DESIGN.md D5).
fn msign_of(marker: u32) -> [f32; 3] {
    [
        if marker == 1 { -1.0 } else { 1.0 },
        if marker == 2 { -1.0 } else { 1.0 },
        if marker == 3 { -1.0 } else { 1.0 },
    ]
}

/// CPU expectation for the world normal of a slot's copy, through the D3
/// chain (model = identity): n' = euler(rot)·(n_local·msign), with
/// n_local = (0,0,-1) (the fixture quad's normal).
fn expected_slot_normal(rot: [f32; 3], marker: u32) -> [f32; 3] {
    let msign = msign_of(marker);
    let n_local = [0.0f32, 0.0, -1.0];
    mat3_vec(
        euler_xyz(rot),
        [n_local[0] * msign[0], n_local[1] * msign[1], n_local[2] * msign[2]],
    )
}

/// The planar-reflection form of the same expectation (INV-MR2 carried
/// into accel space): a mirror producer stores R' = M·R·M and t' = M·t
/// (plane through the origin, SCENE_MIRROR D5), so the folded normal must
/// equal M·(R·n_local) — the true reflection of the source copy's normal.
/// Asserting BOTH forms catches a fixture that encodes the mirror wrong.
fn expected_mirrored_normal(base_rot: [f32; 3], marker: u32) -> [f32; 3] {
    let m = msign_of(marker);
    let base = mat3_vec(euler_xyz(base_rot), [0.0, 0.0, -1.0]);
    [base[0] * m[0], base[1] * m[1], base[2] * m[2]]
}

// ─── Fixture builders ──────────────────────────────────────────────────

fn live_slot(pos: [f32; 3], rot: [f32; 3], marker: u32) -> InstanceTransform {
    InstanceTransform {
        pos_scale: [pos[0], pos[1], pos[2], 1.0],
        rot_pad: [rot[0], rot[1], rot[2], marker as f32],
    }
}

fn dead_slot(pos: [f32; 3]) -> InstanceTransform {
    // D2/INV-RTI1: zero pos_scale.w = dead slot — identity descriptor,
    // mask 0, provably never intersected.
    InstanceTransform {
        pos_scale: [pos[0], pos[1], pos[2], 0.0],
        rot_pad: [0.0; 4],
    }
}

fn write_shared_buffer<T: Copy>(device: &GpuDevice, data: &[T]) -> GpuBuffer {
    let bytes = std::mem::size_of_val(data) as u64;
    let buf = device.create_buffer_shared(bytes.max(16));
    let ptr = buf.mapped_ptr().expect("shared buffer must expose a mapped pointer");
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr().cast::<u8>(), ptr, bytes as usize);
    }
    buf
}

fn upload_texture_f32(
    device: &GpuDevice,
    width: u32,
    height: u32,
    format: GpuTextureFormat,
    pixels: &[f32],
    label: &str,
) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width,
        height,
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label,
        mip_levels: 1,
    });
    let bytes =
        unsafe { slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), std::mem::size_of_val(pixels)) };
    device.upload_texture(&texture, bytes);
    texture
}

// ─── Probe rig ─────────────────────────────────────────────────────────

/// One probe expectation: a PRIMARY ray aimed at `target` (a CPU-computed
/// world position — a live slot's stored translation, or a dead slot's
/// would-be position, or empty space). `Some(normal)` = the copy must be
/// hit with that world normal; `None` = the ray must miss.
struct Probe {
    target: [f32; 3],
    expect: Option<[f32; 3]>,
}

/// Probe-target scheme: targets sit at z = 0.5 with |x| < z (inside the
/// unit-cube frustum) and NOT at a column-center ratio (that makes depth
/// 1.0, which reads as VOID) — each target's x picks its column via the
/// ratio x/z, and the runner derives the depth that puts the pixel ray
/// exactly through it. Fixture x positions: ±0.45 and ±0.2 (columns 0/7
/// and 2/5); the miss target sits at column 4, whose ray clears every
/// quad at the fixture depths.
///
/// Runs the probe suite: builds an instanced accel over the fixture quad
/// with `slots` wired, then one primary ray per [`Probe`] and returns the
/// readback `out_n` rows (`.w` = object index or -1, `.xyz` = normal).
fn run_probes(slots: &[InstanceTransform], probes: &[Probe]) -> Vec<[f32; 4]> {
    let h = harness::shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);

    let vertex_buffer = write_shared_buffer(device, &QUAD);
    let instances_buffer = write_shared_buffer(device, slots);

    let objects = [RtObjectGeometry {
        vertex_buffer: &vertex_buffer,
        vertex_stride: std::mem::size_of::<PackedVertexN>() as u32,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: 2,
        transform: IDENTITY,
        normal_offset: 12,
        uv_offset: 0, // alpha_mask is false — never read.
        alpha_mask: false,
        translucent: false,
        alpha_cutoff: 0.5,
        base_color_texture: None,
        mr_texture: None,
        normal_texture: None,
        emissive_texture: None,
        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
        emissive_uv_t: [0.0, 0.0],
        cast_shadows: true,
        instances_addr: instances_buffer.gpu_address(),
        instances_buffer: Some(&instances_buffer),
        instance_slots: slots.len() as u32,
    }];
    let accel = tracer.build_accel(device, &objects, &[]);

    // D11 tables: canonical row [0, N) + slot rows [N, N+Σ) — production
    // indexing via with_slot_row_base(N) below.
    let mut normal_sources_slot = None;
    let mut normal_sources_capacity = 0usize;
    ensure_normal_sources(
        &mut normal_sources_slot,
        &mut normal_sources_capacity,
        device,
        &objects,
    );
    let normal_sources = normal_sources_slot.expect("ensure_normal_sources must allocate");
    let gi_materials_buffer = device.create_buffer_shared(
        ((objects.len() + slots.len()).max(1) * std::mem::size_of::<GiMaterial>()) as u64,
    );

    // Depth fixture: 8 texels, one column per probe. The kernel derives
    // ndc from the PIXEL (`ndc_x = (col+0.5)/8*2-1`, ndc_y = 0) and takes
    // only z from the depth texture, so the probe ray is the line through
    // (ndc_col, 0, depth). For it to pass through `target`, the depth must
    // satisfy depth = ndc_col · target.z / target.x — the fixtures place
    // every target at a column center (target.x/target.z = ndc_col), which
    // makes depth = target.z exactly. depth must stay in the valid clip
    // range (>= 1.0 - 1e-6 reads as VOID).
    const W: u32 = 8;
    let mut depth_px = [0.0f32; W as usize];
    let mut columns: Vec<u32> = Vec::with_capacity(probes.len());
    for (i, probe) in probes.iter().enumerate() {
        let t = probe.target;
        assert!(
            t[2] > 0.0 && t[0].abs() > 1e-6,
            "probe {i} target {t:?} must have z > 0 and nonzero x (the ray is \
             fixed by the pixel column)"
        );
        let ratio = t[0] / t[2];
        assert!(ratio.abs() < 1.0, "probe {i} target {t:?} is outside the frustum");
        let col = (((ratio + 1.0) * 0.5) * W as f32).floor() as u32;
        let ndc = (col as f32 + 0.5) / W as f32 * 2.0 - 1.0;
        let depth = ndc * t[2] / t[0];
        assert!(
            col < W && !columns.contains(&col),
            "probe {i} at column {col} collides or is out of range — re-space the fixture targets"
        );
        assert!(
            depth > 0.0 && depth < 1.0 - 1e-6,
            "probe {i} needs depth {depth}, outside the valid clip range — re-space the fixture"
        );
        columns.push(col);
        depth_px[col as usize] = depth;
    }
    let depth_tex = upload_texture_f32(device, W, 1, GpuTextureFormat::Depth32Float, &depth_px, "rt-instancing-depth");

    let tex = |label: &str| {
        device.create_texture(&GpuTextureDesc {
            width: W,
            height: 1,
            depth: 1,
            format: GpuTextureFormat::Rgba32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
            label,
            mip_levels: 1,
        })
    };
    let out_sv = tex("rt-instancing-out_sv");
    let out_sv2 = tex("rt-instancing-out_sv2");
    let out_svt = tex("rt-instancing-out_svt");
    let out_irr = tex("rt-instancing-out_irr");
    let out_n = tex("rt-instancing-out_n");
    let out_refl = tex("rt-instancing-out_refl");
    let prefiltered_env = device.create_texture(&GpuTextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ,
        label: "rt-instancing-env-dummy",
        mip_levels: 1,
    });

    // Lighting-only dispatch (RT-A3a): shadow_spp 0 skips the caster loop
    // (zero casters anyway); ao_spp 1 forces the primary-visibility cast
    // that produces out_n. camera_pos = origin (matches IDENTITY
    // inv_view_proj); slot_row_base = N per D11.
    let casters: [RtCasterParams; 0] = [];
    let params = ShadowRayParams::new(
        &casters,
        0,
        0,
        [W, 1],
        [W, 1],
        0.0,
        1,
        0,
        [0.0, 0.0, 0.0],
        IDENTITY,
        0,
        0.6,
        0.1,
        0.0,
        0,
        0.0,
        manifold_gpu::raytrace::SVT_SLOT_NONE,
    )
    .with_slot_row_base(objects.len() as u32);
    let params_buffer =
        device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);
    let dummy_emissive = device.create_buffer_shared(1);

    let mut encoder = device.create_encoder("rt-instancing-probe");
    tracer.dispatch_shadow_rays(
        &mut encoder,
        &accel,
        &params,
        &params_buffer,
        &gi_materials_buffer,
        &normal_sources,
        &[],
        &depth_tex,
        &out_sv,
        &out_sv2,
        &out_svt,
        &out_irr,
        &out_n,
        &out_refl,
        &prefiltered_env,
        &dummy_emissive,
        &dummy_emissive,
        false,
        "trace_shadow_rays-instancing-probe",
    );
    encoder.commit_and_wait_completed();

    let readback = device.create_buffer_shared(W as u64 * 4 * 4);
    let mut enc2 = device.create_encoder("rt-instancing-readback");
    enc2.copy_texture_to_buffer(&out_n, &readback, W, 1, W * 4 * 4);
    enc2.commit_and_wait_completed();
    let ptr = readback.mapped_ptr().expect("readback buffer must be mapped");
    let floats: &[f32] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<f32>(), (W * 4) as usize) };
    let mut rows = Vec::with_capacity(probes.len());
    for col in &columns {
        let base = *col as usize * 4;
        rows.push([floats[base], floats[base + 1], floats[base + 2], floats[base + 3]]);
    }
    rows
}

fn assert_hit(row: &[f32; 4], expected_n: [f32; 3], what: &str) {
    assert_eq!(row[3], 0.0, "{what}: expected a hit (out_n.w == object_index 0), got {:?}", row);
    for (axis, (&got, &want)) in row[..3].iter().zip(expected_n.iter()).enumerate() {
        assert!(
            (got - want).abs() < 1e-2,
            "{what}: normal axis {axis} = {got}, CPU expects {want} (row {row:?})"
        );
    }
}

fn assert_miss(row: &[f32; 4], what: &str) {
    assert_eq!(
        row[3], -1.0,
        "{what}: expected a miss (out_n.w == -1), got {row:?} — a copy intersects that was not expected"
    );
}


// ─── (a) INV-RTI1 — the loop copy set ──────────────────────────────────

/// INV-RTI1: a 4-slot buffer with one live translation-only TRS per slot
/// traces exactly four copies, each at its CPU-expected position (probe
/// hit) with the expected (unrotated) normal; a fifth ray at a position
/// where no 5th slot exists misses.
#[test]
fn loop_copies_trace_at_cpu_expected_positions() {
    // Columns 0/7 (x = ±0.45) and 2/5 (x = ±0.2) — distinct, in-range,
    // enforced by run_probes' collision assert.
    let slots = [
        live_slot([-0.45, 0.0, 0.5], [0.0; 3], 0),
        live_slot([-0.20, 0.0, 0.5], [0.0; 3], 0),
        live_slot([ 0.20, 0.0, 0.5], [0.0; 3], 0),
        live_slot([ 0.45, 0.0, 0.5], [0.0; 3], 0),
    ];
    let probes = [
        Probe { target: [-0.45, 0.0, 0.5], expect: Some([0.0, 0.0, -1.0]) },
        Probe { target: [-0.20, 0.0, 0.5], expect: Some([0.0, 0.0, -1.0]) },
        Probe { target: [ 0.20, 0.0, 0.5], expect: Some([0.0, 0.0, -1.0]) },
        Probe { target: [ 0.45, 0.0, 0.5], expect: Some([0.0, 0.0, -1.0]) },
        // No 5th slot exists at column 4 (x = 0.1041667) — must miss.
        Probe { target: [ 0.1041667, 0.0, 0.5], expect: None },
    ];
    let rows = run_probes(&slots, &probes);
    for (i, probe) in probes.iter().enumerate() {
        match probe.expect {
            Some(n) => assert_hit(&rows[i], n, &format!("loop copy {i}")),
            None => assert_miss(&rows[i], &format!("would-be 5th copy (probe {i})")),
        }
    }
}

// ─── (b) INV-RTI2 — mirror exactness (D5 VERIFY-AT-IMPL) ───────────────

/// INV-RTI2 / D5's arbiter: slots carrying mirror markers trace at the
/// exact planar-reflection positions with the mirrored normals. The
/// fixture stores what a mirror producer mints (SCENE_MIRROR D5): R' =
/// M·R·M, t' = M·t (plane through the origin). The marker-3 copy faces
/// AWAY from the camera (z-flip) — a hit here is the two-sided-default
/// evidence; a miss means Metal culls by winding and D5's VERIFY-AT-IMPL
/// answers "culls" (a REAL finding — this proof must fail then, not be
/// softened).
#[test]
fn mirror_slots_trace_exact_reflection_with_mirrored_normals() {
    const THETA: f32 = 0.5;
    // Base rotations per slot: single-axis so the conjugated R' = M·R·M
    // stays single-axis (closed form, no matrix→euler): rotation about an
    // axis a under reflection M becomes rotation about M·a by -θ; for the
    // axes chosen here that lands the stored euler at ±θ on the same axis.
    // (source: Ry(+θ)) / (marker 1, M=diag(-1,1,1): stored Ry(-θ))
    // (marker 2, M=diag(1,-1,1): base Rx(+θ), stored Rx(-θ))
    // (marker 3, M=diag(1,1,-1): base Ry(+θ), stored Ry(-θ))
    let slots = [
        live_slot([ 0.45, 0.0, 0.5], [0.0,  THETA, 0.0], 0),
        live_slot([-0.45, 0.0, 0.5], [0.0, -THETA, 0.0], 1),
        live_slot([ 0.20, 0.0, 0.5], [-THETA, 0.0, 0.0], 2),
        live_slot([-0.20, 0.0, 0.5], [0.0, -THETA, 0.0], 3),
    ];
    let cases: [(&str, [f32; 3], [f32; 3], u32); 4] = [
        // (name, expected-normal-D3-chain, base-rotation, marker)
        ("source",        expected_slot_normal([0.0, THETA, 0.0], 0), [0.0, THETA, 0.0], 0),
        ("mirror-x (k=1)", expected_slot_normal([0.0, -THETA, 0.0], 1), [0.0, THETA, 0.0], 1),
        ("mirror-y (k=2)", expected_slot_normal([-THETA, 0.0, 0.0], 2), [THETA, 0.0, 0.0], 2),
        ("mirror-z (k=3)", expected_slot_normal([0.0, -THETA, 0.0], 3), [0.0, THETA, 0.0], 3),
    ];
    for (name, d3, base, marker) in cases.iter().copied() {
        if marker != 0 {
            let mirrored = expected_mirrored_normal(base, marker);
            for (axis, (a, b)) in d3.iter().zip(mirrored.iter()).enumerate() {
                assert!(
                    (a - b).abs() < 1e-5,
                    "fixture self-consistency ({name}): D3-chain normal axis {axis} = {a}, \
                     planar-reflection form = {b} — the stored R'/t' do not encode the claimed mirror"
                );
            }
        }
    }
    let probes: [Probe; 4] = [
        Probe { target: [ 0.45, 0.0, 0.5], expect: Some(cases[0].1) },
        Probe { target: [-0.45, 0.0, 0.5], expect: Some(cases[1].1) },
        Probe { target: [ 0.20, 0.0, 0.5], expect: Some(cases[2].1) },
        Probe { target: [-0.20, 0.0, 0.5], expect: Some(cases[3].1) },
    ];
    let rows = run_probes(&slots, &probes);
    for (i, (probe, (name, _, _, _))) in probes.iter().zip(cases.iter()).enumerate() {
        match probe.expect {
            Some(n) => assert_hit(&rows[i], n, &format!("mirrored slot {name}")),
            None => unreachable!(),
        }
    }
}

// ─── (c) D2 — dead slots never intersect ───────────────────────────────

/// D2/INV-RTI1: a zero-scale slot gets identity + mask 0 — a probe aimed
/// EXACTLY at its would-be position must miss (a hit would mean the dead
/// slot was traced).
#[test]
fn dead_slot_never_intersects() {
    let slots = [
        live_slot([0.20, 0.0, 0.5], [0.0; 3], 0),
        dead_slot([-0.20, 0.0, 0.5]),
    ];
    let probes = [
        Probe { target: [ 0.20, 0.0, 0.5], expect: Some([0.0, 0.0, -1.0]) },
        Probe { target: [-0.20, 0.0, 0.5], expect: None },
    ];
    let rows = run_probes(&slots, &probes);
    assert_hit(&rows[0], [0.0, 0.0, -1.0], "live slot");
    assert_miss(&rows[1], "dead slot's would-be position");
}

// ─── (d) INV-RTI5 — capacity traced, not count ─────────────────────────

/// INV-RTI5/BUG-757c: a capacity-8 buffer with only 3 live slots traces
/// exactly 3 copies — the 4th slot is zero-scale (in-band dead), and the
/// remaining capacity contributes nothing. (RT has no count param; the
/// buffer's capacity IS the slot count, live-ness is in-band.)
#[test]
fn capacity_buffer_traces_live_slots_only() {
    let slots = [
        live_slot([-0.45, 0.0, 0.5], [0.0; 3], 0),
        live_slot([-0.20, 0.0, 0.5], [0.0; 3], 0),
        live_slot([ 0.20, 0.0, 0.5], [0.0; 3], 0),
        // Would-be 4th copy at column 7 (x = 0.45) — dead (zero scale).
        dead_slot([ 0.45, 0.0, 0.5]),
        dead_slot([ 0.40, 0.20, 0.5]),
        dead_slot([-0.40, 0.20, 0.5]),
        dead_slot([ 0.40, -0.20, 0.5]),
        dead_slot([-0.40, -0.20, 0.5]),
    ];
    let probes = [
        Probe { target: [-0.45, 0.0, 0.5], expect: Some([0.0, 0.0, -1.0]) },
        Probe { target: [-0.20, 0.0, 0.5], expect: Some([0.0, 0.0, -1.0]) },
        Probe { target: [ 0.20, 0.0, 0.5], expect: Some([0.0, 0.0, -1.0]) },
        Probe { target: [ 0.45, 0.0, 0.5], expect: None },
    ];
    let rows = run_probes(&slots, &probes);
    for (i, probe) in probes.iter().enumerate() {
        match probe.expect {
            Some(n) => assert_hit(&rows[i], n, &format!("live slot {i}")),
            None => assert_miss(&rows[i], "would-be 4th copy (dead slot)"),
        }
    }
}

// ─── (f) P1.5 — a wired 1-capacity buffer takes the GPU path ───────────

/// P1.5 regression lock: a 1-capacity WIRED instances buffer with a
/// non-identity TRS (offset + rotation) traces its copy at slot 0's
/// transformed position with the rotated normal. Under the pre-P1.5 rule
/// (GPU path only for plural capacity) this fixture silently took the CPU
/// fast path and traced an identity descriptor at the base model — this
/// probe at the transformed center would MISS. Hit + rotated normal = the
/// GPU descriptor path served a 1-capacity buffer.
#[test]
fn wired_single_capacity_buffer_uses_gpu_path() {
    const THETA: f32 = 0.5;
    let slots = [live_slot([0.20, 0.0, 0.5], [0.0, THETA, 0.0], 0)];
    let expected = expected_slot_normal([0.0, THETA, 0.0], 0);
    let probes = [
        Probe { target: [0.20, 0.0, 0.5], expect: Some(expected) },
        // Column 4 stays empty — no copy may exist off the slot's TRS.
        Probe { target: [ 0.1041667, 0.0, 0.5], expect: None },
    ];
    let rows = run_probes(&slots, &probes);
    assert_hit(&rows[0], expected, "wired 1-capacity slot 0 (GPU path)");
    assert_miss(&rows[1], "empty column 0 (no stray base-model copy)");
}

// ─── (e) INV-RTI4 — static instances cost zero ─────────────────────────

/// Worker scene for [`static_instances_trigger_no_per_frame_descriptor_dispatches`]:
/// one `node.scene_array` (a STATIC producer — its slot generation never
/// bumps) instancing one cube through `node.scene_object` into an
/// RT-enabled `node.render_scene`. Rendered for 8 frames with constant
/// time/beat (static everything), the accel must build exactly once —
/// ONE descriptor-build dispatch total, zero refit dispatches. Runs only
/// when the parent spawns this same test binary with
/// `RT_INSTANCING_PROBE_WORKER=1` (a plain test run no-ops it).
#[test]
fn probe_worker_static_frames() {
    if std::env::var("RT_INSTANCING_PROBE_WORKER").as_deref() != Ok("1") {
        return;
    }
    let h = harness::shared();
    let registry = manifold_renderer::node_graph::PrimitiveRegistry::with_builtin();
    let json = r#"{"version":2,"name":"RtInstancingProbeWorker","nodes":[
        {"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.cube_mesh","nodeId":"cube"},
        {"id":2,"typeId":"node.unlit_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":0.8},
            "color_g":{"type":"Float","value":0.8},
            "color_b":{"type":"Float","value":0.8}}},
        {"id":3,"typeId":"node.scene_array","nodeId":"array","params":{
            "count":{"type":"Float","value":2.0},
            "axis":{"type":"Enum","value":4},
            "cell_size":{"type":"Float","value":3.0}}},
        {"id":4,"typeId":"node.scene_object","nodeId":"obj"},
        {"id":5,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.7},
            "tilt":{"type":"Float","value":0.95},
            "distance":{"type":"Float","value":10.0},
            "fov_y":{"type":"Float","value":0.8}}},
        {"id":6,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":0},
            "rt_enabled":{"type":"Bool","value":true}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}
        ],"wires":[
        {"fromNode":1,"fromPort":"vertices","toNode":4,"toPort":"vertices"},
        {"fromNode":2,"fromPort":"out","toNode":4,"toPort":"material"},
        {"fromNode":3,"fromPort":"out","toNode":4,"toPort":"instances"},
        {"fromNode":4,"fromPort":"object","toNode":6,"toPort":"object_0"},
        {"fromNode":5,"fromPort":"out","toNode":6,"toPort":"camera"},
        {"fromNode":6,"fromPort":"color","toNode":99,"toPort":"in"}
        ]}"#;
    let mut runtime = manifold_renderer::preset_runtime::PresetRuntime::from_json_str_with_device(
        json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("INV-RTI4 worker scene graph must build");
    let target = h.make_target("rt-instancing-probe-worker");
    // BUG-308's defer needs the build-enqueue frame plus settle time; 8
    // static frames covers request -> enqueue -> completion with margin
    // (same count as RT_WARMUP-ish precedents).
    for frame in 0..8i64 {
        let ctx = manifold_renderer::preset_context::PresetContext {
            time: 0.1,
            beat: 0.2,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("rt-instancing-probe-worker");
        {
            let mut gpu =
                manifold_renderer::gpu_encoder::GpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }
}

/// INV-RTI4: across 8 static frames of a wired-instances scene, the
/// descriptor-build kernel dispatches exactly ONCE (the accel build) —
/// the probe log (`MANIFOLD_PROBE_RT_ACCEL`, one line per dispatch from
/// `encode_descriptor_build`) is captured from a `--nocapture` subprocess
/// of this same test binary (no libc/fd redirection needed; libtest
/// passes the child's stderr through).
#[test]
fn static_instances_trigger_no_per_frame_descriptor_dispatches() {
    let exe = std::env::current_exe().expect("current test binary path");
    let out = std::process::Command::new(exe)
        .args(["--exact", "rt_instancing::probe_worker_static_frames", "--nocapture"])
        .env("MANIFOLD_PROBE_RT_ACCEL", "1")
        .env("RT_INSTANCING_PROBE_WORKER", "1")
        .output()
        .expect("failed to spawn INV-RTI4 probe worker");
    assert!(
        out.status.success(),
        "INV-RTI4 worker failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let dispatches = stderr.matches("descriptor-build dispatch").count();
    assert_eq!(
        dispatches, 1,
        "INV-RTI4: expected exactly ONE descriptor-build dispatch (the accel build) across 8 \
         static frames, got {dispatches} — static instance buffers are refitting per frame \
         (instances_generation riding the accel key wrong) or the accel is rebuilding. stderr:\n{stderr}"
    );
}
