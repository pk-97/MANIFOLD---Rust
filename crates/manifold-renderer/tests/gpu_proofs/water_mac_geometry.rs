//! Native proof of the production generated stationary box geometry shader.
//! Expected fractions use independent f64 interval intersections.
use crate::harness;
use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuDevice};
use manifold_renderer::node_graph::freeze::codegen::ENTRY;
use manifold_renderer::node_graph::primitives::mac_box_fractions::{
    self, DEFAULT_BASIN_MAX, DEFAULT_BASIN_MIN, DEFAULT_BOX_MAX, DEFAULT_BOX_MIN, MAC_BOX_ENTRIES,
    MacBoxCell, MacBoxFractionsUniforms,
};

const H: f64 = 0.0625;
const ORIGIN: [f64; 3] = [-2.0, 0.0, -2.0];
// The fixture coordinates are exactly representable grid multiples. The
// allowance covers f32 clipping products and subtraction against f64.
const FRACTION_EPS: f64 = 3.0e-6;

fn defaults() -> MacBoxFractionsUniforms {
    MacBoxFractionsUniforms {
        basin_min: DEFAULT_BASIN_MIN,
        basin_max: DEFAULT_BASIN_MAX,
        box_min: DEFAULT_BOX_MIN,
        box_max: DEFAULT_BOX_MAX,
        dispatch_count: MAC_BOX_ENTRIES,
        padding: [0; 3],
    }
}

struct Geometry {
    device: &'static GpuDevice,
    pipeline: GpuComputePipeline,
}

impl Geometry {
    fn new() -> Self {
        let device = harness::shared().device.as_ref();
        let source = mac_box_fractions::shader_source();
        Self {
            device,
            pipeline: device.create_compute_pipeline(&source, ENTRY, "gpu-proof.mac-geometry"),
        }
    }

    fn run(&self, uniforms: &MacBoxFractionsUniforms) -> Vec<MacBoxCell> {
        let output = self
            .device
            .create_buffer_shared(u64::from(MAC_BOX_ENTRIES) * 16);
        harness::retry_on_gpu_commit_error(|| {
            unsafe {
                output.write(0, &vec![0xa5; MAC_BOX_ENTRIES as usize * 16]);
            }
            let mut encoder = self.device.create_encoder("gpu-proof.mac-geometry");
            encoder.dispatch_compute(
                &self.pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: &output,
                        offset: 0,
                    },
                ],
                [MAC_BOX_ENTRIES.div_ceil(256), 1, 1],
                "gpu-proof.mac-geometry",
            );
            encoder.commit_and_wait_completed();
        });
        unsafe {
            std::slice::from_raw_parts(
                output.mapped_ptr().unwrap().cast::<MacBoxCell>(),
                MAC_BOX_ENTRIES as usize,
            )
            .to_vec()
        }
    }
}

#[derive(Clone, Copy)]
struct Box64 {
    lo: [f64; 3],
    hi: [f64; 3],
}

impl Box64 {
    fn intersection(self, other: Self) -> Self {
        Self {
            lo: std::array::from_fn(|d| self.lo[d].max(other.lo[d])),
            hi: std::array::from_fn(|d| self.hi[d].min(other.hi[d])),
        }
    }

    fn length(self, axis: usize) -> f64 {
        (self.hi[axis] - self.lo[axis]).max(0.0)
    }

    fn volume(self) -> f64 {
        (0..3).map(|d| self.length(d)).product()
    }
}

fn bounds(uniforms: &MacBoxFractionsUniforms) -> (Box64, Box64) {
    (
        Box64 {
            lo: uniforms.basin_min.map(f64::from),
            hi: uniforms.basin_max.map(f64::from),
        },
        Box64 {
            lo: uniforms.box_min.map(f64::from),
            hi: uniforms.box_max.map(f64::from),
        },
    )
}

fn oracle(coordinate: [usize; 3], basin: Box64, obstacle: Box64) -> [f64; 4] {
    let cell = Box64 {
        lo: std::array::from_fn(|d| ORIGIN[d] + H * coordinate[d] as f64),
        hi: std::array::from_fn(|d| ORIGIN[d] + H * (coordinate[d] + 1) as f64),
    };
    let mut expected = [0.0; 4];
    if coordinate.iter().all(|&c| c < 64) {
        let inside = cell.intersection(basin);
        expected[3] = (inside.volume() - inside.intersection(obstacle).volume()) / H.powi(3);
    }
    for axis in 0..3 {
        let normal = cell.lo[axis];
        if coordinate[axis] == 0
            || coordinate[axis] == 64
            || coordinate
                .iter()
                .enumerate()
                .any(|(d, &c)| d != axis && c == 64)
            || normal <= basin.lo[axis]
            || normal >= basin.hi[axis]
        {
            continue;
        }
        // Integrate the rectangular face only in the two tangential axes.
        let area = |region: Box64| -> f64 {
            (0..3)
                .filter(|&d| d != axis)
                .map(|d| (cell.hi[d].min(region.hi[d]) - cell.lo[d].max(region.lo[d])).max(0.0))
                .product()
        };
        let solid = if normal >= obstacle.lo[axis] && normal <= obstacle.hi[axis] {
            area(basin.intersection(obstacle))
        } else {
            0.0
        };
        expected[axis] = (area(basin) - solid) / H.powi(2);
    }
    expected
}

fn verify(output: &[MacBoxCell], uniforms: &MacBoxFractionsUniforms) -> [usize; 4] {
    assert_eq!(output.len(), MAC_BOX_ENTRIES as usize);
    let (basin, obstacle) = bounds(uniforms);
    let mut partial = [0; 4];
    let mut volume = 0.0;
    for (index, actual) in output.iter().enumerate() {
        let c = [index % 65, (index / 65) % 65, index / (65 * 65)];
        let expected = oracle(c, basin, obstacle);
        for (axis, (&actual, &expected)) in actual.mac_open.iter().zip(&expected).enumerate() {
            assert!(
                (0.0..=1.0).contains(&actual),
                "non-fraction {actual} at {c:?} component {axis}"
            );
            assert!(
                (f64::from(actual) - expected).abs() <= FRACTION_EPS,
                "fraction at {c:?} component {axis}: {actual} != {expected}"
            );
            if expected == 0.0 {
                assert_eq!(
                    actual, 0.0,
                    "closed boundary/padding at {c:?} component {axis}"
                );
            }
            if expected > 0.0 && expected < 1.0 {
                partial[axis] += 1;
            }
        }
        volume += f64::from(actual.mac_open[3]) * H.powi(3);
    }
    // Inclusion-exclusion at world scale independently accounts for all cells.
    let world = Box64 {
        lo: ORIGIN,
        hi: std::array::from_fn(|d| ORIGIN[d] + 64.0 * H),
    };
    let fluid = world.intersection(basin);
    let expected_volume = fluid.volume() - fluid.intersection(obstacle).volume();
    assert!(
        (volume - expected_volume).abs() < 2.0e-5,
        "total volume {volume} != {expected_volume}"
    );
    partial
}

#[test]
fn water_mac_geometry_default_partial_faces_and_volumes_match_f64() {
    let uniforms = defaults();
    let output = Geometry::new().run(&uniforms);
    let partial = verify(&output, &uniforms);
    assert!(
        partial.iter().all(|&n| n > 0),
        "fixture must exercise partial x/y/z faces and cells: {partial:?}"
    );
}

#[test]
fn water_mac_geometry_changed_bounds_and_obstacle_outside_basin_match_f64() {
    let geometry = Geometry::new();
    let original = defaults();
    let baseline = geometry.run(&original);
    let changed = MacBoxFractionsUniforms {
        basin_min: [-1.213, 0.419, -0.781],
        basin_max: [1.481, 2.913, 1.017],
        // Partially outside the basin: subtract only the triple intersection.
        box_min: [-1.543, 0.273, -0.935],
        box_max: [-0.847, 1.339, -0.291],
        ..original
    };
    let output = geometry.run(&changed);
    verify(&output, &changed);
    assert!(
        output
            .iter()
            .zip(&baseline)
            .any(|(a, b)| a.mac_open != b.mac_open)
    );
    let disjoint = MacBoxFractionsUniforms {
        box_min: [-0.5, 10.0, -0.5],
        box_max: [0.5, 11.0, 0.5],
        ..original
    };
    verify(&geometry.run(&disjoint), &disjoint);
}

#[test]
fn water_mac_geometry_world_and_exact_box_boundaries_are_closed() {
    let geometry = Geometry::new();
    let world = MacBoxFractionsUniforms {
        basin_min: [-3.0, -1.0, -3.0],
        basin_max: [3.0, 5.0, 3.0],
        box_min: [6.0; 3],
        box_max: [7.0; 3],
        ..defaults()
    };
    let output = geometry.run(&world);
    verify(&output, &world);
    assert_eq!(output[0].mac_open, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(output[64].mac_open, [0.0; 4]);
    assert_eq!(output[1 + 65 * (1 + 65)].mac_open, [1.0; 4]);

    let aligned = MacBoxFractionsUniforms {
        basin_min: [-1.0, 0.5, -1.0],
        basin_max: [1.0, 3.5, 1.0],
        box_min: [0.0, 1.0, 0.0],
        box_max: [0.5, 2.0, 0.5],
        ..defaults()
    };
    verify(&geometry.run(&aligned), &aligned);
}

#[test]
fn water_mac_geometry_rejects_nonfinite_and_nonincreasing_bounds() {
    let geometry = Geometry::new();
    let mut invalid = Vec::new();
    // Every scalar component participates in the finite check.
    for field in 0..4 {
        for axis in 0..3 {
            let mut value = defaults();
            match field {
                0 => value.basin_min[axis] = f32::NAN,
                1 => value.basin_max[axis] = f32::INFINITY,
                2 => value.box_min[axis] = f32::NEG_INFINITY,
                _ => value.box_max[axis] = f32::NAN,
            }
            invalid.push(value);
        }
    }
    for axis in 0..3 {
        let mut reversed = defaults();
        reversed.basin_min[axis] = reversed.basin_max[axis] + 0.25;
        invalid.push(reversed);
        let mut flat = defaults();
        flat.box_max[axis] = flat.box_min[axis];
        invalid.push(flat);
    }
    for uniforms in invalid {
        let output = geometry.run(&uniforms);
        assert!(
            output
                .iter()
                .flat_map(|cell| cell.mac_open)
                .all(|v| v.is_nan()),
            "invalid bounds must poison every component, including padding"
        );
    }
}
