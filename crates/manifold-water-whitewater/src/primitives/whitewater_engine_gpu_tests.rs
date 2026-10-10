//! BUG-g75v.7: engine value and fusion proofs, 8³ CPU-proven extents only.
use manifold_node_engine::testkit::array_harness::{params, read, Harness};
use manifold_water_liquid::testkit::codegen::{fused, member};
use manifold_water_liquid::testkit::codegen::run;
use super::advect_whitewater::AdvectWhitewater;
use manifold_water_liquid::primitives::{offset_lattice::OffsetLattice, upwind_distance::UpwindDistance};
use {manifold_node_engine::exec::effect_node::NodeInstanceId, manifold_node_engine::freeze::codegen::InputSource, manifold_water_liquid::whitewater::WhitewaterParticle};
#[cfg(feature = "whitewater-oracle")]
use manifold_water_liquid::testkit::marker_phi::marker_phi;

#[test]
fn whitewater_engine_distance_values_and_fusion() {
    let mut h = Harness::new();
    let input: Vec<_> = (0..512).map(|i| 0.2 * (i % 8) as f32 - 0.7).collect();
    let valid = vec![1u32; 512];
    let a = h.array(&input, 512);
    let v = h.array(&valid, 512);
    let values = [
        ("nodes_x", 8.0),
        ("nodes_y", 8.0),
        ("nodes_z", 8.0),
        ("cell_size", 1.0),
        ("offset", 0.125),
    ];
    let got: Vec<f32> = run(
        &mut h,
        &mut UpwindDistance::new(),
        &[("levelset", a.0), ("valid", v.0)],
        512,
        &params(&values),
    );
    let want = super::whitewater_engine_cpu::upwind(&input, [8; 3], 1.0, &vec![true; 512]);
    for (a, b) in got.iter().zip(&want) {
        assert!((a - b).abs() < 2e-6, "GPU {a} CPU {b}");
    }
    let intermediate = h.array(&got, 512);
    let standalone: Vec<f32> = run(
        &mut h,
        &mut OffsetLattice::new(),
        &[("levelset", intermediate.0)],
        512,
        &params(&values),
    );
    let combined: Vec<f32> = fused(
        &mut h,
        vec![
            member::<UpwindDistance>(0, vec![InputSource::External(0), InputSource::External(1)]),
            member::<OffsetLattice>(1, vec![InputSource::Node(NodeInstanceId(0))]),
        ],
        &[&a.1, &v.1],
        512,
        &values,
    );
    assert_eq!(combined, standalone);
    let mut stage = manifold_water_liquid::primitives::whitewater_distance::SurfaceDistance::default();
    stage.prepare(&h.device);
    stage.reserve(&h.device, [8; 3]).unwrap();
    let mut corner = vec![8.0; 512];
    corner[0] = 0.1;
    for input in [input, corner, vec![-5.0; 512]] {
        let source = h.array(&input, 512);
        let mut enc = h.device.create_encoder("whitewater-engine-distance");
        let output = stage.encode(&mut enc, &source.1, 1.0).clone();
        let staged = h.array::<f32>(&[], 512);
        enc.copy_buffer_to_buffer(&output, &staged.1, 512 * 4);
        enc.commit_and_wait_completed();
        let got: Vec<f32> = read(&staged.1, 512);
        let want = super::whitewater_engine_cpu::surface_distance(&input, [8; 3], 1.0);
        for (a, b) in got.iter().zip(&want) {
            assert!((a - b).abs() < 2e-6, "stage {a} CPU {b}");
        }
    }
}

fn motion_values() -> Vec<(&'static str, f32)> {
    vec![
        ("center_x", 4.0),
        ("center_y", 4.0),
        ("center_z", 4.0),
        ("size_x", 8.0),
        ("size_y", 8.0),
        ("size_z", 8.0),
        ("nodes_x", 9.0),
        ("nodes_y", 9.0),
        ("nodes_z", 9.0),
        ("face_cells_x", 8.0),
        ("face_cells_y", 8.0),
        ("face_cells_z", 8.0),
        ("gravity_x", 0.0),
        ("gravity_y", -10.0),
        ("gravity_z", 0.0),
        ("dt", 0.04),
        ("substep_count", 3.0),
        ("field_nodes_x", 2.0),
        ("field_nodes_y", 2.0),
        ("field_nodes_z", 2.0),
        ("field_spacing", 8.0),
        ("force_lattices", 1.0),
    ]
}

#[test]
fn whitewater_engine_substep_force_hit_values_and_fusion() {
    use super::whitewater_particle_cpu::Box3;
    use {crate::primitives::whitewater_pool_cpu as cpu, super::whitewater_pool_cpu::Advect, super::whitewater_pool_cpu::Fields};
    let mut h = Harness::new();
    let pool: Vec<_> = [0, 1, 2, 4]
        .into_iter()
        .map(|kind| WhitewaterParticle {
            position_lifetime: [4.0, 4.0, 4.0, 1.0],
            velocity: [3.0, 0.0, 0.0],
            kind,
            id: 128,
            ..Default::default()
        })
        .collect();
    let p = h.array(&pool, 4);
    let solid = vec![10.0; 729];
    let s = h.array(&solid, 729);
    let old: [Vec<f32>; 3] = std::array::from_fn(|a| vec![if a == 0 { 1.0 } else { 0.0 }; 576]);
    let new: [Vec<f32>; 3] = std::array::from_fn(|a| vec![if a == 0 { 3.0 } else { 0.0 }; 576]);
    let current = new.each_ref().map(|f| h.array(f, 576));
    let history: [Vec<f32>; 3] = std::array::from_fn(|a| {
        old[a]
            .iter()
            .chain(&new[a])
            .chain(&new[a])
            .copied()
            .collect()
    });
    let histories = history.each_ref().map(|f| h.array(f, 1728));
    // Last slot is inactive, so neither its face values nor event may advance.
    let schedule = [
        0.01f32,
        0.01,
        0.0,
        0.0,
        0.03,
        0.04,
        f32::from_bits(0x80000000),
        0.0,
        0.0,
        0.04,
        f32::from_bits(0x80000000),
        0.0,
    ];
    let clock = h.array(&schedule, 12);
    let force: Vec<_> = (0..8).flat_map(|_| [1.0f32, 0.0, 0.0, 0.0]).collect();
    let impulse: Vec<_> = (0..8).flat_map(|_| [2.0f32, 0.0, 0.0, 0.0]).collect();
    let f = h.array(&force, 32);
    let i = h.array(&impulse, 32);
    let values = motion_values();
    let got: Vec<WhitewaterParticle> = run(
        &mut h,
        &mut AdvectWhitewater::new(),
        &[
            ("pool", p.0),
            ("face_u", current[0].0),
            ("face_v", current[1].0),
            ("face_w", current[2].0),
            ("solid", s.0),
            ("substep_schedule", clock.0),
            ("substep_u", histories[0].0),
            ("substep_v", histories[1].0),
            ("substep_w", histories[2].0),
            ("forces", f.0),
            ("impulses", i.0),
        ],
        4,
        &params(&values),
    );
    let grid = Box3 {
        cells: [8; 3],
        center: [4.0; 3],
        size: [8.0; 3],
    };
    for (index, particle) in pool.iter().enumerate() {
        let mut want = *particle;
        for (dt, faces, event) in [(0.01, &old, false), (0.03, &new, true)] {
            if event && want.kind != 1 {
                want.velocity[0] += 2.0;
            }
            let fields = Fields {
                faces: faces.each_ref().map(Vec::as_slice),
                face_cells: [8; 3],
                solid: &solid,
            };
            want = cpu::advect(
                want,
                &fields,
                &grid,
                Advect {
                    dt,
                    gravity: [1.0, -10.0, 0.0],
                    ..Advect::flip()
                },
                None,
            )
            .0;
        }
        for (a, b) in got[index]
            .position_lifetime
            .iter()
            .chain(&got[index].velocity)
            .zip(want.position_lifetime.iter().chain(&want.velocity))
        {
            assert!(
                (a - b).abs() < 2e-5,
                "kind {} GPU {:?} CPU {want:?}",
                particle.kind,
                got[index]
            );
        }
    }
    let combined: Vec<WhitewaterParticle> = fused(
        &mut h,
        vec![member::<AdvectWhitewater>(
            0,
            (0..11).map(InputSource::External).collect(),
        )],
        &[
            &p.1,
            &current[0].1,
            &current[1].1,
            &current[2].1,
            &s.1,
            &clock.1,
            &histories[0].1,
            &histories[1].1,
            &histories[2].1,
            &f.1,
            &i.1,
        ],
        4,
        &values,
    );
    assert_eq!(
        bytemuck::cast_slice::<_, u8>(&combined),
        bytemuck::cast_slice::<_, u8>(&got)
    );
}

#[test]
fn whitewater_engine_outflow_values_and_fusion() {
    use super::keep_whitewater::KeepWhitewater;
    use manifold_water_liquid::{fluid_particles::CellRange, bodies::pack_distance_atlas, bodies::LiquidBody, bodies::LiquidShape};
    let mut h = Harness::new();
    let pool: Vec<_> = [0, 1, 2, 4]
        .into_iter()
        .flat_map(|kind| {
            [3.5, 4.0, 4.5].map(|x| WhitewaterParticle {
                position_lifetime: [x, 4.0, 4.0, 1.0],
                kind,
                ..Default::default()
            })
        })
        .collect();
    let p = h.array(&pool, 12);
    let range = h.array(&[CellRange { start: 0, count: 0 }], 1);
    let order = h.array(&[0u32], 1);
    let solid = h.array(&vec![10.0f32; 729], 729);
    let body = LiquidBody {
        rotation: [0.0, 0.0, 0.0, 1.0],
        angular_velocity: [0.0, 0.0, 0.0, 3.0],
        ..Default::default()
    };
    let shape = LiquidShape {
        origin_spacing: [0.0, 0.0, 0.0, 8.0],
        dims_x: 2,
        dims_y: 2,
        dims_z: 2,
        scale_min: [1.0; 4],
        ..Default::default()
    };
    let regions = h.array(&[body], 1);
    let shapes = h.array(&[shape], 1);
    let mut atlas = Vec::new();
    pack_distance_atlas(&[-4.0, 4.0, -4.0, 4.0, -4.0, 4.0, -4.0, 4.0], &mut atlas);
    let a = h.array(&atlas, 4);
    let mut values = motion_values();
    values.extend([
        ("bins_x", 1.0),
        ("bins_y", 1.0),
        ("bins_z", 1.0),
        ("region_count", 1.0),
    ]);
    let got: Vec<u32> = run(
        &mut h,
        &mut KeepWhitewater::new(),
        &[
            ("pool", p.0),
            ("binned", p.0),
            ("cell_ranges", range.0),
            ("order", order.0),
            ("solid", solid.0),
            ("regions", regions.0),
            ("shapes", shapes.0),
            ("atlas", a.0),
        ],
        12,
        &params(&values),
    );
    // Engine removes d<0: surface exactly at zero survives, all four types.
    assert_eq!(got, [0, 1, 1].repeat(4));
    let combined: Vec<u32> = fused(
        &mut h,
        vec![member::<KeepWhitewater>(
            0,
            (0..8).map(InputSource::External).collect(),
        )],
        &[
            &p.1, &p.1, &range.1, &order.1, &solid.1, &regions.1, &shapes.1, &a.1,
        ],
        12,
        &values,
    );
    assert_eq!(combined, got);
}

#[test]
fn whitewater_engine_seam_publishes_accepted_substeps() {
    use manifold_water_liquid::{fluid_particles::FaceSample, substep_history::SubstepHistory};
    let mut h = Harness::new();
    let faces: Vec<_> = (0..729)
        .map(|i| FaceSample {
            velocity: [i as f32, (i + 1000) as f32, (i + 2000) as f32, 0.0],
            weight: [1.0; 4],
        })
        .collect();
    let source = h.array(&faces, 729);
    let plans = [
        [0.01f32.to_bits(), 0.01f32.to_bits(), 0, 0, 0, 0, 0, 0],
        [
            0.03f32.to_bits(),
            0.04f32.to_bits(),
            0,
            0,
            0,
            0,
            0,
            0x80000002,
        ],
    ];
    let plans = plans.map(|p| h.array(&p, 8));
    let mut history = SubstepHistory::default();
    history.prepare(&h.device);
    history.reserve(&h.device, [8; 3], 2).unwrap();
    let mut enc = h.device.create_encoder("whitewater-engine-seam");
    for (i, plan) in plans.iter().enumerate() {
        history.capture(&mut enc, i as u32, &plan.1, &source.1);
    }
    let staged = std::array::from_fn::<_, 3, _>(|_| h.array::<f32>(&[], 1152));
    for (source, target) in history.faces.as_ref().unwrap().iter().zip(&staged) {
        enc.copy_buffer_to_buffer(source, &target.1, 1152 * 4);
    }
    enc.commit_and_wait_completed();
    assert_eq!(
        read::<u32>(history.schedule.as_ref().unwrap(), 8),
        [
            0.01f32.to_bits(),
            0.01f32.to_bits(),
            0,
            0,
            0.03f32.to_bits(),
            0.04f32.to_bits(),
            0x80000002,
            0
        ]
    );
    for (axis, target) in staged.iter().enumerate() {
        let values = read::<f32>(&target.1, 1152);
        let mut dims = [8usize; 3];
        dims[axis] += 1;
        for (i, value) in values.iter().enumerate() {
            let f = i % 576;
            let c = [f % dims[0], f / dims[0] % dims[1], f / (dims[0] * dims[1])];
            let index = c[0] + 9 * (c[1] + 9 * c[2]);
            assert_eq!(*value, faces[index].velocity[axis]);
        }
    }
}

#[test]
fn whitewater_engine_seam_skips_inactive_grids_and_resumes_capture() {
    use manifold_water_liquid::{fluid_particles::FaceSample, substep_history::SubstepHistory};
    let mut h = Harness::new();
    let faces = |velocity| vec![FaceSample { velocity, weight: [1.0; 4] }; 729];
    let first = h.array(&faces([1.0, 2.0, 3.0, 0.0]), 729);
    let poison = h.array(&faces([f32::NAN; 4]), 729);
    let resumed = h.array(&faces([4.0, 5.0, 6.0, 0.0]), 729);
    let active = h.array(&[0.01f32.to_bits(), 0.01f32.to_bits(), 0, 0, 0, 0, 0, 0], 8);
    let inactive = h.array(&[0, 0.01f32.to_bits(), 0, 0, 0, 0, 0, 0x80000002], 8);
    let mut history = SubstepHistory::default();
    history.prepare(&h.device);
    history.reserve(&h.device, [8; 3], 2).unwrap();
    let held = std::array::from_fn::<_, 3, _>(|_| h.array::<f32>(&[], 1152));
    let final_grids = std::array::from_fn::<_, 3, _>(|_| h.array::<f32>(&[], 1152));
    let held_schedule = h.array::<u32>(&[], 8);
    let mut enc = h.device.create_encoder("whitewater-engine-inactive-history");
    history.capture(&mut enc, 0, &active.1, &first.1);
    // An inactive row must neither read this poisoned grid nor overwrite a
    // previous active capture. A never-active row starts with zero storage.
    history.capture(&mut enc, 0, &inactive.1, &poison.1);
    history.capture(&mut enc, 1, &inactive.1, &poison.1);
    for (source, target) in history.faces.as_ref().unwrap().iter().zip(&held) {
        enc.copy_buffer_to_buffer(source, &target.1, 1152 * 4);
    }
    enc.copy_buffer_to_buffer(history.schedule.as_ref().unwrap(), &held_schedule.1, 32);
    history.capture(&mut enc, 0, &active.1, &resumed.1);
    for (source, target) in history.faces.as_ref().unwrap().iter().zip(&final_grids) {
        enc.copy_buffer_to_buffer(source, &target.1, 1152 * 4);
    }
    enc.commit_and_wait_completed();
    assert_eq!(
        read::<u32>(&held_schedule.1, 8),
        [0, 0.01f32.to_bits(), 0x80000002, 0].repeat(2),
    );
    for axis in 0..3 {
        let held_values = read::<f32>(&held[axis].1, 1152);
        let final_values = read::<f32>(&final_grids[axis].1, 1152);
        assert_eq!(held_values[..576], vec![axis as f32 + 1.0; 576]);
        assert_eq!(final_values[..576], vec![axis as f32 + 4.0; 576]);
        assert_eq!(held_values[576..], vec![0.0; 576]);
        assert_eq!(final_values[576..], vec![0.0; 576]);
    }
}

/// Runs the surface distance stage on `phi` and returns its output.
fn surface_on_gpu(h: &mut Harness, stage: &manifold_water_liquid::primitives::whitewater_distance::SurfaceDistance, phi: &[f32], cell: f32, plan: Option<&manifold_gpu::GpuBuffer>) -> Vec<f32> {
    let n = phi.len();
    let source = h.array(phi, n);
    let mut enc = h.device.create_encoder("whitewater-surface-distance");
    let output = match plan {
        Some(plan) => stage.encode_gated(&mut enc, &source.1, cell, plan),
        None => stage.encode(&mut enc, &source.1, cell),
    }
    .clone();
    let staged = h.array::<f32>(&[], n);
    enc.copy_buffer_to_buffer(&output, &staged.1, (n * 4) as u64);
    enc.commit_and_wait_completed();
    read(&staged.1, n)
}

/// A clock plan (gpu_flip_step.wgsl ClockPlan): live, with `step_dt`.
fn live_plan(h: &mut Harness, step_dt: f32) -> manifold_gpu::GpuBuffer {
    let mut words = [0u32; 12];
    words[0] = step_dt.to_bits();
    words[11] = 1;
    h.array(&words, 12).1
}

/// Every scratch buffer of the stage except its sweep grid, as words.
fn scratch_words(h: &mut Harness, stage: &manifold_water_liquid::primitives::whitewater_distance::SurfaceDistance) -> Vec<Vec<u32>> {
    let buffers = stage.scratch();
    let mut enc = h.device.create_encoder("whitewater-surface-distance-scratch");
    let staged: Vec<_> = buffers[..5]
        .iter()
        .map(|b| {
            let words = (b.size() / 4) as usize;
            let s = h.array::<u32>(&[], words);
            enc.copy_buffer_to_buffer(b, &s.1, b.size());
            (s.1, words)
        })
        .collect();
    enc.commit_and_wait_completed();
    staged.iter().map(|(b, w)| read(b, *w)).collect()
}

/// A sphere of radius 1.6 m centred in a 24³ grid of 0.25 m cells.
fn sphere_phi(centre: f32) -> Vec<f32> {
    const N: usize = 24;
    (0..N * N * N)
        .map(|i| {
            let c = [i % N, i / N % N, i / (N * N)].map(|v| (v as f32 + 0.5) * 0.25 - centre);
            (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt() - 1.6
        })
        .collect()
}

/// The clock gate: a live active plan (and the always-active zero plan of
/// `encode`) give bit-identical output; an inactive slot leaves the output
/// and every field and state word untouched, rewriting only its sweep grid
/// to zero groups.
#[test]
fn whitewater_surface_distance_clock_gate() {
    let mut h = Harness::new();
    let mut stage = manifold_water_liquid::primitives::whitewater_distance::SurfaceDistance::default();
    stage.prepare(&h.device);
    stage.reserve(&h.device, [24; 3]).unwrap();
    let a = sphere_phi(3.0);
    let b = sphere_phi(2.5);
    let ungated = surface_on_gpu(&mut h, &stage, &a, 0.25, None);
    let active = live_plan(&mut h, 0.01);
    let gated = surface_on_gpu(&mut h, &stage, &a, 0.25, Some(&active));
    assert!(ungated.iter().zip(&gated).all(|(x, y)| x.to_bits() == y.to_bits()), "active slot differs from the zero plan");
    let want = super::whitewater_engine_cpu::surface_distance(&a, [24; 3], 0.25);
    for (g, c) in gated.iter().zip(&want) {
        assert!((g - c).abs() < 2e-6, "stage {g} CPU {c}");
    }
    let before = scratch_words(&mut h, &stage);
    let inactive = live_plan(&mut h, 0.0);
    let after_output = surface_on_gpu(&mut h, &stage, &b, 0.25, Some(&inactive));
    assert!(after_output.iter().zip(&gated).all(|(x, y)| x.to_bits() == y.to_bits()), "inactive slot changed the output");
    assert_eq!(scratch_words(&mut h, &stage), before, "inactive slot touched the scratch");
    let mut enc = h.device.create_encoder("whitewater-surface-distance-args");
    let args = h.array::<u32>(&[], 3);
    enc.copy_buffer_to_buffer(&stage.scratch()[5], &args.1, 12);
    enc.commit_and_wait_completed();
    assert_eq!(read::<u32>(&args.1, 3), vec![0, 1, 1], "inactive sweep grid");
    // And the next active slot computes from the new input again.
    let next = surface_on_gpu(&mut h, &stage, &b, 0.25, Some(&active));
    let want = super::whitewater_engine_cpu::surface_distance(&b, [24; 3], 0.25);
    for (g, c) in next.iter().zip(&want) {
        assert!((g - c).abs() < 2e-6, "stage {g} CPU {c}");
    }
}

/// SurfaceDistance against FLIP's own reinitialised surface on the same
/// input, on the splash's analytic sheets and on a field built from its
/// markers, then the sheeter's level-set decisions on the splash markers
/// under each surface.
#[cfg(feature = "whitewater-oracle")]
#[test]
fn whitewater_surface_distance_matches_engine_on_a_splash() {
    use manifold_fluids::{sheet_oracle, sheeter, whitewater_oracle};
    let mut h = Harness::new();
    let (markers, analytic, cells, dx) = sheeter::fixtures::splash();
    let cell = dx as f32;
    let mut stage = manifold_water_liquid::primitives::whitewater_distance::SurfaceDistance::default();
    stage.prepare(&h.device);
    stage.reserve(&h.device, cells).unwrap();
    for (name, input) in [("analytic", analytic), ("markers", marker_phi(&markers, cells, cell))] {
        let native = whitewater_oracle::curvature(&input, cells, dx).expect("oracle").surface_phi;
        let gpu = surface_on_gpu(&mut h, &stage, &input, cell, None);
        let mut errors: Vec<f32> = native.iter().zip(&gpu).map(|(a, b)| (a - b).abs() / cell).collect();
        errors.sort_by(f32::total_cmp);
        let max = errors[errors.len() - 1];
        let p99 = errors[errors.len() * 99 / 100];
        let trace = |phi: &[f32]| sheeter::trace_sheet_particles(&markers, phi, cells, dx, sheet_oracle::DEFAULT_FILL_THRESHOLD).expect("sheeter");
        let (ours, theirs) = (trace(&gpu), trace(&native));
        let flips = |a: &[bool], b: &[bool]| a.iter().zip(b).filter(|(x, y)| x != y).count();
        eprintln!(
            "SURFACE {name}: max {max:e} h, p99 {p99:e} h; thin flips {}, kept flips {}, candidates {} vs {}, seeds {} vs {}, candidate lists equal {}",
            flips(&ours.thin, &theirs.thin),
            flips(&ours.kept, &theirs.kept),
            ours.candidates.len(),
            theirs.candidates.len(),
            ours.seeds.len(),
            theirs.seeds.len(),
            ours.candidates == theirs.candidates,
        );
        // Both run the same upwind rule in f32 on fields within ±3h, where an
        // ulp is under 4e-7 h; differences are operation order and fused
        // multiply-adds, so ten ulps bounds them. Measured: under 1e-6 h.
        assert!(max < 4e-6, "{name}: surfaces differ by {max} h");
        assert_eq!(ours.thin, theirs.thin, "{name}: phase-1 sheet test flipped");
        assert_eq!(ours.kept, theirs.kept, "{name}: phase-2 band flipped");
        assert_eq!(ours.candidates, theirs.candidates, "{name}: candidate band flipped");
        assert_eq!(ours.seeds.len(), theirs.seeds.len(), "{name}: seed counts differ");
        assert!(!theirs.seeds.is_empty(), "{name}: the splash seeds");
    }
}

/// The gated stage under encode replay: a sequence of active and inactive
/// slots, with the plan's and the input's contents changing in the same
/// buffers between slots, recorded once and replayed. After every slot the
/// replayed stage's output and scratch equal a directly encoded twin's.
#[test]
fn whitewater_surface_distance_replay_matches_direct() {
    use manifold_gpu::GpuReplayCache;
    let device = manifold_gpu::testkit::test_device();
    let make = || {
        let mut stage = manifold_water_liquid::primitives::whitewater_distance::SurfaceDistance::default();
        stage.prepare(&device);
        stage.reserve(&device, [24; 3]).unwrap();
        let plan = device.create_buffer_shared(48);
        let source = device.create_buffer_shared(24 * 24 * 24 * 4);
        (stage, plan, source)
    };
    let (direct, direct_plan, direct_source) = make();
    let (replayed, replay_plan, replay_source) = make();
    let mut cache = Some(GpuReplayCache::default());
    let inputs = [sphere_phi(3.0), sphere_phi(2.5), sphere_phi(2.75)];
    let slots = [(0.01f32, 0), (0.0, 1), (0.01, 1), (0.01, 0), (0.0, 2), (0.0, 0), (0.01, 2), (0.01, 0)];
    let words = |stage: &manifold_water_liquid::primitives::whitewater_distance::SurfaceDistance| -> Vec<Vec<u32>> {
        let mut enc = device.create_encoder("surface replay readback");
        let staged: Vec<_> = stage.scratch()[..6]
            .iter()
            .map(|b| {
                let s = device.create_buffer_shared(b.size());
                enc.copy_buffer_to_buffer(b, &s, b.size());
                (s, (b.size() / 4) as usize)
            })
            .collect();
        enc.commit_and_wait_completed();
        staged.iter().map(|(s, n)| read::<u32>(s, *n)).collect()
    };
    for (slot, &(step_dt, input)) in slots.iter().enumerate() {
        let mut plan = [0u32; 12];
        plan[0] = step_dt.to_bits();
        plan[11] = 1;
        for (p, s) in [(&direct_plan, &direct_source), (&replay_plan, &replay_source)] {
            // SAFETY: shared buffers sized for the writes; no GPU work in flight.
            unsafe {
                p.write(0, bytemuck::cast_slice(&plan));
                s.write(0, bytemuck::cast_slice(&inputs[input]));
            }
        }
        let mut enc = device.create_encoder("surface direct");
        direct.encode_gated(&mut enc, &direct_source, 0.25, &direct_plan);
        enc.commit_and_wait_completed();
        let mut enc = device.create_encoder("surface replay");
        enc.begin_replay(&device, cache.take().expect("cache"));
        replayed.encode_gated(&mut enc, &replay_source, 0.25, &replay_plan);
        cache = Some(enc.end_replay());
        enc.commit_and_wait_completed();
        assert!(words(&direct) == words(&replayed), "slot {slot} (step {step_dt}, input {input}): replay differs from direct");
    }
    let stats = cache.expect("cache").stats();
    assert!(stats.replayed > 0, "nothing replayed: {stats:?}");
    eprintln!("SURFACE replay: {stats:?}");
}
