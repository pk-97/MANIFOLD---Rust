//! GPU proofs for the foam field: its sampling against the closed form of a
//! linear field on world tiles, and its damping of the ripple detail.
use super::super::FIELD_TILES_WGSL;
use super::super::water_march_gpu_tests::shared_buffer;
use super::super::water_shade::ripple_uniforms;
use super::gpu_tests::{
    PROBE_STUB, PROBES, angle, field, lattices, made_tile, probe_normals, read_f32,
};
use super::*;
use manifold_node_engine::scene::world_tiles::READ_SIDE;

/// 64 points in the node box of the test tiles, then two over tiles the table
/// does not hold.
const INSIDE: usize = 64;
const POINTS: usize = INSIDE + 2;

/// Reads the foam field at the points in `probe_in`.
const FOAM_KERNEL: &str = "\n\
@group(0) @binding(91) var<storage, read> probe_in: array<vec4<f32>>;\n\
@group(0) @binding(90) var<storage, read_write> probe_out: array<f32>;\n\
@compute @workgroup_size(64)\n\
fn cs_probe(@builtin(global_invocation_id) id: vec3<u32>) {\n\
    let i = id.x;\n\
    if i >= arrayLength(&probe_out) { return; }\n\
    probe_out[i] = foam_coverage_at(probe_in[i].xyz);\n\
}\n";

/// Probe point `i` inside the test tiles' node box (node p at (p - 1/2)
/// cells, so y from -0.025 to 0.325 m: a point above the last node row
/// interpolates with the tile above, which the table does not hold), not on
/// a node.
fn inside_point(i: usize) -> [f64; 3] {
    let (ix, iz) = ((i % 8) as f64, (i / 8) as f64);
    let y = 0.03 + 0.29 * ((i % 8 * 3 + i / 8 * 5) % 7) as f64 / 6.0;
    [0.03 + 0.13 * ix, y, 0.02 + 0.137 * iz]
}

/// The foam cover `0.25 x + 0.5 z - 0.1` at an object-space position.
fn cover(p: [f64; 3]) -> f64 {
    0.25 * p[0] + 0.5 * p[2] - 0.1
}

/// The look's `foam_coverage_at` at the probe points, with the foam field
/// bound or not.
fn coverage(
    device: &GpuDevice,
    object: &FieldObject,
    foam: &GpuBuffer,
    points: &GpuBuffer,
    has_foam: bool,
) -> Vec<f32> {
    let uniforms =
        ripple_uniforms(object, None, has_foam, 0.0, [0.0, 0.0]).expect("a real model inverts");
    let wgsl = format!(
        "{READ_SIDE}{FIELD_TILES_WGSL}{PROBE_STUB}{}{}{FOAM_KERNEL}",
        include_str!("../shaders/look_ripple_detail.wgsl"),
        include_str!("../shaders/foam_field.wgsl"),
    );
    let pipeline = device.create_compute_pipeline(&wgsl, "cs_probe", "foam probe");
    let out = device.create_buffer_shared((POINTS * 4) as u64);
    let mut enc = device.create_encoder("foam probe");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&[0.0f32, 3.0, 0.0, 1.0]),
            },
            GpuBinding::Bytes {
                binding: 64,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 66,
                buffer: foam,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 70,
                buffer: object.tiles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 90,
                buffer: &out,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 91,
                buffer: points,
                offset: 0,
            },
        ],
        [POINTS.div_ceil(64) as u32, 1, 1],
        "foam probe",
    );
    enc.commit_and_wait_completed();
    read_f32(&out, POINTS)
}

/// A linear foam field is read back exactly (trilinear interpolation is exact
/// on it) up to the clamp to 0..1, reads zero over a tile the table does not
/// hold, and reads zero when no field is bound.
#[test]
fn foam_cover_is_the_clamped_linear_field() {
    let device = manifold_gpu::testkit::test_device();
    let lattices = lattices(&device, [0.0; 3]);
    let object = field(&lattices);
    let foam_pool = lattices.table.pool(0.0f32, |p| {
        cover(lattices.table.position(p).map(f64::from)) as f32
    });
    let foam = shared_buffer(&device, &foam_pool);

    let mut world = Vec::new();
    let mut expected = Vec::new();
    for i in 0..INSIDE {
        let p = inside_point(i).map(|c| c as f32);
        world.extend([p[0], p[1], p[2], 0.0]);
        // Rebuilt from the f32 point the GPU is given.
        expected.push(cover(p.map(f64::from)).clamp(0.0, 1.0));
    }
    // Over tiles the table does not hold: x = 1.5 is in tile 3, z = -0.6 in
    // tile -2. The field there is canonical, however large the cover is
    // nearby.
    for p in [[1.5f32, 0.2, 0.5, 0.0], [0.5, 0.2, -0.6, 0.0]] {
        world.extend(p);
        expected.push(0.0);
    }
    let points = shared_buffer(&device, &world);

    let bound = coverage(&device, &object, &foam, &points, true);
    for (i, (&got, &want)) in bound.iter().zip(&expected).enumerate() {
        assert!(
            (f64::from(got) - want).abs() < 1e-5,
            "point {i}: cover {got}, expected {want}"
        );
    }
    assert!(
        expected[..INSIDE].contains(&0.0),
        "no probe point reaches the clamp at 0"
    );
    assert!(
        expected.iter().any(|&c| c > 0.3),
        "the probe points never see a real cover"
    );

    let unbound = coverage(&device, &object, &foam, &points, false);
    assert!(
        unbound.iter().all(|&c| c == 0.0),
        "an unbound foam field reads {unbound:?}"
    );
}

/// Full cover flattens the ripples, none leaves them bit for bit.
#[test]
fn foam_damps_the_ripple_detail() {
    let device = manifold_gpu::testkit::test_device();
    let lattices = lattices(&device, [0.6, 0.0, 0.0]);
    let object = field(&lattices);
    let ripples = made_tile(&device, &object);
    let covered = shared_buffer(&device, &lattices.table.pool(0.0f32, |_| 1.0));
    let bare = shared_buffer(&device, &lattices.table.pool(0.0f32, |_| 0.0));

    let times = [2.0f32];
    let plain = &probe_normals(&device, &ripples, &object, None, &times)[0];
    let flat = &probe_normals(&device, &ripples, &object, Some(&covered), &times)[0];
    let zero = &probe_normals(&device, &ripples, &object, Some(&bare), &times)[0];

    assert_eq!(plain.len(), PROBES);
    let tilt = plain
        .iter()
        .map(|n| angle(*n, [0.0, 1.0, 0.0]))
        .sum::<f64>()
        / PROBES as f64;
    assert!(
        tilt.to_degrees() > 0.5,
        "the plain ripples tilt {} degrees",
        tilt.to_degrees()
    );
    for (i, n) in flat.iter().enumerate() {
        assert!(
            (n[0].abs() <= 1e-6) && ((n[1] - 1.0).abs() <= 1e-6) && (n[2].abs() <= 1e-6),
            "point {i}: normal {n:?} under full foam"
        );
    }
    assert_eq!(zero, plain, "zero cover must not change the normals");
}
