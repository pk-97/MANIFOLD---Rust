//! Actual generated MAC liquid-SDF shader versus a brute-force f64 sphere union.
//! The oracle never uses bins; CPU bin construction supplies only GPU inputs.
use bytemuck::Zeroable;
use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuDevice};
use manifold_renderer::node_graph::freeze::codegen::ENTRY;
use manifold_renderer::node_graph::primitives::mac_liquid_sdf;
use manifold_renderer::node_graph::water::WaterParticle;

use crate::harness;

const N: usize = 64;
const CELLS: usize = N * N * N;
const PADDED: usize = 65 * 65 * 65;
const BINS: usize = 32 * 32 * 32;
const H: f64 = 0.0625;
const ORIGIN: [f64; 3] = [-2.0, 0.0, -2.0];
const CAP: f64 = 3.0 * H;
const EPSILON: f64 = 0.005 * H;
// Includes f32 particle positions, distance arithmetic, and sqrt rounding.
// Near-zero fixtures stay well away from a rounding-ambiguous sign change.
const DISTANCE_EPS: f64 = 5.0e-7;

fn index(c: [usize; 3]) -> usize {
    c[0] + N * (c[1] + N * c[2])
}

fn padded_index(c: [usize; 3]) -> usize {
    c[0] + 65 * (c[1] + 65 * c[2])
}

fn center(c: [usize; 3]) -> [f64; 3] {
    std::array::from_fn(|a| ORIGIN[a] + H * (c[a] as f64 + 0.5))
}

fn particle(position: [f64; 3]) -> WaterParticle {
    let mut particle = WaterParticle::zeroed();
    particle.position_mass = [
        position[0] as f32,
        position[1] as f32,
        position[2] as f32,
        1.0,
    ];
    particle.velocity_density[3] = 1000.0;
    particle
}

fn at_cell_distance(c: [usize; 3], distance: f64) -> WaterParticle {
    let mut position = center(c);
    position[0] += distance;
    particle(position)
}

struct Links {
    heads: Vec<u32>,
    next: Vec<u32>,
}

impl Links {
    fn new(particles: &[WaterParticle]) -> Self {
        let mut bins = Self {
            heads: vec![0; BINS],
            next: vec![0; particles.len()],
        };
        for (i, particle) in particles.iter().enumerate() {
            if particle.position_mass[3] == 0.0 {
                continue;
            }
            let coordinate = std::array::from_fn(|a| {
                let q = (f64::from(particle.position_mass[a]) - ORIGIN[a]) / (2.0 * H);
                assert!(q.is_finite() && (0.0..32.0).contains(&q));
                q.floor() as usize
            });
            bins.insert(i, coordinate);
        }
        bins
    }

    fn insert(&mut self, particle: usize, bin: [usize; 3]) {
        let i = bin[0] + 32 * (bin[1] + 32 * bin[2]);
        self.next[particle] = self.heads[i];
        self.heads[i] = particle as u32 + 1;
    }
}

struct Sdf {
    device: &'static GpuDevice,
    pipeline: GpuComputePipeline,
}

impl Sdf {
    fn new() -> Self {
        let device = harness::shared().device.as_ref();
        let source = mac_liquid_sdf::shader_source();
        Self {
            device,
            pipeline: device.create_compute_pipeline(&source, ENTRY, "gpu-proof.mac-sdf"),
        }
    }

    fn run(&self, particles: &[WaterParticle], links: &Links, geometry: &[[f32; 4]]) -> Vec<f32> {
        let particle_buffer = self
            .device
            .create_buffer_shared(std::mem::size_of_val(particles) as u64);
        let head_buffer = self
            .device
            .create_buffer_shared(std::mem::size_of_val(links.heads.as_slice()) as u64);
        let next_buffer = self
            .device
            .create_buffer_shared(std::mem::size_of_val(links.next.as_slice()) as u64);
        let geometry_buffer = self
            .device
            .create_buffer_shared(std::mem::size_of_val(geometry) as u64);
        let output = self.device.create_buffer_shared((CELLS * 4) as u64);
        let uniforms = [CELLS as u32, 0, 0, 0];
        harness::retry_on_gpu_commit_error(|| {
            unsafe {
                particle_buffer.write(0, bytemuck::cast_slice(particles));
                head_buffer.write(0, bytemuck::cast_slice(&links.heads));
                next_buffer.write(0, bytemuck::cast_slice(&links.next));
                geometry_buffer.write(0, bytemuck::cast_slice(geometry));
                output.write(0, &vec![0xa5; CELLS * 4]);
            }
            let mut encoder = self.device.create_encoder("gpu-proof.mac-sdf");
            encoder.dispatch_compute(
                &self.pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: &particle_buffer,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: &head_buffer,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: &next_buffer,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 4,
                        buffer: &geometry_buffer,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 5,
                        buffer: &output,
                        offset: 0,
                    },
                ],
                [(CELLS as u32).div_ceil(256), 1, 1],
                "gpu-proof.mac-sdf",
            );
            encoder.commit_and_wait_completed();
        });
        unsafe {
            std::slice::from_raw_parts(output.mapped_ptr().unwrap().cast::<f32>(), CELLS).to_vec()
        }
    }
}

fn oracle(c: [usize; 3], particles: &[WaterParticle], open: f32) -> f64 {
    let x = center(c);
    let radius = 3.0_f64.sqrt() * H / 2.0;
    let mut phi = particles
        .iter()
        .filter(|p| p.position_mass[3] > 0.0)
        .map(|p| {
            let squared: f64 = x
                .iter()
                .zip(&p.position_mass[..3])
                .map(|(&x, &p)| (x - f64::from(p)).powi(2))
                .sum();
            squared.sqrt() - radius
        })
        .fold(CAP, f64::min);
    if open == 0.0 && phi < H / 2.0 {
        phi = -H / 2.0;
    }
    if phi.abs() < EPSILON {
        phi = if phi > 0.0 { EPSILON } else { -EPSILON };
    }
    phi
}

fn verify_all(output: &[f32], particles: &[WaterParticle], geometry: &[[f32; 4]]) {
    assert_eq!(output.len(), CELLS);
    for (i, &actual) in output.iter().enumerate() {
        let c = [i % N, (i / N) % N, i / (N * N)];
        let expected = oracle(c, particles, geometry[padded_index(c)][3]);
        assert!(
            (f64::from(actual) - expected).abs() <= DISTANCE_EPS,
            "SDF at {c:?}: {actual} != {expected}"
        );
    }
}

#[test]
fn water_mac_sdf_full_lattice_matches_brute_force_sphere_union() {
    let target = [16, 16, 16];
    let first = at_cell_distance(target, 0.23);
    let mut inactive = WaterParticle::zeroed();
    inactive.position_mass = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0];
    let particles = [
        first,
        particle([-0.712, 1.087, -0.947]),
        particle([0.483, 1.783, 0.217]),
        particle([-1.361, 3.109, 1.072]),
        particle([-1.983, 0.017, -1.981]),
        inactive,
    ];
    let mut links = Links::new(&particles);
    // Deliberately link an inactive nonfinite record: visiting it must skip
    // its coordinates and still traverse to the next active particle.
    links.insert(5, [10, 8, 8]);
    let geometry = vec![[1.0; 4]; PADDED];
    let output = Sdf::new().run(&particles, &links, &geometry);
    verify_all(&output, &particles, &geometry);
    assert!(f64::from(output[index(target)]) < CAP - 0.005);
    // target center is in bin 8, while the nearest particle is in bin 10.
    // A +/-one-bin search would return the cap and fail the comparison.
    assert_eq!(
        ((f64::from(first.position_mass[0]) - ORIGIN[0]) / (2.0 * H)).floor() as usize,
        10
    );
    assert_eq!(output[index([63, 63, 63])], CAP as f32);
}

#[test]
fn water_mac_sdf_near_zero_and_closed_cell_extension_match_f64() {
    let radius = 3.0_f64.sqrt() * H / 2.0;
    let outside = [10, 10, 10];
    let inside = [26, 26, 26];
    let solid = [45, 20, 45];
    let partial = [20, 45, 20];
    let particles = [
        at_cell_distance(outside, radius + EPSILON / 4.0),
        at_cell_distance(inside, radius - EPSILON / 4.0),
        at_cell_distance(solid, radius + H / 4.0),
        at_cell_distance(partial, radius + H / 4.0),
    ];
    let links = Links::new(&particles);
    let mut geometry = vec![[1.0; 4]; PADDED];
    geometry[padded_index(solid)][3] = 0.0;
    geometry[padded_index(partial)][3] = 0.01;
    geometry[padded_index([63, 63, 63])][3] = 0.0;
    let output = Sdf::new().run(&particles, &links, &geometry);
    verify_all(&output, &particles, &geometry);
    assert!((f64::from(output[index(outside)]) - EPSILON).abs() <= DISTANCE_EPS);
    assert!((f64::from(output[index(inside)]) + EPSILON).abs() <= DISTANCE_EPS);
    assert_eq!(output[index(solid)], -(H / 2.0) as f32);
    assert!(
        output[index(partial)] > 0.0,
        "partial open cells must retain the geometric distance"
    );
    assert_eq!(
        output[index([63, 63, 63])],
        CAP as f32,
        "far solid cells are not extended"
    );
}

#[test]
fn water_mac_sdf_rejects_bad_links_cycles_and_active_nonfinite_records() {
    let sdf = Sdf::new();
    let target = [16, 16, 16];
    let valid_particle = particle(center(target));
    let geometry = vec![[1.0; 4]; PADDED];
    let bin = 8 + 32 * (8 + 32 * 8);
    let check = |particles: &[WaterParticle], links: &Links| {
        let output = sdf.run(particles, links, &geometry);
        assert!(
            output[index(target)].is_nan(),
            "corrupt local bin must reject its SDF samples"
        );
        assert_eq!(
            output[index([63, 63, 63])],
            CAP as f32,
            "unvisited corruption is local to its bin neighborhood"
        );
    };
    let mut bad_head = Links::new(&[valid_particle]);
    bad_head.heads[bin] = 2;
    check(&[valid_particle], &bad_head);
    let mut bad_next = Links::new(&[valid_particle]);
    bad_next.next[0] = u32::MAX;
    check(&[valid_particle], &bad_next);
    let mut cycle = Links::new(&[valid_particle]);
    cycle.next[0] = 1;
    check(&[valid_particle], &cycle);

    let links = Links::new(&[valid_particle]);
    for invalid_mass in [f32::NAN, f32::INFINITY, -1.0] {
        let mut invalid = valid_particle;
        invalid.position_mass[3] = invalid_mass;
        check(&[invalid], &links);
    }
    for axis in 0..3 {
        let mut invalid = valid_particle;
        invalid.position_mass[axis] = f32::NAN;
        check(&[invalid], &links);
    }
}

#[test]
fn water_mac_sdf_rejects_invalid_open_cell_fractions() {
    let sdf = Sdf::new();
    let particles = [particle(center([16, 16, 16]))];
    let links = Links::new(&particles);
    let bad_cells = [[4, 7, 11], [13, 17, 21], [33, 35, 37], [41, 43, 47]];
    let mut geometry = vec![[1.0; 4]; PADDED];
    for (c, value) in bad_cells
        .into_iter()
        .zip([f32::NAN, f32::INFINITY, -0.1, 1.1])
    {
        geometry[padded_index(c)][3] = value;
    }
    let output = sdf.run(&particles, &links, &geometry);
    for c in bad_cells {
        assert!(output[index(c)].is_nan(), "invalid geometry at {c:?}");
    }
    assert_eq!(output[index([63, 63, 63])], CAP as f32);
}
