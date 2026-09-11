//! Native component labels compared with independent CPU breadth-first search.
//! The fit proof checks Eq.17's center filtering and Eq.1's unfiltered density.
use crate::harness;
use bytemuck::{Pod, Zeroable};
use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuDevice};
use manifold_renderer::node_graph::primitives::water_components::{
    roots_shader_source, seed_shader_source, union_shader_source,
};
use manifold_renderer::node_graph::primitives::water_surface_fit_shader;
use manifold_renderer::node_graph::water::WaterParticle;
use std::collections::VecDeque;

const CONNECTION_RADIUS: f32 = 0.03125;
const H: f32 = 0.0625;
const INACTIVE: u32 = u32::MAX;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Shape {
    center: [f32; 4],
    x: [f32; 4],
    y: [f32; 4],
    z: [f32; 4],
}

fn particle(x: [f32; 3], mass: f32) -> WaterParticle {
    WaterParticle {
        position_mass: [x[0], x[1], x[2], mass],
        velocity_density: [1.0, -2.0, 3.0, 1000.0],
        previous_position: [x[0] - 0.01, x[1] + 0.01, x[2], 0.0],
        ..WaterParticle::zeroed()
    }
}

fn live(p: &WaterParticle) -> bool {
    p.position_mass.iter().all(|v| v.is_finite())
        && p.position_mass[3] > 0.0
        && (0..3).all(|d| {
            p.position_mass[d] >= [-2.0, 0.0, -2.0][d] && p.position_mass[d] < [2.0, 4.0, 2.0][d]
        })
}

fn distance2(a: &WaterParticle, b: &WaterParticle) -> f64 {
    (0..3)
        .map(|d| (f64::from(a.position_mass[d]) - f64::from(b.position_mass[d])).powi(2))
        .sum()
}

/// Exhaustive graph BFS, independent of GPU bins and union-find parent order.
fn component_oracle(points: &[WaterParticle], radius: f32) -> Vec<u32> {
    let mut labels = vec![INACTIVE; points.len()];
    for seed in 0..points.len() {
        if labels[seed] != INACTIVE || !live(&points[seed]) {
            continue;
        }
        labels[seed] = seed as u32;
        let mut queue = VecDeque::from([seed]);
        while let Some(i) = queue.pop_front() {
            for (j, p) in points.iter().enumerate() {
                if labels[j] == INACTIVE
                    && live(p)
                    && distance2(&points[i], p) <= f64::from(radius).powi(2)
                {
                    labels[j] = seed as u32;
                    queue.push_back(j);
                }
            }
        }
    }
    labels
}

fn bins(points: &[WaterParticle]) -> (Vec<u32>, Vec<u32>) {
    let mut heads = vec![0; 32768];
    let mut next = vec![0; points.len()];
    for (i, p) in points.iter().enumerate() {
        if !live(p) {
            continue;
        }
        let c: [usize; 3] = std::array::from_fn(|d| {
            ((p.position_mass[d] - [-2.0, 0.0, -2.0][d]) / 0.125).floor() as usize
        });
        let bin = c[0] + 32 * (c[1] + 32 * c[2]);
        next[i] = heads[bin];
        heads[bin] = i as u32 + 1;
    }
    (heads, next)
}

struct Components {
    device: &'static GpuDevice,
    seed: GpuComputePipeline,
    union: GpuComputePipeline,
    roots: GpuComputePipeline,
    fit: GpuComputePipeline,
}
struct ResultData {
    labels: Vec<u32>,
    filtered: Vec<Shape>,
    unfiltered: Vec<Shape>,
}
impl Components {
    fn new() -> Self {
        let device = harness::shared().device.as_ref();
        Self {
            device,
            seed: device.create_compute_pipeline(
                &seed_shader_source(),
                "cs_main",
                "component-seed-proof",
            ),
            union: device.create_compute_pipeline(
                union_shader_source(),
                "cs_main",
                "component-union-proof",
            ),
            roots: device.create_compute_pipeline(
                &roots_shader_source(),
                "cs_main",
                "component-roots-proof",
            ),
            fit: device.create_compute_pipeline(
                &water_surface_fit_shader(),
                "cs_main",
                "component-fit-proof",
            ),
        }
    }
    fn run(&self, points: &[WaterParticle], radius: f32, fit: bool) -> ResultData {
        assert!(!points.is_empty());
        let (heads, next) = bins(points);
        let expected_inputs: [Vec<u8>; 3] = [
            bytemuck::cast_slice(points).to_vec(),
            bytemuck::cast_slice(&heads).to_vec(),
            bytemuck::cast_slice(&next).to_vec(),
        ];
        let inputs = expected_inputs
            .each_ref()
            .map(|bytes| self.device.create_buffer_shared(bytes.len() as u64));
        let parents = self.device.create_buffer_shared(points.len() as u64 * 4);
        let labels = self.device.create_buffer_shared(points.len() as u64 * 4);
        let sentinel = self.device.create_buffer_shared(4);
        let filtered = self.device.create_buffer_shared(points.len() as u64 * 64);
        let unfiltered = self.device.create_buffer_shared(points.len() as u64 * 64);
        let count = points.len() as u32;
        let dispatch = [count.div_ceil(256), 1, 1];
        let unary = [count, 0, 0, 0];
        let union = [radius.to_bits(), count, 0, 0];
        let fit_uniform = [H.to_bits(), 0.95_f32.to_bits(), count, 0];
        harness::retry_on_gpu_commit_error(|| {
            for (input, expected) in inputs.iter().zip(&expected_inputs) {
                unsafe {
                    input.write(0, expected);
                }
            }
            unsafe {
                sentinel.write(0, bytemuck::bytes_of(&0_u32));
            }
            let mut encoder = self.device.create_encoder("water-components-proof");
            encoder.dispatch_compute(
                &self.seed,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&unary),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: &inputs[0],
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: &parents,
                        offset: 0,
                    },
                ],
                dispatch,
                "component-seed-proof",
            );
            encoder.dispatch_compute(
                &self.union,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&union),
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
                        buffer: &parents,
                        offset: 0,
                    },
                ],
                dispatch,
                "component-union-proof",
            );
            encoder.dispatch_compute(
                &self.roots,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&unary),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: &parents,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: &labels,
                        offset: 0,
                    },
                ],
                dispatch,
                "component-roots-proof",
            );
            if fit {
                for (components, output) in [(&labels, &filtered), (&sentinel, &unfiltered)] {
                    encoder.dispatch_compute(
                        &self.fit,
                        &[
                            GpuBinding::Bytes {
                                binding: 0,
                                data: bytemuck::bytes_of(&fit_uniform),
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
                                buffer: components,
                                offset: 0,
                            },
                            GpuBinding::Buffer {
                                binding: 5,
                                buffer: output,
                                offset: 0,
                            },
                        ],
                        dispatch,
                        "component-fit-proof",
                    );
                }
            }
            encoder.commit_and_wait_completed();
        });
        for (i, (input, expected)) in inputs.iter().zip(&expected_inputs).enumerate() {
            let actual =
                unsafe { std::slice::from_raw_parts(input.mapped_ptr().unwrap(), expected.len()) };
            assert_eq!(actual, expected, "components modified GPU input {i}");
        }
        let actual_labels = unsafe {
            std::slice::from_raw_parts(labels.mapped_ptr().unwrap().cast::<u32>(), points.len())
                .to_vec()
        };
        assert_eq!(
            actual_labels,
            component_oracle(points, radius),
            "native components disagree with BFS"
        );
        let actual_parents = unsafe {
            std::slice::from_raw_parts(parents.mapped_ptr().unwrap().cast::<u32>(), points.len())
        };
        for (i, &parent) in actual_parents.iter().enumerate() {
            if live(&points[i]) {
                assert!(parent <= i as u32, "parent must descend");
            } else {
                assert_eq!(parent, INACTIVE);
            }
        }
        assert_eq!(unsafe { *sentinel.mapped_ptr().unwrap().cast::<u32>() }, 0);
        let read_shapes = |buffer: &manifold_gpu::GpuBuffer| unsafe {
            std::slice::from_raw_parts(buffer.mapped_ptr().unwrap().cast::<Shape>(), points.len())
                .to_vec()
        };
        ResultData {
            labels: actual_labels,
            filtered: if fit {
                read_shapes(&filtered)
            } else {
                Vec::new()
            },
            unfiltered: if fit {
                read_shapes(&unfiltered)
            } else {
                Vec::new()
            },
        }
    }
}

fn sheets(bridge: bool) -> Vec<WaterParticle> {
    let mut points = Vec::new();
    for y in [1.0, 1.05] {
        for z in -3..=3 {
            for x in -3..=3 {
                points.push(particle([x as f32 * 0.025, y, z as f32 * 0.025], 0.03));
            }
        }
    }
    if bridge {
        points.push(particle([0.0, 1.025, 0.0], 0.03));
    }
    points.push(particle([0.0, 1.025, 0.0], 0.0));
    points
}

fn center_and_density(
    points: &[WaterParticle],
    labels: Option<&[u32]>,
    index: usize,
) -> ([f64; 3], f64) {
    let h = f64::from(H);
    let support = 2.0 * h;
    let mut weighted = [0.0; 3];
    let mut total = 0.0;
    let mut density = 0.0;
    for (j, p) in points.iter().enumerate().filter(|(_, p)| live(p)) {
        let distance = distance2(&points[index], p).sqrt();
        if distance >= support {
            continue;
        }
        let q = distance / h;
        let cubic = if q < 1.0 {
            1.0 - 1.5 * q * q + 0.75 * q * q * q
        } else {
            0.25 * (2.0 - q).powi(3)
        };
        density += f64::from(p.position_mass[3]) * cubic / (std::f64::consts::PI * h.powi(3));
        if labels.is_none_or(|labels| labels[index] == labels[j]) {
            let weight = 1.0 - (distance / support).powi(3);
            for (d, sum) in weighted.iter_mut().enumerate() {
                *sum += weight * f64::from(p.position_mass[d]);
            }
            total += weight;
        }
    }
    (
        std::array::from_fn(|d| {
            0.05 * f64::from(points[index].position_mass[d]) + 0.95 * weighted[d] / total
        }),
        density,
    )
}

#[test]
fn water_components_separate_sheets_prevent_fit_attraction_and_bridge_rejoins() {
    let gpu = Components::new();
    for bridge in [false, true] {
        let points = sheets(bridge);
        let result = gpu.run(&points, CONNECTION_RADIUS, true);
        assert_eq!(result.labels[0], 0);
        assert_eq!(result.labels[49], if bridge { 0 } else { 49 });
        for (i, p) in points.iter().enumerate().filter(|(_, p)| live(p)) {
            for (labels, actual) in [
                (Some(result.labels.as_slice()), &result.filtered[i]),
                (None, &result.unfiltered[i]),
            ] {
                let (center, density) = center_and_density(&points, labels, i);
                for (d, &expected) in center.iter().enumerate() {
                    assert!(
                        (f64::from(actual.center[d]) - expected).abs() < 2.0e-6,
                        "center {i}/{d}"
                    );
                }
                assert!(
                    (f64::from(actual.x[3]) / density - 1.0).abs() < 3.0e-4,
                    "SPH density must retain all neighbors"
                );
            }
            assert!(p.position_mass[3] > 0.0);
        }
        if bridge {
            assert_eq!(
                bytemuck::cast_slice::<Shape, u8>(&result.filtered),
                bytemuck::cast_slice::<Shape, u8>(&result.unfiltered),
                "a connected bridge restores the unrestricted neighborhood"
            );
        } else {
            let lower_center = 24;
            assert!((result.filtered[lower_center].center[1] - 1.0).abs() < 1.0e-6);
            assert!(
                result.unfiltered[lower_center].center[1] - 1.0 > 0.015,
                "fixture must expose inter-sheet attraction"
            );
            let actual = &result.filtered[lower_center];
            let axes = [actual.x, actual.y, actual.z];
            let xx: f64 = axes.iter().map(|a| f64::from(a[0]).powi(2)).sum();
            let yy: f64 = axes.iter().map(|a| f64::from(a[1]).powi(2)).sum();
            assert!(
                (xx / yy - 16.0).abs() < 0.01,
                "filtered covariance must preserve thin-sheet normal"
            );
            let own_sheet_density = center_and_density(&points[..49], None, lower_center).1;
            assert!(
                f64::from(actual.x[3]) > own_sheet_density * 1.2,
                "density must include the other sheet"
            );
        }
        let inactive = result.filtered.last().unwrap();
        assert!(bytemuck::bytes_of(inactive).iter().all(|b| *b == 0));
    }
}

#[test]
fn water_components_shuffled_chain_longer_than_workgroup_matches_bfs() {
    // Twelve rows form a single chain of 801 points. Rows are separated by
    // 4ra, with three extra bridge points at alternate ends. Thus graph
    // distance to the first point exceeds 256 without spatial shortcuts.
    let mut ordered = Vec::new();
    for row in 0..12 {
        for column in 0..64 {
            let x = if row % 2 == 0 { column } else { 63 - column };
            ordered.push(particle(
                [
                    -1.0 + x as f32 * CONNECTION_RADIUS,
                    1.0,
                    -1.0 + row as f32 * 0.125,
                ],
                0.03,
            ));
        }
        if row < 11 {
            let x = if row % 2 == 0 { 63 } else { 0 };
            for step in 1..4 {
                ordered.push(particle(
                    [
                        -1.0 + x as f32 * CONNECTION_RADIUS,
                        1.0,
                        -1.0 + row as f32 * 0.125 + step as f32 * CONNECTION_RADIUS,
                    ],
                    0.03,
                ));
            }
        }
    }
    assert_eq!(ordered.len(), 801);
    let mut points: Vec<_> = (0..ordered.len())
        .map(|i| ordered[(i * 337) % ordered.len()])
        .collect();
    points.push(particle([1.5, 3.0, 1.5], 0.03));
    points.push(particle([-1.0, 1.0, -1.0], 0.0));
    let result = Components::new().run(&points, CONNECTION_RADIUS, false);
    assert!(result.labels[..801].iter().all(|l| *l == 0));
    assert_eq!(result.labels[801], 801);
    assert_eq!(result.labels[802], INACTIVE);
}

#[test]
fn water_components_inclusive_radius_and_inactive_records() {
    let gpu = Components::new();
    let points = vec![
        particle([0.0, 2.0, 0.0], 0.03),
        particle([CONNECTION_RADIUS, 2.0, 0.0], 0.03),
        particle([0.06250006, 2.0, 0.0], 0.03),
        particle([0.046875, 2.0, 0.0], 0.0),
        particle([f32::NAN, 2.0, 0.0], 0.03),
    ];
    let result = gpu.run(&points, CONNECTION_RADIUS, false);
    assert_eq!(result.labels, vec![0, 0, 2, INACTIVE, INACTIVE]);
    let smaller = gpu.run(&points, 0.02, false);
    assert_eq!(smaller.labels, vec![0, 1, 2, INACTIVE, INACTIVE]);
    let inactive = vec![WaterParticle::zeroed(); 513];
    assert!(
        gpu.run(&inactive, CONNECTION_RADIUS, false)
            .labels
            .iter()
            .all(|label| *label == INACTIVE)
    );
}
