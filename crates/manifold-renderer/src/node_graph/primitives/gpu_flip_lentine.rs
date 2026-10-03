//! Standalone stage-2 component coarse projection (BUG-lxxl).
//!
//! Not called by the step until boundary scatter and local projection exist.
//! The graph is Pᵀ A_boundary P / 2 in integrated fine-flux units at h=1,
//! exactly as scripts/lentine_reference.py. Its potential has the opposite
//! sign to the step pressure. No velocity is modified here.
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

const SHADER: &str = include_str!("shaders/gpu_flip_lentine.wgsl");
const ENTRIES: [&str; 10] = [
    "labels_main",
    "gather_main",
    "pockets_main",
    "init_main",
    "apply_main",
    "alpha_main",
    "update_main",
    "beta_main",
    "transfer_main",
    "finish_main",
];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    n: [u32; 3],
    count: u32,
    tolerance: f32,
    padding: [u32; 3],
}

/// Fine-grid inputs, all GPU buffers. `links` has one vec4 per cell: positive
/// axis open-face weights xyz and prescribed Dirichlet conductance w. Box
/// walls have zero weight. Edges to dry cells are excluded, never joined.
/// `source` is the COMPLETE integrated divergence to REMOVE, including child
/// solid sources minus the prescribed density source (h * step divergence).
/// Anchors are explicit: this is not stage 5's mixed free-surface solver.
pub struct ComponentInput<'a> {
    pub water: &'a GpuBuffer,
    pub links: &'a GpuBuffer,
    pub source: &'a GpuBuffer,
}

/// First word of `progress`. Following words: iterations, failing pocket,
/// reserved, then f32 rr, alpha, beta, initial |rhs|∞, true final |rhs-Ap|∞.
/// Incompatible sources are rejected without mean removal or pivot floors.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentStatus {
    Running = 0,
    Converged = 1,
    Capped = 2,
    Incompatible = 3,
    Breakdown = 4,
    InvalidInput = 5,
}

/// Reusable buffers; allocate only when the lattice changes. Pressure/rhs
/// live at the lowest fine-cell index of each component, zero elsewhere.
/// Labels map wet cells to those slots (dry = u32::MAX). `transfer` holds
/// oriented positive-axis boundary flux corrections xyz and cell Dirichlet
/// correction w, to SUBTRACT from input fluxes. Internal/closed faces are
/// zero. Invalid solves write NaNs. Consumers must check `progress`.
pub struct ComponentSolver {
    params: Params,
    pipelines: [GpuComputePipeline; 10],
    pub labels: GpuBuffer,
    pub pressure: GpuBuffer,
    pub rhs: GpuBuffer,
    pub transfer: GpuBuffer,
    pub progress: GpuBuffer,
    pockets: GpuBuffer,
    sums: GpuBuffer,
    compensation: GpuBuffer,
    vectors: GpuBuffer,
}

impl ComponentSolver {
    /// No algorithmic size cap; reject empty lattices and index overflow.
    pub fn new(device: &GpuDevice, lattice: [u32; 3]) -> Result<Self, String> {
        let count = lattice
            .into_iter()
            .try_fold(1u32, |n, side| n.checked_mul(side))
            .filter(|&n| n > 0 && n < u32::MAX)
            .ok_or_else(|| {
                format!("component lattice has empty or overflowing cell count: {lattice:?}")
            })?;
        let bytes = u64::from(count) * 4;
        Ok(Self {
            params: Params {
                n: lattice,
                count,
                tolerance: super::gpu_flip_pressure::TOLERANCE,
                padding: [0; 3],
            },
            pipelines: ENTRIES.map(|entry| device.create_compute_pipeline(SHADER, entry, entry)),
            labels: device.create_buffer_shared(bytes),
            pressure: device.create_buffer_shared(bytes),
            rhs: device.create_buffer_shared(bytes),
            transfer: device.create_buffer_shared(bytes * 4),
            progress: device.create_buffer_shared(64),
            pockets: device.create_buffer_shared(bytes),
            sums: device.create_buffer_shared(bytes * 4),
            compensation: device.create_buffer_shared(bytes * 4),
            vectors: device.create_buffer_shared(bytes * 4),
        })
    }

    /// Internal storage, excluding caller inputs.
    pub fn scratch_bytes(lattice: [u32; 3]) -> u64 {
        lattice.into_iter().map(u64::from).product::<u64>() * 80 + 64
    }

    /// Graph construction, gauged CG and conservative transfer. No readback,
    /// per-encode allocation, step dispatch changes or fallback. The iteration
    /// cap and stop tolerance are inherited from the existing pressure solver.
    pub fn encode(
        &self,
        encoder: &mut GpuEncoder,
        input: ComponentInput<'_>,
    ) -> Result<(), String> {
        let bytes = u64::from(self.params.count) * 4;
        for (name, buffer, required) in [
            ("water", input.water, bytes),
            ("links", input.links, bytes * 4),
            ("source", input.source, bytes),
        ] {
            if buffer.size() < required {
                return Err(format!(
                    "component {name} needs {required} bytes, got {}",
                    buffer.size()
                ));
            }
        }
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&self.params),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: input.water,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: input.links,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: input.source,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &self.labels,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: &self.rhs,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: &self.pressure,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 7,
                buffer: &self.pockets,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 8,
                buffer: &self.sums,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 9,
                buffer: &self.compensation,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 10,
                buffer: &self.vectors,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 11,
                buffer: &self.progress,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 12,
                buffer: &self.transfer,
                offset: 0,
            },
        ];
        let cells = [self.params.count.div_ceil(256), 1, 1];
        let blocks: u32 = self.params.n.into_iter().map(|n| n.div_ceil(2)).product();
        let mut pass = |index: usize, groups| {
            encoder.dispatch_compute(&self.pipelines[index], &bindings, groups, ENTRIES[index])
        };
        pass(0, [blocks.div_ceil(256), 1, 1]);
        pass(1, cells);
        pass(2, [1; 3]);
        pass(3, [1; 3]);
        for _ in 0..super::gpu_flip_pressure::MAX_ITERATIONS {
            pass(4, cells);
            pass(5, [1; 3]);
            pass(6, cells);
            pass(7, [1; 3]);
        }
        pass(9, [1; 3]);
        pass(8, cells);
        Ok(())
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::super::liquid_surface_tests::read;
    use super::*;

    #[derive(serde::Deserialize)]
    struct Fixture {
        name: String,
        lattice: [u32; 3],
        water: Vec<f32>,
        links: Vec<[f32; 4]>,
        source: Vec<f32>,
        status: u32,
        #[serde(default)]
        labels: Vec<u32>,
        #[serde(default)]
        rhs: Vec<f32>,
        #[serde(default)]
        pressure: Vec<f32>,
        #[serde(default)]
        transfer: Vec<[f32; 4]>,
    }
    fn upload<T: bytemuck::Pod>(device: &GpuDevice, values: &[T]) -> GpuBuffer {
        let buffer = device.create_buffer_shared(std::mem::size_of_val(values) as u64);
        // SAFETY: fresh shared allocation, sized for values, no in-flight work.
        unsafe {
            buffer.write(0, bytemuck::cast_slice(values));
        }
        buffer
    }
    fn close(actual: f32, expected: f32, scale: f32, message: &str) {
        assert!(
            actual.is_finite() && (actual - expected).abs() <= 2e-5 * scale.max(1.0),
            "{message}: {actual} != {expected}"
        );
    }

    /// Fresh f64 Cholesky oracle, not a Rust/CPU copy of the shader. Checks
    /// pressures, root identity, conservative gather/transfer, gauges, pocket
    /// rejection and component residuals. Re-encode on poisoned reusable state
    /// proves that construction does not rely on zero-filled allocations.
    #[test]
    fn component_coarse_matches_reference() {
        let reference = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/lentine_reference.py");
        let output = std::process::Command::new("python3")
            .arg(reference)
            .arg("--gpu-fixtures")
            .output()
            .expect("run f64 reference");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let fixtures: Vec<Fixture> =
            serde_json::from_slice(&output.stdout).expect("reference fixture JSON");
        assert_eq!(fixtures.len(), 10);
        let device = crate::test_device();
        for fixture in fixtures {
            let count = fixture.water.len();
            let solver = ComponentSolver::new(&device, fixture.lattice).unwrap();
            let water = upload(&device, &fixture.water);
            let links = upload(&device, &fixture.links);
            let source = upload(&device, &fixture.source);
            for repeat in 0..2 {
                for buffer in [
                    &solver.labels,
                    &solver.pressure,
                    &solver.rhs,
                    &solver.transfer,
                    &solver.progress,
                    &solver.pockets,
                    &solver.sums,
                    &solver.compensation,
                    &solver.vectors,
                ] {
                    // SAFETY: previous submission has completed, every byte is
                    // in bounds; poison also catches stale buffers on reuse.
                    unsafe {
                        buffer.write(0, &vec![0xff; buffer.size() as usize]);
                    }
                }
                let mut encoder = device.create_encoder("gpu-flip-lentine-component-proof");
                solver
                    .encode(
                        &mut encoder,
                        ComponentInput {
                            water: &water,
                            links: &links,
                            source: &source,
                        },
                    )
                    .unwrap();
                encoder.commit_and_wait_completed();
                let progress = read::<u32>(&solver.progress, 16);
                assert_eq!(
                    progress[0], fixture.status,
                    "{} repeat {repeat}: {progress:?}",
                    fixture.name
                );
                let pressure = read::<f32>(&solver.pressure, count);
                let flux = read::<[f32; 4]>(&solver.transfer, count);
                if fixture.status == ComponentStatus::Incompatible as u32 {
                    assert_eq!(progress[2], 0, "first incompatible pocket");
                    assert!(pressure.iter().all(|p| p.is_nan()));
                    assert!(flux.iter().flatten().all(|p| p.is_nan()));
                    continue;
                }
                let labels = read::<u32>(&solver.labels, count);
                assert_eq!(labels, fixture.labels, "{} labels", fixture.name);
                let rhs = read::<f32>(&solver.rhs, count);
                let pressure_scale = fixture.pressure.iter().fold(0.0_f32, |m, p| m.max(p.abs()));
                let flux_scale = fixture
                    .transfer
                    .iter()
                    .flatten()
                    .fold(0.0_f32, |m, p| m.max(p.abs()));
                let source_scale = fixture.rhs.iter().fold(0.0_f32, |m, p| m.max(p.abs()));
                close(
                    f32::from_bits(progress[8]),
                    0.0,
                    source_scale,
                    &format!("{} true residual", fixture.name),
                );
                let mut remainder: Vec<f64> =
                    fixture.source.iter().map(|&s| f64::from(s)).collect();
                for i in 0..count {
                    close(
                        rhs[i],
                        fixture.rhs[i],
                        source_scale,
                        &format!("{} rhs {i}", fixture.name),
                    );
                    close(
                        pressure[i],
                        fixture.pressure[i],
                        pressure_scale,
                        &format!("{} pressure {i}", fixture.name),
                    );
                    for (a, &actual) in flux[i].iter().enumerate() {
                        close(
                            actual,
                            fixture.transfer[i][a],
                            flux_scale,
                            &format!("{} flux {i}:{a}", fixture.name),
                        );
                    }
                    remainder[i] -= f64::from(flux[i][3]);
                    let n = fixture.lattice.map(|v| v as usize);
                    let p = [i % n[0], (i / n[0]) % n[1], i / (n[0] * n[1])];
                    for a in 0..3 {
                        if p[a] + 1 >= n[a] {
                            assert_eq!(flux[i][a], 0.0);
                            continue;
                        }
                        let j = i + [1, n[0], n[0] * n[1]][a];
                        if labels[i] == labels[j] || fixture.links[i][a] == 0.0 {
                            assert_eq!(flux[i][a], 0.0);
                        }
                        remainder[i] -= f64::from(flux[i][a]);
                        remainder[j] += f64::from(flux[i][a]);
                    }
                }
                for (root, &label) in labels.iter().enumerate() {
                    if label != root as u32 {
                        continue;
                    }
                    let sum: f64 = remainder
                        .iter()
                        .zip(&labels)
                        .filter(|(_, l)| **l == label)
                        .map(|(v, _)| v)
                        .sum();
                    close(
                        sum as f32,
                        0.0,
                        source_scale,
                        &format!("{} component {root} compatibility", fixture.name),
                    );
                }
                assert_eq!(read::<f32>(&source, count), fixture.source);
                assert_eq!(read::<[f32; 4]>(&links, count), fixture.links);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn component_shader_validates() {
        let module = naga::front::wgsl::parse_str(SHADER)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        assert_eq!(module.entry_points.len(), ENTRIES.len());
        assert_eq!(size_of::<Params>(), 32);
        assert_eq!(ComponentSolver::scratch_bytes([7, 3, 2]), 42 * 80 + 64);
        assert!(super::super::gpu_flip_step::atomic_sites_outside(SHADER, &[]).is_empty());
    }
}
