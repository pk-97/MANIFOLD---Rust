//! BUG-k1n2w (RT trace hangs GPU on alpha-masked foliage): no scene input
//! can make one RT trace command buffer run toward the GPU watchdog.
//!
//! Two mechanisms, two halves of the proof:
//!
//! - The candidate walk tests at most `RT_WALK_MAX_CANDIDATES` candidates per
//!   ray. A stack of 256 fully transparent alpha-masked cards would make an
//!   unbounded walk test all 256; the bounded walk stops at the cap and
//!   commits (closest-hit) or reports blocked (any-hit). Below the cap the
//!   walk is unchanged: an ordinary stack of cards traces identical pixels
//!   with the cap and without it.
//! - Trace tiles are sized from measured GPU time. On the pathological stack
//!   the first trace splits into small tiles, every measured buffer stays
//!   under `TRACE_BUFFER_CEILING_SECONDS`, and nothing here needs a real
//!   hang to fail.
//!
//! Fixture: the identity-camera shadow model `rt_t2c` uses. A depth image of
//! constant 0.3 reconstructs shading points across x,y in (-1, 1) at z = 0.3;
//! suns point along +z, so every shadow ray crosses the stack of cards above.

use std::slice;

use manifold_gpu::raytrace::{
    ensure_normal_sources, DebugRayQueryHit, DebugRayQueryRay, GiMaterial, MetalShadowRayTracer,
    RtAccel, RtCasterParams, RtGeometryChange, RtObjectGeometry, ShadowRayParams, ShadowRayTracer,
    RT_WALK_MAX_CANDIDATES,
};
use manifold_gpu::{
    GpuBuffer, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage, TRACE_BUFFER_CEILING_SECONDS, TRACE_RESTART_PIXELS_PER_TILE,
};

use manifold_nodes_scene::testkit::gpu_harness as harness;

#[repr(C)]
#[derive(Clone, Copy)]
struct PackedVertexUV {
    pos: [f32; 3],
    uv: [f32; 2],
}

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Cards span x,y in [-HALF, HALF]; u = (x + HALF) / (2 HALF).
const HALF: f32 = 2.0;
/// Deep enough that an unbounded walk tests four times the cap.
const PATHOLOGICAL_LAYERS: usize = 256;
const ORDINARY_LAYERS: usize = 12;

fn write_shared<T: Copy>(device: &GpuDevice, data: &[T]) -> GpuBuffer {
    let bytes = std::mem::size_of_val(data) as u64;
    let buf = device.create_buffer_shared(bytes.max(16));
    let ptr = buf.mapped_ptr().expect("shared buffer must be CPU-mapped");
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr().cast::<u8>(), ptr, bytes as usize) };
    buf
}

fn upload_f32(device: &GpuDevice, size: [u32; 2], format: GpuTextureFormat, pixels: &[f32], label: &str) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: size[0],
        height: size[1],
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label,
        mip_levels: 1,
    });
    let bytes = unsafe { slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), std::mem::size_of_val(pixels)) };
    device.upload_texture(&texture, bytes);
    texture
}

fn output(device: &GpuDevice, size: [u32; 2], label: &str) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: size[0],
        height: size[1],
        depth: 1,
        // Rgba16Float: the production trace format, and what
        // `harness::readback_rgba_f32` decodes.
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
        label,
        mip_levels: 1,
    })
}

/// `layers` cards stacked along +z from z = 1, 0.01 apart. Each card maps
/// half the texture, offset by `shift` per layer (kept inside [0, 1]) so
/// stacked texels do not line up.
fn card_stack(layers: usize, shift: f32) -> Vec<PackedVertexUV> {
    let mut verts = Vec::with_capacity(layers * 6);
    for layer in 0..layers {
        let z = 1.0 + 0.01 * layer as f32;
        let su = (shift * layer as f32).rem_euclid(0.5);
        let sv = (0.37 * shift * layer as f32).rem_euclid(0.5);
        let v = |x: f32, y: f32| PackedVertexUV {
            pos: [x, y, z],
            uv: [(x + HALF) / (4.0 * HALF) + su, (y + HALF) / (4.0 * HALF) + sv],
        };
        verts.extend_from_slice(&[
            v(-HALF, -HALF), v(HALF, -HALF), v(HALF, HALF),
            v(-HALF, -HALF), v(HALF, HALF), v(-HALF, HALF),
        ]);
    }
    verts
}

/// Square RGBA texture, alpha 1 where `opaque(x, y)`, else 0.
fn alpha_texture(device: &GpuDevice, size: u32, opaque: impl Fn(u32, u32) -> bool, label: &str) -> GpuTexture {
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            px.extend_from_slice(&[1.0, 1.0, 1.0, if opaque(x, y) { 1.0 } else { 0.0 }]);
        }
    }
    upload_f32(device, [size, size], GpuTextureFormat::Rgba32Float, &px, label)
}

fn card_object<'a>(vertices: &'a GpuBuffer, layers: usize, texture: &'a GpuTexture) -> RtObjectGeometry<'a> {
    RtObjectGeometry {
        instance_inner: RtObjectGeometry::NO_INNER,
        material_attributes: Default::default(),
        vertex_buffer: vertices,
        vertex_stride: std::mem::size_of::<PackedVertexUV>() as u32,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: (layers * 2) as u32,
        transform: IDENTITY,
        normal_offset: 0,
        uv_offset: std::mem::size_of::<[f32; 3]>() as u32,
        alpha_mask: true,
        translucent: false,
        alpha_cutoff: 0.5,
        base_color_texture: Some(texture),
        mr_texture: None,
        normal_texture: None,
        emissive_texture: None,
        extra_material_textures: [None; 15],
        emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
        emissive_uv_t: [0.0, 0.0],
        cast_shadows: true,
        instances_addr: 0,
        instances_buffer: None,
        instance_slots: 1,
        appearance_weights: None,
        appearance_gain: 1.0,
        base_color_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        mr_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        normal_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        normal_scale: 1.0,
        base_color_alpha: 1.0,
        tangent_offset: u32::MAX,
    }
}

struct Scene<'a> {
    accel: RtAccel,
    normal_sources: GpuBuffer,
    textures: manifold_gpu::raytrace::RtMaterialTextures<'a>,
    materials: GpuBuffer,
}

fn build<'a>(device: &GpuDevice, tracer: &MetalShadowRayTracer, objects: &[RtObjectGeometry<'a>]) -> Scene<'a> {
    let plan = tracer.plan_accel(device, None, objects).expect("plan accel");
    let mut slot = None;
    tracer.prepare_accel(device, &mut slot, plan).expect("prepare accel");
    let mut accel = slot.expect("prepare produces an accel");
    let mut ns_slot = None;
    let mut ns_capacity = 0usize;
    let textures = ensure_normal_sources(&mut ns_slot, &mut ns_capacity, device, objects);
    let rows = vec![GiMaterial::new([0.0; 3], [0.0; 3], [0.0; 4], [0.0; 4]); objects.len()];
    let changes = vec![RtGeometryChange::Rebuild; objects.len()];
    let mut enc = device.create_encoder("rt-alpha-walk-bound-prepare");
    tracer
        .encode_accel_update(device, &mut enc, &mut accel, objects, &changes, &rows, true, true)
        .expect("encode accel update");
    enc.commit_and_wait_completed();
    Scene {
        accel,
        normal_sources: ns_slot.expect("ensure_normal_sources must allocate"),
        textures,
        materials: device.create_buffer_shared(std::mem::size_of::<GiMaterial>() as u64),
    }
}

fn query(device: &GpuDevice, tracer: &MetalShadowRayTracer, scene: &Scene<'_>, rays: &[DebugRayQueryRay]) -> Vec<DebugRayQueryHit> {
    let mut enc = device.create_encoder("rt-alpha-walk-bound-query");
    let hits = tracer.debug_ray_query(device, &mut enc, &scene.accel, &scene.normal_sources, rays, Some(&scene.textures), 0, 5);
    enc.commit_and_wait_completed();
    let ptr = hits.mapped_ptr().expect("hit buffer must be CPU-mapped");
    unsafe { slice::from_raw_parts(ptr as *const DebugRayQueryHit, rays.len()).to_vec() }
}

/// Four soft suns leaning a little off +z, every ray inside the stack.
fn suns() -> [RtCasterParams; 4] {
    let dir = |x: f32, y: f32| {
        let len = (x * x + y * y + 1.0).sqrt();
        [x / len, y / len, 1.0 / len]
    };
    [
        RtCasterParams::new(dir(0.0, 0.0), 0.05, [1.0; 3], 0),
        RtCasterParams::new(dir(0.1, 0.0), 0.05, [1.0; 3], 0),
        RtCasterParams::new(dir(0.0, 0.1), 0.05, [1.0; 3], 0),
        RtCasterParams::new(dir(-0.1, -0.1), 0.05, [1.0; 3], 0),
    ]
}

/// Encode one production shadow trace of `size` pixels and wait for it.
/// Returns `out_sv` read back as RGBA f32.
fn trace(device: &GpuDevice, tracer: &MetalShadowRayTracer, scene: &Scene<'_>, objects: &[RtObjectGeometry<'_>], size: [u32; 2], frame_index: u32) -> Vec<f32> {
    let pixels = (size[0] * size[1]) as usize;
    let depth = upload_f32(device, size, GpuTextureFormat::Depth32Float, &vec![0.3f32; pixels], "rt-alpha-walk-depth");
    let out_sv = output(device, size, "rt-alpha-walk-out_sv");
    let stub = |label| output(device, size, label);
    let (out_sv2, out_svt, out_irr, out_n, out_refl) = (
        stub("rt-alpha-walk-sv2"), stub("rt-alpha-walk-svt"), stub("rt-alpha-walk-irr"),
        stub("rt-alpha-walk-n"), stub("rt-alpha-walk-refl"),
    );
    let env = output(device, [1, 1], "rt-alpha-walk-env");
    let casters = suns();
    let params = ShadowRayParams::new(
        &casters, 2, frame_index, size, size, 0.0, 0, 0, [0.0; 3], IDENTITY, 0, 0.6, 0.1,
        manifold_gpu::raytrace::SVT_SLOT_NONE,
    );
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);
    let dummy_emissive = harness::dummy_emissive_buffer(device);
    let mut encoder = device.create_encoder("rt-alpha-walk-trace");
    tracer.dispatch_shadow_rays(
        &mut encoder, device, &scene.accel, tracer.zero_emissive_stats(), &params, &params_buffer,
        &scene.materials, &scene.normal_sources, objects, &scene.textures, &depth,
        &out_sv, &out_sv2, &out_svt, &out_irr, &out_n, &out_refl, &env,
        &dummy_emissive, &dummy_emissive, false, "trace_shadow_rays-alpha-walk-bound",
    );
    encoder.commit_and_wait_completed();
    harness::readback_rgba_f32(device, &out_sv)
}

fn ray_up(x: f32, y: f32) -> DebugRayQueryRay {
    DebugRayQueryRay { origin: [x, y, 0.3], direction: [0.0, 0.0, 1.0], min_distance: 0.0, max_distance: 100.0 }
}

/// The bounded walk tests exactly the cap through the pathological stack and
/// commits a hit; the unbounded walk tests every card and misses.
#[test]
fn alpha_walk_stops_at_the_candidate_cap() {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let device = &h.device;
    let verts = write_shared(device, &card_stack(PATHOLOGICAL_LAYERS, 0.0));
    let clear = alpha_texture(device, 8, |_, _| false, "rt-alpha-walk-clear");
    let objects = [card_object(&verts, PATHOLOGICAL_LAYERS, &clear)];
    // Off the quad diagonal, so each card contributes exactly one triangle.
    let rays = [ray_up(0.3, -0.5), ray_up(-0.7, -1.2), ray_up(1.1, 0.4)];

    let capped = MetalShadowRayTracer::new(device);
    let scene = build(device, &capped, &objects);
    for (i, hit) in query(device, &capped, &scene, &rays).iter().enumerate() {
        assert_eq!(hit.candidates, RT_WALK_MAX_CANDIDATES, "ray {i}: the bounded walk tests exactly the cap");
        assert_eq!(hit.hit, 1, "ray {i}: past the cap a closest-hit walk commits the candidate it stopped on");
        assert!(hit.distance > 0.69 && hit.distance < 0.7 + 0.01 * PATHOLOGICAL_LAYERS as f32,
            "ray {i}: the committed hit is one of the cards, distance {}", hit.distance);
    }

    let unbounded = MetalShadowRayTracer::with_walk_cap_for_proof(device, u32::MAX);
    let scene = build(device, &unbounded, &objects);
    for (i, hit) in query(device, &unbounded, &scene, &rays).iter().enumerate() {
        assert!(hit.candidates as usize >= PATHOLOGICAL_LAYERS,
            "ray {i}: without the cap the walk tests every transparent card ({})", hit.candidates);
        assert_eq!(hit.hit, 0, "ray {i}: every card is transparent, so the unbounded walk misses");
    }
}

/// Below the cap the walk is unchanged: an ordinary stack of sparse cutout
/// cards traces bit-identical shadows with the cap and without it.
#[test]
fn alpha_walk_cap_leaves_ordinary_cards_unchanged() {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let device = &h.device;
    let verts = write_shared(device, &card_stack(ORDINARY_LAYERS, 0.173));
    // About one texel in eight is opaque leaf.
    let leaves = alpha_texture(device, 16, |x, y| (x * 7 + y * 3) % 8 == 0, "rt-alpha-walk-leaves");
    let objects = [card_object(&verts, ORDINARY_LAYERS, &leaves)];
    let size = [64, 64];

    let capped = MetalShadowRayTracer::new(device);
    let scene = build(device, &capped, &objects);
    let with_cap = trace(device, &capped, &scene, &objects, size, 3);
    let depth_rays: Vec<_> = (0..64).map(|i| ray_up(-0.95 + 0.03 * i as f32, 0.9 - 0.028 * i as f32)).collect();
    let deepest = query(device, &capped, &scene, &depth_rays).iter().map(|h| h.candidates).max().unwrap();
    assert!(deepest > 0 && deepest < RT_WALK_MAX_CANDIDATES,
        "ordinary rays stay well under the cap ({deepest} candidates)");

    let unbounded = MetalShadowRayTracer::with_walk_cap_for_proof(device, u32::MAX);
    let scene = build(device, &unbounded, &objects);
    let without_cap = trace(device, &unbounded, &scene, &objects, size, 3);

    let sv: Vec<f32> = with_cap.chunks(4).map(|p| p[0]).collect();
    let (dark, lit) = (sv.iter().filter(|&&v| v == 0.0).count(), sv.iter().filter(|&&v| v == 1.0).count());
    assert!(sv.iter().any(|&v| v > 0.0) && sv.iter().any(|&v| v < 1.0),
        "the fixture must hold lit and shadowed pixels, else the comparison is vacuous: \
         {dark} dark, {lit} lit, {} partial of {}, first texels {:?}",
        sv.len() - dark - lit, sv.len(), &with_cap[..8]);
    let differing = with_cap.iter().zip(&without_cap).filter(|(a, b)| a.to_bits() != b.to_bits()).count();
    assert_eq!(differing, 0, "capped and unbounded walks must trace identical pixels below the cap");
}

/// The pathological stack under production tiling: the first trace starts
/// from small tiles, and no measured trace buffer reaches the ceiling.
#[test]
fn alpha_walk_trace_buffers_stay_under_the_time_ceiling() {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let device = &h.device;
    let verts = write_shared(device, &card_stack(PATHOLOGICAL_LAYERS, 0.011));
    // Mostly transparent: one texel in 64 is opaque.
    let sparse = alpha_texture(device, 64, |x, y| x % 8 == 3 && y % 8 == 5, "rt-alpha-walk-sparse");
    let objects = [card_object(&verts, PATHOLOGICAL_LAYERS, &sparse)];
    let size = [512, 512];
    let tracer = MetalShadowRayTracer::new(device);
    let scene = build(device, &tracer, &objects);

    let mut first_buffers = 0;
    let mut sv = Vec::new();
    for frame in 0..4 {
        sv = trace(device, &tracer, &scene, &objects, size, frame);
        let report = tracer.poll_trace_timing();
        if frame == 0 {
            first_buffers = report.last_dispatch_buffers;
            assert_eq!(report.last_pixels_per_tile, TRACE_RESTART_PIXELS_PER_TILE);
        }
        eprintln!("[rt_alpha_walk_bound] frame {frame}: {report:?}");
    }
    let report = tracer.poll_trace_timing();
    let expected_first = (size[0] * size[1]) as u64 / TRACE_RESTART_PIXELS_PER_TILE;
    assert_eq!(u64::from(first_buffers), expected_first, "the first trace splits into restart-sized tiles");
    assert!(report.measured_buffers >= expected_first, "trace buffers were measured: {report:?}");
    assert!(report.longest_buffer_seconds > 0.0 && report.longest_buffer_seconds <= TRACE_BUFFER_CEILING_SECONDS,
        "every trace buffer stays under the {TRACE_BUFFER_CEILING_SECONDS}s ceiling: {report:?}");
    // Every shadow ray crosses 256 nearly transparent cards; whether it
    // stops on a leaf or at the cap, it reads blocked.
    assert!(sv.chunks(4).all(|p| p[0] == 0.0), "rays through the pathological stack read blocked");
}
