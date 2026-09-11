//! Native density reconstruction against an independent f64 ellipsoid oracle.
//! The oracle scans every particle and inverts its absolute full-support axis matrix;
//! linked bins supply GPU inputs only and do not determine expected support.
use bytemuck::{Pod, Zeroable};
use half::f16;
use manifold_gpu::{
    GpuBinding, GpuComputePipeline, GpuDevice, GpuTextureDesc, GpuTextureDimension,
    GpuTextureFormat, GpuTextureUsage,
};
use manifold_renderer::node_graph::freeze::codegen::{ENTRY, VOLUME_WORKGROUP_3D};
use manifold_renderer::node_graph::primitives::water_density_field;
use manifold_renderer::node_graph::water::WaterParticle;

use crate::harness;

const N: u32 = 64;
const ORIGIN: [f64; 3] = [-2.0, 0.0, -2.0];
const RADIUS: f32 = 0.0625;
// fp16 normal values round by <= 0.05%; 0.2% also covers f32 matrix/shape
// evaluation. The absolute allowance covers fp16 subnormal quantization.
const DENSITY_REL_EPS: f64 = 0.002;
const DENSITY_ABS_EPS: f64 = 2.0e-7;
const FOAM_EPS: f64 = 0.002;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Shape {
    center_radius: [f32; 4],
    axis_x: [f32; 4],
    axis_y: [f32; 4],
    axis_z: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<Shape>() == 64);

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(&a, b)| a * b).sum()
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn shape(center: [f64; 3], axes: [[f64; 3]; 3], density: f64, original: [f64; 3]) -> Shape {
    let record = |v: [f64; 3], w: f64| [v[0] as f32, v[1] as f32, v[2] as f32, w as f32];
    let max_radius = axes.iter().map(|&a| dot(a, a).sqrt()).fold(0.0, f64::max);
    Shape {
        center_radius: record(center, max_radius),
        axis_x: record(axes[0], density),
        axis_y: record(
            axes[1],
            max_radius
                + dot(
                    std::array::from_fn(|d| center[d] - original[d]),
                    std::array::from_fn(|d| center[d] - original[d]),
                )
                .sqrt(),
        ),
        axis_z: record(axes[2], 0.0),
    }
}

fn particle(position: [f32; 3], mass: f32) -> WaterParticle {
    WaterParticle {
        position_mass: [position[0], position[1], position[2], mass],
        velocity_density: [1.0, -2.0, 3.0, 1000.0],
        affine_x: [0.1, 0.2, 0.3, 0.0],
        affine_y: [-0.1, 0.4, 0.2, 0.0],
        affine_z: [0.3, -0.2, 0.5, 0.0],
        previous_position: [
            position[0] - 0.01,
            position[1] + 0.02,
            position[2] - 0.03,
            0.0,
        ],
    }
}

fn bin(position: [f64; 3]) -> [usize; 3] {
    std::array::from_fn(|d| {
        let coordinate = ((position[d] - ORIGIN[d]) / 0.125).floor();
        assert!((0.0..32.0).contains(&coordinate));
        coordinate as usize
    })
}

struct Fixture {
    particles: Vec<WaterParticle>,
    foam: Vec<f32>,
    shapes: Vec<Shape>,
    heads: Vec<u32>,
    next: Vec<u32>,
}

fn fixture() -> Fixture {
    let particles = vec![
        // Shift plus the absolute 0.3 m long axis reaches two bins away.
        particle([0.015625, 0.515625, 0.015625], 0.03),
        particle([-0.75, 1.515, 0.70], 0.025),
        // Overlaps the first particle, with distinct foam and mass.
        particle([0.046875, 0.5625, 0.046875], 0.018),
        // Linked deliberately despite zero mass: neither density nor foam.
        particle([-1.0, 2.5, -1.0], 0.0),
    ];
    let (s, c) = std::f64::consts::FRAC_PI_4.sin_cos();
    let shapes = vec![
        shape(
            [0.125625, 0.515625, 0.015625],
            [[0.3, 0.0, 0.0], [0.0, 0.075, 0.0], [0.0, 0.0, 0.075]],
            420.0,
            [0.015625, 0.515625, 0.015625],
        ),
        shape(
            [-0.71875, 1.53125, 0.71875],
            [
                [0.28 * c, 0.28 * s, 0.0],
                [-0.07 * s, 0.07 * c, 0.0],
                [0.0, 0.0, 0.07],
            ],
            730.0,
            [-0.75, 1.515, 0.70],
        ),
        Shape::zeroed(),
        Shape::zeroed(),
    ];
    let mut heads = vec![0; 32 * 32 * 32];
    let mut next = vec![0; particles.len()];
    for (i, p) in particles.iter().enumerate() {
        let c = bin(std::array::from_fn(|a| f64::from(p.position_mass[a])));
        let index = c[0] + 32 * (c[1] + 32 * c[2]);
        next[i] = heads[index];
        heads[index] = i as u32 + 1;
    }
    Fixture {
        particles,
        foam: vec![0.2, 0.55, 0.85, 1.0],
        shapes,
        heads,
        next,
    }
}

/// Direct reciprocal-basis inversion of the supplied full support axes.
/// No geometric-mean normalization: both support and det(A) are physical.
fn kernel_weight(particle: &WaterParticle, shape: Option<&Shape>, world: [f64; 3]) -> f64 {
    let mass = f64::from(particle.position_mass[3]);
    if mass <= 0.0 {
        return 0.0;
    }
    let radius = f64::from(RADIUS);
    let (q, determinant, density) = if let Some(shape) = shape.filter(|s| s.center_radius[3] > 0.0)
    {
        let a = [shape.axis_x, shape.axis_y, shape.axis_z]
            .map(|v| [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])]);
        let determinant = dot(a[0], cross(a[1], a[2]));
        let delta = std::array::from_fn(|d| world[d] - f64::from(shape.center_radius[d]));
        let inverse_rows = [cross(a[1], a[2]), cross(a[2], a[0]), cross(a[0], a[1])];
        let q2 = inverse_rows
            .iter()
            .map(|&row| (dot(row, delta) / determinant).powi(2))
            .sum::<f64>();
        (q2.sqrt(), determinant.abs(), f64::from(shape.axis_x[3]))
    } else {
        let distance2: f64 = (0..3)
            .map(|d| (world[d] - f64::from(particle.position_mass[d])).powi(2))
            .sum();
        (distance2.sqrt() / radius, radius.powi(3), 1000.0)
    };
    // Express the standard support-2 cubic at argument 2q. This deliberately
    // differs algebraically from the shader's support-1 polynomial branches.
    let u = 2.0 * q;
    let cubic = if u < 1.0 {
        ((2.0 - u).powi(3) - 4.0 * (1.0 - u).powi(3)) / 4.0
    } else if u < 2.0 {
        (2.0 - u).powi(3) / 4.0
    } else {
        0.0
    };
    mass / density * 8.0 / (std::f64::consts::PI * determinant) * cubic
}

fn reach_source() -> String {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/presets/WaterWaveTankApic.json"))
            .expect("APIC preset JSON");
    let surface = fixture["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"].as_u64() == Some(40))
        .expect("surface group 40");
    let reduction = surface["group"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"].as_u64() == Some(54))
        .expect("reach reduction 54");
    assert_eq!(reduction["typeId"], "node.wgsl_compute");
    reduction["wgslSource"]
        .as_str()
        .expect("production reach shader")
        .to_owned()
}

struct Reconstruction {
    device: &'static GpuDevice,
    pipeline: GpuComputePipeline,
    reach_pipeline: GpuComputePipeline,
}

impl Reconstruction {
    fn new() -> Self {
        let device = harness::shared().device.as_ref();
        Self {
            device,
            reach_pipeline: device.create_compute_pipeline(
                &reach_source(),
                ENTRY,
                "water-reach-proof",
            ),
            pipeline: device.create_compute_pipeline(
                &water_density_field::shader_source(),
                ENTRY,
                "water-reconstruction-proof",
            ),
        }
    }

    fn run(&self, fixture: &Fixture, shapes: &[Shape]) -> Vec<[u16; 4]> {
        // Keep expected bytes independently of the mapped GPU buffers.
        let expected_inputs = [
            bytemuck::cast_slice(fixture.particles.as_slice()).to_vec(),
            bytemuck::cast_slice(fixture.heads.as_slice()).to_vec(),
            bytemuck::cast_slice(fixture.next.as_slice()).to_vec(),
            bytemuck::cast_slice(fixture.foam.as_slice()).to_vec(),
            bytemuck::cast_slice(shapes).to_vec(),
        ];
        let inputs = expected_inputs
            .each_ref()
            .map(|bytes| self.device.create_buffer_shared(bytes.len() as u64));
        let reach = self.device.create_buffer_shared(4);
        let reach_uniforms = [1_u32, 0, 0, 0];
        let texture = self.device.create_texture(&GpuTextureDesc {
            width: N,
            height: N,
            depth: N,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D3,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "water-reconstruction-proof",
            mip_levels: 1,
        });
        let readback = self.device.create_buffer_shared(u64::from(N * N * N * 8));
        let uniforms = [N, N, RADIUS.to_bits(), 0];
        harness::retry_on_gpu_commit_error(|| {
            for (buffer, bytes) in inputs.iter().zip(&expected_inputs) {
                unsafe {
                    buffer.write(0, bytes);
                }
            }
            let mut encoder = self.device.create_encoder("water-reconstruction-proof");
            // Same reduction source and dispatch as the production graph.
            encoder.dispatch_compute(
                &self.reach_pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&reach_uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: &inputs[4],
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: &reach,
                        offset: 0,
                    },
                ],
                [1, 1, 1],
                "water-reach-proof",
            );
            encoder.dispatch_compute(
                &self.pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: &inputs[0],
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: &inputs[1],
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: &inputs[2],
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 4,
                        buffer: &inputs[3],
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 5,
                        buffer: &inputs[4],
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 6,
                        buffer: &reach,
                        offset: 0,
                    },
                    GpuBinding::Texture {
                        binding: 7,
                        texture: &texture,
                    },
                ],
                [N.div_ceil(VOLUME_WORKGROUP_3D); 3],
                "water-reconstruction-proof",
            );
            encoder.copy_texture_3d_to_buffer(&texture, &readback, N, N, N, N * 8);
            encoder.commit_and_wait_completed();
        });
        let actual_reach = unsafe { *reach.mapped_ptr().unwrap().cast::<u32>() };
        let expected_reach = shapes.iter().map(|s| s.axis_y[3]).fold(0.0_f32, f32::max);
        assert_eq!(
            actual_reach,
            expected_reach.to_bits(),
            "reduction or density modified reach"
        );
        for (i, (buffer, expected)) in inputs.iter().zip(&expected_inputs).enumerate() {
            let actual =
                unsafe { std::slice::from_raw_parts(buffer.mapped_ptr().unwrap(), expected.len()) };
            assert_eq!(
                actual,
                expected.as_slice(),
                "GPU input {i} changed during reconstruction"
            );
        }
        unsafe {
            std::slice::from_raw_parts(
                readback.mapped_ptr().unwrap().cast::<[u16; 4]>(),
                (N * N * N) as usize,
            )
            .to_vec()
        }
    }
}

fn verify(output: &[[u16; 4]], fixture: &Fixture, shapes: &[Shape], require_far_support: bool) {
    let mut positive = 0;
    let mut rotated_positive = 0;
    let mut far_positive = 0;
    let mut mixed_foam = 0;
    let original_bin = bin(std::array::from_fn(|d| {
        f64::from(fixture.particles[0].position_mass[d])
    }));
    for (i, bits) in output.iter().enumerate() {
        let c = [
            i % N as usize,
            (i / N as usize) % N as usize,
            i / (N * N) as usize,
        ];
        let world = std::array::from_fn(|d| ORIGIN[d] + (c[d] as f64 + 0.5) * 4.0 / f64::from(N));
        let weights: [f64; 4] =
            std::array::from_fn(|p| kernel_weight(&fixture.particles[p], shapes.get(p), world));
        let density = weights.iter().sum::<f64>();
        let foam_sum: f64 = weights
            .iter()
            .zip(&fixture.foam)
            .map(|(&w, &f)| w * f64::from(f))
            .sum();
        let actual = bits.map(|value| f16::from_bits(value).to_f64());
        assert!(
            actual.iter().all(|v| v.is_finite()),
            "nonfinite output at {c:?}: {actual:?}"
        );
        assert!(
            (actual[0] - density).abs() <= DENSITY_ABS_EPS + DENSITY_REL_EPS * density,
            "density at {c:?}: {} != {density}",
            actual[0]
        );
        if density > 1.0e-6 {
            positive += 1;
            assert!(actual[0] > 0.0, "positive oracle density vanished at {c:?}");
            assert!(
                (actual[1] - foam_sum / density).abs() <= FOAM_EPS,
                "foam at {c:?}: {} != {}",
                actual[1],
                foam_sum / density
            );
        } else if density == 0.0 {
            assert_eq!(actual[0], 0.0, "density outside every kernel at {c:?}");
            assert_eq!(actual[1], 0.0, "foam outside every kernel at {c:?}");
        }
        assert_eq!(actual[2], 0.0);
        assert_eq!(actual[3], 0.0);
        if weights[1] > 1.0e-6 {
            rotated_positive += 1;
        }
        let beyond_one_bin = bin(world)
            .iter()
            .zip(original_bin)
            .any(|(&a, b)| a.abs_diff(b) > 1);
        let original_bin_distance_sq: f64 = (0..3)
            .map(|d| {
                let lo = ORIGIN[d] + original_bin[d] as f64 * 0.125;
                let nearest = world[d].clamp(lo, lo + 0.125);
                (world[d] - nearest).powi(2)
            })
            .sum();
        if weights[0] > 1.0e-6
            && beyond_one_bin
            && original_bin_distance_sq > f64::from(RADIUS).powi(2)
        {
            far_positive += 1;
        }
        if weights[0] > 1.0e-5 && weights[2] > 1.0e-5 {
            mixed_foam += 1;
        }
    }
    assert!(
        positive >= if require_far_support { 15 } else { 4 },
        "fixture is undersampled: {positive} positive voxels"
    );
    assert!(
        rotated_positive >= if require_far_support { 4 } else { 1 },
        "rotated kernel is undersampled: {rotated_positive}"
    );
    assert!(
        mixed_foam > 0,
        "foam oracle never blended distinct particles"
    );
    if require_far_support {
        assert!(
            far_positive > 0,
            "no positive voxel outside the original +/-one-bin reach"
        );
    }
}

#[test]
fn water_reconstruction_rotated_shifted_kernels_match_f64_and_preserve_gpu_inputs() {
    let fixture = fixture();
    assert_eq!(
        fixture.shapes.len(),
        fixture.particles.len(),
        "wired shapes cover every particle slot"
    );
    for (shape, particle) in fixture.shapes.iter().zip(&fixture.particles) {
        if shape.center_radius[3] == 0.0 {
            continue;
        }
        let axes = [shape.axis_x, shape.axis_y, shape.axis_z]
            .map(|a| [f64::from(a[0]), f64::from(a[1]), f64::from(a[2])]);
        let sizes = axes.map(|a| dot(a, a).sqrt());
        assert!(
            sizes.iter().copied().fold(0.0, f64::max)
                / sizes.iter().copied().fold(f64::INFINITY, f64::min)
                <= 4.000001
        );
        let determinant = dot(axes[0], cross(axes[1], axes[2])).abs();
        assert!(
            (determinant / f64::from(RADIUS).powi(3) - 1.0).abs() > 1.0,
            "fixture would not expose accidental determinant normalization"
        );
        assert!(shape.axis_x[3] > 0.0 && shape.axis_x[3] != 1000.0);
        let shift = std::array::from_fn(|d| {
            f64::from(shape.center_radius[d]) - f64::from(particle.position_mass[d])
        });
        assert!(dot(shift, shift).sqrt() <= 0.125);
    }
    let output = Reconstruction::new().run(&fixture, &fixture.shapes);
    verify(&output, &fixture, &fixture.shapes, true);
}

#[test]
fn water_reconstruction_absent_shapes_match_spherical_oracle() {
    let fixture = fixture();
    let reconstruction = Reconstruction::new();
    // Exactly the runtime's single 64-byte unwired sentinel, deliberately
    // shorter than the particle array. No shape entry may be read past it.
    let absent = [Shape::zeroed()];
    let output = reconstruction.run(&fixture, &absent);
    verify(&output, &fixture, &absent, false);
    let explicit_spheres = vec![Shape::zeroed(); fixture.particles.len()];
    let full_output = reconstruction.run(&fixture, &explicit_spheres);
    assert_eq!(
        output, full_output,
        "unwired sentinel and fully sized spherical shapes differ"
    );
}

#[test]
fn water_reconstruction_actual_reach_reduction_handles_strided_tail_and_reset() {
    let device = harness::shared().device.as_ref();
    let pipeline = device.create_compute_pipeline(&reach_source(), ENTRY, "water-reach-proof");
    let uniforms = [1_u32, 0, 0, 0];
    for count in [1, 257, 777] {
        let mut shapes = vec![Shape::zeroed(); count];
        for (i, shape) in shapes.iter_mut().enumerate() {
            shape.axis_y[3] = (i % 29) as f32 / 128.0;
        }
        shapes[count - 1].axis_y[3] = 0.8125;
        let input = device.create_buffer_shared(count as u64 * 64);
        let output = device.create_buffer_shared(4);
        // Run again with all zero records against the same output. A stale
        // atomicMax accumulator would retain the previous frame's reach.
        for clear in [false, true] {
            if clear {
                shapes.fill(Shape::zeroed());
            }
            let expected_bytes = bytemuck::cast_slice::<Shape, u8>(&shapes).to_vec();
            harness::retry_on_gpu_commit_error(|| {
                unsafe {
                    input.write(0, &expected_bytes);
                    output.write(0, bytemuck::bytes_of(&99.0_f32));
                }
                let mut encoder = device.create_encoder("water-reach-proof");
                encoder.dispatch_compute(
                    &pipeline,
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
                            buffer: &output,
                            offset: 0,
                        },
                    ],
                    [1, 1, 1],
                    "water-reach-proof",
                );
                encoder.commit_and_wait_completed();
            });
            let actual = unsafe { *output.mapped_ptr().unwrap().cast::<u32>() };
            let expected = shapes.iter().map(|s| s.axis_y[3]).fold(0.0_f32, f32::max);
            assert_eq!(actual, expected.to_bits(), "count={count}, clear={clear}");
            let after = unsafe {
                std::slice::from_raw_parts(input.mapped_ptr().unwrap(), expected_bytes.len())
            };
            assert_eq!(after, expected_bytes, "reach reduction modified GPU shapes");
        }
    }
}
