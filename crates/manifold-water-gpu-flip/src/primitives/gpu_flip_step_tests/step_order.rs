//! Step order proofs: extension, the cell cap and inflow timing.

use super::super::gpu_flip_step::GpuFlipStep;
use super::*;
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::effect_node::ParamValues;

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
            let mut pass = Pass::new();
            pass.bind(3, &got, Layout::Records);
            pass.allocated_tiles(&lattice());
            got = pass.run_list("extend_faces", &lattice(), 4, face_len());
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
            .run_list("constrain_solid_faces", &lattice(), 20, face_len());
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
        LIQUID_PARTICLE_READ, ParticleSorter, SortCount, SortJob, SortLabels,
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
        .bind_ranges(&ranges)
        .bind(38, &sorted, Layout::Dense)
        .run_list("remove_crowded_markers", &lattice(), 38, 300);
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
                count: SortCount::Slots(300),
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

/// Pool slots of the inflow fixture.
const INFLOW_POOL: usize = 512;

/// The step node over the small box with one inflow region, and the birth
/// identity as a reset seeds it: next id, epoch, reserved base, full-reset
/// request, then the dispatch triple and slot count the step publishes.
struct InflowStep {
    h: Harness,
    node: GpuFlipStep,
    inputs: [(&'static str, Slot); 7],
    outputs: [(&'static str, Slot); 4],
    params: ParamValues,
    particles: GpuBuffer,
    identity: GpuBuffer,
    faces: Slot,
    tiles: Slot,
}

impl InflowStep {
    /// An inflow box of `cells` whole cells from `lo`, moving at 0.25 m/s in x.
    fn new(lo: [usize; 3], cells: [u32; 3]) -> Self {
        use manifold_node_engine::primitive::Primitive;
        let mut h = Harness::new();
        let mut node = GpuFlipStep::new();
        node.prepare_pipelines(&h.device);
        let particles = h.array::<FluidParticle>(&[], INFLOW_POOL);
        let capped = h.array::<u32>(
            &[],
            2 * INFLOW_POOL
                + manifold_water_liquid::primitives::liquid_stats::SOLVER_WORDS as usize,
        );
        let faces = h.array::<FaceSample>(&[], 1);
        let tiles = h.array::<u32>(&[], 1);
        let (row, shape, atlas) = region_box(NODE_MIN, 2.0, lo, cells, [0.25, 0.0, 0.0]);
        let regions = h.array(&[row], 1);
        // The inflow's own sites. NODE_MIN is on a world cell's centre, so a
        // box cell whose low side is m cells from the origin covers the half
        // cells 2m + 1 and 2m + 2.
        let first =
            std::array::from_fn(|a| 2 * ((NODE_MIN[a] / H).round() as i32 + lo[a] as i32) + 1);
        let sites = site_box(first, cells.map(|n| 2 * n), 0);
        let site_total = sites[7] as f32;
        let sites = h.array(&sites, 8);
        let shapes = h.array(&[shape], 1);
        let atlas = h.array(&atlas, atlas.len());
        let bodies = h.array::<LiquidBody>(&[], 1);
        let params = params(
            &[
                &node_lattice()[..],
                &[
                    ("gravity_y", 0.0),
                    ("interval_duration", 0.125),
                    ("volume_projection", 0.0),
                    ("region_count", 1.0),
                    ("region_site_total", site_total),
                    ("iterations", 0.0),
                    ("flip", 1.0),
                ],
            ]
            .concat(),
        );
        let identity = h.array::<u32>(&[1, 1, 0, 0, 0, 1, 1, 0], 8);
        let inputs = [
            ("particles", particles.0),
            ("regions", regions.0),
            ("region_sites", sites.0),
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
        Self {
            h,
            node,
            inputs,
            outputs,
            params,
            particles: particles.1,
            identity: identity.1,
            faces: faces.0,
            tiles: tiles.0,
        }
    }

    /// One tick of the step. The fixture's one region row is this tick's.
    fn tick(&mut self, tick: i32) {
        for name in ["tick_index", "first_tick"] {
            self.params
                .insert(name.into(), ParamValue::Float(tick as f32));
        }
        let (_, errors) = self
            .h
            .run(&mut self.node, &self.inputs, &self.outputs, &self.params);
        assert!(errors.is_empty(), "{errors:?}");
    }
}

#[test]
pub(super) fn gpu_flip_step_order_inflow_waits_until_next_step() {
    let capacity = INFLOW_POOL;
    let mut rig = InflowStep::new([2, 2, 1], [1, 1, 1]);
    // The step solves on the native grid: three more cells, its origin 1.5
    // cells outside the box.
    let solver = N.map(|n| n + 3);
    let solver_min: [f32; 3] = std::array::from_fn(|a| NODE_MIN[a] - 1.5 * H);
    rig.tick(0);
    let first: Vec<FluidParticle> = read(&rig.particles, capacity);
    let alive: Vec<_> = first
        .iter()
        .filter(|p| p.position_radius[3] > 0.0)
        .copied()
        .collect();
    assert_eq!(alive.len(), 8);
    for marker in &alive {
        assert_eq!(marker.velocity, [0.25, 0.0, 0.0]);
        let q = (marker.position_radius[0] - NODE_MIN[0]) / H;
        assert!(q == 2.25 || q == 2.75, "fresh inflow moved: {q}");
    }
    rig.tick(1);
    let second: Vec<FluidParticle> = read(&rig.particles, capacity);
    let field = step_faces(
        &rig.h,
        rig.faces,
        rig.tiles,
        box_origin(solver_min, H),
        solver.map(|n| n as u32),
    );
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

/// The slots in use follow the step: after every tick the identity holds
/// `[groups, 1, 1, n]` with `n` the live particles (the count the stats'
/// live word reports), the particles packed in slots `[0, n)` and nothing past
/// them. The inflow fills a box of cells on the first tick and the sorts
/// compact the pool on every one.
#[test]
pub(super) fn gpu_flip_step_publishes_the_slots_in_use_every_tick() {
    let mut rig = InflowStep::new([1, 1, 1], [3, 2, 2]);
    for tick in 0..3 {
        rig.tick(tick);
        let particles: Vec<FluidParticle> = read(&rig.particles, INFLOW_POOL);
        let live = particles
            .iter()
            .filter(|p| p.position_radius[3] > 0.0)
            .count();
        assert!(live > 8, "tick {tick}: the inflow fills its box: {live}");
        let record: Vec<u32> = read(&rig.identity, 8);
        assert_eq!(
            record[4..],
            [live.div_ceil(256).max(1) as u32, 1, 1, live as u32],
            "tick {tick}: the record"
        );
        assert!(
            particles[..live].iter().all(|p| p.position_radius[3] > 0.0),
            "tick {tick}: slots [0, n) are live"
        );
        assert!(
            particles[live..]
                .iter()
                .all(|p| p.position_radius[3] <= 0.0),
            "tick {tick}: nothing lives past n"
        );
    }
}
