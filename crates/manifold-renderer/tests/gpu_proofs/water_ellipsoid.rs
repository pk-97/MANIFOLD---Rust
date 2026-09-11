//! Independent f64 oracle and fixtures for anisotropic water raster proofs.
//! Native raster outputs are compared to independent f64 ray intersections.

use manifold_gpu::{GpuBinding, GpuDevice};
use manifold_renderer::node_graph::camera::delinearize_depth;
use manifold_renderer::node_graph::primitives::SurfaceColliderUniforms;

use manifold_renderer::node_graph::water::WaterParticle;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Ellipsoid {
    pub center: [f64; 3],
    pub axes: [[f64; 3]; 3],
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Returns the front and back ray parameters for the oriented ellipsoid.
pub(crate) fn ray_intersection(
    e: Ellipsoid,
    origin: [f64; 3],
    ray: [f64; 3],
) -> Option<(f64, f64)> {
    let q = sub(origin, e.center);
    let mut u = [0.0; 3];
    let mut v = [0.0; 3];
    for j in 0..3 {
        let aa = dot(e.axes[j], e.axes[j]);
        if !aa.is_finite() || aa <= 0.0 {
            return None;
        }
        u[j] = dot(ray, e.axes[j]) / aa;
        v[j] = dot(q, e.axes[j]) / aa;
    }
    let a = dot(u, u);
    let b = dot(u, v);
    let c = dot(v, v) - 1.0;
    let disc = b * b - a * c;
    if !a.is_finite() || !disc.is_finite() || disc < 0.0 {
        return None;
    }
    let root = disc.sqrt();
    let near = (-b - root) / a;
    let far = (-b + root) / a;
    (far > 0.0).then_some((near.max(0.0), far))
}

pub(crate) fn pinhole_ray(px: [u32; 2], size: [u32; 2], fov_y: f64) -> [f64; 3] {
    let aspect = f64::from(size[0]) / f64::from(size[1]);
    let x =
        ((f64::from(px[0]) + 0.5) / f64::from(size[0]) * 2.0 - 1.0) * (fov_y * 0.5).tan() * aspect;
    let y = (1.0 - (f64::from(px[1]) + 0.5) / f64::from(size[1]) * 2.0) * (fov_y * 0.5).tan();
    let n = (x * x + y * y + 1.0).sqrt();
    [x / n, y / n, 1.0 / n]
}

pub(crate) fn fixtures() -> [Ellipsoid; 2] {
    let a = [0.4, 0.1, 0.2];
    let angle = std::f64::consts::FRAC_PI_6;
    let (s, c) = angle.sin_cos();
    [
        Ellipsoid {
            center: [0.2, -0.1, 3.0],
            axes: [[a[0], 0.0, 0.0], [0.0, a[1], 0.0], [0.0, 0.0, a[2]]],
        },
        Ellipsoid {
            center: [0.2, -0.1, 3.0],
            axes: [
                [a[0] * c, a[0] * s, 0.0],
                [-a[1] * s, a[1] * c, 0.0],
                [0.0, 0.0, a[2]],
            ],
        },
    ]
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Shape64 {
    center_radius: [f32; 4],
    axis_x: [f32; 4],
    axis_y: [f32; 4],
    axis_z: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view: [[f32; 4]; 4],
    tan_half_fov: f32,
    near: f32,
    far: f32,
    radius: f32,
    width: u32,
    height: u32,
    count: u32,
    pad: u32,
}
#[test]
fn anisotropic_depth_native_matches_f64_oracle() {
    let device = GpuDevice::new();
    let shader = concat!(
        include_str!("../../src/node_graph/primitives/shaders/particle_splat_common.wgsl"),
        "\n",
        include_str!("../../src/node_graph/primitives/shaders/particle_surface_depth_splat.wgsl")
    );
    let fitted = device.create_compute_pipeline(shader, "cs_anisotropic", "ellipsoid-proof");
    let legacy = device.create_compute_pipeline(shader, "cs_main", "sphere-proof");
    let (w, h) = (128u32, 128u32);
    let count = (w * h) as usize;
    let particles = device.create_buffer_shared(96);
    let shapes = device.create_buffer_shared(64);
    let depth = device.create_buffer_shared((count * 4) as u64);
    let coverage = device.create_buffer_shared((count * 4) as u64);
    let mut cases = fixtures().to_vec();
    cases.push(Ellipsoid {
        center: [0.2, -0.1, 3.0],
        axes: [[0.2, 0., 0.], [0., 0.2, 0.], [0., 0., 0.2]],
    });
    let mut u = Uniforms {
        view: [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ],
        tan_half_fov: (0.7f32 * 0.5).tan(),
        near: 0.05,
        far: 100.,
        radius: 0.2,
        width: w,
        height: h,
        count: 1,
        pad: 0,
    };
    let clip = SurfaceColliderUniforms {
        camera_to_world: [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., -1., 0.],
            [0., 0., 0., 1.],
        ],
        collider_center: [0.; 4],
        collider_half: [0.; 4],
    };
    for (case, e) in cases.into_iter().enumerate() {
        let r = e
            .axes
            .iter()
            .map(|a| dot(*a, *a).sqrt())
            .fold(0.0, f64::max) as f32;
        let world = |a: [f64; 3], last: f32| [a[0] as f32, a[1] as f32, -a[2] as f32, last];
        let record = Shape64 {
            center_radius: world(e.center, r),
            axis_x: world(e.axes[0], 0.),
            axis_y: world(e.axes[1], 0.),
            axis_z: world(e.axes[2], 0.),
        };
        let p = WaterParticle {
            position_mass: world(e.center, 1.),
            ..bytemuck::Zeroable::zeroed()
        };
        unsafe {
            particles.write(0, bytemuck::bytes_of(&p));
            shapes.write(0, bytemuck::bytes_of(&record));
        }
        let mut fitted_depth: Vec<f32> = Vec::new();
        for sphere in [false, true] {
            if sphere && case != 2 {
                continue;
            }
            u.radius = r;
            unsafe {
                depth.write(0, bytemuck::cast_slice(&vec![1.0f32; count]));
            }
            let mut enc = device.create_encoder("ellipsoid-proof");
            enc.clear_buffer(&coverage);
            let mut bindings = vec![
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &depth,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &coverage,
                    offset: 0,
                },
            ];
            if !sphere {
                bindings.push(GpuBinding::Buffer {
                    binding: 4,
                    buffer: &shapes,
                    offset: 0,
                });
            }
            bindings.push(GpuBinding::Bytes {
                binding: 5,
                data: bytemuck::bytes_of(&clip),
            });
            enc.dispatch_compute(
                if sphere { &legacy } else { &fitted },
                &bindings,
                [1, 1, 1],
                "ellipsoid-proof",
            );
            enc.commit_and_wait_completed();
            let actual = unsafe {
                std::slice::from_raw_parts(depth.mapped_ptr().unwrap().cast::<f32>(), count)
            };
            let covered = unsafe {
                std::slice::from_raw_parts(coverage.mapped_ptr().unwrap().cast::<u32>(), count)
            };
            let mut hits = 0;
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) as usize;
                    let ray = pinhole_ray([x, y], [w, h], 0.7);
                    if let Some((front, _)) = ray_intersection(e, [0.; 3], ray) {
                        assert_ne!(covered[i], 0, "missing case={case}, pixel={x},{y}");
                        hits += 1;
                        let expected = delinearize_depth((front * ray[2]) as f32, u.near, u.far);
                        assert!(
                            (actual[i] - expected).abs() < 1e-5,
                            "case={case}, pixel={x},{y}: {} != {expected}",
                            actual[i]
                        );
                    } else {
                        assert_eq!(covered[i], 0, "extra case={case}, pixel={x},{y}");
                    }
                }
            }
            assert!(hits > 100, "insufficient covered samples");
            if sphere {
                for (a, b) in actual.iter().zip(&fitted_depth) {
                    assert!((a - b).abs() < 1e-5);
                }
            } else {
                fitted_depth = actual.to_vec();
            }
        }
    }
}

// Yu–Turk Eqs. 6, 9–16. The oracle accumulates a centered covariance in
// two passes and diagonalizes it with largest-pivot, converged f64 rotations.
// Compare the support tensor AA^T: repeated eigenvalues and eigenvector signs
// must not make a correct reconstruction fail.
type Matrix3 = [[f64; 3]; 3];

fn identity3() -> Matrix3 {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
}

fn multiply3(a: Matrix3, b: Matrix3) -> Matrix3 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

fn transpose3(a: Matrix3) -> Matrix3 {
    std::array::from_fn(|i| std::array::from_fn(|j| a[j][i]))
}

fn eigen_f64(mut a: Matrix3) -> ([f64; 3], Matrix3) {
    let mut vectors = identity3();
    for _ in 0..64 {
        let (p, q) = [(0, 1), (0, 2), (1, 2)]
            .into_iter()
            .max_by(|&(p, q), &(r, s)| a[p][q].abs().total_cmp(&a[r][s].abs()))
            .unwrap();
        if a[p][q].abs() < 1.0e-18 {
            return (std::array::from_fn(|i| a[i][i]), vectors);
        }
        let angle = 0.5 * (2.0 * a[p][q]).atan2(a[q][q] - a[p][p]);
        let (s, c) = angle.sin_cos();
        let mut rotation = identity3();
        rotation[p][p] = c;
        rotation[q][q] = c;
        rotation[p][q] = s;
        rotation[q][p] = -s;
        a = multiply3(transpose3(rotation), multiply3(a, rotation));
        vectors = multiply3(vectors, rotation);
    }
    panic!("f64 covariance eigensolver did not converge");
}

fn cubic_sph(q: f64) -> f64 {
    if q < 1.0 {
        1.0 - 1.5 * q * q + 0.75 * q * q * q
    } else if q < 2.0 {
        0.25 * (2.0 - q).powi(3)
    } else {
        0.0
    }
}

struct FitOracle {
    center: [f64; 3],
    support_tensor: Matrix3,
    max_axis: f64,
    density: f64,
    reach: f64,
    neighbors: usize,
}

fn fit_oracle(points: &[WaterParticle], index: usize, h: f64) -> FitOracle {
    let original = std::array::from_fn(|d| f64::from(points[index].position_mass[d]));
    let r = 2.0 * h;
    let neighbors: Vec<_> = points
        .iter()
        .filter(|p| p.position_mass[3] > 0.0)
        .filter_map(|p| {
            let position = std::array::from_fn(|d| f64::from(p.position_mass[d]));
            let delta = sub(position, original);
            let distance = dot(delta, delta).sqrt();
            (distance < r).then_some((position, distance, f64::from(p.position_mass[3])))
        })
        .collect();
    let weights: Vec<_> = neighbors
        .iter()
        .map(|(_, d, _)| 1.0 - (d / r).powi(3))
        .collect();
    let total: f64 = weights.iter().sum();
    let mean: [f64; 3] = std::array::from_fn(|d| {
        neighbors
            .iter()
            .zip(&weights)
            .map(|((x, _, _), w)| x[d] * w)
            .sum::<f64>()
            / total
    });
    let covariance: Matrix3 = std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            neighbors
                .iter()
                .zip(&weights)
                .map(|((x, _, _), w)| w * (x[i] - mean[i]) * (x[j] - mean[j]))
                .sum::<f64>()
                / total
        })
    });
    let center = std::array::from_fn(|d| original[d] + 0.95 * (mean[d] - original[d]));
    let (support_tensor, max_axis) = if neighbors.len() > 25 {
        let (values, rotation) = eigen_f64(covariance);
        let largest = values.into_iter().fold(0.0, f64::max);
        let ks = 20.0 / (3.0 * r * r);
        let lengths = values.map(|value| 2.0 * h * ks * value.max(largest / 4.0));
        let diagonal = std::array::from_fn(|i| {
            std::array::from_fn(|j| if i == j { lengths[i] * lengths[i] } else { 0.0 })
        });
        (
            multiply3(rotation, multiply3(diagonal, transpose3(rotation))),
            lengths.into_iter().fold(0.0, f64::max),
        )
    } else {
        (identity3().map(|row| row.map(|x| x * h * h)), h)
    };
    let density = neighbors
        .iter()
        .map(|(_, distance, mass)| {
            mass * cubic_sph(distance / h) / (std::f64::consts::PI * h.powi(3))
        })
        .sum();
    let displacement = sub(center, original);
    FitOracle {
        center,
        support_tensor,
        max_axis,
        density,
        reach: dot(displacement, displacement).sqrt() + max_axis,
        neighbors: neighbors.len(),
    }
}

fn shape_tensor(shape: &Shape64) -> Matrix3 {
    let axes = [shape.axis_x, shape.axis_y, shape.axis_z];
    std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            axes.iter()
                .map(|axis| f64::from(axis[i]) * f64::from(axis[j]))
                .sum()
        })
    })
}

fn fit_particle(position: [f32; 3], mass: f32) -> WaterParticle {
    WaterParticle {
        position_mass: [position[0], position[1], position[2], mass],
        velocity_density: [1.0, -2.0, 3.0, 777.0],
        previous_position: [position[0], position[1], position[2], 0.0],
        ..bytemuck::Zeroable::zeroed()
    }
}

fn fit_fixture() -> Vec<WaterParticle> {
    let mut points = Vec::new();
    // Interior covariance is isotropic; boundary records exercise relocation.
    for z in -3..=3 {
        for y in -3..=3 {
            for x in -3..=3 {
                points.push(fit_particle(
                    [
                        -0.55 + x as f32 * 0.03125,
                        0.55 + y as f32 * 0.03125,
                        -0.55 + z as f32 * 0.03125,
                    ],
                    0.03,
                ));
            }
        }
    }
    let (s, c) = std::f32::consts::FRAC_PI_6.sin_cos();
    for z in -4..=4 {
        for x in -4..=4 {
            let u = x as f32 * 0.0234375;
            points.push(fit_particle(
                [0.5 + u * c, 0.55 + u * s, 0.5 + z as f32 * 0.0234375],
                0.025,
            ));
        }
    }
    // Sparse pair must still relocate; mass changes density, not fit weights.
    points.push(fit_particle([-0.6, 1.3, 0.5], 0.03));
    points.push(fit_particle([-0.55, 1.3, 0.5], 0.015));
    points.push(fit_particle([0.4, 1.4, -0.5], 0.03));
    points.push(fit_particle([0.4, 1.4, -0.5], 0.0));
    points
}

fn native_fit(points: &[WaterParticle], h: f32) -> Vec<Shape64> {
    let device = crate::harness::shared().device.as_ref();
    let source = manifold_renderer::node_graph::primitives::water_surface_fit_shader();
    let pipeline = device.create_compute_pipeline(&source, "cs_main", "yu-turk-fit-proof");
    let mut heads = vec![0_u32; 32768];
    let mut next = vec![0_u32; points.len()];
    for (i, p) in points.iter().enumerate() {
        // Deliberately link inactive records; the fit must exclude them.
        let cell: [i32; 3] = std::array::from_fn(|d| {
            ((p.position_mass[d] - [-2.0, 0.0, -2.0][d]) / 0.125).floor() as i32
        });
        assert!(cell.iter().all(|c| (0..32).contains(c)));
        let bin = (cell[0] + 32 * (cell[1] + 32 * cell[2])) as usize;
        next[i] = heads[bin];
        heads[bin] = i as u32 + 1;
    }
    let expected: [Vec<u8>; 3] = [
        bytemuck::cast_slice(points).to_vec(),
        bytemuck::cast_slice(&heads).to_vec(),
        bytemuck::cast_slice(&next).to_vec(),
    ];
    let inputs = expected
        .each_ref()
        .map(|bytes| device.create_buffer_shared(bytes.len() as u64));
    let shapes = device.create_buffer_shared(points.len() as u64 * 64);
    let components = device.create_buffer_shared(4);
    components.zero_fill();
    let uniform = [h.to_bits(), 0.95_f32.to_bits(), points.len() as u32, 0];
    crate::harness::retry_on_gpu_commit_error(|| {
        for (input, bytes) in inputs.iter().zip(&expected) {
            unsafe {
                input.write(0, bytes);
            }
        }
        let mut encoder = device.create_encoder("yu-turk-fit-proof");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniform),
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
                GpuBinding::Buffer { binding: 4, buffer: &components, offset: 0 },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: &shapes,
                    offset: 0,
                },
            ],
            [(points.len() as u32).div_ceil(256), 1, 1],
            "yu-turk-fit-proof",
        );
        encoder.commit_and_wait_completed();
    });
    for (i, (input, bytes)) in inputs.iter().zip(&expected).enumerate() {
        let actual =
            unsafe { std::slice::from_raw_parts(input.mapped_ptr().unwrap(), bytes.len()) };
        assert_eq!(actual, bytes, "fit modified GPU input {i}");
    }
    unsafe {
        std::slice::from_raw_parts(shapes.mapped_ptr().unwrap().cast::<Shape64>(), points.len())
            .to_vec()
    }
}

fn verify_fit(points: &[WaterParticle], h: f64, shapes: &[Shape64]) {
    for (index, (p, actual)) in points.iter().zip(shapes).enumerate() {
        if p.position_mass[3] == 0.0 {
            assert!(bytemuck::bytes_of(actual).iter().all(|b| *b == 0));
            continue;
        }
        let expected = fit_oracle(points, index, h);
        let tensor = shape_tensor(actual);
        for (i, row) in tensor.iter().enumerate() {
            assert!(
                (f64::from(actual.center_radius[i]) - expected.center[i]).abs() < 2.0e-6,
                "center record {index}, axis {i}: {} != {}",
                actual.center_radius[i],
                expected.center[i]
            );
            for (j, &value) in row.iter().enumerate() {
                assert!(
                    (value - expected.support_tensor[i][j]).abs()
                        <= 2.0e-7 * h * h + 3.0e-4 * expected.max_axis.powi(2),
                    "support tensor record {index} ({i},{j}): {} != {}",
                    value,
                    expected.support_tensor[i][j]
                );
            }
        }
        for (label, actual, expected) in [
            (
                "max support",
                f64::from(actual.center_radius[3]),
                expected.max_axis,
            ),
            ("SPH density", f64::from(actual.axis_x[3]), expected.density),
            ("search reach", f64::from(actual.axis_y[3]), expected.reach),
        ] {
            assert!(
                (actual - expected).abs() <= 1.0e-7 + 3.0e-4 * expected.abs(),
                "{label}, record {index}: {actual} != {expected}"
            );
        }
        assert_eq!(actual.axis_z[3], 0.0);
        let axes = [actual.axis_x, actual.axis_y, actual.axis_z];
        for (i, a) in axes.iter().enumerate() {
            for b in axes.iter().skip(i + 1) {
                let a = a.map(f64::from);
                let b = b.map(f64::from);
                let ab: f64 = (0..3).map(|d| a[d] * b[d]).sum();
                assert!(ab.abs() <= expected.max_axis.powi(2) * 1.0e-5);
            }
        }
    }
}

#[test]
fn water_surface_fit_yu_turk_lattice_plane_sparse_and_scale_match_f64() {
    let points = fit_fixture();
    let h = 0.0625;
    let output = native_fit(&points, h);
    verify_fit(&points, f64::from(h), &output);
    let lattice = fit_oracle(&points, 171, f64::from(h));
    assert!(lattice.neighbors > 25);
    let plane = fit_oracle(&points, 343 + 40, f64::from(h));
    assert!(plane.neighbors > 25);
    let (eigenvalues, _) = eigen_f64(plane.support_tensor);
    assert!(
        (eigenvalues.into_iter().fold(0.0, f64::max)
            / eigenvalues.into_iter().fold(f64::INFINITY, f64::min)
            - 16.0)
            .abs()
            < 1.0e-8
    );
    let sparse = fit_oracle(&points, points.len() - 4, f64::from(h));
    assert_eq!(sparse.neighbors, 2);
    assert!(sparse.center[0] - f64::from(points[points.len() - 4].position_mass[0]) > 0.02);
    let isolated = fit_oracle(&points, points.len() - 2, f64::from(h));
    assert_eq!(isolated.neighbors, 1);
    assert!(isolated.density > 0.0, "self contribution is required");
    // Scale geometry and h together, holding particle masses fixed.
    let scaled: Vec<_> = points
        .iter()
        .map(|p| {
            let mut p = *p;
            for coordinate in &mut p.position_mass[..3] {
                *coordinate *= 2.0;
            }
            p
        })
        .collect();
    let scaled_output = native_fit(&scaled, h * 2.0);
    verify_fit(&scaled, f64::from(h * 2.0), &scaled_output);
    for (i, (a, b)) in output.iter().zip(&scaled_output).enumerate() {
        if points[i].position_mass[3] == 0.0 {
            continue;
        }
        for (&original, &scaled) in a.center_radius.iter().zip(&b.center_radius) {
            assert!((scaled - 2.0 * original).abs() < 4.0e-6);
        }
        let ta = shape_tensor(a);
        let tb = shape_tensor(b);
        for (original_row, scaled_row) in ta.iter().zip(&tb) {
            for (&original, &scaled) in original_row.iter().zip(scaled_row) {
                assert!(
                    (scaled - 4.0 * original).abs()
                        < 2.0e-5 * f64::from(b.center_radius[3]).powi(2)
                );
            }
        }
        assert!((f64::from(b.axis_x[3]) * 8.0 / f64::from(a.axis_x[3]) - 1.0).abs() < 1.0e-4);
        assert!((b.axis_y[3] - 2.0 * a.axis_y[3]).abs() < 4.0e-6);
    }
}

#[test]
fn water_surface_fit_yu_turk_sparse_threshold_includes_self() {
    // Every point is within 2h of every other point. Exactly 25 records must
    // use the sparse branch; adding the 26th must use the covariance branch.
    let mut points = Vec::new();
    for y in -2..=2 {
        for x in -2..=2 {
            points.push(fit_particle(
                [0.5 + x as f32 * 0.01, 1.0 + y as f32 * 0.01, 0.5],
                0.03,
            ));
        }
    }
    let sparse = native_fit(&points, 0.0625);
    verify_fit(&points, 0.0625, &sparse);
    assert_eq!(fit_oracle(&points, 12, 0.0625).neighbors, 25);
    points.push(fit_particle([0.5, 1.0, 0.51], 0.03));
    let dense = native_fit(&points, 0.0625);
    verify_fit(&points, 0.0625, &dense);
    assert_eq!(fit_oracle(&points, 12, 0.0625).neighbors, 26);
    assert!((dense[12].center_radius[3] - sparse[12].center_radius[3]).abs() > 0.02);
}
