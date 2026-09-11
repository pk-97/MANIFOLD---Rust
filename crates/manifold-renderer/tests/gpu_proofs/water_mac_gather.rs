//! Production generated MAC gather shader compared with the independent f64
//! trilinear APIC/RK3 reference. No test-only GPU kernel is involved.
use bytemuck::Zeroable;
use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuDevice};
use manifold_renderer::node_graph::freeze::codegen::ENTRY;
use manifold_renderer::node_graph::primitives::{mac_gather_advect, mac_resolve::MacResolvedCell};
use manifold_renderer::node_graph::water::{DOMAIN_ORIGIN, GRID_SPACING, WaterParticle};

use crate::harness;

use crate::water_mac_atoms::reference;

const EDGE: usize = 65;
const ENTRIES: usize = EDGE * EDGE * EDGE;
// f32 interpolation of bounded fields and gradients (h^-1 = 16) against f64.
const VELOCITY_EPS: f64 = 1.0e-5;
const AFFINE_EPS: f64 = 1.0e-4;
const POSITION_EPS: f64 = 2.0e-6;

fn index(c: [usize; 3]) -> usize {
    c[0] + EDGE * (c[1] + EDGE * c[2])
}

fn particle(q: [f32; 3]) -> WaterParticle {
    let pos = std::array::from_fn::<_, 3, _>(|a| DOMAIN_ORIGIN[a] + GRID_SPACING * q[a]);
    WaterParticle {
        position_mass: [pos[0], pos[1], pos[2], 2.75],
        velocity_density: [9.0, 8.0, 7.0, 987.5],
        affine_x: [3.0; 4],
        affine_y: [4.0; 4],
        affine_z: [5.0; 4],
        previous_position: [6.0; 4],
    }
}

fn field(b: [f64; 3], c: [[f64; 3]; 3]) -> (Vec<MacResolvedCell>, reference::Grid) {
    let mut reference = reference::Grid::new(
        [64; 3],
        f64::from(GRID_SPACING),
        DOMAIN_ORIGIN.map(f64::from),
    );
    let mut grid = vec![MacResolvedCell::zeroed(); ENTRIES];
    for axis in 0..3 {
        let dims = reference.dims(axis);
        for z in 0..dims[2] {
            for y in 0..dims[1] {
                for x in 0..dims[0] {
                    let coordinate = [x, y, z];
                    let pos = reference.position(axis, coordinate);
                    let value = (b[axis] + (0..3).map(|d| c[axis][d] * pos[d]).sum::<f64>()) as f32;
                    let face = &mut grid[index(coordinate)];
                    face.mac_velocity[axis] = value;
                    face.mac_valid[axis] = 1.0;
                    let i = reference.index(axis, coordinate);
                    // The oracle sees the same rounded input samples as Metal.
                    reference.velocity[axis][i] = f64::from(value);
                }
            }
        }
    }
    (grid, reference)
}

struct Gather {
    device: &'static GpuDevice,
    pipeline: GpuComputePipeline,
}

impl Gather {
    fn new() -> Self {
        let device = harness::shared().device.as_ref();
        let source = mac_gather_advect::shader_source();
        Self {
            device,
            pipeline: device.create_compute_pipeline(&source, ENTRY, "gpu-proof.mac-gather"),
        }
    }

    fn run(
        &self,
        particles: &[WaterParticle],
        grid: &[MacResolvedCell],
        dt: f32,
    ) -> Vec<WaterParticle> {
        let bytes = std::mem::size_of_val(particles) as u64;
        let input = self.device.create_buffer_shared(bytes);
        let faces = self
            .device
            .create_buffer_shared(std::mem::size_of_val(grid) as u64);
        let output = self.device.create_buffer_shared(bytes);
        let uniforms = mac_gather_advect::GatherAdvectUniforms {
            step_dt: dt,
            dispatch_count: particles.len() as u32,
            _pad0: 0,
            _pad1: 0,
        };
        harness::retry_on_gpu_commit_error(|| {
            unsafe {
                input.write(0, bytemuck::cast_slice(particles));
                faces.write(0, bytemuck::cast_slice(grid));
                // Poison output so an unwritten inactive slot cannot pass.
                output.write(0, &vec![0xa5; bytes as usize]);
            }
            let mut encoder = self.device.create_encoder("gpu-proof.mac-gather");
            encoder.dispatch_compute(
                &self.pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: &input,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: &faces,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: &output,
                        offset: 0,
                    },
                ],
                [(particles.len() as u32).div_ceil(256), 1, 1],
                "gpu-proof.mac-gather",
            );
            encoder.commit_and_wait_completed();
        });
        unsafe {
            std::slice::from_raw_parts(
                output.mapped_ptr().unwrap().cast::<WaterParticle>(),
                particles.len(),
            )
            .to_vec()
        }
    }
}

fn close(actual: f32, expected: f64, tolerance: f64, label: &str) {
    assert!(
        (f64::from(actual) - expected).abs() <= tolerance,
        "{label}: {actual} != {expected} (tolerance {tolerance})"
    );
}

#[test]
fn water_mac_gather_affine_rows_and_rk3_match_f64() {
    let b = [0.2, -0.1, 0.3];
    let c = [
        [0.125, 0.25, -0.375],
        [-0.5, 0.0625, 0.1875],
        [0.3125, -0.25, 0.125],
    ];
    let (grid, reference) = field(b, c);
    let inputs = [
        particle([16.75, 24.75, 32.75]),
        particle([41.13, 37.92, 14.31]),
        particle([0.7, 1.2, 1.3]),
        particle([62.7, 62.8, 62.7]),
    ];
    let dt = mac_gather_advect::DEFAULT_STEP_DT;
    let outputs = Gather::new().run(&inputs, &grid, dt);
    for (input, output) in inputs.iter().zip(outputs) {
        let pos = std::array::from_fn(|a| f64::from(input.position_mass[a]));
        let (velocity, affine) = reference.gather(pos).unwrap();
        let next = reference.advect(pos, f64::from(dt)).unwrap();
        let rows = [output.affine_x, output.affine_y, output.affine_z];
        for axis in 0..3 {
            close(
                output.velocity_density[axis],
                velocity[axis],
                VELOCITY_EPS,
                "velocity",
            );
            close(
                output.position_mass[axis],
                next[axis],
                POSITION_EPS,
                "RK3 position",
            );
            assert_eq!(
                output.previous_position[axis].to_bits(),
                input.position_mass[axis].to_bits()
            );
            for d in 0..3 {
                close(rows[axis][d], affine[axis][d], AFFINE_EPS, "APIC gradient");
                close(rows[axis][d], c[axis][d], AFFINE_EPS, "analytic affine row");
            }
            assert_eq!(rows[axis][3], 0.0);
        }
        assert_eq!(
            output.position_mass[3].to_bits(),
            input.position_mass[3].to_bits()
        );
        assert_eq!(
            output.velocity_density[3].to_bits(),
            input.velocity_density[3].to_bits()
        );
        assert_eq!(output.previous_position[3], 0.0);
        // This varying field must distinguish RK3 from a first-order step.
        assert!((0..3).any(|a| (next[a] - pos[a] - f64::from(dt) * velocity[a]).abs() > 3.0e-6));
    }
}

#[test]
fn water_mac_gather_constant_translation_and_inactive_capacity() {
    let velocity = [1.25, -0.75, 0.5];
    let (grid, reference) = field(velocity, [[0.0; 3]; 3]);
    let mut inputs = vec![particle([30.37, 27.79, 22.53]); 258];
    inputs[256].position_mass = [f32::NAN, f32::INFINITY, -0.0, 0.0];
    inputs[256].velocity_density = [f32::from_bits(0x7fc00123); 4];
    let dt = mac_gather_advect::DEFAULT_STEP_DT;
    let outputs = Gather::new().run(&inputs, &grid, dt);
    for (input, output) in inputs.iter().zip(outputs) {
        if input.position_mass[3] == 0.0 {
            assert_eq!(bytemuck::bytes_of(&output), bytemuck::bytes_of(input));
            continue;
        }
        let pos = std::array::from_fn(|a| f64::from(input.position_mass[a]));
        let next = reference.advect(pos, f64::from(dt)).unwrap();
        for axis in 0..3 {
            close(
                output.velocity_density[axis],
                velocity[axis],
                VELOCITY_EPS,
                "constant velocity",
            );
            close(
                output.position_mass[axis],
                next[axis],
                POSITION_EPS,
                "constant translation",
            );
        }
        for value in [output.affine_x, output.affine_y, output.affine_z]
            .iter()
            .flatten()
        {
            close(*value, 0.0, AFFINE_EPS, "constant affine");
        }
    }
}

fn rejected(output: &WaterParticle, input: &WaterParticle) {
    assert!(
        output.position_mass[..3].iter().all(|v| v.is_nan()),
        "invalid candidate was accepted"
    );
    assert_eq!(
        output.position_mass[3].to_bits(),
        input.position_mass[3].to_bits()
    );
    assert_eq!(
        output.velocity_density[3].to_bits(),
        input.velocity_density[3].to_bits()
    );
}

#[test]
fn water_mac_gather_rejects_invalid_faces_positions_and_dt() {
    let gather = Gather::new();
    let (mut grid, _) = field([0.0; 3], [[0.0; 3]; 3]);
    let input = particle([16.75, 24.75, 32.75]);
    let dt = mac_gather_advect::DEFAULT_STEP_DT;
    // Legal storage padding must never be treated as a legal face stencil.
    let mut invalid = vec![
        particle([0.25, 20.0, 20.0]),
        particle([63.75, 20.0, 20.0]),
        particle([20.0, 0.25, 20.0]),
        particle([20.0, 20.0, 63.75]),
    ];
    let mut nonfinite = input;
    nonfinite.position_mass[0] = f32::INFINITY;
    invalid.push(nonfinite);
    for (output, input) in gather.run(&invalid, &grid, dt).iter().zip(&invalid) {
        rejected(output, input);
    }
    for bad_dt in [-0.01, f32::NAN, f32::INFINITY] {
        rejected(&gather.run(&[input], &grid, bad_dt)[0], &input);
    }
    let face = index([16, 24, 32]);
    for (valid, velocity) in [(0.0, 0.0), (f32::NAN, 0.0), (1.0, f32::INFINITY)] {
        grid[face].mac_valid[0] = valid;
        grid[face].mac_velocity[0] = velocity;
        rejected(&gather.run(&[input], &grid, dt)[0], &input);
    }
}

#[test]
fn water_mac_gather_rejects_missing_rk3_intermediate_support() {
    let gather = Gather::new();
    let (mut grid, _) = field([30.0, 0.0, 0.0], [[0.0; 3]; 3]);
    let input = particle([16.75, 24.75, 32.75]);
    let dt = mac_gather_advect::DEFAULT_STEP_DT;
    // Total displacement is four cells: midpoint needs x=18/19, the third
    // stage needs x=19/20. The original gather uses only x=16/17.
    for x in [18, 20] {
        grid[index([x, 24, 32])].mac_valid[0] = 0.0;
        rejected(&gather.run(&[input], &grid, dt)[0], &input);
        grid[index([x, 24, 32])].mac_valid[0] = 1.0;
    }
}
