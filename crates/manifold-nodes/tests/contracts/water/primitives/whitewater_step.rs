mod fused_tests {
use manifold_node_engine::exec::effect_node::{EffectNodeContext,ParamValues};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::water::primitives::whitewater_step::WhitewaterStep;
#[test]
fn whitewater_unpack_extent_matches_adapter_storage() {
    use manifold_node_engine::water::primitives::gpu_flip_preset::{render_def, with_whitewater_axes, WaterScene};
    use manifold_node_engine::water::liquid::extent::check_preset_extents;
    let packed = render_def(WaterScene::dam_break(64));
    let axes = with_whitewater_axes(packed.clone());
    assert_eq!(check_preset_extents(&packed, 64).unwrap().scene_bytes,
        check_preset_extents(&axes, 64).unwrap().scene_bytes,
        "the stage holds exactly the three adapter arrays it replaces");
}

fn face_refusal(packed: bool, axes: [bool; 3], tick: bool, phrase: &str) {
    use manifold_node_engine::{exec::effect_node::FrameTime, exec::backend::MockBackend, bindings::NodeInputs, bindings::NodeOutputs, bindings::Slot};
    use manifold_node_engine::water::primitives::gpu_flip_preset::{render_def, WaterScene};
    use manifold_node_engine::water::primitives::gpu_flip_preset::with_whitewater_axes;
    use manifold_node_engine::exec::extent::{ExtentError};
use manifold_node_engine::water::liquid::extent::{check_preset_extents};
    let backend = MockBackend::new();
    let mut inputs = Vec::new();
    if tick { inputs.push(("distance", Slot(0))); }
    if packed { inputs.push(("faces", Slot(1))); }
    for (axis, port) in ["face_u", "face_v", "face_w"].into_iter().enumerate() {
        if axes[axis] { inputs.push((port, Slot(axis as u32 + 2))); }
    }
    let (mut scalars, mut cameras, mut lights, mut materials, mut transforms, mut atmospheres, mut modes, mut objects) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let outputs = NodeOutputs::new(&[], &backend, &mut scalars, &mut cameras, &mut lights,
        &mut materials, &mut transforms, &mut atmospheres, &mut modes, &mut objects);
    let params = ParamValues::default();
    let mut errors = Vec::new();
    let time = FrameTime { beats: manifold_core::Beats(0.0), seconds: manifold_core::Seconds(0.0), delta: manifold_core::Seconds(0.0), frame_count: 0 };
    let mut ctx = EffectNodeContext::new(time, &params, NodeInputs::new(&inputs, &backend, &[]), outputs, None)
        .with_errors(&mut errors);
    WhitewaterStep::new().run(&mut ctx);
    assert!(!ctx.gpu_accessed, "refusal must precede any GPU access");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains(phrase), "{:?}", errors);

    let mut def = with_whitewater_axes(render_def(WaterScene::dam_break(16)));
    let whitewater = def.nodes.iter().find(|n| n.node_id.as_str() == "whitewater").unwrap().id;
    let step = def.nodes.iter().find(|n| n.node_id.as_str() == "step").unwrap().id;
    def.wires.retain(|w| {
        if w.to_node != whitewater { return true; }
        if !tick && w.to_port == "distance" { return false; }
        ["face_u", "face_v", "face_w"].iter().position(|p| *p == w.to_port).is_none_or(|axis| axes[axis])
    });
    // An unused adapter would itself be an illegal reader outside the
    // liquid region, masking the whitewater refusal under test.
    let unused: Vec<_> = def.nodes.iter().filter(|n| ["whitewater_face_u", "whitewater_face_v", "whitewater_face_w"].iter()
        .position(|name| *name == n.node_id.as_str()).is_some_and(|axis| !axes[axis])).map(|n| n.id).collect();
    def.nodes.retain(|n| !unused.contains(&n.id));
    def.wires.retain(|w| !unused.contains(&w.from_node) && !unused.contains(&w.to_node));
    if packed {
        def.wires.push(serde_json::from_value(serde_json::json!({"fromNode": step, "fromPort": "faces", "toNode": whitewater, "toPort": "faces"})).unwrap());
    }
    match check_preset_extents(&def, 16) {
        Err(ExtentError::Refused { node, reason }) => {
            assert!(node.contains("whitewater_step"), "{node}");
            assert_eq!(reason, errors[0]);
        }
        other => panic!("expected {phrase} through extent rule, got {other:?}"),
    }
}

#[test]
fn whitewater_refuses_two_face_sources() {
    for mask in 1..8 { face_refusal(true, std::array::from_fn(|a| mask & (1 << a) != 0), true, "not both"); }
}

#[test]
fn whitewater_refuses_no_face_source() {
    for tick in [false, true] { face_refusal(false, [false; 3], tick, "neither"); }
}

#[test]
fn whitewater_refuses_partial_axes() {
    for mask in 1..7 {
        let axes = std::array::from_fn(|a| mask & (1 << a) != 0);
        for tick in [false, true] { face_refusal(false, axes, tick, "partial axes"); }
        face_refusal(true, axes, true, "not both");
    }
}

#[test]
fn whitewater_legacy_refuses_packed_faces() {
    face_refusal(true, [false; 3], false, "legacy level-set interface");
}

#[cfg(feature = "gpu-proofs")]
mod gpu {
use manifold_node_engine::scene::transform::Transform;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::water::primitives::whitewater_step::fused_tests::synthetic_shape;
use manifold_node_engine::water::liquid::fields::FieldBinding;
use manifold_node_engine::water::whitewater::WhitewaterParticle;
use manifold_node_engine::water::primitives::whitewater_step::*;
use manifold_node_engine::water::primitives::whitewater_step::fused_tests::gpu::*;
use manifold_node_engine::testkit::array_harness::read;
use manifold_node_engine::testkit::whitewater_scene::{Show, whitewater_render_def, with_tick_probe};
use manifold_node_engine::water::primitives::gpu_flip_preset::{WaterScene, with_whitewater_axes};
use manifold_node_engine::water::liquid::grid::face_len;
use manifold_node_engine::water::whitewater::face_offset;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_gpu::GpuBuffer;
const PORTS: [&str; 6] = ["proof_sampled", "proof_unscaled", "proof_energy", "proof_counts", "proof_dust_energy", "proof_dust_counts"];
fn scene(dust: bool, particle_ports: &[&str]) {
        for packed in [false, true] {
            scene_variant(dust, particle_ports, packed);
        }
    }
fn scene_variant(dust: bool, particle_ports: &[&str], packed: bool) {
        let def = if dust { crate::contracts::water::primitives::whitewater_golden_tests::all_emitters(None) }
            else { with_tick_probe(whitewater_render_def(WaterScene::dam_break(64))) };
        let axes = with_whitewater_axes(def.clone());
        let mut fused = Show::new_with_emitter_oracle(if packed { def } else { axes.clone() }, (96, 54), false, &[], Some(false));
        let mut reference = Show::new_with_emitter_oracle(axes, (96, 54), false, &[], Some(true));
        fused.restart();
        reference.restart();
        const TICKS: usize = 120;
        let mut ticks = 0;
        let mut saw_counts = false;
        let mut saw_dust_counts = false;
        for _ in 0..2 * TICKS {
            fused.frame(false);
            reference.frame(false);
            assert!(fused.errors().is_empty());
            assert!(reference.errors().is_empty());
            let [due] = fused.probes(["ticks"]);
            assert_eq!([due], reference.probes(["ticks"]));
            assert!(due == 0.0 || due == 1.0);
            if due == 0.0 { continue; }
            ticks += 1;
            let held_face_bytes = fused.provided_bytes("state", "faces");
            if ticks == 1 { println!("packed={packed} dust={dust}: liquid_state held faces = {held_face_bytes} bytes"); }
            assert_eq!(held_face_bytes, 0, "held face grid must be unallocated");
            for port in PORTS[..if dust { 6 } else { 4 }].iter().chain([&"proof_turbulence"]) {
                if !dust && *port == "proof_unscaled" { continue; }
                let a = fused.provided_all_bytes("whitewater", port);
                let b = reference.provided_all_bytes("whitewater", port);
                equal_words(bytemuck::cast_slice(&a), bytemuck::cast_slice(&b), ticks, port);
                let nonzero = bytemuck::cast_slice::<_, u32>(&a).iter().any(|&word| word != 0);
                if *port == "proof_counts" { saw_counts |= nonzero; }
                if *port == "proof_dust_counts" { saw_dust_counts |= nonzero; }
            }
            if !particle_ports.is_empty() {
                let shared = ["pool_out", "state_out", "counts_out", "foam_particles", "bubble_particles", "spray_particles", "dust_particles"];
                for port in particle_ports.iter().chain(&shared) {
                    let a = fused.provided_all_bytes("whitewater", port);
                    let b = reference.provided_all_bytes("whitewater", port);
                    equal_words(bytemuck::cast_slice(&a), bytemuck::cast_slice(&b), ticks, port);
                }
            }
            if ticks == TICKS { break; }
        }
        assert_eq!(ticks, TICKS, "scene did not exercise enough accepted ticks");
        assert!(saw_counts, "scene never produced nonzero normal emission counts");
        assert!(!dust || saw_dust_counts, "scene never produced nonzero dust emission counts");
    }
#[test]
    fn whitewater_fused_turbulence_matches_reference() {
        scene(false, &["proof_turbulence"]);
    }
#[test]
    fn whitewater_fused_emit_matches_reference() {
        scene(false, &[]);
        synthetic(false);
    }
#[test]
    fn whitewater_fused_dust_matches_reference() {
        scene(true, &[]);
        synthetic(true);
    }
#[test]
    fn whitewater_unpacked_faces_match_adapters() {
        let device = manifold_gpu::testkit::test_device();
        let mut show = Show::new_with_emitter_oracle(with_tick_probe(whitewater_render_def(WaterScene::dam_break(64))),
            (96, 54), false, &[], Some(false));
        show.restart();
        let mut ticks = 0;
        for _ in 0..8 {
            show.frame(false);
            assert!(show.errors().is_empty());
            if show.probes(["ticks"])[0] == 0.0 { continue; }
            let bytes = show.provided_all_bytes("step", "faces");
            let packed = shared(&device, &bytes);
            // Resolution 64 has 67 solver cells after native lattice padding.
            let axes = adapter_outputs(&device, &packed, [67; 3]);
            let mut saw_nonzero_axis = false;
            for (axis, port) in ["proof_unpack_u", "proof_unpack_v", "proof_unpack_w"].into_iter().enumerate() {
                let actual = show.provided_all_bytes("whitewater", port);
                let words: &[u32] = bytemuck::cast_slice(&actual);
                equal_words(words, &read::<u32>(&axes[axis], axes[axis].size as usize / 4), ticks, port);
                saw_nonzero_axis |= words[..face_len([67; 3], axis) as usize].iter().any(|&word| word != 0);
            }
            assert!(saw_nonzero_axis, "scene tick {ticks}: unpack comparison must contain a nonzero logical face");
            ticks += 1;
            if ticks == 3 { break; }
        }
        assert_eq!(ticks, 3);

        let shape = synthetic_shape();
        assert_eq!(face_offset(shape.nodes, shape.face_cells).unwrap(), [2; 3]);
        let packed = shared(&device, &mixed_faces());
        let axes = adapter_outputs(&device, &packed, shape.face_cells);
        let [mut stage, _reference] = particle_stages(&device, shape, 8);
        assert!(stage.unpacked_faces_for_test().is_none(), "axes allocate no unpack arrays");
        stage.reserve_faces(&device, true).unwrap();
        let mut enc = device.create_encoder("whitewater unpack mixed-weight fixture");
        stage.unpack_faces(&mut enc, &shape, FaceSource::Packed(&packed));
        let captured = stage.unpacked_faces_for_test().unwrap().each_ref().map(|src| copy_shared(&device, &mut enc, src));
        enc.commit_and_wait_completed();
        for axis in 0..3 {
            compare_buffers(&captured[axis], &axes[axis], 0, "whole unpacked axis including tail");
            let words = read::<u32>(&captured[axis], 9 * 9 * 9);
            assert!(words[..face_len(shape.face_cells, axis) as usize].iter().any(|&word| word != 0),
                "fixture axis {axis}: unpack comparison must contain a nonzero logical face");
            assert!(words[face_len(shape.face_cells, axis) as usize..].iter().all(|&word| word == 0));
        }
        stage.reserve_faces(&device, false).unwrap();
        assert!(stage.unpacked_faces_for_test().is_none());
        let resized = StepShape::new([15; 3], [15; 3], [10; 3], 1.0,
            Some(Transform { pos: [0.7; 3], scale: [1.4; 3], ..Default::default() }), 256).unwrap();
        stage.reserve_faces(&device, true).unwrap();
        stage.reserve(&device, resized, 8, true).unwrap();
        assert!(stage.unpacked_faces_for_test().is_none(), "shape changes drop the old arrays");
        stage.reserve_faces(&device, true).unwrap();
        assert!(stage.unpacked_faces_for_test().unwrap().iter().all(|axis| axis.size == 11 * 11 * 11 * 4));
    }
#[test]
    fn whitewater_packed_faces_match_axis_arrays() {
        crate::contracts::water::primitives::whitewater_golden_tests::packed_scene_fingerprints();
        let device = manifold_gpu::testkit::test_device();
        let fixture = Fixture::new(&device);
        let records: Vec<_> = fixture.records.iter().enumerate().map(|(i, p)| FluidParticle {
            position_radius: [3.0 / 7.0 + i as f32 / 113.0, 4.0 / 9.0, 5.0 / 11.0, p.position_radius[3]], ..*p
        }).collect();
        let particles = shared(&device, &records);
        let shape = fixture.shape;
        assert_eq!(face_offset(shape.nodes, shape.face_cells).unwrap(), [2; 3]);
        // Nonzero values behind invalid weights must be masked, including
        // negative weights. The fourth lanes and unused axis tails are poison.
        let samples = mixed_faces();
        let packed = shared(&device, &samples);
        let axes = adapter_outputs(&device, &packed, shape.face_cells);
        let schedule = shared(&device, &[1.0f32 / 128.0, 0.0, 0.0, 0.0, 1.0 / 128.0, 0.0, 0.0, 0.0]);
        let history: [GpuBuffer; 3] = std::array::from_fn(|axis| {
            let values: Vec<f32> = (0..2).flat_map(|step| std::iter::repeat_n(
                (step + 1) as f32 * [0.125, 0.0625, 0.03125][axis], face_len(shape.face_cells, axis) as usize)).collect();
            shared(&device, &values)
        });
        let mut pool = vec![empty_slot(); 256];
        for (i, p) in pool[..12].iter_mut().enumerate() {
            *p = WhitewaterParticle { position_lifetime: [3.0 / 7.0, 4.0 / 9.0, 5.0 / 11.0, 7.0], kind: (i % 5) as u32,
                id: i as u32, ..Default::default() };
        }
        let pool = shared(&device, &pool);
        let state = shared(&device, &[12u32, 12, 0, 0, 0, 0, 0, 0]);
        let mut stages = [Step::default(), Step::default(), Step::default()];
        for stage in &mut stages { stage.set_reference_capture_for_test(true); }
        stages[2].set_reference_enabled_for_test(true);
        for tick in 0..3 {
            let mut enc = device.create_encoder("whitewater packed faces, padded mixed weights and substep history");
            let mut captures = Vec::new();
            for (variant, stage) in stages.iter_mut().enumerate() {
                let mut inputs = fixture.inputs();
                inputs.particles = &particles;
                inputs.faces = if variant == 1 { FaceSource::Packed(&packed) } else { FaceSource::Axes(axes.each_ref()) };
                inputs.motion = Some(MotionInputs { schedule: &schedule, faces: history.each_ref(), count: 2,
                    fields: FieldBinding { nodes: [2; 3], spacing: 0.5, force_lattices: 0, impulse_tick: 0,
                        first_tick: 0, forces: None, impulses: None }, tick_index: tick as f32,
                    regions: None, shapes: None, atlas: None, region_count: 0 });
                let mut frame = fixture.frame(true);
                frame.ticks = 1;
                frame.epoch += tick;
                frame.preserve_foam = tick % 2 == 0;
                stage.advance_tick(&mut GpuEncoder::new(&mut enc, &device), &frame, &inputs, &pool, &state, true).unwrap();
                let mut row = snapshots(&device, &mut enc, stage);
                row.push(copy_shared(&device, &mut enc, stage.turbulence_for_test()));
                for src in stage.particle_snapshots_for_test().unwrap() { row.push(copy_shared(&device, &mut enc, src)); }
                for port in ["pool_out", "state_out", "counts_out", "foam_particles", "bubble_particles", "spray_particles", "dust_particles"] {
                    row.push(copy_shared(&device, &mut enc, stage.tick_output(port).unwrap()));
                }
                captures.push(row);
            }
            enc.commit_and_wait_completed();
            let names = ["proof_sampled", "proof_unscaled", "proof_energy", "proof_counts", "proof_dust_energy", "proof_dust_counts",
                "proof_turbulence", "proof_typed", "proof_dust_typed", "proof_lifecycle", "pool_out", "state_out", "counts_out",
                "foam_particles", "bubble_particles", "spray_particles", "dust_particles"];
            for variant in [1, 2] {
                for (i, name) in names.into_iter().enumerate() { compare_buffers(&captures[variant][i], &captures[0][i], tick as usize, name); }
            }
            assert!(read::<f32>(&captures[1][6], shape.cell_count() as usize).iter().any(|&v| v > 0.0), "mixed weights must exercise turbulence");
            let lifecycle = read::<WhitewaterParticle>(&captures[1][9], 256);
            assert!(lifecycle[1].velocity[0] > 0.0, "foam must read substep history beyond the stand-in length; bitwise values are checked against the axis oracle above");
        }
        for stage in &stages { assert_eq!(stage.turbulence_dispatches_for_test(), 3); }
    }
#[test]
    fn whitewater_fused_spawn_matches_reference() {
        scene(false, &["proof_typed"]);
        scene(true, &["proof_typed", "proof_dust_typed"]);
        spawn_boundaries();
        spawn_overflow();
    }
#[test]
    fn whitewater_fused_lifecycle_matches_reference() {
        scene(false, &["proof_lifecycle"]);
        scene(true, &["proof_lifecycle"]);
        lifecycle_history();
    }
}
}
