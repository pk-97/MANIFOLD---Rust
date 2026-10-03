//! CPU naga and opt-in GPU proofs for `gpu_flip_narrow_band.wgsl`.
//!
//! The integrated proofs exercise `GpuFlipStep::run`, rather than only the
//! narrow-band kernels.  The stage follows F. Ferstl, R. Ando, C. Wojtan,
//! R. Westermann and N. Thuerey, "Narrow Band FLIP for Liquid Simulations",
//! Computer Graphics Forum 35(2), 225–232, 2016, doi:10.1111/cgf.12825.
//!
//! The GPU module is deliberately kept behind `gpu-proofs`: these tests use a
//! real native Metal device and are not part of the ordinary renderer test
//! set.  The unconditional naga test still catches malformed WGSL and checks
//! the complete binding/uniform contract.

const SHADER: &str = include_str!("shaders/gpu_flip_narrow_band.wgsl");

#[cfg(test)]
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct NbParams {
    n: [u32; 3],
    slots: u32,
    minimum: [f32; 3],
    h: f32,
    dt: f32,
    axis: u32,
    initialized: u32,
    closed_faces: u32,
}

#[cfg(feature = "gpu-proofs")]
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct NbFace {
    velocity: [f32; 4],
    weight: [f32; 4],
}

#[cfg(feature = "gpu-proofs")]
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct NbParticle {
    position_radius: [f32; 4],
    velocity: [f32; 3],
    id: u32,
}

#[cfg(feature = "gpu-proofs")]
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct NbRange {
    start: u32,
    count: u32,
}

#[cfg(test)]
mod cpu_tests {
    use std::collections::BTreeSet;

    use super::{NbParams, SHADER};
    use crate::node_graph::fluid_particles::FluidParticle;

    pub(super) const STEP_CELLS: [usize; 3] = [16, 16, 16];
    const POOL_TOP: usize = 12;

    pub(super) fn quarter_pool() -> Vec<FluidParticle> {
        let mut particles = Vec::with_capacity(STEP_CELLS[0] * POOL_TOP * STEP_CELLS[2] * 8);
        let mut id = 0;
        for z in 0..STEP_CELLS[2] {
            for y in 0..POOL_TOP {
                for x in 0..STEP_CELLS[0] {
                    for site in 0..8 {
                        particles.push(FluidParticle {
                            position_radius: [
                                x as f32 + 0.25 + 0.5 * (site & 1) as f32,
                                y as f32 + 0.25 + 0.5 * ((site >> 1) & 1) as f32,
                                z as f32 + 0.25 + 0.5 * ((site >> 2) & 1) as f32,
                                0.31017,
                            ],
                            velocity: [0.0; 3],
                            id,
                        });
                        id += 1;
                    }
                }
            }
        }
        particles
    }

    /// Analytic particle field for `quarter_pool`, using the existing FLIP
    /// finite gather contract, not an unbounded nearest-particle distance.
    /// See gpu_flip_step_tests::{cpu_gather_distance,
    /// gpu_flip_particle_distance_is_the_engines_level_set}: no live particle
    /// in the 27 adjacent cells means 3h, even if ring two contains particles.
    pub(super) fn stationary_pool_particle_phi() -> Vec<f32> {
        (0..cells(STEP_CELLS))
            .map(|i| {
                let y = coords(i, STEP_CELLS)[1];
                if y > POOL_TOP {
                    return 3.0;
                }
                // All x/z columns are filled through y=POOL_TOP-1. The
                // nearest quarter site is in the same or adjacent y cell,
                // inside its 2r scatter box. Neither value needs the eps snap.
                let dy = if y < POOL_TOP { 0.25 } else { 0.75 };
                (0.125_f32 + dy * dy).sqrt() - 0.8660254
            })
            .collect()
    }

    /// First-enable oracle: nb.initialized is false for this whole step, so
    /// nb_union uses only the particle field. The freshly initialized history
    /// supplies transport; Eq. (4)'s shrunken-history union starts next step.
    /// At zero velocity the particle field does not change during transport.
    pub(super) fn stationary_pool_initial_phi() -> Vec<f32> {
        redistance_oracle(stationary_pool_particle_phi())
    }

    /// Brute-force CPU reinitialization oracle. It computes edge crossings
    /// once, then takes the direct minimum over every seed and Manhattan
    /// lattice distance; it deliberately does not reproduce the GPU sweeps.
    fn redistance_oracle(field: Vec<f32>) -> Vec<f32> {
        let sentinel = STEP_CELLS.iter().sum::<usize>() as f32;
        let mut seeds = vec![sentinel; field.len()];
        for z in 0..STEP_CELLS[2] {
            for y in 0..STEP_CELLS[1] {
                for x in 0..STEP_CELLS[0] {
                    let here_index = x + STEP_CELLS[0] * (y + STEP_CELLS[1] * z);
                    let here = field[here_index];
                    if here == 0.0 {
                        seeds[here_index] = 0.0;
                    }
                    for axis in 0..3 {
                        let mut next = [x, y, z];
                        if next[axis] + 1 >= STEP_CELLS[axis] {
                            continue;
                        }
                        next[axis] += 1;
                        let next_index =
                            next[0] + STEP_CELLS[0] * (next[1] + STEP_CELLS[1] * next[2]);
                        let other = field[next_index];
                        if (here < 0.0) == (other < 0.0) {
                            continue;
                        }
                        let denominator = here.abs() + other.abs();
                        if denominator > 0.0 {
                            seeds[here_index] = seeds[here_index].min(here.abs() / denominator);
                            seeds[next_index] = seeds[next_index].min(other.abs() / denominator);
                        }
                    }
                }
            }
        }
        let mut result = Vec::with_capacity(field.len());
        for z in 0..STEP_CELLS[2] {
            for y in 0..STEP_CELLS[1] {
                for x in 0..STEP_CELLS[0] {
                    let index = x + STEP_CELLS[0] * (y + STEP_CELLS[1] * z);
                    let mut distance = sentinel;
                    for sz in 0..STEP_CELLS[2] {
                        for sy in 0..STEP_CELLS[1] {
                            for sx in 0..STEP_CELLS[0] {
                                let seed_index = sx + STEP_CELLS[0] * (sy + STEP_CELLS[1] * sz);
                                let grid_distance =
                                    x.abs_diff(sx) + y.abs_diff(sy) + z.abs_diff(sz);
                                distance = distance.min(seeds[seed_index] + grid_distance as f32);
                            }
                        }
                    }
                    result.push(if field[index] < 0.0 {
                        -distance
                    } else {
                        distance
                    });
                }
            }
        }
        result
    }

    pub(super) fn cells(n: [usize; 3]) -> usize {
        n.iter().product()
    }

    pub(super) fn coords(i: usize, n: [usize; 3]) -> [usize; 3] {
        [i % n[0], (i / n[0]) % n[1], i / (n[0] * n[1])]
    }

    pub(super) fn expected_retained(input: &[FluidParticle], phi: &[f32]) -> BTreeSet<u32> {
        input
            .iter()
            .filter(|p| {
                let [x, y, z, _] = p.position_radius;
                let wall = x.min(16.0 - x).min(y.min(16.0 - y)).min(z.min(16.0 - z));
                // Deletion samples the cell-centred field at the particle,
                // not at its containing cell's centre (the band-mask rule).
                wall <= 3.0 || sample_cell_phi(phi, [x, y, z]) >= -3.0
            })
            .map(|p| p.id)
            .collect()
    }

    /// Clamped trilinear sampling of the fixture's h=1, origin=0.5 field.
    /// Port of the independent `narrow_band_grid_reference.Field.sample`.
    fn sample_cell_phi(phi: &[f32], position: [f32; 3]) -> f32 {
        let q: [f32; 3] =
            std::array::from_fn(|a| (position[a] - 0.5).clamp(0.0, (STEP_CELLS[a] - 1) as f32));
        let lo = q.map(|value| value.floor() as usize);
        let hi: [usize; 3] = std::array::from_fn(|a| (lo[a] + 1).min(STEP_CELLS[a] - 1));
        let t: [f32; 3] = std::array::from_fn(|a| q[a] - lo[a] as f32);
        let mut value = 0.0;
        for z in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    let bits = [x, y, z];
                    let p: [usize; 3] =
                        std::array::from_fn(|a| if bits[a] == 0 { lo[a] } else { hi[a] });
                    let weight: f32 = (0..3)
                        .map(|a| if bits[a] == 0 { 1.0 - t[a] } else { t[a] })
                        .product();
                    value += weight * phi[p[0] + STEP_CELLS[0] * (p[1] + STEP_CELLS[1] * p[2])];
                }
            }
        }
        value
    }

    #[test]
    fn narrow_band_oracle_samples_cell_centres_and_clamps_edges() {
        let affine = |p: [f32; 3]| p[0] + 2.0 * p[1] - 4.0 * p[2];
        let phi = (0..cells(STEP_CELLS))
            .map(|i| affine(coords(i, STEP_CELLS).map(|v| v as f32 + 0.5)))
            .collect::<Vec<_>>();
        for p in [[8.25, 9.75, 7.25], [0.25, 4.75, 15.75], [15.75, 0.25, 4.75]] {
            assert_eq!(
                sample_cell_phi(&phi, p),
                affine(p.map(|v| v.clamp(0.5, 15.5)))
            );
        }
    }

    #[test]
    fn narrow_band_stationary_pool_finite_support_pins_all_cell_values() {
        let particle = stationary_pool_particle_phi();
        let phi = stationary_pool_initial_phi();
        // Independent f64 planar derivation: y=13 has no occupied cell in
        // its 3x3x3 neighbourhood. The old unbounded oracle used the nearest
        // particle in y=11 instead, shifting every reinitialized cell.
        let below = (0.125_f64 + 0.75 * 0.75).sqrt() - 3.0_f64.sqrt() / 2.0;
        let crossing = -below / (3.0 - below);
        let unbounded_above = (0.125_f64 + 1.75 * 1.75).sqrt() - 3.0_f64.sqrt() / 2.0;
        let unbounded_crossing = -below / (unbounded_above - below);
        assert!((unbounded_crossing - crossing).abs() > 0.026);
        for (i, actual) in phi.iter().enumerate() {
            let p = coords(i, STEP_CELLS);
            if p[1] > POOL_TOP {
                assert_eq!(particle[i], 3.0, "unsupported air cell {p:?}");
            }
            let expected = p[1] as f64 - POOL_TOP as f64 - crossing;
            assert!(
                (f64::from(*actual) - expected).abs() < 2.0e-6,
                "cell {p:?}: {actual} vs analytic {expected}"
            );
        }
        eprintln!(
            "finite-support crossing={crossing:.10}, unbounded={unbounded_crossing:.10}; \
            all {} cells checked; interior[0,0,0]={}",
            phi.len(),
            phi[0]
        );
    }

    #[test]
    fn narrow_band_stationary_pool_oracle_retains_upper_sites_at_band_edge() {
        let input = quarter_pool();
        let phi = stationary_pool_initial_phi();
        let retained = expected_retained(&input, &phi);
        // This pool has a planar crossing between centres y=12.5 and 13.5.
        // Derive its height from the nearest supported particle distance and
        // the unsupported air sentinel, independently of the transform.
        let below = (0.125_f32 + 0.75 * 0.75).sqrt() - 0.8660254;
        let above = 3.0;
        let surface_y = 12.5 + (-below) / (above - below);
        let analytic: BTreeSet<_> = input
            .iter()
            .filter(|p| {
                let [x, y, z, _] = p.position_radius;
                let wall = x.min(16.0 - x).min(y.min(16.0 - y)).min(z.min(16.0 - z));
                wall <= 3.0 || y - surface_y >= -3.0
            })
            .map(|p| p.id)
            .collect();
        assert_eq!(retained, analytic);

        // The old containing-cell lookup rejected all eight sites at y=9.
        // Only the lower four are deeper than 3h, away from the solid band.
        let cell_retained: BTreeSet<_> = input
            .iter()
            .filter(|p| {
                let [x, y, z, _] = p.position_radius;
                let wall = x.min(16.0 - x).min(y.min(16.0 - y)).min(z.min(16.0 - z));
                let cell = x.floor() as usize
                    + STEP_CELLS[0] * (y.floor() as usize + STEP_CELLS[1] * z.floor() as usize);
                wall <= 3.0 || phi[cell] >= -3.0
            })
            .map(|p| p.id)
            .collect();
        let mut upper_ids = BTreeSet::new();
        for z in 3..13 {
            for x in 3..13 {
                for site in [2, 3, 6, 7] {
                    upper_ids.insert((8 * (x + 16 * (9 + POOL_TOP * z)) + site) as u32);
                }
            }
        }
        assert_eq!(
            retained
                .difference(&cell_retained)
                .copied()
                .collect::<BTreeSet<_>>(),
            upper_ids
        );
        assert!(cell_retained.is_subset(&retained));
    }

    #[test]
    fn gpu_flip_narrow_band_shader_validates() {
        let module = naga::front::wgsl::parse_str(SHADER)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
        let entries: Vec<&str> = module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        for entry in [
            "nb_advect_phi",
            "nb_advect_faces",
            "nb_union",
            "nb_distance_seed",
            "nb_distance_sweep",
            "nb_band_mask",
            "nb_combine_faces",
            "nb_delete",
            "nb_reseed_flags",
            "nb_reseed_status",
            "nb_reseed_write",
        ] {
            assert!(entries.contains(&entry), "missing entry {entry}");
        }

        let mut bindings: Vec<(u32, u32)> = module
            .global_variables
            .iter()
            .filter_map(|(_, global)| global.binding.as_ref().map(|b| (b.group, b.binding)))
            .collect();
        bindings.sort_unstable();
        assert_eq!(
            bindings,
            (0..=12).chain([46]).map(|binding| (0, binding)).collect::<Vec<_>>()
        );

        let params = module
            .global_variables
            .iter()
            .find(|(_, global)| {
                global.space == naga::AddressSpace::Uniform
                    && global
                        .binding
                        .as_ref()
                        .is_some_and(|b| b.group == 0 && b.binding == 0)
            })
            .expect("NbParams uniform at binding 0")
            .1;
        let naga::TypeInner::Struct { members, span } = &module.types[params.ty].inner else {
            panic!("NbParams binding is not a struct");
        };
        assert_eq!(members.len(), 8);
        assert_eq!(*span as usize, std::mem::size_of::<NbParams>());
        assert_eq!(std::mem::size_of::<NbParams>(), 48);
        assert_eq!(
            std::mem::offset_of!(NbParams, n),
            members[0].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, slots),
            members[1].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, minimum),
            members[2].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, h),
            members[3].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, dt),
            members[4].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, axis),
            members[5].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, initialized),
            members[6].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, closed_faces),
            members[7].offset as usize
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use std::collections::BTreeSet;

    use super::super::gpu_flip_step::{GpuFlipStep, StepParams, dispatch_pass};
    use super::super::liquid_surface_tests::{Harness, params as effect_params, read};
    use super::super::prefix_scan::PrefixScan;
    use super::super::liquid_stats::{SOLVER_WORDS, NARROW_BAND_SHORTAGE_TAIL};
    use super::cpu_tests::{
        STEP_CELLS, cells, coords, expected_retained, quarter_pool, stationary_pool_initial_phi,
        stationary_pool_particle_phi,
    };
    use super::{NbFace, NbParams, NbParticle, NbRange, SHADER};
    use crate::node_graph::fluid_particles::{FaceSample, FluidParticle};
    use crate::node_graph::primitive::Primitive;
    use manifold_gpu::{GpuBinding, GpuBuffer};

    const N: [usize; 3] = [8, 8, 8];
    const H: f32 = 1.0;

    const STEP_NODES: [usize; 3] = [23, 23, 23];

    fn step_params(
        narrow_band: Option<f32>,
        epoch: Option<f32>,
        tick: u32,
    ) -> crate::node_graph::effect_node::ParamValues {
        let mut values = vec![
            ("lattice_min_x", -3.0),
            ("lattice_min_y", -3.0),
            ("lattice_min_z", -3.0),
            ("cell_size", 1.0),
            ("nodes_x", STEP_NODES[0] as f32),
            ("nodes_y", STEP_NODES[1] as f32),
            ("nodes_z", STEP_NODES[2] as f32),
            ("gravity_x", 0.0),
            ("gravity_y", 0.0),
            ("gravity_z", 0.0),
            ("steps", 1.0),
            ("tick_index", tick as f32),
            ("flip", 0.0),
            ("iterations", 1.0),
            ("top_speed", 1.0),
            ("ghost_fluid", 1.0),
            ("volume_projection", 0.0),
            ("closed_faces", 63.0),
            ("solve_level", 0.0),
        ];
        if let Some(value) = narrow_band {
            values.push(("narrow_band", value));
        }
        if let Some(value) = epoch {
            values.push(("epoch", value));
        }
        effect_params(&values)
    }

    struct StepRun {
        particles: Vec<FluidParticle>,
        faces: Vec<FaceSample>,
        interior: Vec<f32>,
        errors: Vec<String>,
        live: u32,
    }

    fn run_step(
        harness: &mut Harness,
        step: &mut GpuFlipStep,
        input: &[FluidParticle],
        capacity: usize,
        narrow_band: Option<f32>,
        epoch: Option<f32>,
        tick: u32,
    ) -> StepRun {
        let (particles_slot, _) = harness.array(input, capacity);
        let count_slot = harness.scalar_input(input.len() as f32);
        let (out_slot, _) = harness.array::<FluidParticle>(&[], capacity);
        let (faces_slot, _) = harness.array::<FaceSample>(&[], 1);
        let (capped_slot, _) = harness.array::<u32>(&[], 2 * capacity + SOLVER_WORDS as usize);
        let (interior_out_slot, _) = harness.array::<f32>(&[], STEP_CELLS.iter().product());

        step.prepare_pipelines(&harness.device);
        let inputs = vec![("particles", particles_slot), ("count", count_slot)];
        let (_, errors) = harness.run(
            step,
            &inputs,
            &[
                ("out", out_slot),
                ("faces", faces_slot),
                ("capped", capped_slot),
                ("interior", interior_out_slot),
            ],
            &step_params(narrow_band, epoch, tick),
        );
        let out: Vec<FluidParticle> = read(&harness.buffer(out_slot), capacity);
        let face_count: usize = STEP_CELLS.iter().map(|n| n + 1).product();
        let faces = read(&harness.buffer(faces_slot), face_count);
        // The step provides private interior storage, replacing the harness's
        // shared placeholder. Read it back as the body proofs read private arrays.
        let interior_buffer = harness.buffer(interior_out_slot);
        let staging = harness.device.create_buffer_shared(interior_buffer.size);
        let mut encoder = harness
            .device
            .create_encoder("narrow-band interior readback");
        encoder.copy_buffer_to_buffer(&interior_buffer, &staging, interior_buffer.size);
        encoder.commit_and_wait_completed();
        let interior = read(&staging, STEP_CELLS.iter().product());
        let live = out.iter().filter(|p| p.position_radius[3] > 0.0).count() as u32;
        StepRun {
            particles: out,
            faces,
            interior,
            errors,
            live,
        }
    }

    fn active_ids(particles: &[FluidParticle]) -> BTreeSet<u32> {
        particles
            .iter()
            .filter(|p| p.position_radius[3] > 0.0)
            .map(|p| p.id)
            .collect()
    }

    fn site_keys(particles: &[FluidParticle]) -> BTreeSet<(u32, u32, u32)> {
        particles
            .iter()
            .filter(|p| p.position_radius[3] > 0.0)
            .map(|p| {
                (
                    (p.position_radius[0] * 4.0).round() as u32,
                    (p.position_radius[1] * 4.0).round() as u32,
                    (p.position_radius[2] * 4.0).round() as u32,
                )
            })
            .collect()
    }

    fn faces(n: [usize; 3]) -> usize {
        (n[0] + 1) * (n[1] + 1) * (n[2] + 1)
    }

    fn index(p: [usize; 3], n: [usize; 3]) -> usize {
        p[0] + n[0] * (p[1] + n[1] * p[2])
    }

    fn params(n: [usize; 3]) -> NbParams {
        NbParams {
            n: n.map(|v| v as u32),
            slots: 0,
            minimum: [0.0; 3],
            h: H,
            dt: 0.0,
            axis: 0,
            initialized: 1,
            closed_faces: 0,
        }
    }

    fn shared<T: bytemuck::Pod>(device: &crate::TestDevice, values: &[T]) -> GpuBuffer {
        let buffer = device.create_buffer_shared((size_of_val(values) as u64).max(16));
        buffer.zero_fill();
        if !values.is_empty() {
            // SAFETY: the shared buffer is sized for `values`; no work is in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
        }
        buffer
    }

    fn run<T: bytemuck::Pod>(
        device: &crate::TestDevice,
        entry: &str,
        uniform: &NbParams,
        bindings: Vec<GpuBinding<'_>>,
        output: &GpuBuffer,
        count: usize,
    ) -> Vec<T> {
        let pipeline = device.create_compute_pipeline(SHADER, entry, "gpu-flip-narrow-band-proof");
        let mut all = vec![GpuBinding::Bytes {
            binding: 0,
            data: bytemuck::bytes_of(uniform),
        }];
        let clock = device.create_buffer_shared(48);
        clock.zero_fill();
        all.push(bind(46, &clock));
        all.extend(bindings);
        let mut encoder = device.create_encoder("gpu-flip-narrow-band-proof");
        encoder.dispatch_compute(&pipeline, &all, [count.div_ceil(256) as u32, 1, 1], entry);
        encoder.commit_and_wait_completed();
        read(output, count)
    }

    fn bind(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
        GpuBinding::Buffer {
            binding,
            buffer,
            offset: 0,
        }
    }

    fn close(actual: f32, expected: f64, scale: f64, what: &str) {
        assert!(
            (f64::from(actual) - expected).abs() <= 2e-5 * scale.max(1.0),
            "{what}: {actual} vs {expected}"
        );
    }

    fn scalar_cell_values(f: impl Fn([usize; 3]) -> f32) -> Vec<f32> {
        (0..cells(N)).map(|i| f(coords(i, N))).collect()
    }

    #[test]
    fn gpu_flip_narrow_band_mask() {
        let device = crate::test_device();
        let phi = scalar_cell_values(|p| match p[0] {
            0 => -3.0,
            1 => -2.5,
            2 => -3.5,
            _ => 1.0,
        });
        let solid = vec![4.0_f32; faces(N)];
        let phi_buffer = shared(&device, &phi);
        let solid_buffer = shared(&device, &solid);
        let output = shared(&device, &vec![0_u32; cells(N)]);
        let got: Vec<u32> = run(
            &device,
            "nb_band_mask",
            &params(N),
            vec![
                bind(1, &phi_buffer),
                bind(6, &solid_buffer),
                bind(7, &output),
            ],
            &output,
            cells(N),
        );
        for (i, value) in got.iter().enumerate() {
            let x = coords(i, N)[0];
            assert_eq!(*value, u32::from(x != 0 && x != 2), "cell {i}");
        }

        // A negative cell outside the liquid band remains included when the
        // solid signed distance is inside the three-cell proximity band.
        let solid_buffer = shared(&device, &vec![3.0_f32; faces(N)]);
        let phi = scalar_cell_values(|_| -4.0);
        let phi_buffer = shared(&device, &phi);
        let got: Vec<u32> = run(
            &device,
            "nb_band_mask",
            &params(N),
            vec![
                bind(1, &phi_buffer),
                bind(6, &solid_buffer),
                bind(7, &output),
            ],
            &output,
            cells(N),
        );
        assert!(got.iter().all(|&value| value == 1));
    }

    #[test]
    fn gpu_flip_narrow_band_advect() {
        let device = crate::test_device();
        let phi = scalar_cell_values(|p| p[0] as f32 + 2.0 * p[1] as f32 - p[2] as f32);
        let face_count = faces(N);
        let faces_in: Vec<NbFace> = (0..face_count)
            .map(|_| NbFace {
                velocity: [0.5, -0.25, 0.125, 0.0],
                weight: [1.0; 4],
            })
            .collect();
        let phi_buffer = shared(&device, &phi);
        let face_buffer = shared(&device, &faces_in);
        let output = shared(&device, &vec![0.0_f32; cells(N)]);
        let mut uniform = params(N);
        uniform.dt = 0.25;
        let got: Vec<f32> = run(
            &device,
            "nb_advect_phi",
            &uniform,
            vec![
                bind(1, &phi_buffer),
                bind(3, &face_buffer),
                bind(2, &output),
            ],
            &output,
            cells(N),
        );
        for (i, actual) in got.iter().enumerate() {
            let p = coords(i, N).map(|v| v as f64 + 0.5);
            let departure = [p[0] - 0.125, p[1] + 0.0625, p[2] - 0.03125];
            let clamped = [
                departure[0].clamp(0.5, N[0] as f64 - 0.5),
                departure[1].clamp(0.5, N[1] as f64 - 0.5),
                departure[2].clamp(0.5, N[2] as f64 - 0.5),
            ];
            close(
                *actual,
                clamped[0] - 0.5 + 2.0 * (clamped[1] - 0.5) - (clamped[2] - 0.5),
                8.0,
                "advected phi",
            );
        }
    }

    #[test]
    fn gpu_flip_narrow_band_combine() {
        let device = crate::test_device();
        let grid = vec![
            NbFace {
                velocity: [7.0, 8.0, 9.0, 0.0],
                weight: [0.75, 0.75, 0.75, 0.0]
            };
            faces(N)
        ];
        let particle = vec![
            NbFace {
                velocity: [1.0, 2.0, 3.0, 0.0],
                weight: [1.0, 1.0, 1.0, 0.0]
            };
            faces(N)
        ];
        let solid = vec![3.0_f32; faces(N)];
        for (phi_value, expected) in [(-2.0_f32, [1.0_f32, 2.0, 3.0]), (-2.01, [7.0, 8.0, 9.0])] {
            let phi = vec![phi_value; cells(N)];
            let phi_buffer = shared(&device, &phi);
            let grid_buffer = shared(&device, &grid);
            let particle_buffer = shared(&device, &particle);
            let solid_buffer = shared(&device, &solid);
            let got: Vec<NbFace> = run(
                &device,
                "nb_combine_faces",
                &params(N),
                vec![
                    bind(1, &phi_buffer),
                    bind(3, &grid_buffer),
                    bind(4, &particle_buffer),
                    bind(6, &solid_buffer),
                ],
                &particle_buffer,
                faces(N),
            );
            let face = got[index([3, 3, 3], N.map(|v| v + 1))];
            for (axis, expected) in expected.iter().enumerate() {
                assert_eq!(
                    face.velocity[axis], *expected,
                    "phi {phi_value}, axis {axis}"
                );
            }
        }
    }

    #[test]
    fn gpu_flip_narrow_band_delete() {
        let device = crate::test_device();
        let phi = vec![-4.0_f32; cells(N)];
        let solid: Vec<f32> = (0..faces(N))
            .map(|i| {
                if coords(i, N.map(|v| v + 1))[0] < 4 {
                    2.0
                } else {
                    4.0
                }
            })
            .collect();
        let particles = vec![
            NbParticle {
                position_radius: [2.2, 2.2, 2.2, 1.0],
                velocity: [1.0, 0.0, 0.0],
                id: 1,
            },
            NbParticle {
                position_radius: [6.2, 2.2, 2.2, 1.0],
                velocity: [2.0, 0.0, 0.0],
                id: 2,
            },
            NbParticle {
                position_radius: [6.2, 3.2, 2.2, 0.0],
                velocity: [3.0, 0.0, 0.0],
                id: 3,
            },
        ];
        let phi_buffer = shared(&device, &phi);
        let solid_buffer = shared(&device, &solid);
        let particle_buffer = shared(&device, &particles);
        let mut uniform = params(N);
        uniform.slots = particles.len() as u32;
        let got: Vec<NbParticle> = run(
            &device,
            "nb_delete",
            &uniform,
            vec![
                bind(1, &phi_buffer),
                bind(6, &solid_buffer),
                bind(9, &particle_buffer),
            ],
            &particle_buffer,
            particles.len(),
        );
        assert_eq!(
            got[0].position_radius[3], 1.0,
            "deep particle near solid is retained"
        );
        assert_eq!(
            got[1].position_radius[3], 0.0,
            "deep particle far from solid is deleted"
        );
        assert_eq!(
            got[2].position_radius[3], 0.0,
            "already-dead slot remains clear"
        );
    }

    #[test]
    fn gpu_flip_narrow_band_redistance() {
        let device = crate::test_device();
        // Rectangular extents catch swapped line axes; shallow fields require
        // propagation rather than returning the input distance unchanged.
        let n = [5, 7, 3];
        for plane in 0..4 {
            let expected: Vec<f32> = (0..cells(n))
                .map(|i| {
                    let p = coords(i, n);
                    if plane < 3 {
                        p[plane] as f32 - 1.5
                    } else {
                        p.iter().sum::<usize>() as f32 - 4.5
                    }
                })
                .collect();
            let phi: Vec<f32> = expected.iter().map(|v| 0.125 * v.signum()).collect();
            let phi_buffer = shared(&device, &phi);
            let output = shared(&device, &vec![0.0_f32; cells(n)]);
            let mut uniform = params(n);
            let _: Vec<f32> = run(
                &device,
                "nb_distance_seed",
                &uniform,
                vec![bind(1, &phi_buffer), bind(2, &output)],
                &output,
                cells(n),
            );
            for axis in 0usize..3 {
                uniform.axis = axis as u32;
                let _: Vec<f32> = run(
                    &device,
                    "nb_distance_sweep",
                    &uniform,
                    vec![bind(2, &output)],
                    &output,
                    n[(axis + 1) % 3] * n[(axis + 2) % 3],
                );
            }
            let got: Vec<f32> = read(&output, cells(n));
            for (actual, expected) in got.iter().zip(expected) {
                close(*actual, f64::from(expected), 8.0, "redistanced phi");
            }
        }
    }

    #[test]
    fn gpu_flip_narrow_band_union() {
        let device = crate::test_device();
        let history = shared(&device, &vec![-10.0_f32; cells(N)]);
        let particle_values = scalar_cell_values(|p| if p[0] < 4 { 3.0 } else { -12.0 });
        let particles = shared(&device, &particle_values);
        let output = shared(&device, &vec![99.0_f32; cells(N)]);
        for initialized in [0, 1] {
            let mut uniform = params(N);
            uniform.initialized = initialized;
            let got: Vec<f32> = run(
                &device,
                "nb_union",
                &uniform,
                vec![bind(1, &history), bind(5, &particles), bind(2, &output)],
                &output,
                cells(N),
            );
            for (i, &actual) in got.iter().enumerate() {
                let expected = if initialized == 0 {
                    particle_values[i]
                } else {
                    particle_values[i].min(-9.0)
                };
                assert_eq!(actual, expected, "cell {i}, initialized {initialized}");
            }
        }
    }

    #[test]
    fn gpu_flip_narrow_band_advect_faces() {
        let device = crate::test_device();
        let phi = shared(&device, &vec![-4.0_f32; cells(N)]);
        // u=y, v=2, w=0: the analytic departure has y decreased by 2dt.
        let input: Vec<NbFace> = (0..faces(N))
            .map(|i| {
                let p = coords(i, N.map(|n| n + 1));
                NbFace {
                    velocity: [p[1] as f32 + 0.5, 2.0, 0.0, 0.0],
                    weight: [1.0; 4],
                }
            })
            .collect();
        let source = shared(&device, &input);
        let output = shared(&device, &vec![NbFace::default(); faces(N)]);
        let mut uniform = params(N);
        uniform.dt = 0.25;
        uniform.closed_faces = 0b11_1111;
        let got: Vec<NbFace> = run(
            &device,
            "nb_advect_faces",
            &uniform,
            vec![bind(1, &phi), bind(3, &source), bind(4, &output)],
            &output,
            faces(N),
        );
        for (i, face) in got.iter().enumerate() {
            let p = coords(i, N.map(|n| n + 1));
            for a in 0..3 {
                if (0..3).any(|b| b != a && p[b] == N[b]) {
                    assert_eq!(face.weight[a], 0.0, "padding {i}/{a}");
                    assert_eq!(face.velocity[a], 0.0);
                } else if p[a] == 0 || p[a] == N[a] {
                    assert_eq!(face.velocity[a], 0.0, "wall {i}/{a}");
                    assert_eq!(face.weight[a], 1.0);
                } else {
                    let expected = match a {
                        0 => (p[1] as f64).max(0.5),
                        1 => 2.0,
                        _ => 0.0,
                    };
                    close(face.velocity[a], expected, 8.0, "advected face");
                    assert_eq!(face.weight[a], 1.0);
                }
            }
        }
    }

    #[test]
    fn gpu_flip_narrow_band_reseed() {
        let device = crate::test_device();
        let eligible = [index([2, 3, 3], N), index([3, 3, 3], N)];
        let mut previous = vec![-2.0_f32; cells(N)];
        for &i in &eligible {
            previous[i] = -3.0;
        }
        let previous_buffer = shared(&device, &previous);
        let phi = shared(&device, &vec![-2.0_f32; cells(N)]);
        let solid = shared(&device, &vec![4.0_f32; faces(N)]);
        let mut ranges = vec![NbRange::default(); cells(N)];
        let mut live = 0;
        for (i, range) in ranges.iter_mut().enumerate() {
            range.start = live;
            range.count = u32::from(eligible.contains(&i));
            live += range.count;
        }
        let ranges_buffer = shared(&device, &ranges);
        let mut input = vec![NbParticle::default(); 20];
        for (slot, x) in [2.0, 3.0].into_iter().enumerate() {
            input[slot] = NbParticle {
                position_radius: [x + 0.1, 3.1, 3.1, 0.31],
                velocity: [9.0; 3],
                id: 100 + slot as u32,
            };
        }
        let particles = shared(&device, &input);
        let flags = shared(&device, &vec![99_u32; 8 * cells(N)]);
        let ranks = shared(&device, &vec![0_u32; 8 * cells(N)]);
        let status = shared(&device, &[0_u32; 2]);
        let velocity = shared(
            &device,
            &vec![
                NbFace {
                    velocity: [1.0, 2.0, 3.0, 0.0],
                    weight: [1.0; 4]
                };
                faces(N)
            ],
        );
        let mut uniform = params(N);
        uniform.slots = input.len() as u32;
        let got: Vec<u32> = run(
            &device,
            "nb_reseed_flags",
            &uniform,
            vec![
                bind(1, &phi),
                bind(6, &solid),
                bind(8, &previous_buffer),
                bind(9, &particles),
                bind(10, &ranges_buffer),
                bind(11, &flags),
            ],
            &flags,
            8 * cells(N),
        );
        for (i, &flag) in got.iter().enumerate() {
            assert_eq!(
                flag,
                u32::from(eligible.contains(&(i / 8)) && i % 8 != 0),
                "site {i}"
            );
        }
        let mut scan = PrefixScan::default();
        scan.prepare(&device);
        scan.parents(&device, got.len()).unwrap();
        let mut encoder = device.create_encoder("narrow-band-reseed-scan");
        scan.encode_into(&mut encoder, got.len(), &flags, &ranks);
        encoder.commit_and_wait_completed();
        // A shortage must prevent every append, including the prefix that fits.
        for capacity in [10, 20] {
            uniform.slots = capacity;
            let result: Vec<u32> = run(
                &device,
                "nb_reseed_status",
                &uniform,
                vec![
                    bind(10, &ranges_buffer),
                    bind(11, &ranks),
                    bind(12, &status),
                ],
                &status,
                2,
            );
            assert_eq!(result, vec![16_u32.saturating_sub(capacity), 16]);
            let pipeline =
                device.create_compute_pipeline(SHADER, "nb_reseed_write", "nb-reseed-write");
            let mut encoder = device.create_encoder("nb-reseed-write");
            encoder.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniform),
                    },
                    bind(3, &velocity),
                    bind(9, &particles),
                    bind(10, &ranges_buffer),
                    bind(11, &ranks),
                    bind(12, &status),
                ],
                [(8 * cells(N)).div_ceil(256) as u32, 1, 1],
                "nb-reseed-write",
            );
            encoder.commit_and_wait_completed();
            let written: Vec<NbParticle> = read(&particles, input.len());
            if capacity == 10 {
                assert_eq!(
                    bytemuck::cast_slice::<_, u8>(&written),
                    bytemuck::cast_slice::<_, u8>(&input)
                );
                continue;
            }
            for slot in 0..2 {
                assert_eq!(written[slot].id, input[slot].id);
            }
            for (rank, particle) in written[2..16].iter().enumerate() {
                let site = rank % 7 + 1;
                let x = 2.0 + (rank / 7) as f32;
                assert_eq!(
                    &particle.position_radius[..3],
                    &[
                        x + 0.25 + 0.5 * (site & 1) as f32,
                        3.25 + 0.5 * ((site >> 1) & 1) as f32,
                        3.25 + 0.5 * ((site >> 2) & 1) as f32
                    ]
                );
                assert_eq!(particle.velocity, [1.0, 2.0, 3.0]);
                assert_eq!(particle.position_radius[3], 0.31017);
            }
            assert!(written[16..].iter().all(|p| p.position_radius[3] == 0.0));
        }
    }

    #[test]
    fn gpu_flip_narrow_band_restore_overflow_latches_without_partial_write() {
        let device = crate::test_device();
        let eligible = [index([2, 3, 3], N), index([3, 3, 3], N)];
        // Only these two cells are liquid. Even the outer quarter sites sample
        // at most 1 - 5 * (3/4)^3 = -1.109375, below the -h restore cutoff.
        let mut phi_values = vec![1.0_f32; cells(N)];
        for &i in &eligible {
            phi_values[i] = -4.0;
        }
        let phi = shared(&device, &phi_values);
        let previous = shared(&device, &vec![0.0_f32; cells(N)]);
        let solid = shared(&device, &vec![4.0_f32; faces(N)]);
        let mut ranges = vec![NbRange::default(); cells(N)];
        let mut live = 0;
        for (i, range) in ranges.iter_mut().enumerate() {
            range.start = live;
            range.count = u32::from(eligible.contains(&i));
            live += range.count;
        }
        let ranges_buffer = shared(&device, &ranges);
        let mut input = vec![NbParticle::default(); 20];
        input[..2].copy_from_slice(&[
            NbParticle {
                position_radius: [2.1, 3.1, 3.1, 0.31],
                velocity: [9.0; 3],
                id: 100,
            },
            NbParticle {
                position_radius: [3.1, 3.1, 3.1, 0.31],
                velocity: [9.0; 3],
                id: 101,
            },
        ]);
        let particles = shared(&device, &input);
        let flags = shared(&device, &vec![99_u32; 8 * cells(N)]);
        let ranks = shared(&device, &vec![0_u32; 8 * cells(N)]);
        let status = shared(&device, &[0_u32; 2]);
        let velocity = shared(
            &device,
            &vec![
                NbFace {
                    velocity: [1.0, 2.0, 3.0, 0.0],
                    weight: [1.0; 4],
                };
                faces(N)
            ],
        );
        let mut uniform = params(N);
        // Eight slots cannot hold the two existing particles plus fourteen
        // restore sites. The input buffer is deliberately larger than the
        // declared capacity so a forbidden partial write is observable.
        uniform.slots = 8;
        let got_flags: Vec<u32> = run(
            &device,
            "nb_restore_flags",
            &uniform,
            vec![
                bind(1, &phi),
                bind(6, &solid),
                bind(8, &previous),
                bind(9, &particles),
                bind(10, &ranges_buffer),
                bind(11, &flags),
            ],
            &flags,
            8 * cells(N),
        );
        let selected = got_flags.iter().filter(|&&flag| flag != 0).count() as u32;
        for (i, &flag) in got_flags.iter().enumerate() {
            assert_eq!(
                flag,
                u32::from(eligible.contains(&(i / 8)) && i % 8 != 0),
                "restore site {i}"
            );
        }
        assert_eq!(
            selected, 14,
            "two cells with one occupied site need seven each"
        );

        let mut scan = PrefixScan::default();
        scan.prepare(&device);
        scan.parents(&device, got_flags.len()).unwrap();
        let mut encoder = device.create_encoder("narrow-band-restore-overflow-scan");
        scan.encode_into(&mut encoder, got_flags.len(), &flags, &ranks);
        encoder.commit_and_wait_completed();

        let status_values: Vec<u32> = run(
            &device,
            "nb_reseed_status",
            &uniform,
            vec![
                bind(10, &ranges_buffer),
                bind(11, &ranks),
                bind(12, &status),
            ],
            &status,
            2,
        );
        // CPU oracle: live=2, additions=14, capacity=8.
        assert_eq!(status_values, [8, 16]);
        let before = read(&particles, input.len());
        let pipeline =
            device.create_compute_pipeline(SHADER, "nb_reseed_write", "nb-restore-write");
        let mut encoder = device.create_encoder("narrow-band-restore-overflow-write");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniform),
                },
                bind(3, &velocity),
                bind(9, &particles),
                bind(10, &ranges_buffer),
                bind(11, &ranks),
                bind(12, &status),
            ],
            [(8 * cells(N)).div_ceil(256) as u32, 1, 1],
            "narrow-band-restore-overflow-write",
        );
        encoder.commit_and_wait_completed();
        assert_eq!(
            bytemuck::cast_slice::<NbParticle, u8>(&read::<NbParticle>(&particles, input.len())),
            bytemuck::cast_slice::<NbParticle, u8>(&before),
            "restore wrote despite shortage"
        );

        // The step's failure word is a max latch: a later zero status cannot
        // erase the shortage, while the next tick's tally keeps the stats
        // word until an explicit reset clears the failure.
        let capped = shared(&device, &[0_u32; SOLVER_WORDS as usize]);
        let failure = shared(&device, &[0_u32; 1]);
        let mut step = StepParams {
            n: N.map(|value| value as u32),
            capacity: uniform.slots,
            step_in_tick: 0,
            ..StepParams::default()
        };
        dispatch_pass(
            &device,
            "narrow_latch",
            &step,
            &[(43, &status), (44, &failure)],
            1,
        );
        assert_eq!(read::<u32>(&failure, 1), [8]);
        unsafe {
            status.write(0, bytemuck::cast_slice::<u32, u8>(&[0, 16]));
        }
        dispatch_pass(
            &device,
            "narrow_latch",
            &step,
            &[(43, &status), (44, &failure)],
            1,
        );
        assert_eq!(read::<u32>(&failure, 1), [8]);
        unsafe {
            status.write(0, bytemuck::cast_slice::<u32, u8>(&[8, 16]));
        }
        dispatch_pass(
            &device,
            "narrow_tally",
            &step,
            &[(22, &capped), (43, &status)],
            1,
        );
        assert_eq!(read::<u32>(&capped, SOLVER_WORDS as usize)[NARROW_BAND_SHORTAGE_TAIL as usize], 8);
        step.step_in_tick = 1;
        unsafe {
            status.write(0, bytemuck::cast_slice::<u32, u8>(&[0, 16]));
        }
        dispatch_pass(
            &device,
            "narrow_tally",
            &step,
            &[(22, &capped), (43, &status)],
            1,
        );
        assert_eq!(read::<u32>(&capped, SOLVER_WORDS as usize)[NARROW_BAND_SHORTAGE_TAIL as usize], 8);
        failure.zero_fill();
        dispatch_pass(
            &device,
            "narrow_latch",
            &step,
            &[(43, &status), (44, &failure)],
            1,
        );
        assert_eq!(read::<u32>(&failure, 1), [0]);
    }

    #[test]
    fn gpu_flip_step_narrow_band_off_is_bit_identical_to_default() {
        let input = quarter_pool();
        let capacity = input.len() + 8 * STEP_CELLS[0] * STEP_CELLS[2];

        let mut default_harness = Harness::new();
        let mut default_step = GpuFlipStep::new();
        let default = run_step(
            &mut default_harness,
            &mut default_step,
            &input,
            capacity,
            None,
            None,
            0,
        );
        assert!(
            default.errors.is_empty(),
            "default step errors: {:?}",
            default.errors
        );

        let mut explicit_harness = Harness::new();
        let mut explicit_step = GpuFlipStep::new();
        let explicit = run_step(
            &mut explicit_harness,
            &mut explicit_step,
            &input,
            capacity,
            Some(0.0),
            Some(7.0),
            0,
        );
        assert!(
            explicit.errors.is_empty(),
            "explicit off errors: {:?}",
            explicit.errors
        );
        assert_eq!(
            bytemuck::cast_slice::<FluidParticle, u8>(&default.particles),
            bytemuck::cast_slice::<FluidParticle, u8>(&explicit.particles),
            "narrow_band=0 changes particles"
        );
        assert_eq!(
            bytemuck::cast_slice::<FaceSample, u8>(&default.faces),
            bytemuck::cast_slice::<FaceSample, u8>(&explicit.faces),
            "narrow_band=0 changes faces"
        );
    }

    #[test]
    fn gpu_flip_narrow_band_stationary_pool_particle_field_matches_finite_support() {
        let device = crate::test_device();
        let input = quarter_pool();
        let mut start = 0;
        let ranges: Vec<_> = (0..cells(STEP_CELLS))
            .map(|i| {
                let count = if coords(i, STEP_CELLS)[1] < 12 { 8 } else { 0 };
                let range = NbRange { start, count };
                start += count;
                range
            })
            .collect();
        assert_eq!(start as usize, input.len());
        let sorted = shared(&device, &input);
        let ranges = shared(&device, &ranges);
        let phi = shared(&device, &vec![0.0_f32; cells(STEP_CELLS)]);
        // The dense narrow-band branch skips this list, but the unspecialized
        // shader still declares the baseline cell-index binding.
        let tiles = shared(&device, &(0..8_u32).collect::<Vec<_>>());
        let params = StepParams {
            n: STEP_CELLS.map(|n| n as u32),
            cell_size: 1.0,
            capacity: input.len() as u32,
            particles: input.len() as u32,
            narrow_band: 1,
            ..StepParams::default()
        };
        dispatch_pass(
            &device,
            "particle_distance",
            &params,
            &[(1, &ranges), (2, &sorted), (5, &phi), (29, &tiles)],
            cells(STEP_CELLS) as u64,
        );
        for (i, (actual, expected)) in read::<f32>(&phi, cells(STEP_CELLS))
            .into_iter()
            .zip(stationary_pool_particle_phi())
            .enumerate()
        {
            close(
                actual,
                f64::from(expected),
                1.0,
                &format!("particle field cell {:?}", coords(i, STEP_CELLS)),
            );
        }
    }

    #[test]
    fn gpu_flip_step_narrow_band_stationary_pool_matches_oracle() {
        let input = quarter_pool();
        let capacity = input.len() + 8 * STEP_CELLS[0] * STEP_CELLS[2];
        let mut harness = Harness::new();
        let mut step = GpuFlipStep::new();
        let result = run_step(
            &mut harness,
            &mut step,
            &input,
            capacity,
            Some(1.0),
            Some(7.0),
            0,
        );
        assert!(
            result.errors.is_empty(),
            "narrow-band errors: {:?}",
            result.errors
        );

        // The first stationary pass only retires particles outside R=3h; the
        // solid is consulted separately for the near-wall retention rule.
        let expected_phi = stationary_pool_initial_phi();
        assert_eq!(
            active_ids(&result.particles),
            expected_retained(&input, &expected_phi)
        );
        for (i, (actual, expected)) in result.interior.iter().zip(expected_phi).enumerate() {
            assert!(
                (actual - expected).abs() <= 2.0e-3,
                "interior field cell {:?} (index {i}): {actual} vs {expected}",
                coords(i, STEP_CELLS)
            );
        }
    }

    #[test]
    fn gpu_flip_step_narrow_band_enable_disable_restores_pool_and_reset_is_repeatable() {
        let input = quarter_pool();
        let capacity = input.len() + 8 * STEP_CELLS[0] * STEP_CELLS[2];
        let mut harness = Harness::new();
        let mut step = GpuFlipStep::new();
        let enabled = run_step(
            &mut harness,
            &mut step,
            &input,
            capacity,
            Some(1.0),
            Some(11.0),
            0,
        );
        assert!(
            enabled.errors.is_empty(),
            "enable errors: {:?}",
            enabled.errors
        );
        let retained: Vec<_> = enabled
            .particles
            .iter()
            .filter(|p| p.position_radius[3] > 0.0)
            .copied()
            .collect();
        let restored = run_step(
            &mut harness,
            &mut step,
            &retained,
            capacity,
            Some(0.0),
            Some(11.0),
            1,
        );
        assert!(
            restored.errors.is_empty(),
            "disable errors: {:?}",
            restored.errors
        );
        assert_eq!(restored.live as usize, input.len());
        assert_eq!(site_keys(&restored.particles), site_keys(&input));
        assert!(
            restored.particles[..restored.live as usize]
                .iter()
                .all(|p| p.velocity == [0.0; 3] && p.position_radius[3] > 0.0)
        );
        assert!(
            restored.particles[restored.live as usize..]
                .iter()
                .all(|p| p.position_radius[3] == 0.0)
        );

        // A changed epoch invalidates the history. A fresh instance at that
        // epoch must produce the same values, including the interior field.
        let reset = run_step(
            &mut harness,
            &mut step,
            &input,
            capacity,
            Some(1.0),
            Some(12.0),
            0,
        );
        let mut fresh_harness = Harness::new();
        let mut fresh_step = GpuFlipStep::new();
        let fresh = run_step(
            &mut fresh_harness,
            &mut fresh_step,
            &input,
            capacity,
            Some(1.0),
            Some(12.0),
            0,
        );
        assert!(reset.errors.is_empty() && fresh.errors.is_empty());
        assert_eq!(active_ids(&reset.particles), active_ids(&fresh.particles));
        assert_eq!(reset.interior, fresh.interior);
        assert_eq!(
            bytemuck::cast_slice::<FaceSample, u8>(&reset.faces),
            bytemuck::cast_slice::<FaceSample, u8>(&fresh.faces)
        );
    }
}
