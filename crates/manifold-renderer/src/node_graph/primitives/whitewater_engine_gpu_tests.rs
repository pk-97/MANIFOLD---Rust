//! BUG-g75v.7: engine value and fusion proofs, 8³ CPU-proven extents only.
use super::liquid_surface_tests::{params, read, Harness};
use super::whitewater_emitter_gpu_tests::{fused, member};
use super::whitewater_grid_tests::run;
use super::{
    advect_whitewater::AdvectWhitewater, offset_lattice::OffsetLattice,
    upwind_distance::UpwindDistance,
};
use crate::node_graph::{
    effect_node::NodeInstanceId, freeze::codegen::InputSource, whitewater::WhitewaterParticle,
};

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
    let mut stage = super::whitewater_distance::SurfaceDistance::default();
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
    use super::whitewater_pool_cpu::{self as cpu, Advect, Fields};
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
    use crate::node_graph::{
        fluid_particles::CellRange,
        liquid::bodies::{pack_distance_atlas, LiquidBody, LiquidShape},
    };
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
    use crate::node_graph::{fluid_particles::FaceSample, liquid::substep_history::SubstepHistory};
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
    use crate::node_graph::{fluid_particles::FaceSample, liquid::substep_history::SubstepHistory};
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
