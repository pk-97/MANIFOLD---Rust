//! SCENE_MODIFIER_RT_DESIGN.md P6/A3 — selective BLAS refit witnesses.
//!
//! The fixture keeps one resident BLAS while its vertex bytes move through
//! expansion, collapse and revival. Every update is compared with a fresh
//! build on the same bytes, and the wired-instance section proves that a
//! transform-only update refits the TLAS without touching the BLAS.

use std::slice;

use manifold_gpu::raytrace::{
    DebugRayQueryHit, DebugRayQueryRay, MetalShadowRayTracer, RtAccelError, RtGeometryChange,
    RtObjectGeometry, ShadowRayTracer, ensure_normal_sources,
};
use manifold_gpu::{GpuBuffer, GpuDevice};

use crate::harness;

#[repr(C)]
#[derive(Clone, Copy)]
struct PackedVertex {
    pos: [f32; 4],
    normal: [f32; 4],
    uv: [f32; 2],
}

const VERTEX_STRIDE: u32 = std::mem::size_of::<PackedVertex>() as u32;
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn triangle_at(cx: f32) -> [PackedVertex; 3] {
    [
        PackedVertex {
            pos: [cx - 0.25, -0.25, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0, 0.0],
            uv: [0.0, 0.0],
        },
        PackedVertex {
            pos: [cx + 0.25, -0.25, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0, 0.0],
            uv: [1.0, 0.0],
        },
        PackedVertex {
            pos: [cx, 0.25, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0, 0.0],
            uv: [0.5, 1.0],
        },
    ]
}

fn degenerate_triangle() -> [PackedVertex; 3] {
    [
        PackedVertex {
            pos: [-0.25, 0.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0, 0.0],
            uv: [0.0, 0.0],
        },
        PackedVertex {
            pos: [0.25, 0.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0, 0.0],
            uv: [1.0, 0.0],
        },
        PackedVertex {
            pos: [0.25, 0.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0, 0.0],
            uv: [0.5, 1.0],
        },
    ]
}

fn write_vertices(device: &GpuDevice, verts: &[PackedVertex; 3]) -> GpuBuffer {
    let buffer = device.create_buffer_shared(std::mem::size_of_val(verts) as u64);
    let ptr = buffer
        .mapped_ptr()
        .expect("fixture vertex buffer must be mapped");
    unsafe { std::ptr::copy_nonoverlapping(verts.as_ptr(), ptr.cast::<PackedVertex>(), 3) };
    buffer
}

fn rewrite_vertices(buffer: &GpuBuffer, verts: &[PackedVertex; 3]) {
    let ptr = buffer
        .mapped_ptr()
        .expect("fixture vertex buffer must be mapped");
    unsafe { std::ptr::copy_nonoverlapping(verts.as_ptr(), ptr.cast::<PackedVertex>(), 3) };
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InstanceTransform {
    pos_scale: [f32; 4],
    rot_pad: [f32; 4],
}

fn write_instance(buffer: &GpuBuffer, x: f32) {
    let ptr = buffer
        .mapped_ptr()
        .expect("fixture instance buffer must be mapped");
    let value = InstanceTransform {
        pos_scale: [x, 0.0, 0.0, 1.0],
        rot_pad: [0.0; 4],
    };
    unsafe { ptr.cast::<InstanceTransform>().write(value) };
}

fn object<'a>(vertex: &'a GpuBuffer, instances: Option<&'a GpuBuffer>) -> RtObjectGeometry<'a> {
    RtObjectGeometry {
        vertex_buffer: vertex,
        vertex_stride: VERTEX_STRIDE,
        vertex_offset: 0,
        index_buffer: None,
        triangle_count: 1,
        transform: IDENTITY,
        normal_offset: 16,
        uv_offset: 32,
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
        instances_addr: instances.map_or(0, |buffer| buffer.gpu_address()),
        instances_buffer: instances,
        instance_slots: u32::from(instances.is_some()),
        appearance_weights: None,
        appearance_gain: 1.0,
    }
}

fn ray_at(x: f32) -> DebugRayQueryRay {
    DebugRayQueryRay {
        origin: [x, -1.0 / 12.0, 2.0],
        direction: [0.0, 0.0, -1.0],
        min_distance: 0.0,
        max_distance: 10.0,
    }
}

fn read_hit(buffer: &GpuBuffer) -> DebugRayQueryHit {
    let ptr = buffer.mapped_ptr().expect("hit buffer must be mapped");
    unsafe { slice::from_raw_parts(ptr.cast::<DebugRayQueryHit>(), 1)[0] }
}

fn assert_numerical_parity(actual: &DebugRayQueryHit, fresh: &DebugRayQueryHit, label: &str) {
    assert_eq!(actual.hit, fresh.hit, "{label}: hit state differs");
    assert_eq!(actual.object_id, fresh.object_id, "{label}: object differs");
    if actual.hit == 1 {
        assert!(
            (actual.distance - fresh.distance).abs() <= 1e-4,
            "{label}: distance differs"
        );
        for (a, b) in actual.bary.into_iter().zip(fresh.bary) {
            assert!((a - b).abs() <= 2e-4, "{label}: barycentrics differ");
        }
        for (a, b) in actual.uv.into_iter().zip(fresh.uv) {
            assert!((a - b).abs() <= 2e-4, "{label}: UV differs");
        }
    }
}

fn fresh_build_hit(
    device: &GpuDevice,
    vertices_data: &[PackedVertex; 3],
    ray_x: f32,
    instance_x: Option<f32>,
) -> DebugRayQueryHit {
    let tracer = MetalShadowRayTracer::new(device);
    let vertices = write_vertices(device, vertices_data);
    let instances = instance_x.map(|x| {
        let buffer = device.create_buffer_shared(32);
        write_instance(&buffer, x);
        buffer
    });
    let objects = [object(&vertices, instances.as_ref())];
    let plan = tracer
        .plan_accel(device, None, &objects)
        .expect("fresh plan");
    let mut resident = None;
    tracer
        .prepare_accel(device, &mut resident, plan)
        .expect("fresh prepare");
    let mut accel = resident.expect("fresh resident");
    let mut normal_slot = None;
    let mut normal_capacity = 0;
    let textures = ensure_normal_sources(&mut normal_slot, &mut normal_capacity, device, &objects);
    let normal_sources = normal_slot.expect("fresh normal sources");
    let mut encoder = device.create_encoder("rt-refit-fresh");
    let update = tracer
        .encode_accel_update(
            device,
            &mut encoder,
            &mut accel,
            &objects,
            &[RtGeometryChange::Rebuild],
            &[],
            false,
            false,
        )
        .expect("fresh encode");
    assert_eq!(update.blas_builds, 1);
    assert_eq!(update.blas_refits, 0);
    let hit = tracer.debug_ray_query(
        device,
        &mut encoder,
        &accel,
        &normal_sources,
        &[ray_at(ray_x)],
        Some(&textures),
        0,
        0,
    );
    encoder.commit_and_wait_completed();
    read_hit(&hit)
}

#[test]
fn rt_dynamic_selective_refit_matches_fresh_build_and_instances() {
    let h = harness::shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);
    let vertices = write_vertices(device, &triangle_at(0.0));
    let objects = [object(&vertices, None)];
    let plan = tracer.plan_accel(device, None, &objects).expect("plan");
    let mut resident = None;
    tracer
        .prepare_accel(device, &mut resident, plan)
        .expect("prepare");
    let mut accel = resident.expect("resident");
    let mut normal_slot = None;
    let mut normal_capacity = 0;
    ensure_normal_sources(&mut normal_slot, &mut normal_capacity, device, &objects);
    let normal_sources = normal_slot.expect("normal sources");

    let encode_query =
        |accel: &mut manifold_gpu::raytrace::RtAccel, change: RtGeometryChange, ray_x: f32| {
            let mut encoder = device.create_encoder("rt-refit-update");
            let update = tracer
                .encode_accel_update(
                    device,
                    &mut encoder,
                    accel,
                    &objects,
                    &[change],
                    &[],
                    false,
                    false,
                )
                .expect("accel update");
            let hit = tracer.debug_ray_query(
                device,
                &mut encoder,
                accel,
                &normal_sources,
                &[ray_at(ray_x)],
                None,
                0,
                0,
            );
            encoder.commit_and_wait_completed();
            (update, read_hit(&hit))
        };

    let (initial, hit_initial) = encode_query(&mut accel, RtGeometryChange::Rebuild, 0.0);
    assert_eq!(
        (
            initial.blas_builds,
            initial.blas_refits,
            initial.tlas_builds,
            initial.tlas_refits
        ),
        (1, 0, 1, 0)
    );
    assert_eq!(hit_initial.hit, 1);

    let mut idle_encoder = device.create_encoder("rt-refit-idle");
    let idle = tracer
        .encode_accel_update(
            device,
            &mut idle_encoder,
            &mut accel,
            &objects,
            &[RtGeometryChange::Reuse],
            &[],
            false,
            false,
        )
        .expect("idle update");
    assert_eq!(idle, Default::default(), "idle must encode no AS work");
    drop(idle_encoder);

    rewrite_vertices(&vertices, &triangle_at(3.0));
    let (expanded, hit_expanded) = encode_query(&mut accel, RtGeometryChange::Refit, 3.0);
    assert_eq!(
        (
            expanded.blas_builds,
            expanded.blas_refits,
            expanded.tlas_builds,
            expanded.tlas_refits
        ),
        (0, 1, 0, 1)
    );
    assert_numerical_parity(
        &hit_expanded,
        &fresh_build_hit(device, &triangle_at(3.0), 3.0, None),
        "bounds expansion",
    );

    rewrite_vertices(&vertices, &degenerate_triangle());
    let (collapsed, hit_collapsed) = encode_query(&mut accel, RtGeometryChange::Refit, 3.0);
    assert_eq!(
        (
            collapsed.blas_builds,
            collapsed.blas_refits,
            collapsed.tlas_builds,
            collapsed.tlas_refits
        ),
        (0, 1, 0, 1)
    );
    assert_numerical_parity(
        &hit_collapsed,
        &fresh_build_hit(device, &degenerate_triangle(), 3.0, None),
        "degenerate collapse",
    );

    rewrite_vertices(&vertices, &triangle_at(-3.0));
    let (revived, hit_revived) = encode_query(&mut accel, RtGeometryChange::Refit, -3.0);
    assert_eq!(
        (
            revived.blas_builds,
            revived.blas_refits,
            revived.tlas_builds,
            revived.tlas_refits
        ),
        (0, 1, 0, 1)
    );
    assert_numerical_parity(
        &hit_revived,
        &fresh_build_hit(device, &triangle_at(-3.0), -3.0, None),
        "degenerate revival",
    );

    let instance_vertices = write_vertices(device, &triangle_at(0.0));
    let instances = device.create_buffer_shared(32);
    write_instance(&instances, 2.0);
    let instance_objects = [object(&instance_vertices, Some(&instances))];
    let plan = tracer
        .plan_accel(device, None, &instance_objects)
        .expect("instance plan");
    let mut instance_slot = None;
    tracer
        .prepare_accel(device, &mut instance_slot, plan)
        .expect("instance prepare");
    let mut instance_accel = instance_slot.expect("instance resident");
    let mut instance_normal_slot = None;
    let mut instance_normal_capacity = 0;
    ensure_normal_sources(
        &mut instance_normal_slot,
        &mut instance_normal_capacity,
        device,
        &instance_objects,
    );
    let instance_normal = instance_normal_slot.expect("instance normal sources");

    let mut encoder = device.create_encoder("rt-refit-instance-build");
    let first = tracer
        .encode_accel_update(
            device,
            &mut encoder,
            &mut instance_accel,
            &instance_objects,
            &[RtGeometryChange::Reuse],
            &[],
            true,
            false,
        )
        .expect("instance build");
    assert_eq!(
        (
            first.blas_builds,
            first.blas_refits,
            first.tlas_builds,
            first.tlas_refits
        ),
        (1, 0, 1, 0)
    );
    encoder.commit_and_wait_completed();

    write_instance(&instances, 4.0);
    let mut encoder = device.create_encoder("rt-refit-instance");
    let moved = tracer
        .encode_accel_update(
            device,
            &mut encoder,
            &mut instance_accel,
            &instance_objects,
            &[RtGeometryChange::Reuse],
            &[],
            true,
            false,
        )
        .expect("instance refit");
    assert_eq!(
        (
            moved.blas_builds,
            moved.blas_refits,
            moved.tlas_builds,
            moved.tlas_refits
        ),
        (0, 0, 0, 1)
    );
    let hit = tracer.debug_ray_query(
        device,
        &mut encoder,
        &instance_accel,
        &instance_normal,
        &[ray_at(4.0)],
        None,
        0,
        0,
    );
    encoder.commit_and_wait_completed();
    assert_numerical_parity(
        &read_hit(&hit),
        &fresh_build_hit(device, &triangle_at(0.0), 4.0, Some(4.0)),
        "instance transform",
    );
}

#[test]
fn rt_dynamic_refit_atomic_validation_and_retained_lifetime() {
    let h = harness::shared();
    let device = &h.device;
    let tracer = MetalShadowRayTracer::new(device);
    let vertices = write_vertices(device, &triangle_at(0.0));
    let objects = [object(&vertices, None)];
    let plan = tracer.plan_accel(device, None, &objects).expect("plan");
    let mut resident = None;
    tracer
        .prepare_accel(device, &mut resident, plan)
        .expect("prepare");
    let mut accel = resident.expect("resident");
    let mut encoder = device.create_encoder("rt-refit-invalid");
    assert!(matches!(
        tracer.encode_accel_update(
            device,
            &mut encoder,
            &mut accel,
            &objects,
            &[],
            &[],
            false,
            false
        ),
        Err(RtAccelError::Encode(_))
    ));
    assert!(
        accel.check_topology(&objects).is_ok(),
        "rejected input must not publish topology"
    );
    drop(encoder);

    let mut normal_slot = None;
    let mut normal_capacity = 0;
    ensure_normal_sources(&mut normal_slot, &mut normal_capacity, device, &objects);
    let normal_sources = normal_slot.expect("normal sources");
    let mut encoder = device.create_encoder("rt-refit-lifetime");
    tracer
        .encode_accel_update(
            device,
            &mut encoder,
            &mut accel,
            &objects,
            &[RtGeometryChange::Rebuild],
            &[],
            false,
            false,
        )
        .expect("build");
    let hit = tracer.debug_ray_query(
        device,
        &mut encoder,
        &accel,
        &normal_sources,
        &[ray_at(0.0)],
        None,
        0,
        0,
    );
    drop(accel);
    drop(vertices);
    encoder.commit_and_wait_completed();
    assert_eq!(
        read_hit(&hit).hit,
        1,
        "completion pins must survive teardown before commit"
    );
}
