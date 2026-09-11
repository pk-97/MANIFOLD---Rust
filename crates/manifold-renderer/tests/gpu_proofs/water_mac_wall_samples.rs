//! Generated stationary-wall extension proofs. Expected reflected velocities
//! are independent f64 trilinear samples at explicitly chosen mirror points.
use crate::harness;
use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuDevice};
use manifold_renderer::node_graph::freeze::codegen::ENTRY;
use manifold_renderer::node_graph::primitives::{
    mac_box_fractions::{MAC_BOX_ENTRIES, MacBoxFractionsUniforms},
    mac_box_sample_extension,
    mac_resolve::MacResolvedCell,
};

const EDGE: usize = 65;
const ENTRIES: usize = EDGE * EDGE * EDGE;
const H: f64 = 0.0625;
const ORIGIN: [f64; 3] = [-2.0, 0.0, -2.0];
const EPS: f64 = 2.0e-6;

fn index(c: [usize; 3]) -> usize {
    c[0] + EDGE * (c[1] + EDGE * c[2])
}

fn position(c: [usize; 3], axis: usize) -> [f64; 3] {
    std::array::from_fn(|d| ORIGIN[d] + H * (c[d] as f64 + if d == axis { 0.0 } else { 0.5 }))
}

fn bounds() -> MacBoxFractionsUniforms {
    MacBoxFractionsUniforms {
        basin_min: [-1.5, 0.5, -1.5],
        basin_max: [1.5, 3.5, 1.5],
        box_min: [6.0; 3],
        box_max: [7.0; 3],
        dispatch_count: MAC_BOX_ENTRIES,
        padding: [0; 3],
    }
}

fn geometry(u: &MacBoxFractionsUniforms) -> Vec<[f32; 4]> {
    (0..ENTRIES)
        .map(|i| {
            let c = [i % EDGE, (i / EDGE) % EDGE, i / (EDGE * EDGE)];
            let lo: [f64; 3] = std::array::from_fn(|d| ORIGIN[d] + H * c[d] as f64);
            let mut fractions = [0.0; 4];
            for axis in 0..3 {
                if c[axis] == 0
                    || c[axis] == 64
                    || c.iter().enumerate().any(|(d, &x)| d != axis && x == 64)
                    || lo[axis] <= f64::from(u.basin_min[axis])
                    || lo[axis] >= f64::from(u.basin_max[axis])
                {
                    continue;
                }
                let mut open = 1.0;
                let mut blocked = 1.0;
                for d in (0..3).filter(|&d| d != axis) {
                    let l = lo[d].max(f64::from(u.basin_min[d]));
                    let h = (lo[d] + H).min(f64::from(u.basin_max[d]));
                    open *= (h - l).max(0.0);
                    blocked *=
                        (h.min(f64::from(u.box_max[d])) - l.max(f64::from(u.box_min[d]))).max(0.0);
                }
                if lo[axis] < f64::from(u.box_min[axis]) || lo[axis] > f64::from(u.box_max[axis]) {
                    blocked = 0.0;
                }
                fractions[axis] = ((open - blocked) / (H * H)) as f32;
            }
            fractions
        })
        .collect()
}

fn field(b: [f64; 3], a: [[f64; 3]; 3]) -> Vec<MacResolvedCell> {
    (0..ENTRIES)
        .map(|i| {
            let c = [i % EDGE, (i / EDGE) % EDGE, i / (EDGE * EDGE)];
            let mut out = MacResolvedCell {
                mac_velocity: [0.0, 0.0, 0.0, 13.0],
                mac_valid: [1.0, 1.0, 1.0, -7.0],
            };
            for axis in 0..3 {
                let p = position(c, axis);
                out.mac_velocity[axis] =
                    (b[axis] + a[axis].iter().zip(p).map(|(&a, p)| a * p).sum::<f64>()) as f32;
            }
            out
        })
        .collect()
}

// Samples just one component of the immutable input, independently of the
// shader's reflection decisions and without depending on its helper code.
fn sample(grid: &[MacResolvedCell], p: [f64; 3], axis: usize) -> Option<f64> {
    let q: [f64; 3] =
        std::array::from_fn(|d| (p[d] - ORIGIN[d]) / H - if d == axis { 0.0 } else { 0.5 });
    let mut base = [0; 3];
    let mut fraction = [0.0; 3];
    for d in 0..3 {
        let dimension = if d == axis { 65.0 } else { 64.0 };
        if !q[d].is_finite() || q[d] < 0.0 || q[d].floor() + 1.0 >= dimension {
            return None;
        }
        base[d] = q[d].floor() as usize;
        fraction[d] = q[d] - base[d] as f64;
    }
    let mut value = 0.0;
    for corner in 0..8 {
        let bit = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
        let weight: f64 = (0..3)
            .map(|d| {
                if bit[d] == 0 {
                    1.0 - fraction[d]
                } else {
                    fraction[d]
                }
            })
            .product();
        if weight == 0.0 {
            continue;
        }
        let face = grid[index(std::array::from_fn(|d| base[d] + bit[d]))];
        if !face.mac_valid[axis].is_finite()
            || face.mac_valid[axis] <= 0.0
            || !face.mac_velocity[axis].is_finite()
        {
            return None;
        }
        value += weight * f64::from(face.mac_velocity[axis]);
    }
    Some(value)
}

struct Extension {
    device: &'static GpuDevice,
    pipeline: GpuComputePipeline,
}
impl Extension {
    fn new() -> Self {
        let device = harness::shared().device.as_ref();
        Self {
            device,
            pipeline: device.create_compute_pipeline(
                &mac_box_sample_extension::shader_source(),
                ENTRY,
                "gpu-proof.mac-wall-samples",
            ),
        }
    }
    fn run(
        &self,
        grid: &[MacResolvedCell],
        geometry: &[[f32; 4]],
        u: &MacBoxFractionsUniforms,
    ) -> Vec<MacResolvedCell> {
        let input = self
            .device
            .create_buffer_shared(std::mem::size_of_val(grid) as u64);
        let faces = self
            .device
            .create_buffer_shared(std::mem::size_of_val(geometry) as u64);
        let output = self.device.create_buffer_shared((ENTRIES * 32) as u64);
        harness::retry_on_gpu_commit_error(|| {
            unsafe {
                input.write(0, bytemuck::cast_slice(grid));
                faces.write(0, bytemuck::cast_slice(geometry));
                output.write(0, &vec![0xa5; ENTRIES * 32]);
            }
            let mut encoder = self.device.create_encoder("gpu-proof.mac-wall-samples");
            encoder.dispatch_compute(
                &self.pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(u),
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
                [MAC_BOX_ENTRIES.div_ceil(256), 1, 1],
                "gpu-proof.mac-wall-samples",
            );
            encoder.commit_and_wait_completed();
        });
        unsafe {
            std::slice::from_raw_parts(
                output.mapped_ptr().unwrap().cast::<MacResolvedCell>(),
                ENTRIES,
            )
            .to_vec()
        }
    }
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= EPS,
        "wall sample {actual} != {expected}"
    );
}

fn assert_open_unchanged(
    input: &[MacResolvedCell],
    output: &[MacResolvedCell],
    geometry: &[[f32; 4]],
) {
    let mut count = 0;
    for ((input, output), open) in input.iter().zip(output).zip(geometry) {
        for (axis, &area) in open[..3].iter().enumerate() {
            if area > 0.0 {
                assert_eq!(
                    input.mac_velocity[axis].to_bits(),
                    output.mac_velocity[axis].to_bits()
                );
                assert_eq!(
                    input.mac_valid[axis].to_bits(),
                    output.mac_valid[axis].to_bits()
                );
                count += 1;
            }
        }
        assert_eq!(
            input.mac_velocity[3].to_bits(),
            output.mac_velocity[3].to_bits()
        );
        assert_eq!(input.mac_valid[3].to_bits(), output.mac_valid[3].to_bits());
    }
    assert!(count > 1000);
}

fn reflected(
    input: &[MacResolvedCell],
    output: &[MacResolvedCell],
    c: [usize; 3],
    axis: usize,
    mirror: [f64; 3],
    sign: f64,
) {
    let expected = sign * sample(input, mirror, axis).expect("fixture mirror must be available");
    let actual = output[index(c)];
    assert_eq!(actual.mac_valid[axis], 1.0);
    close(f64::from(actual.mac_velocity[axis]), expected);
}

#[test]
fn water_mac_wall_samples_floor_retains_tangent_and_odd_normal() {
    let u = bounds();
    let geometry = geometry(&u);
    let mut grid = field([1.0, 2.0, 1.0], [[0.0; 3]; 3]);
    // Pressure faces in solids start at zero; ghosts must recover tangents.
    for (cell, open) in grid.iter_mut().zip(&geometry) {
        for (axis, &area) in open[..3].iter().enumerate() {
            if area == 0.0 {
                cell.mac_velocity[axis] = 0.0;
            }
        }
    }
    let output = Extension::new().run(&grid, &geometry, &u);
    assert_open_unchanged(&grid, &output, &geometry);
    for (axis, expected) in [1.0, 0.0, 1.0].into_iter().enumerate() {
        close(
            sample(&output, [0.03125, 0.5, 0.03125], axis).unwrap(),
            expected,
        );
    }
    close(
        sample(&output, [0.03125, 0.5 - H / 2.0, 0.03125], 0).unwrap(),
        1.0,
    );
    close(
        sample(&output, [0.03125, 0.5 + H / 2.0, 0.03125], 0).unwrap(),
        1.0,
    );
    close(
        sample(&output, [0.03125, 0.5 - H / 2.0, 0.03125], 1).unwrap(),
        -1.0,
    );
    close(
        sample(&output, [0.03125, 0.5 + H / 2.0, 0.03125], 1).unwrap(),
        1.0,
    );
    let mut mirror = position([32, 7, 32], 1);
    mirror[1] = 1.0 - mirror[1];
    reflected(&grid, &output, [32, 7, 32], 1, mirror, -1.0);
}

#[test]
fn water_mac_wall_samples_box_and_non_aligned_bounds_match_f64_mirrors() {
    let extension = Extension::new();
    let grid = field(
        [1.0, 2.0, 3.0],
        [[0.2, 0.5, -0.25], [0.25, 0.125, 0.5], [-0.5, 0.25, 0.1]],
    );
    let boxed = MacBoxFractionsUniforms {
        box_min: [-0.25, 1.25, -0.25],
        box_max: [0.25, 1.75, 0.25],
        ..bounds()
    };
    let geometry = geometry(&boxed);
    let output = extension.run(&grid, &geometry, &boxed);
    assert_open_unchanged(&grid, &output, &geometry);
    for (axis, sign) in [(0, -1.0), (1, 1.0)] {
        let c = [29, 24, 32];
        let mut mirror = position(c, axis);
        mirror[0] = -0.5 - mirror[0];
        reflected(&grid, &output, c, axis, mirror, sign);
    }
    assert_eq!(output[index([28, 24, 32])].mac_velocity[0], 0.0);
    assert_eq!(output[index([28, 24, 32])].mac_valid[0], 1.0);

    let shifted = MacBoxFractionsUniforms {
        basin_min: [-1.47, 0.53, -1.43],
        basin_max: [1.41, 3.47, 1.39],
        ..bounds()
    };
    let open = self::geometry(&shifted);
    assert!(
        open.iter()
            .any(|a| a[..3].iter().any(|&v| v > 0.0 && v < 1.0))
    );
    let output = extension.run(&grid, &open, &shifted);
    assert_open_unchanged(&grid, &output, &open);
    let c = [32, 7, 32];
    for (axis, sign) in [(0, 1.0), (1, -1.0), (2, 1.0)] {
        let mut mirror = position(c, axis);
        mirror[1] = 2.0 * f64::from(shifted.basin_min[1]) - mirror[1];
        reflected(&grid, &output, c, axis, mirror, sign);
    }
}

#[test]
fn water_mac_wall_samples_unavailable_or_nonfinite_mirrors_are_invalid() {
    let extension = Extension::new();
    let u = bounds();
    let mut geometry = geometry(&u);
    let mut grid = field([1.0; 3], [[0.0; 3]; 3]);
    let ghost = [32, 7, 32];
    let source = [32, 8, 32];
    grid[index(source)].mac_valid[0] = 0.0;
    // A blocked face with no matching wall must not retain stale validity.
    geometry[index([16, 16, 16])][0] = 0.0;
    let output = extension.run(&grid, &geometry, &u);
    for c in [ghost, [16, 16, 16]] {
        assert_eq!(output[index(c)].mac_valid[0], 0.0);
        assert_eq!(output[index(c)].mac_velocity[0], 0.0);
    }
    grid[index(source)].mac_valid[0] = 1.0;
    grid[index(source)].mac_velocity[0] = f32::INFINITY;
    let output = extension.run(&grid, &geometry, &u);
    assert_eq!(output[index(ghost)].mac_valid[0], 0.0);
    assert_eq!(output[index(ghost)].mac_velocity[0], 0.0);
    // Reflection of the world-side ghost is outside the available grid.
    let narrow = MacBoxFractionsUniforms {
        basin_min: [1.0, 0.5, -1.5],
        ..u
    };
    let open = self::geometry(&narrow);
    let output = extension.run(&grid, &open, &narrow);
    assert_eq!(output[index([0, 20, 20])].mac_valid[0], 0.0);
}

#[test]
fn water_mac_wall_samples_invalid_bounds_and_geometry_reject() {
    let extension = Extension::new();
    let grid = field([1.0; 3], [[0.0; 3]; 3]);
    let mut u = bounds();
    let mut geometry = geometry(&u);
    for (c, value) in
        [[16, 16, 16], [20, 20, 20], [24, 24, 24]]
            .into_iter()
            .zip([f32::NAN, -0.1, 1.1])
    {
        geometry[index(c)][0] = value;
    }
    let output = extension.run(&grid, &geometry, &u);
    for c in [[16, 16, 16], [20, 20, 20], [24, 24, 24]] {
        assert!(output[index(c)].mac_velocity[0].is_nan());
        assert_eq!(output[index(c)].mac_valid[0], 0.0);
    }
    u.box_max[2] = f32::NAN;
    let output = extension.run(&grid, &geometry, &u);
    assert!(
        output
            .iter()
            .flat_map(|e| e.mac_velocity)
            .all(|v| v.is_nan())
    );
    u = bounds();
    u.basin_min[1] = u.basin_max[1];
    let output = extension.run(&grid, &geometry, &u);
    assert!(
        output
            .iter()
            .flat_map(|e| e.mac_velocity)
            .all(|v| v.is_nan())
    );
}
