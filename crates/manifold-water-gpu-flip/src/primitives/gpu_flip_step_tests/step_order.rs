//! Step order proofs: extension, the cell cap and inflow timing.

use super::*;

#[test]
pub(super) fn gpu_flip_step_order_extend_constraint_value_proof() {
    use super::super::gpu_flip_extension_tests::{cpu_extend, moving_wall_fixture};
    let (initial, wall) = moving_wall_fixture(N);
    assert_eq!(initial.len(), face_len());
    for cfl in [1, 3, 5, 8] {
        let mut want = initial.clone();
        let mut got = initial.clone();
        for _ in 0..super::super::gpu_flip_step::band_layers(cfl) {
            want = cpu_extend(&want, N);
            got = Pass::new().bind(3, &got, Layout::Records).run(
                "extend_faces",
                &lattice(),
                4,
                face_len(),
                face_len(),
            );
        }
        let mut open = vec![
            FaceSample {
                weight: [1.0; 4],
                ..FaceSample::default()
            };
            face_len()
        ];
        let mut moving = vec![FaceSample::default(); face_len()];
        open[wall].weight[0] = 0.5;
        moving[wall].weight[0] = 0.5;
        moving[wall].velocity[0] = 7.0;
        let open64: Vec<f64> = bytemuck::cast_slice::<_, f32>(&open)
            .iter()
            .map(|&x| f64::from(x))
            .collect();
        let moving64: Vec<f64> = bytemuck::cast_slice::<_, f32>(&moving)
            .iter()
            .map(|&x| f64::from(x))
            .collect();
        let expected = cpu_constrain(bytemuck::cast_slice(&want), &open64, &moving64, N);
        let constrained: Vec<FaceSample> = Pass::new()
            .bind(20, &got, Layout::Records)
            .bind(10, &open, Layout::Records)
            .bind(11, &moving, Layout::Records)
            .run(
                "constrain_solid_faces",
                &lattice(),
                20,
                face_len(),
                face_len(),
            );
        assert_close(
            bytemuck::cast_slice(&constrained),
            &expected,
            "extend then constrain moving wall",
        );
        // The wall face's z + 1 neighbour is this lattice's held border row:
        // the extension averages its 0 with the seed's 2.
        assert_eq!(constrained[wall].velocity[0], 4.0);
    }
}

#[test]
pub(super) fn gpu_flip_step_order_cell_cap_compacts_preserving_ids() {
    use manifold_water_liquid::primitives::prefix_scan::ScanLabels;
    use manifold_water_liquid::primitives::sort_particles_into_cells::{
        LIQUID_PARTICLE_READ, ParticleSorter, SortJob, SortLabels,
    };
    let particles: Vec<_> = (0..300)
        .map(|i| FluidParticle {
            position_radius: [
                MIN[0] + 2.25 * H,
                MIN[1] + 2.25 * H,
                MIN[2] + 2.25 * H,
                0.05,
            ],
            velocity: [1.0, 0.0, 0.0],
            id: 1000 + i,
        })
        .collect();
    let (mut sorted, ranges) = cpu_sort(&particles);
    // The clock removed an extreme marker, but its pre-removal cell rank
    // must still consume a quota place, exactly as the native loop does.
    sorted[0].position_radius[3] = 0.0;
    assert_eq!(ranges.len(), cell_len());
    assert_eq!(ranges.iter().map(|r| r.count).sum::<u32>(), 300);
    let mut pass = Pass::new();
    let removed: Vec<FluidParticle> = pass
        .bind(1, &ranges, Layout::Dense)
        .bind(38, &sorted, Layout::Dense)
        .run("remove_crowded_markers", &lattice(), 38, 300, cell_len());
    let mut speeds = [1.0; 300];
    speeds[0] = 1000.0;
    let want = super::super::gpu_flip_extension_tests::native_cell_survivors(&speeds, 6.0);
    for (i, p) in removed.iter().enumerate() {
        assert_eq!(p.position_radius[3] > 0.0, want.contains(&i));
        assert_eq!(p.id, sorted[i].id);
    }
    let input = &pass.bound.iter().find(|(b, _)| *b == 38).unwrap().1;
    let output = pass.device.create_buffer_shared(300 * 32);
    let mut sorter = ParticleSorter::default();
    sorter.prepare(&pass.device);
    sorter
        .reserve_ranges(&pass.device, N.map(|n| n as u32))
        .unwrap();
    let mut enc = pass.device.create_encoder("step order compact proof");
    let labels = SortLabels {
        clear: "clear",
        count: "count",
        scan: ScanLabels {
            blocks: "blocks",
            add: "add",
        },
        ranges: "ranges",
        tail: "tail",
        scatter: "scatter",
        stabilise: "stable",
    };
    sorter
        .encode(
            &pass.device,
            &mut enc,
            &SortJob {
                particles: input,
                read: LIQUID_PARTICLE_READ,
                capacity: 300,
                count: 300,
                bin_min: MIN,
                inv_cell: 1.0 / H,
                bins: N.map(|n| n as u32),
                sorted: Some(&output),
                order: None,
                gate: None,
            },
            &labels,
        )
        .unwrap();
    enc.commit_and_wait_completed();
    let compacted: Vec<FluidParticle> = read(&output, 300);
    assert_eq!(&compacted[..249], &particles[1..250]);
    assert!(compacted[249..].iter().all(|p| p.position_radius[3] == 0.0));
}

#[test]
pub(super) fn gpu_flip_step_order_inflow_waits_until_next_step() {
    use super::super::gpu_flip_step::GpuFlipStep;
    use manifold_node_engine::primitive::Primitive;
    let capacity = 512;
    // Extent arithmetic before device creation: 120 cell ranges, 210 face
    // records, 960 emit sites, 512 particle records, two counters per slot.
    assert_eq!((cell_len(), face_len()), (120, 210));
    assert_eq!(
        super::super::gpu_flip_step::emit_sites(N.map(|n| n as u32)),
        960
    );
    let mut h = Harness::new();
    let mut node = GpuFlipStep::new();
    node.prepare_pipelines(&h.device);
    let particles = h.array::<FluidParticle>(&[], capacity);
    let capped = h.array::<u32>(
        &[],
        2 * capacity + manifold_water_liquid::primitives::liquid_stats::SOLVER_WORDS as usize,
    );
    // The step solves on the native grid: three more cells, its origin 1.5
    // cells outside the box.
    let solver = N.map(|n| n + 3);
    let solver_min: [f32; 3] = std::array::from_fn(|a| MIN[a] - 1.5 * H);
    let faces = h.array::<FaceSample>(&[], 1);
    let tiles = h.array::<u32>(&[], 1);
    let (row, shape, atlas) = region_box(2.0, [2, 2, 1], [1, 1, 1], [0.25, 0.0, 0.0]);
    let regions = h.array(&[row], 1);
    let shapes = h.array(&[shape], 1);
    let atlas = h.array(&atlas, atlas.len());
    let bodies = h.array::<LiquidBody>(&[], 1);
    let pad = PADDING_NODES as f32;
    let mut p = params(&[
        ("nodes_x", N[0] as f32 + 1.0 + 2.0 * pad),
        ("nodes_y", N[1] as f32 + 1.0 + 2.0 * pad),
        ("nodes_z", N[2] as f32 + 1.0 + 2.0 * pad),
        ("lattice_min_x", MIN[0] - pad * H),
        ("lattice_min_y", MIN[1] - pad * H),
        ("lattice_min_z", MIN[2] - pad * H),
        ("cell_size", H),
        ("gravity_y", 0.0),
        ("interval_duration", 0.125),
        ("volume_projection", 0.0),
        ("region_count", 1.0),
        ("iterations", 0.0),
        ("flip", 1.0),
    ]);
    // liquid_state's birth identity as its reset seeds it: next id, epoch,
    // reserved base, full-reset request.
    let identity = h.array::<u32>(&[1, 1, 0, 0], 4);
    let inputs = [
        ("particles", particles.0),
        ("regions", regions.0),
        ("shapes", shapes.0),
        ("atlas", atlas.0),
        ("bodies", bodies.0),
        ("identity", identity.0),
    ];
    let outputs = [
        ("out", particles.0),
        ("faces", faces.0),
        ("tiles", tiles.0),
        ("capped", capped.0),
    ];
    let (_, errors) = h.run(&mut node, &inputs, &outputs, &p);
    assert!(errors.is_empty(), "{errors:?}");
    let first: Vec<FluidParticle> = read(&particles.1, capacity);
    let alive: Vec<_> = first
        .iter()
        .filter(|p| p.position_radius[3] > 0.0)
        .copied()
        .collect();
    assert_eq!(alive.len(), 8);
    for marker in &alive {
        assert_eq!(marker.velocity, [0.25, 0.0, 0.0]);
        let q = (marker.position_radius[0] - MIN[0]) / H;
        assert!(q == 2.25 || q == 2.75, "fresh inflow moved: {q}");
    }
    p.insert("tick_index".into(), ParamValue::Float(1.0));
    p.insert("first_tick".into(), ParamValue::Float(1.0));
    let (_, errors) = h.run(&mut node, &inputs, &outputs, &p);
    assert!(errors.is_empty(), "{errors:?}");
    let second: Vec<FluidParticle> = read(&particles.1, capacity);
    let field = step_faces(&h, faces.0, tiles.0, solver.map(|n| n as u32));
    assert!(
        alive
            .iter()
            .any(|before| second.iter().any(|after| after.id == before.id
                && after.position_radius[0] > before.position_radius[0] + 1e-6)),
        "the first inflow must move on the second step"
    );
    // Native RK3 on the second step's published field, with no density move.
    for before in &alive {
        let after = second
            .iter()
            .find(|p| p.id == before.id && p.position_radius[3] > 0.0)
            .unwrap();
        let q = std::array::from_fn(|a| f64::from((before.position_radius[a] - solver_min[a]) / H));
        let v1 = cpu_sample_on(q, &field, solver);
        let v2 = cpu_sample_on(
            std::array::from_fn(|a| q[a] + 0.5 * 0.125 / f64::from(H) * v1[a]),
            &field,
            solver,
        );
        let v3 = cpu_sample_on(
            std::array::from_fn(|a| q[a] + 0.75 * 0.125 / f64::from(H) * v2[a]),
            &field,
            solver,
        );
        for a in 0..3 {
            let expected = f64::from(before.position_radius[a])
                + 0.125 * (2.0 / 9.0 * v1[a] + 3.0 / 9.0 * v2[a] + 4.0 / 9.0 * v3[a]);
            close(
                after.position_radius[a],
                expected,
                1.0,
                "second-step inflow RK3",
            );
        }
    }
}
