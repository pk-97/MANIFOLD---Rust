//! GPU proofs for the foam field: its sampling against the closed form of a
//! linear field, and its damping of the ripple detail.
use super::super::water_shade::ripple_uniforms;
use super::gpu_tests::{
    PROBE_STUB, PROBES, angle, field, lattices, made_tile, probe_normals, read_f32,
};
use super::*;

/// 64 points in a 9 x 5 x 9 lattice on a 1 m cube centred on the origin.
const POINTS: usize = 64;
const FOAM_NODES: [u32; 3] = [9, 5, 9];

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

fn shared_f32(device: &GpuDevice, values: &[f32]) -> GpuBuffer {
    let buffer = device.create_buffer_shared((values.len() * 4) as u64);
    unsafe {
        buffer.write(0, bytemuck::cast_slice(values));
    }
    buffer
}

/// Distance from the lattice's min corner along each axis, in metres, of the
/// probe point `i`: inside the cube, and not on a node.
fn local_point(i: usize) -> [f64; 3] {
    let (ix, iz) = ((i % 8) as f64, (i / 8) as f64);
    let y = 0.05 + 0.9 * ((i % 8 * 3 + i / 8 * 5) % 7) as f64 / 6.0;
    [0.03 + 0.134 * ix, y, 0.02 + 0.137 * iz]
}

/// The foam cover `0.25 x + 0.5 z - 0.1` at a distance from the min corner, in metres.
fn cover(local: [f64; 3]) -> f64 {
    0.25 * local[0] + 0.5 * local[2] - 0.1
}

/// The look's `foam_coverage_at` at the probe points, with the foam field bound or not.
fn coverage(device: &GpuDevice, foam: &GpuBuffer, points: &GpuBuffer, has_foam: bool) -> Vec<f32> {
    let levelset = device.create_buffer_shared(16);
    let velocity = device.create_buffer_shared(16);
    let mut object = field(&levelset, &velocity);
    object.nodes = FOAM_NODES;
    object.size = [1.0; 3];
    let uniforms =
        ripple_uniforms(&object, None, has_foam, 0.0, [0.0, 0.0]).expect("a real model inverts");
    let wgsl = format!(
        "{PROBE_STUB}{}{}{FOAM_KERNEL}",
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
        [(POINTS / 64) as u32, 1, 1],
        "foam probe",
    );
    enc.commit_and_wait_completed();
    read_f32(&out, POINTS)
}

/// A linear foam field is read back exactly (trilinear interpolation is exact
/// on it) up to the clamp to 0..1, and reads zero when no field is bound.
#[test]
fn foam_cover_is_the_clamped_linear_field() {
    let device = manifold_gpu::testkit::test_device();
    let mut values = Vec::new();
    for iz in 0..FOAM_NODES[2] {
        for _iy in 0..FOAM_NODES[1] {
            for ix in 0..FOAM_NODES[0] {
                let local = [
                    f64::from(ix) / f64::from(FOAM_NODES[0] - 1),
                    0.0,
                    f64::from(iz) / f64::from(FOAM_NODES[2] - 1),
                ];
                values.push(cover(local) as f32);
            }
        }
    }
    let foam = shared_f32(&device, &values);

    // The lattice's min corner is (-0.5, -0.5, -0.5).
    let mut world = Vec::new();
    let mut expected = Vec::new();
    for i in 0..POINTS {
        let local = local_point(i);
        let p = local.map(|c| (c - 0.5) as f32);
        world.extend([p[0], p[1], p[2], 0.0]);
        // Rebuilt from the f32 point the GPU is given.
        let exact = [
            f64::from(p[0]) + 0.5,
            f64::from(p[1]) + 0.5,
            f64::from(p[2]) + 0.5,
        ];
        expected.push(cover(exact).clamp(0.0, 1.0));
    }
    let points = shared_f32(&device, &world);

    let bound = coverage(&device, &foam, &points, true);
    for (i, (&got, &want)) in bound.iter().zip(&expected).enumerate() {
        assert!(
            (f64::from(got) - want).abs() < 1e-5,
            "point {i}: cover {got}, expected {want}"
        );
    }
    assert!(
        expected.contains(&0.0),
        "no probe point reaches the clamp at 0"
    );
    assert!(
        expected.iter().any(|&c| c > 0.3),
        "the probe points never see a real cover"
    );

    let unbound = coverage(&device, &foam, &points, false);
    assert!(
        unbound.iter().all(|&c| c == 0.0),
        "an unbound foam field reads {unbound:?}"
    );
}

/// Full cover flattens the ripples, none leaves them bit for bit.
#[test]
fn foam_damps_the_ripple_detail() {
    let device = manifold_gpu::testkit::test_device();
    let (levelset, velocity) = lattices(&device, [0.6, 0.0, 0.0]);
    let object = field(&levelset, &velocity);
    let ripples = made_tile(&device, &object);
    let count = (super::gpu_tests::NODES.iter().product::<u32>()) as usize;
    let covered = shared_f32(&device, &vec![1.0; count]);
    let bare = shared_f32(&device, &vec![0.0; count]);

    let times = [2.0f32];
    let plain = &probe_normals(&device, &ripples, &object, &velocity, None, &times)[0];
    let flat = &probe_normals(
        &device,
        &ripples,
        &object,
        &velocity,
        Some(&covered),
        &times,
    )[0];
    let zero = &probe_normals(&device, &ripples, &object, &velocity, Some(&bare), &times)[0];

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
