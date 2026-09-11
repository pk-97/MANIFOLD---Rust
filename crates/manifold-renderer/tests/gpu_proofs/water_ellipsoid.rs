//! Independent f64 oracle and fixtures for anisotropic water raster proofs.
//! Native raster outputs are compared to independent f64 ray intersections.

use manifold_gpu::{GpuBinding, GpuDevice};
use manifold_renderer::node_graph::camera::delinearize_depth;

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
            let bindings = [
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
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &shapes,
                    offset: 0,
                },
            ];
            enc.dispatch_compute(
                if sphere { &legacy } else { &fitted },
                &bindings[..if sphere { 4 } else { 5 }],
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

#[test]
fn water_surface_fit_native_plane_and_droplet() {
    let device = GpuDevice::new();
    let bins = device.create_compute_pipeline(
        include_str!("../../src/node_graph/primitives/shaders/water_particle_bins.wgsl"),
        "cs_main",
        "fit-bins-proof",
    );
    let source = manifold_renderer::node_graph::primitives::water_surface_fit_shader();
    let fit = device.create_compute_pipeline(&source, "cs_main", "fit-proof");
    let radius = 0.046875f32;
    let mut points = vec![WaterParticle {
        position_mass: [0., 1., 0., 1.],
        ..bytemuck::Zeroable::zeroed()
    }];
    let (sin, cos) = std::f32::consts::FRAC_PI_6.sin_cos();
    for i in -3..=3 {
        for j in -3..=3 {
            if i == 0 && j == 0 {
                continue;
            }
            let x = i as f32 * 0.025;
            points.push(WaterParticle {
                position_mass: [x * cos, 1. + x * sin, j as f32 * 0.025, 1.],
                ..bytemuck::Zeroable::zeroed()
            });
        }
    }
    points.push(WaterParticle {
        position_mass: [0., 2., 0., 1.],
        ..bytemuck::Zeroable::zeroed()
    });
    points.push(WaterParticle {
        position_mass: [0., 3., 0., 0.],
        ..bytemuck::Zeroable::zeroed()
    });
    let n = points.len() as u32;
    let pb = device.create_buffer_shared(u64::from(n) * 96);
    let heads = device.create_buffer_shared(32768 * 4);
    let next = device.create_buffer_shared(u64::from(n) * 4);
    let shapes = device.create_buffer_shared(u64::from(n) * 64);
    unsafe {
        pb.write(0, bytemuck::cast_slice(&points));
    }
    let bu = [n, 0, 0, 0];
    let fu = [radius, 0.5, f32::from_bits(n), 0.];
    let mut e = device.create_encoder("fit-proof");
    e.clear_buffer(&heads);
    e.clear_buffer(&next);
    e.dispatch_compute(
        &bins,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::cast_slice(&bu),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pb,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &heads,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &next,
                offset: 0,
            },
        ],
        [n.div_ceil(256), 1, 1],
        "fit-bins-proof",
    );
    e.dispatch_compute(
        &fit,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::cast_slice(&fu),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &pb,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &heads,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &next,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &shapes,
                offset: 0,
            },
        ],
        [n.div_ceil(256), 1, 1],
        "fit-proof",
    );
    e.commit_and_wait_completed();
    let out = unsafe {
        std::slice::from_raw_parts(shapes.mapped_ptr().unwrap().cast::<Shape64>(), n as usize)
    };
    let norm = |a: [f32; 4]| (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    let p = out[0];
    let axes = [p.axis_x, p.axis_y, p.axis_z];
    let sizes = axes.map(norm);
    assert!(sizes.iter().all(|v| v.is_finite() && *v > 0.));
    let shortest = (0..3)
        .min_by(|a, b| sizes[*a].total_cmp(&sizes[*b]))
        .unwrap();
    let normal = [-sin, cos, 0.];
    let a = axes[shortest];
    let alignment =
        (a[0] * normal[0] + a[1] * normal[1] + a[2] * normal[2]).abs() / sizes[shortest];
    assert!(
        alignment > 0.999,
        "thin axis must align with rotated plane normal: {alignment}"
    );
    assert!(
        (sizes.iter().product::<f32>() / radius.powi(3) - 1.).abs() < 1e-4,
        "ellipsoid volume drift"
    );
    assert!(
        (sizes.iter().copied().fold(0., f32::max) / sizes[shortest] - 4.).abs() < 0.01,
        "fit must flatten the plane"
    );
    for i in 0..3 {
        for j in i + 1..3 {
            let a = axes[i];
            let b = axes[j];
            assert!((a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).abs() / (sizes[i] * sizes[j]) < 1e-4);
        }
    }
    assert!((p.center_radius[1] - 1.).abs() < 1e-5);
    let drop = out[out.len() - 2];
    for a in [drop.axis_x, drop.axis_y, drop.axis_z] {
        assert!((norm(a) - radius).abs() < 1e-6);
    }
    assert!(
        bytemuck::bytes_of(&out[out.len() - 1])
            .iter()
            .all(|v| *v == 0)
    );
    let after = unsafe { std::slice::from_raw_parts(pb.mapped_ptr().unwrap(), points.len() * 96) };
    assert_eq!(
        after,
        bytemuck::cast_slice::<WaterParticle,u8>(&points),
        "fitting must not modify simulation state"
    );
}
