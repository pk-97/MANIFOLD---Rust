//! Native Metal proofs for the production trilinear MAC transfer atoms.
//!
//! The kernels are taken from the public production primitive modules.  The
//! f64 reference is kept independent in `water_linear_apic_reference.rs` and
//! supplies the expected face weights, momenta, and one-layer extrapolation.

use std::slice;

use bytemuck::{Pod, Zeroable};
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};
use manifold_renderer::node_graph::freeze::codegen::ENTRY;
use manifold_renderer::node_graph::primitives::{
    mac_extrapolate, mac_resolve, mac_scatter_mass_momentum,
};
use manifold_renderer::node_graph::water::{
    DOMAIN_ORIGIN, FAULT_INVALID_DENSITY, FAULT_OUTSIDE_DOMAIN, GRID_FIXED_SCALE, GRID_NODES,
    GRID_SPACING, WaterParticle,
};

use crate::harness;

#[path = "../water_linear_apic_reference.rs"]
pub(super) mod reference;

const PADDED_EDGE: usize = 65;
const PADDED_ENTRIES: usize = PADDED_EDGE * PADDED_EDGE * PADDED_EDGE;
const ACCUM_SLOTS: usize = PADDED_ENTRIES * 6;
const Q: f64 = GRID_FIXED_SCALE as f64;

// Three particles can contribute to a face, and each contribution is rounded
// independently.  The ideal fixed-point bound is 3 * 0.5 / Q; 1e-5 leaves
// room for the production f32 stencil arithmetic with an explicit allowance of roughly ten
// fixed-point units per face.
const ACCUM_TOLERANCE: f64 = 1.0e-5;
// The three particles contribute eight faces each, so the ideal total-rounding
// bound is 24 * 0.5 / Q ~= 1.15e-5; the remaining budget covers f32 evaluation.
const ACCOUNTING_TOLERANCE: f64 = 3.0e-5;
// The fixture's smallest nonzero face mass is 0.75 * 0.25^3, so the Q20
// denominator error remains below 2e-4 m/s for its bounded affine velocities.
const RESOLVE_TOLERANCE: f64 = 8.0e-4;

#[derive(Clone, Copy)]
struct Pipelines<'a> {
    device: &'a GpuDevice,
    scatter: &'a GpuComputePipeline,
    resolve: &'a GpuComputePipeline,
    extrapolate: &'a GpuComputePipeline,
}

fn device() -> &'static GpuDevice {
    harness::shared().device.as_ref()
}

fn with_pipelines<T>(f: impl FnOnce(Pipelines<'_>) -> T) -> T {
    let d = device();
    let scatter = d.create_compute_pipeline(
        mac_scatter_mass_momentum::WGSL,
        "cs_main",
        "gpu-proof.mac-scatter",
    );
    let resolve_source = mac_resolve::shader_source();
    let resolve = d.create_compute_pipeline(&resolve_source, ENTRY, "gpu-proof.mac-resolve");
    let extrapolate_source = mac_extrapolate::shader_source();
    let extrapolate =
        d.create_compute_pipeline(&extrapolate_source, ENTRY, "gpu-proof.mac-extrapolate");
    f(Pipelines {
        device: d,
        scatter: &scatter,
        resolve: &resolve,
        extrapolate: &extrapolate,
    })
}

fn common_index(c: [usize; 3]) -> usize {
    c[0] + PADDED_EDGE * (c[1] + PADDED_EDGE * c[2])
}

fn face_is_legal(axis: usize, c: [usize; 3]) -> bool {
    match axis {
        0 => {
            c[0] <= GRID_NODES as usize && c[1] < GRID_NODES as usize && c[2] < GRID_NODES as usize
        }
        1 => {
            c[0] < GRID_NODES as usize && c[1] <= GRID_NODES as usize && c[2] < GRID_NODES as usize
        }
        2 => {
            c[0] < GRID_NODES as usize && c[1] < GRID_NODES as usize && c[2] <= GRID_NODES as usize
        }
        _ => unreachable!(),
    }
}

fn mapped_copy<T: Pod>(buffer: &GpuBuffer, count: usize) -> Vec<T> {
    let ptr = buffer
        .mapped_ptr()
        .expect("shared GPU proof buffer must be CPU-mapped")
        .cast::<T>();
    unsafe { slice::from_raw_parts(ptr, count).to_vec() }
}

fn write<T: Pod>(buffer: &GpuBuffer, value: &[T]) {
    unsafe { buffer.write(0, bytemuck::cast_slice(value)) };
}

fn read_status(buffer: &GpuBuffer) -> u32 {
    mapped_copy::<u32>(buffer, 1)[0]
}

fn scatter_resolve(
    p: Pipelines<'_>,
    particles: &[WaterParticle],
) -> (Vec<i32>, Vec<mac_resolve::MacResolvedCell>, u32) {
    let particle_buffer = p
        .device
        .create_buffer_shared((std::mem::size_of_val(particles)) as u64);
    let accumulator = p
        .device
        .create_buffer_shared((ACCUM_SLOTS * std::mem::size_of::<i32>()) as u64);
    let status = p.device.create_buffer_shared(4);
    let output = p.device.create_buffer_shared(
        (PADDED_ENTRIES * std::mem::size_of::<mac_resolve::MacResolvedCell>()) as u64,
    );

    harness::retry_on_gpu_commit_error(|| {
        write(&particle_buffer, particles);
        accumulator.zero_fill();
        status.zero_fill();
        output.zero_fill();

        let scatter_uniforms = mac_scatter_mass_momentum::MacScatterUniforms {
            active_count: particles.len() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let resolve_uniforms = mac_resolve::MacResolveUniforms {
            dispatch_count: PADDED_ENTRIES as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let mut encoder = p.device.create_encoder("gpu-proof.mac-scatter-resolve");
        encoder.dispatch_compute(
            p.scatter,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&scatter_uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &particle_buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &accumulator,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &status,
                    offset: 0,
                },
            ],
            [(particles.len() as u32).div_ceil(256), 1, 1],
            "gpu-proof.mac-scatter",
        );
        encoder.dispatch_compute(
            p.resolve,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&resolve_uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &accumulator,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &output,
                    offset: 0,
                },
            ],
            [(PADDED_ENTRIES as u32).div_ceil(256), 1, 1],
            "gpu-proof.mac-resolve",
        );
        encoder.commit_and_wait_completed();

        (
            mapped_copy(&accumulator, ACCUM_SLOTS),
            mapped_copy(&output, PADDED_ENTRIES),
            read_status(&status),
        )
    })
}

fn extrapolate(
    p: Pipelines<'_>,
    input: &[mac_resolve::MacResolvedCell],
) -> Vec<mac_resolve::MacResolvedCell> {
    let input_buffer = p.device.create_buffer_shared(
        (PADDED_ENTRIES * std::mem::size_of::<mac_resolve::MacResolvedCell>()) as u64,
    );
    let output_buffer = p.device.create_buffer_shared(
        (PADDED_ENTRIES * std::mem::size_of::<mac_resolve::MacResolvedCell>()) as u64,
    );
    let uniforms = mac_extrapolate::ExtrapolateUniforms {
        dispatch_count: PADDED_ENTRIES as u32,
        padding: [0; 3],
    };

    harness::retry_on_gpu_commit_error(|| {
        write(&input_buffer, input);
        output_buffer.zero_fill();
        let mut encoder = p.device.create_encoder("gpu-proof.mac-extrapolate");
        encoder.dispatch_compute(
            p.extrapolate,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &input_buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &output_buffer,
                    offset: 0,
                },
            ],
            [(PADDED_ENTRIES as u32).div_ceil(256), 1, 1],
            "gpu-proof.mac-extrapolate",
        );
        encoder.commit_and_wait_completed();
        mapped_copy(&output_buffer, PADDED_ENTRIES)
    })
}

fn make_particle(
    q: [f32; 3],
    mass: f32,
    velocity: [f32; 3],
    affine: [[f32; 3]; 3],
) -> WaterParticle {
    let position: [f32; 3] =
        std::array::from_fn(|axis| DOMAIN_ORIGIN[axis] + q[axis] * GRID_SPACING);
    WaterParticle {
        position_mass: [position[0], position[1], position[2], mass],
        velocity_density: [velocity[0], velocity[1], velocity[2], 1000.0],
        affine_x: [affine[0][0], affine[0][1], affine[0][2], 0.0],
        affine_y: [affine[1][0], affine[1][1], affine[1][2], 0.0],
        affine_z: [affine[2][0], affine[2][1], affine[2][2], 0.0],
        previous_position: [position[0], position[1], position[2], 0.0],
    }
}

fn affine_fixture() -> Vec<WaterParticle> {
    vec![
        make_particle(
            [12.75, 16.75, 20.75],
            0.75,
            [1.2, -0.8, 0.6],
            [
                [0.20, 0.11, -0.07],
                [-0.15, 0.30, 0.09],
                [0.12, -0.05, 0.18],
            ],
        ),
        make_particle(
            [13.75, 17.75, 21.75],
            1.25,
            [-0.4, 0.7, 1.1],
            [
                [-0.08, 0.21, 0.13],
                [0.17, -0.12, 0.24],
                [-0.20, 0.06, 0.10],
            ],
        ),
        make_particle(
            [14.75, 18.75, 22.75],
            1.75,
            [0.3, 1.4, -0.9],
            [
                [0.09, -0.16, 0.22],
                [0.04, 0.19, -0.14],
                [0.16, 0.08, -0.11],
            ],
        ),
    ]
}

fn to_reference(p: &WaterParticle) -> reference::Particle {
    reference::Particle {
        p: [
            f64::from(p.position_mass[0]),
            f64::from(p.position_mass[1]),
            f64::from(p.position_mass[2]),
        ],
        v: [
            f64::from(p.velocity_density[0]),
            f64::from(p.velocity_density[1]),
            f64::from(p.velocity_density[2]),
        ],
        c: [
            [
                f64::from(p.affine_x[0]),
                f64::from(p.affine_x[1]),
                f64::from(p.affine_x[2]),
            ],
            [
                f64::from(p.affine_y[0]),
                f64::from(p.affine_y[1]),
                f64::from(p.affine_y[2]),
            ],
            [
                f64::from(p.affine_z[0]),
                f64::from(p.affine_z[1]),
                f64::from(p.affine_z[2]),
            ],
        ],
        m: f64::from(p.position_mass[3]),
    }
}

fn reference_grid(particles: &[WaterParticle]) -> reference::Grid {
    let mut grid = reference::Grid::new(
        [GRID_NODES as usize; 3],
        f64::from(GRID_SPACING),
        DOMAIN_ORIGIN.map(f64::from),
    );
    let particles: Vec<_> = particles.iter().map(to_reference).collect();
    grid.transfer(&particles)
        .expect("affine fixture fits all MAC faces");
    grid
}

#[test]
fn mac_scatter_and_resolve_match_trilinear_reference() {
    with_pipelines(|p| {
        let particles = affine_fixture();
        let (accumulator, resolved, status) = scatter_resolve(p, &particles);
        assert_eq!(status, 0, "valid affine fixture raised status {status:#x}");
        let reference = reference_grid(&particles);

        let mut expected_mass_total = [0.0; 3];
        let mut expected_momentum_total = [0.0; 3];
        let mut actual_mass_total = [0.0; 3];
        let mut actual_momentum_total = [0.0; 3];

        for z in 0..PADDED_EDGE {
            for y in 0..PADDED_EDGE {
                for x in 0..PADDED_EDGE {
                    let c = [x, y, z];
                    let padded = common_index(c);
                    for axis in 0..3 {
                        let mass_slot = padded * 6 + axis * 2;
                        let momentum_slot = mass_slot + 1;
                        if !face_is_legal(axis, c) {
                            assert_eq!(
                                accumulator[mass_slot], 0,
                                "padded mass at {c:?}, axis {axis}"
                            );
                            assert_eq!(
                                accumulator[momentum_slot], 0,
                                "padded momentum at {c:?}, axis {axis}"
                            );
                            assert_eq!(resolved[padded].mac_velocity[axis], 0.0);
                            assert_eq!(resolved[padded].mac_valid[axis], 0.0);
                            continue;
                        }

                        let reference_index = reference.index(axis, c);
                        let expected_mass = reference.weight[axis][reference_index];
                        let expected_momentum =
                            expected_mass * reference.velocity[axis][reference_index];
                        let actual_mass = f64::from(accumulator[mass_slot]) / Q;
                        let actual_momentum = f64::from(accumulator[momentum_slot]) / Q;
                        assert!(
                            (actual_mass - expected_mass).abs() <= ACCUM_TOLERANCE,
                            "face {c:?} axis {axis} mass: {actual_mass} != {expected_mass}"
                        );
                        assert!(
                            (actual_momentum - expected_momentum).abs() <= ACCUM_TOLERANCE,
                            "face {c:?} axis {axis} momentum: {actual_momentum} != {expected_momentum}"
                        );
                        actual_mass_total[axis] += actual_mass;
                        actual_momentum_total[axis] += actual_momentum;
                        expected_mass_total[axis] += expected_mass;
                        expected_momentum_total[axis] += expected_momentum;

                        if expected_mass > 0.0 {
                            assert_eq!(resolved[padded].mac_valid[axis], 1.0);
                            let expected_velocity = expected_momentum / expected_mass;
                            assert!(
                                (f64::from(resolved[padded].mac_velocity[axis])
                                    - expected_velocity)
                                    .abs()
                                    <= RESOLVE_TOLERANCE,
                                "face {c:?} axis {axis} velocity: {} != {expected_velocity}",
                                resolved[padded].mac_velocity[axis]
                            );
                        } else {
                            assert_eq!(resolved[padded].mac_velocity[axis], 0.0);
                            assert_eq!(resolved[padded].mac_valid[axis], 0.0);
                        }
                    }
                    assert_eq!(resolved[padded].mac_velocity[3], 0.0);
                    assert_eq!(resolved[padded].mac_valid[3], 0.0);
                }
            }
        }

        for axis in 0..3 {
            let expected_mass = particles
                .iter()
                .map(|particle| f64::from(particle.position_mass[3]))
                .sum::<f64>();
            let expected_momentum = particles
                .iter()
                .map(|particle| {
                    f64::from(particle.position_mass[3])
                        * f64::from(particle.velocity_density[axis])
                })
                .sum::<f64>();
            assert!(
                (actual_mass_total[axis] - expected_mass_total[axis]).abs() <= ACCOUNTING_TOLERANCE
            );
            assert!(
                (actual_momentum_total[axis] - expected_momentum_total[axis]).abs()
                    <= ACCOUNTING_TOLERANCE
            );
            // Partition of unity makes each axis's total mass exact, and the
            // affine term sums to zero because the face positions reproduce
            // the particle position under trilinear weights.
            assert!((actual_mass_total[axis] - expected_mass).abs() <= ACCOUNTING_TOLERANCE);
            assert!(
                (actual_momentum_total[axis] - expected_momentum).abs() <= ACCOUNTING_TOLERANCE
            );
        }
    });
}

#[test]
fn mac_scatter_skips_zero_mass_and_faults_invalid_particles_without_writes() {
    with_pipelines(|p| {
        let mut zero = WaterParticle::zeroed();
        zero.position_mass = [f32::NAN, f32::NAN, f32::NAN, 0.0];
        zero.velocity_density = [f32::NAN; 4];
        let (accumulator, _, status) = scatter_resolve(p, &[zero]);
        assert_eq!(
            status, 0,
            "inactive zero-mass slot raised status {status:#x}"
        );
        assert!(accumulator.iter().all(|value| *value == 0));

        let negative = make_particle([12.75, 16.75, 20.75], -1.0, [1.0, 2.0, 3.0], [[0.0; 3]; 3]);
        let (accumulator, _, status) = scatter_resolve(p, &[negative]);
        assert_eq!(
            status, FAULT_INVALID_DENSITY,
            "negative mass status {status:#x}"
        );
        assert!(accumulator.iter().all(|value| *value == 0));

        let outside = make_particle([0.25, 0.25, 0.25], 1.0, [1.0, 2.0, 3.0], [[0.0; 3]; 3]);
        let (accumulator, _, status) = scatter_resolve(p, &[outside]);
        assert_eq!(
            status, FAULT_OUTSIDE_DOMAIN,
            "outside particle status {status:#x}"
        );
        assert!(accumulator.iter().all(|value| *value == 0));
    });
}

#[test]
fn mac_extrapolates_one_layer_preserves_known_faces_and_clears_padding() {
    with_pipelines(|p| {
        let mut input = vec![mac_resolve::MacResolvedCell::zeroed(); PADDED_ENTRIES];
        let mut reference = reference::Grid::new(
            [GRID_NODES as usize; 3],
            f64::from(GRID_SPACING),
            DOMAIN_ORIGIN.map(f64::from),
        );
        let mut valid: [Vec<bool>; 3] =
            std::array::from_fn(|axis| vec![false; reference.velocity[axis].len()]);

        for (axis, valid_axis) in valid.iter_mut().enumerate() {
            for (c, velocity) in [
                ([30, 31, 31], 1.5 + axis as f32),
                ([32, 31, 31], 5.5 + axis as f32),
            ] {
                let common = common_index(c);
                input[common].mac_velocity[axis] = velocity;
                input[common].mac_valid[axis] = 1.0;
                let index = reference.index(axis, c);
                reference.velocity[axis][index] = f64::from(velocity);
                valid_axis[index] = true;
            }
        }
        reference.extrapolate(&mut valid, 1);

        let output = extrapolate(p, &input);
        for z in 0..PADDED_EDGE {
            for y in 0..PADDED_EDGE {
                for x in 0..PADDED_EDGE {
                    let c = [x, y, z];
                    let common = common_index(c);
                    for (axis, valid_axis) in valid.iter().enumerate() {
                        if !face_is_legal(axis, c) {
                            assert_eq!(
                                output[common].mac_velocity[axis], 0.0,
                                "padding velocity {c:?} axis {axis}"
                            );
                            assert_eq!(
                                output[common].mac_valid[axis], 0.0,
                                "padding validity {c:?} axis {axis}"
                            );
                            continue;
                        }
                        let index = reference.index(axis, c);
                        let expected_valid = if valid_axis[index] { 1.0 } else { 0.0 };
                        let expected_velocity = reference.velocity[axis][index] as f32;
                        assert_eq!(
                            output[common].mac_valid[axis], expected_valid,
                            "validity {c:?} axis {axis}"
                        );
                        assert!(
                            (output[common].mac_velocity[axis] - expected_velocity).abs() <= 1.0e-6,
                            "velocity {c:?} axis {axis}: {} != {expected_velocity}",
                            output[common].mac_velocity[axis]
                        );
                        if input[common].mac_valid[axis] > 0.0 {
                            assert_eq!(
                                output[common].mac_velocity[axis].to_bits(),
                                input[common].mac_velocity[axis].to_bits(),
                                "known face changed at {c:?} axis {axis}"
                            );
                        }
                    }
                    assert_eq!(output[common].mac_velocity[3], 0.0);
                    assert_eq!(output[common].mac_valid[3], 0.0);
                }
            }
        }
    });
}
