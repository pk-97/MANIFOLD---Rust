//! Shader checks on the CPU, bitwise stage oracles on the GPU.
use super::*;
use manifold_water_liquid::testkit::world_box::BoxTable;

#[cfg(test)]
fn validate_wgsl(source: &str) -> naga::Module {
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{e:?}"));
    module
}

#[test]
fn whitewater_fused_and_gather_validate_on_cpu() {
    let module = validate_wgsl(&fused_source());
    assert_eq!(std::mem::size_of::<EmitParams>(), 128);
    assert_eq!(std::mem::size_of::<SpawnParams>(), 112);
    assert_eq!(std::mem::size_of::<LifecycleParams>(), 144);
    assert_eq!(std::mem::size_of::<TurbulenceParams>(), 32);
    assert_eq!(std::mem::size_of::<GatherParams>(), 48);
    assert_eq!(module.entry_points.len(), 5);
    for entry in &module.entry_points {
        assert_eq!(entry.workgroup_size, [256, 1, 1]);
    }
    let gather = validate_wgsl(&gather_source());
    assert_eq!(gather.entry_points.len(), 1);
    assert_eq!(gather.entry_points[0].name, GATHER_ENTRY.0);
    assert_eq!(gather.entry_points[0].workgroup_size, [256, 1, 1]);
}

/// A dense face-cells distance lattice as the step's pooled field: the cells
/// at the shape's world cell origin, in a world-tile table holding the tiles
/// they touch. Returns the pooled phi, the table words and the pool's tiles.
/// Cells of those tiles outside the lattice read 0, the canonical value.
pub fn pooled_distance(dense: &[f32], shape: &StepShape) -> (Vec<f32>, Vec<u32>, u32) {
    let table = BoxTable::new(shape.cell_origin, shape.face_cells);
    (table.to_pool(dense, 0.0), table.words, table.pool_tiles)
}

/// A dense face-record lattice (`(n+1)^3`) as the step's pooled faces: record
/// p lives with world cell cell_origin + p, in a world-tile table holding the
/// tiles the records touch. Returns the pooled records, the table words and
/// the pool's tiles. Cells of those tiles outside the lattice hold no face.
pub fn pooled_faces(dense: &[FaceSample], shape: &StepShape) -> (Vec<FaceSample>, Vec<u32>, u32) {
    let table = BoxTable::new(shape.cell_origin, shape.face_cells.map(|n| n + 1));
    (
        table.to_pool(dense, FaceSample::default()),
        table.words,
        table.pool_tiles,
    )
}

#[cfg(any(test, feature = "gpu-proofs"))]
fn synthetic_faces() -> [Vec<f32>; 3] {
    std::array::from_fn(|a| vec![[2.0, 1.0, -0.5][a]; face_len([8; 3], a) as usize])
}

pub fn synthetic_shape() -> StepShape {
    StepShape::new(
        [13; 3],
        [8; 3],
        1.0,
        Some(Transform {
            pos: [0.6; 3],
            scale: [1.2; 3],
            ..Default::default()
        }),
        256,
    )
    .unwrap()
}

#[cfg(any(test, feature = "gpu-proofs"))]
fn synthetic_records() -> Vec<FluidParticle> {
    (0..8)
        .map(|i| FluidParticle {
            position_radius: [
                0.45 + 0.015 * i as f32,
                0.5,
                0.5,
                if i == 6 {
                    0.0
                } else if i == 7 {
                    -1.0
                } else {
                    0.025
                },
            ],
            velocity: [17.0, -3.25, 0.75],
            id: 255 - i,
        })
        .collect()
}

#[test]
fn whitewater_half_integer_fixture_has_saturated_energy() {
    use super::super::whitewater_particle_cpu::{Box3, energy, jitter, sample_faces};
    let faces = synthetic_faces();
    let shape = synthetic_shape();
    let grid = Box3 {
        cells: shape.cells,
        center: shape.center,
        size: shape.size,
    };
    for (axis, face) in faces.iter().enumerate() {
        assert_eq!(
            bytemuck::cast_slice::<_, u8>(face).len(),
            face_len([8; 3], axis) as usize * 4
        );
    }
    for (i, particle) in synthetic_records().into_iter().take(6).enumerate() {
        let sampled = sample_faces(
            jitter(particle, i as u32, shape.cell_size, 0.375, 7.0),
            faces.each_ref().map(Vec::as_slice),
            [8; 3],
            &grid,
        );
        assert_eq!(
            energy(sampled, 0.0, 1.0).to_bits(),
            1.0f32.to_bits(),
            "emitter {i}"
        );
    }
}

#[cfg(feature = "gpu-proofs")]
pub mod gpu {
    use super::super::empty_slot;
    use super::*;
    use manifold_node_engine::testkit::array_harness::read;

    const PORTS: [&str; 6] = [
        "proof_sampled",
        "proof_unscaled",
        "proof_energy",
        "proof_counts",
        "proof_dust_energy",
        "proof_dust_counts",
    ];

    pub fn equal_words(got: &[u32], want: &[u32], tick: usize, name: &str) {
        assert_eq!(got.len(), want.len(), "tick {tick} {name}: length");
        if let Some(i) = got.iter().zip(want).position(|(a, b)| a != b) {
            let stride = if name.contains("lifecycle") || name == "pool_out" {
                12
            } else if name.contains("sampled")
                || name.contains("unscaled")
                || name.contains("typed")
            {
                8
            } else {
                1
            };
            panic!(
                "tick {tick} {name} element {} word {}: fused {:08x}, reference {:08x}",
                i / stride,
                i % stride,
                got[i],
                want[i]
            );
        }
    }

    pub fn shared<T: bytemuck::Pod>(device: &GpuDevice, data: &[T]) -> GpuBuffer {
        let bytes = bytemuck::cast_slice(data);
        let buffer = device.create_buffer_shared(bytes.len().max(16) as u64);
        buffer.zero_fill();
        // SAFETY: fresh shared storage, sized for the source; not submitted yet.
        unsafe { buffer.write(0, bytes) };
        buffer
    }

    /// The identity words of `n` liquid particles in use: the dispatch triple
    /// at words 4-6 (at least one group) and `n` at word 7.
    pub fn identity_words(n: u32) -> [u32; 8] {
        [0, 0, 0, 0, n.div_ceil(256).max(1), 1, 1, n]
    }

    pub fn identity_record(device: &GpuDevice, n: u32) -> GpuBuffer {
        shared(device, &identity_words(n))
    }

    pub struct Fixture {
        pub shape: StepShape,
        pub records: Vec<FluidParticle>,
        particles: GpuBuffer,
        identity: GpuBuffer,
        solid: GpuBuffer,
        distance: GpuBuffer,
        tiles: GpuBuffer,
        pool_tiles: u32,
        faces: [GpuBuffer; 3],
        source: GpuBuffer,
        pool: GpuBuffer,
        state: GpuBuffer,
    }

    impl Fixture {
        pub fn new(device: &GpuDevice) -> Self {
            let shape = synthetic_shape();
            let records = synthetic_records();
            let (phi, table, pool_tiles) = pooled_distance(&vec![-1.0f32; 8 * 8 * 8], &shape);
            Self {
                shape,
                particles: shared(device, &records),
                identity: identity_record(device, records.len() as u32),
                records,
                solid: shared(device, &vec![0.1f32; 13 * 13 * 13]),
                distance: shared(device, &phi),
                tiles: shared(device, &table),
                pool_tiles,
                faces: synthetic_faces().map(|face| shared(device, &face)),
                source: shared(
                    device,
                    &vec![
                        WhitewaterSource {
                            influence: 1.0,
                            dust_strength: 1.0,
                            kind: 2,
                            pad: 0
                        };
                        13 * 13 * 13
                    ],
                ),
                pool: shared(device, &vec![empty_slot(); 256]),
                state: shared(device, &[0u32; 8]),
            }
        }
        pub fn frame(&self, dust: bool) -> StepFrame {
            StepFrame {
                shape: self.shape,
                ticks: 3,
                dt: 0.015625,
                epoch: 7,
                seed: 0.375,
                gravity: [0.0; 3],
                wavecrest_emission: 0.0,
                turbulence_emission: 32.0,
                min_turbulence: -1.0,
                max_turbulence: -0.5,
                inside_emission: true,
                generation_rate: 1.0,
                spray_speed: 2.0,
                dust_emission: dust,
                boundary_dust: true,
                dust_rate: 32.0,
                influence_base: 1.0,
                influence_decay: 0.0,
                min_energy: 0.0,
                max_energy: 1.0,
                preserve_foam: true,
            }
        }
        pub fn inputs(&self) -> StepInputs<'_> {
            StepInputs {
                motion: None,
                particles: &self.particles,
                identity: &self.identity,
                solid: &self.solid,
                obstacle_source: Some(&self.source),
                faces: FaceSource::Axes(self.faces.each_ref()),
                distance: &self.distance,
                tiles: &self.tiles,
                pool_tiles: self.pool_tiles,
            }
        }
        /// Count `n` of the fixture's particles in use from now on. Call
        /// between ticks: no committed encoder may still read the identity.
        pub fn set_emitters(&self, n: u32) {
            // SAFETY: the identity's shared storage holds eight words and the
            // caller's last encoder has completed.
            unsafe {
                self.identity
                    .write(0, bytemuck::cast_slice(&identity_words(n)));
            }
        }
    }

    pub fn snapshots(
        device: &GpuDevice,
        enc: &mut manifold_gpu::GpuEncoder,
        step: &Step,
    ) -> Vec<GpuBuffer> {
        step.reference
            .snapshots
            .as_ref()
            .unwrap()
            .iter()
            .map(|src| {
                let dst = device.create_buffer_shared(src.size);
                enc.copy_buffer_to_buffer(src, &dst, src.size);
                dst
            })
            .collect()
    }

    pub fn synthetic(dust: bool) {
        let device = manifold_gpu::testkit::test_device();
        let fixtures = [Fixture::new(&device), Fixture::new(&device)];
        let mut stages = [Step::default(), Step::default()];
        stages[0].reference.capture = true;
        stages[1].reference.capture = true;
        stages[1].reference.enabled = true;
        // Exactly representable rates put per_tick immediately around 0.5,
        // then at 1.5. ticks=3 distinguishes round-then-multiply from its inverse.
        let cases = [
            (31.99999, 8, 0),
            (32.0, 8, 3),
            (32.00001, 8, 3),
            (96.0, 8, 6),
            (32.0, 0, 0),
            (32.0, 8, 3),
        ];
        for (tick, (rate, count, expected)) in cases.into_iter().enumerate() {
            let mut encoder = device.create_encoder("whitewater synthetic tick");
            let mut captured = Vec::new();
            for (stage, fixture) in stages.iter_mut().zip(&fixtures) {
                let mut frame = fixture.frame(dust);
                frame.turbulence_emission = rate;
                frame.dust_rate = rate;
                // The fused kernels count from the identity, the atoms from
                // the scalar; both are the same `count` of the eight slots.
                fixture.set_emitters(count);
                stage.reference.emitters = Some(count);
                stage
                    .advance_tick(
                        &mut GpuEncoder::new(&mut encoder, &device),
                        &frame,
                        &fixture.inputs(),
                        &fixture.pool,
                        &fixture.state,
                        true,
                    )
                    .unwrap();
                captured.push(snapshots(&device, &mut encoder, stage));
            }
            encoder.commit_and_wait_completed();
            for (i, name) in PORTS[..if dust { 6 } else { 4 }].iter().enumerate() {
                if !dust && i == 1 {
                    continue;
                }
                let a = read::<u32>(&captured[0][i], captured[0][i].size as usize / 4);
                let b = read::<u32>(&captured[1][i], captured[1][i].size as usize / 4);
                equal_words(&a, &b, tick, name);
                if i == 3 || i == 5 {
                    assert_eq!(
                        &a[..6],
                        &[expected; 6],
                        "tick {tick} {name}: half-integer count coverage"
                    );
                    assert_eq!(&a[6..8], &[0, 0], "dead slots must not emit");
                }
            }
            if count > 0 && dust {
                let unscaled = read::<FluidParticle>(&captured[0][1], 8);
                assert_eq!(
                    bytemuck::bytes_of(&unscaled[6]),
                    bytemuck::bytes_of(&fixtures[0].records[6])
                );
                assert_eq!(
                    bytemuck::bytes_of(&unscaled[7]),
                    bytemuck::bytes_of(&fixtures[0].records[7])
                );
            }
        }
        // The fused passes dispatch every tick, an empty population included;
        // the atoms skip it.
        assert_eq!(stages[0].reference.emit_dispatches.get(), 6);
        assert_eq!(stages[1].reference.emit_dispatches.get(), 35);
        assert_eq!(
            stages[0].reference.dust_dispatches.get(),
            if dust { 6 } else { 0 }
        );
        assert_eq!(
            stages[1].reference.dust_dispatches.get(),
            if dust { 15 } else { 0 }
        );
    }

    pub fn particle_stages(device: &GpuDevice, shape: StepShape, slots: u32) -> [Step; 2] {
        let mut stages = [Step::default(), Step::default()];
        stages[1].reference.enabled = true;
        for stage in &mut stages {
            stage.reference.capture = true;
            stage.pipelines.prepare(device);
            stage.surface_distance.prepare(device);
            stage.emission_scan.prepare(device);
            stage.slot_scan.prepare(device);
            stage.sort.prepare(device);
            stage.reserve(device, shape, u64::from(slots)).unwrap();
        }
        stages
    }

    pub fn copy_shared(
        device: &GpuDevice,
        enc: &mut manifold_gpu::GpuEncoder,
        src: &GpuBuffer,
    ) -> GpuBuffer {
        let dst = device.create_buffer_shared(src.size);
        enc.copy_buffer_to_buffer(src, &dst, src.size);
        dst
    }

    pub fn compare_buffers(a: &GpuBuffer, b: &GpuBuffer, tick: usize, name: &str) {
        equal_words(
            &read::<u32>(a, a.size as usize / 4),
            &read::<u32>(b, b.size as usize / 4),
            tick,
            name,
        );
    }

    pub fn mixed_faces() -> Vec<FaceSample> {
        (0..9 * 9 * 9)
            .map(|i| FaceSample {
                velocity: [
                    (i % 17) as f32 / 7.0,
                    -((i % 11) as f32) / 3.0,
                    (i % 13) as f32 / 19.0,
                    12345.0,
                ],
                weight: [
                    [-1.0 / 3.0, 0.0, 1.0 / 7.0, 2.0 / 3.0][i % 4],
                    [-1.0 / 7.0, 0.0, 1.0 / 3.0, 3.0 / 7.0][(i + 1) % 4],
                    [-2.0 / 3.0, 0.0, 2.0 / 7.0, 1.0 / 3.0][(i + 2) % 4],
                    -9876.0,
                ],
            })
            .collect()
    }

    // Compile the actual face component, with its own generated uniform
    // layout/body, over the pooled faces and their slot table. Dispatch its
    // whole output extent, including the zero tail.
    pub fn adapter_outputs(
        device: &GpuDevice,
        packed: &GpuBuffer,
        tiles: &GpuBuffer,
        pool_tiles: u32,
        cells: [u32; 3],
        cell_origin: [i32; 3],
    ) -> [GpuBuffer; 3] {
        use manifold_water_liquid::primitives::face_sample_component::FaceSampleComponent;
        let mut pipeline = None;
        standalone_pipeline::<FaceSampleComponent>(&mut pipeline, device);
        let count = cell_total(cells.map(|n| n + 1)) as u32;
        let out = std::array::from_fn(|_| shared(device, &vec![12345.0f32; count as usize]));
        let [nx, ny, nz] = cells.map(|n| (n + 4) as f32);
        let [ox, oy, oz] = cell_origin.map(|c| c as f32);
        let mut enc = device.create_encoder("whitewater face adapter oracle");
        for (axis, output) in out.iter().enumerate() {
            atom::<FaceSampleComponent>(
                &mut enc,
                pipeline.as_ref().unwrap(),
                &[
                    ("axis", axis as f32),
                    ("nodes_x", nx),
                    ("nodes_y", ny),
                    ("nodes_z", nz),
                    ("pool_tiles", pool_tiles as f32),
                    ("cell_origin_x", ox),
                    ("cell_origin_y", oy),
                    ("cell_origin_z", oz),
                ],
                &[packed, tiles, output],
                count,
                "whitewater adapter oracle",
            );
        }
        enc.commit_and_wait_completed();
        out
    }

    pub fn spawn_boundaries() {
        let device = manifold_gpu::testkit::test_device();
        let fixtures = [Fixture::new(&device), Fixture::new(&device)];
        let shape = StepShape::new(
            [17; 3],
            [16; 3],
            1.0,
            Some(Transform {
                pos: [1.0; 3],
                scale: [2.0; 3],
                ..Default::default()
            }),
            256,
        )
        .unwrap();
        let mut stages = particle_stages(&device, shape, 3);
        let identity = identity_record(&device, 3);
        for stage in &mut stages {
            stage.reference.emitters = Some(3);
        }
        let records: Vec<_> = (0..3)
            .map(|i| FluidParticle {
                position_radius: [0.9 + 0.1 * i as f32, 1.0, 1.0, 0.025],
                velocity: [2.0, 1.0, -0.5],
                id: 255 - i,
            })
            .collect();
        // Three unequal emitters and nonmatching IDs make slot-vs-emitter RNG
        // mistakes visible when sw_mul_div thins 4097 emissions into 256 slots.
        let counts = [[1u32, 3, 7], [1001, 2049, 4097]];
        let h = shape.cell_size;
        let epsilon = h / 1024.0;
        let mut saw = [false; 5];
        for (case, (distance, expected)) in [
            (-h - epsilon, Some(0)),
            (-h, None),
            (-h + epsilon, Some(1)),
            (h - epsilon, Some(1)),
            (h, None),
            (h + epsilon, Some(2)),
        ]
        .into_iter()
        .enumerate()
        {
            for (overflow, offsets) in counts.iter().enumerate() {
                for dust in [false, true] {
                    let mut enc = device.create_encoder("whitewater spawn thresholds");
                    let mut captures = Vec::new();
                    for (stage, fixture) in stages.iter_mut().zip(&fixtures) {
                        let mut frame = fixture.frame(dust);
                        frame.shape = shape;
                        frame.dt = 0.0;
                        let sampled = shared(&device, &records);
                        let energy = shared(&device, &[1.0f32; 3]);
                        let offsets = shared(&device, offsets);
                        let surface = shared(&device, &vec![distance; shape.cell_count() as usize]);
                        let air = shared(&device, &vec![0u32; shape.cell_count() as usize]);
                        let solid = shared(&device, &vec![1.0f32; 17 * 17 * 17]);
                        let faces: [GpuBuffer; 3] = std::array::from_fn(|a| {
                            shared(
                                &device,
                                &vec![[2.0f32, 1.0, -0.5][a]; face_len([16; 3], a) as usize],
                            )
                        });
                        enc.copy_buffer_to_buffer(&air, &stage.fields().cells, air.size);
                        let mut inputs = fixture.inputs();
                        inputs.solid = &solid;
                        inputs.identity = &identity;
                        inputs.faces = FaceSource::Axes(faces.each_ref());
                        stage.spawn(
                            &mut enc, &frame, &inputs, &surface, &offsets, &sampled, &energy, dust,
                        );
                        captures.push(copy_shared(
                            &device,
                            &mut enc,
                            &stage.reference.particle_snapshots.as_ref().unwrap()
                                [usize::from(dust)],
                        ));
                    }
                    enc.commit_and_wait_completed();
                    compare_buffers(&captures[0], &captures[1], case, "proof_typed");
                    let typed = read::<WhitewaterSpawn>(&captures[0], 256);
                    let live = if overflow == 0 { 7 } else { 256 };
                    assert!(
                        typed[..live].iter().all(|p| p.position_lifetime[3] > 0.0),
                        "spawn fixture lost a live slot"
                    );
                    assert!(
                        typed[live..].iter().all(|p| p.position_lifetime[3] == 0.0),
                        "unused slots not empty"
                    );
                    for p in &typed[..live] {
                        saw[p.kind as usize] = true;
                    }
                    if let Some(kind) = if dust { Some(4) } else { expected } {
                        assert!(
                            typed[..live].iter().all(|p| p.kind == kind),
                            "classification threshold {distance}, dust={dust}"
                        );
                    }
                }
            }
        }
        assert!(saw[0] && saw[1] && saw[2] && saw[4]);
        assert_eq!(
            stages[1].reference.spawn_dispatches.get(),
            2 * stages[0].reference.spawn_dispatches.get()
        );
    }

    pub fn spawn_overflow() {
        let device = manifold_gpu::testkit::test_device();
        let fixtures = [Fixture::new(&device), Fixture::new(&device)];
        let mut stages = particle_stages(&device, fixtures[0].shape, 8);
        let mut enc = device.create_encoder("whitewater append overflow");
        let mut captures = Vec::new();
        for (stage, fixture) in stages.iter_mut().zip(&fixtures) {
            let mut frame = fixture.frame(true);
            frame.turbulence_emission = 8192.0;
            frame.dust_rate = 8192.0;
            let pool = shared(
                &device,
                &vec![
                    WhitewaterParticle {
                        position_lifetime: [0.5, 0.5, 0.5, 7.0],
                        velocity: [0.0; 3],
                        kind: 0,
                        id: 255,
                        pad0: 0,
                        pad1: 0,
                        pad2: 0
                    };
                    256
                ],
            );
            let state = shared(&device, &[255u32, 255, 0, 0, 0, 0, 0, 0]);
            stage
                .advance_tick(
                    &mut GpuEncoder::new(&mut enc, &device),
                    &frame,
                    &fixture.inputs(),
                    &pool,
                    &state,
                    true,
                )
                .unwrap();
            captures.push(
                ["proof_typed", "proof_dust_typed", "state_out"].map(|port| {
                    let src = if port == "state_out" {
                        stage.tick_output(port).unwrap()
                    } else {
                        stage.reference.output(port).unwrap()
                    };
                    copy_shared(&device, &mut enc, src)
                }),
            );
        }
        enc.commit_and_wait_completed();
        for (i, name) in ["proof_typed", "proof_dust_typed", "state_out"]
            .into_iter()
            .enumerate()
        {
            compare_buffers(&captures[0][i], &captures[1][i], 0, name);
        }
        let state = read::<u32>(&captures[0][2], 8);
        assert!(state[2] > 0, "pool overflow not exercised");
        assert!(state[4] > 0, "capacity thinning not exercised");
        assert_eq!(state[1], 0, "one append must wrap id 255");
        assert_eq!(stages[0].reference.spawn_dispatches.get(), 2);
        assert_eq!(stages[1].reference.spawn_dispatches.get(), 4);
    }

    pub fn lifecycle_history() {
        let device = manifold_gpu::testkit::test_device();
        let fixtures = [Fixture::new(&device), Fixture::new(&device)];
        let shape = fixtures[0].shape;
        let mut stages = particle_stages(&device, shape, 8);
        let mut controls = particle_stages(&device, shape, 8);
        let mut records = vec![empty_slot(); 256];
        for (i, p) in records[..12].iter_mut().enumerate() {
            *p = WhitewaterParticle {
                position_lifetime: [0.5, 0.5, 0.5, if i < 6 { 7.0 } else { -1.0 }],
                velocity: [0.0; 3],
                kind: (i % 6) as u32,
                id: (255 - i) as u32,
                pad0: 17,
                pad1: 29,
                pad2: 43,
            };
        }
        // Slots 3/5 and 9/11 pass through aw_step, but must first receive both
        // impulse events. Foam slots 1/7 receive history velocity, never a
        // direct impulse. Negative lifetime must not bypass retype or age.
        for (tick, preserve) in [false, true, true, false].into_iter().enumerate() {
            let mut enc = device.create_encoder("whitewater lifecycle substep history");
            let mut captures = Vec::new();
            for (impulses_on, pair) in [(true, &mut stages), (false, &mut controls)] {
                for (stage, fixture) in pair.iter_mut().zip(&fixtures) {
                    let mut frame = fixture.frame(false);
                    frame.dt = 3.0 / 64.0;
                    frame.preserve_foam = preserve;
                    let surface = shared(&device, &vec![0.0f32; shape.cell_count() as usize]);
                    let air = shared(&device, &vec![0u32; shape.cell_count() as usize]);
                    let pool = shared(&device, &records);
                    let state = shared(&device, &[12u32, 255, 0, 0, 0, 0, 0, 0]);
                    let schedule = shared(
                        &device,
                        &[
                            1.0f32 / 64.0,
                            0.0,
                            f32::from_bits(if impulses_on { 0x80000000 } else { 0 }),
                            0.0,
                            1.0 / 64.0,
                            0.0,
                            0.0,
                            0.0,
                            1.0 / 64.0,
                            0.0,
                            f32::from_bits(if impulses_on { 0x80000001 } else { 0 }),
                            0.0,
                        ],
                    );
                    let histories: [GpuBuffer; 3] = std::array::from_fn(|axis| {
                        let data: Vec<f32> = (0..3)
                            .flat_map(|step| {
                                std::iter::repeat_n(
                                    (step + 1) as f32 * [0.125, 0.0625, 0.03125][axis],
                                    face_len(shape.face_cells, axis) as usize,
                                )
                            })
                            .collect();
                        shared(&device, &data)
                    });
                    let forces = shared(
                        &device,
                        &[[0.125f32, 0.25, -0.125, 0.0], [0.0; 4]].repeat(16),
                    );
                    let mut impulse_data = vec![[0.25f32, -0.5, 0.125, 0.0]; 8];
                    impulse_data.extend_from_slice(&[[0.75, 0.5, -0.125, 0.0]; 8]);
                    let impulses = shared(&device, &impulse_data);
                    let mut inputs = fixture.inputs();
                    inputs.motion = Some(MotionInputs {
                        schedule: &schedule,
                        faces: histories.each_ref(),
                        count: 3,
                        fields: ForceLatticeBinding {
                            nodes: [2; 3],
                            spacing: 0.5,
                            min: [0.0; 3],
                            force_lattices: 2,
                            impulse_tick: 0,
                            first_tick: 3,
                            forces: Some(&forces),
                            impulses: Some(&impulses),
                        },
                        tick_index: 4.0,
                        regions: None,
                        shapes: None,
                        atlas: None,
                        region_count: 0,
                    });
                    let old_current = stage.current;
                    enc.copy_buffer_to_buffer(&pool, &stage.fields().pools[old_current], pool.size);
                    enc.copy_buffer_to_buffer(&state, &stage.fields().state, state.size);
                    enc.copy_buffer_to_buffer(&air, &stage.fields().cells, air.size);
                    stage
                        .tick(&mut enc, &device, &frame, &inputs, &surface)
                        .unwrap();
                    assert_eq!(
                        stage.current,
                        if preserve {
                            1 - old_current
                        } else {
                            old_current
                        }
                    );
                    captures.push([
                        copy_shared(
                            &device,
                            &mut enc,
                            &stage.reference.particle_snapshots.as_ref().unwrap()[2],
                        ),
                        copy_shared(&device, &mut enc, stage.tick_output("pool_out").unwrap()),
                        copy_shared(&device, &mut enc, stage.tick_output("state_out").unwrap()),
                    ]);
                }
            }
            enc.commit_and_wait_completed();
            for start in [0, 2] {
                for (i, name) in ["proof_lifecycle", "pool_out", "state_out"]
                    .into_iter()
                    .enumerate()
                {
                    compare_buffers(&captures[start][i], &captures[start + 1][i], tick, name);
                }
            }
            let with = read::<WhitewaterParticle>(&captures[0][0], 256);
            let without = read::<WhitewaterParticle>(&captures[2][0], 256);
            for i in [3, 5, 9, 11] {
                assert!(
                    with[i].velocity[0] > 0.9,
                    "dead/pass-through slot {i} missed impulses"
                );
                assert_eq!(without[i].velocity, [0.0; 3]);
                assert_eq!(with[i].position_lifetime, records[i].position_lifetime);
            }
            for i in [1, 7] {
                assert_eq!(
                    bytemuck::bytes_of(&with[i]),
                    bytemuck::bytes_of(&without[i]),
                    "foam must not receive a direct impulse"
                );
                assert!(
                    with[i].velocity[0] > 0.37 && with[i].velocity[0] < 0.38,
                    "foam did not read the final substep history"
                );
            }
            assert_eq!(with[6].kind, 1, "dead bubble still retypes");
            assert!(
                with[6].position_lifetime[3] < -1.0,
                "dead bubble still ages"
            );
            assert_eq!(
                with[4].position_lifetime[3].to_bits(),
                (7.0f32 - 3.0 / 64.0).to_bits(),
                "dust ages at 1/s"
            );
        }
        assert_eq!(stages[0].reference.lifecycle_dispatches.get(), 4);
        assert_eq!(stages[1].reference.lifecycle_dispatches.get(), 12);
    }

    /// Emission reads the liquid particles in use and nothing past them:
    /// the same `n` emitters in a buffer that goes on with live-looking and
    /// NaN records give the same pool, state, counts and populations as in a
    /// buffer that ends at `n`. `n` covers one partial group and two groups,
    /// the second one partial; the 300 emitters make more spawns than the
    /// pool holds, so the thinned subset is compared too.
    #[test]
    fn whitewater_emission_ignores_slots_past_the_count() {
        let device = manifold_gpu::testkit::test_device();
        let fixture = Fixture::new(&device);
        let emitter = |i: u32| FluidParticle {
            position_radius: [
                0.35 + 0.5 * (i as f32 * 0.618_034).fract(),
                0.35 + 0.5 * (i as f32 * 0.414_214).fract(),
                0.5,
                0.025,
            ],
            velocity: [17.0, -3.25, 0.75],
            id: 255 - i % 256,
        };
        let ports = [
            "pool_out",
            "state_out",
            "counts_out",
            "foam_particles",
            "bubble_particles",
            "spray_particles",
            "dust_particles",
        ];
        for n in [6u32, 300] {
            let live: Vec<_> = (0..n).map(emitter).collect();
            let mut past = live.clone();
            for i in 0..n {
                past.push(if i % 3 == 0 {
                    FluidParticle {
                        position_radius: [f32::NAN, 0.5, 0.5, 0.025],
                        velocity: [f32::NAN; 3],
                        id: 7,
                    }
                } else {
                    emitter(n + i)
                });
            }
            let identity = identity_record(&device, n);
            let mut rows = Vec::new();
            for records in [&live, &past] {
                let particles = shared(&device, records);
                let mut inputs = fixture.inputs();
                inputs.particles = &particles;
                inputs.identity = &identity;
                let mut stage = Step::default();
                let mut enc = device.create_encoder("whitewater emission past the count");
                stage
                    .advance_tick(
                        &mut GpuEncoder::new(&mut enc, &device),
                        &fixture.frame(true),
                        &inputs,
                        &fixture.pool,
                        &fixture.state,
                        true,
                    )
                    .unwrap();
                let row: Vec<_> = ports
                    .iter()
                    .map(|port| copy_shared(&device, &mut enc, stage.tick_output(port).unwrap()))
                    .collect();
                enc.commit_and_wait_completed();
                rows.push(row);
            }
            for (i, port) in ports.into_iter().enumerate() {
                compare_buffers(&rows[1][i], &rows[0][i], n as usize, port);
            }
            let emitted = read::<u32>(&rows[0][1], 8)[3];
            assert!(emitted > 0, "n {n}: the emitters must spawn");
        }
    }

    #[test]
    fn whitewater_fused_pipelines_are_dispatch_legal() {
        let device = manifold_gpu::testkit::test_device();
        let mut stage = Step::default();
        stage.pipelines.prepare(&device);
        stage.reference.enabled = true;
        stage.reference.reserve(&device, 1, 1).unwrap();
        for pipeline in &stage.pipelines.fused {
            let max = pipeline.max_threads_per_threadgroup();
            println!(
                "fused {}: max_threads={max:?}, dispatched=256",
                pipeline.label
            );
            // GPU proofs are Metal-only for now; Vulkan reports None here.
            assert!(max.is_some_and(|n| n >= 256));
        }
        stage.reference.print_pipeline_limits();
    }
}
