//! Per-tick whitewater fingerprints on the shipped GPU FLIP Dam Break
//! (docs/WHITEWATER_STAGE_FUSION_DESIGN.md P0, invariant I1). Every
//! simulation tick's `pool_out`, `state_out`, `counts_out` and four
//! populations are hashed whole and held to a golden recorded before any
//! stage change. This module performs a read-only comparison against the
//! shipped golden. The historical recorder was retired after its pinned
//! pre-T1 source path was found to be unavailable.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_gpu::GpuBuffer;
use serde_json::{Value, json};

use manifold_water_whitewater::primitives::energy_potential::{MAX_ENERGY, MIN_ENERGY};
use manifold_nodes_water::presets::gpu_flip::WaterScene;
use manifold_node_engine::testkit::array_harness::{Harness, read};
use manifold_nodes_water::testkit::whitewater_scene::{whitewater_render_def, with_tick_probe};
use manifold_water_whitewater::primitives::whitewater_step::{Step, StepFrame, StepInputs, StepShape};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_physics::clock::TICK;
use manifold_node_engine::particles::FluidParticle;
use manifold_water_liquid::grid::face_len;
use manifold_node_engine::scene::transform::Transform;
use manifold_water_liquid::whitewater::{WHITEWATER_EMPTY, WhitewaterParticle};

const GOLDEN: &str = "whitewater_tick_golden.txt";





use manifold_nodes_water::testkit::conformance::json_node_mut;

/// The shipped def with these whitewater params and, when given, this
/// whitewater budget (the card's node, which sizes stage and boundary alike).
fn variant(params: &[(&str, Value)], budget: Option<f64>) -> EffectGraphDef {
    let mut def = serde_json::to_value(whitewater_render_def(WaterScene::dam_break(64))).expect("def serialises");
    let node = json_node_mut(&mut def, WHITEWATER).expect("whitewater node in Water group");
    assert_eq!(node["typeId"], "node.whitewater_step");
    for (name, value) in params {
        node["params"][*name] = value.clone();
    }
    if let Some(budget) = budget {
        let budget_node = json_node_mut(&mut def, "whitewater_budget").expect("whitewater budget in Water group");
        budget_node["params"]["value"] = json!({"type": "Float", "value": budget});
    }
    with_tick_probe(serde_json::from_value(def).expect("variant def"))
}

pub(super) fn all_emitters(budget: Option<f64>) -> EffectGraphDef {
    let on = json!({"type": "Bool", "value": true});
    let float = |v: f64| json!({"type": "Float", "value": v});
    variant(
        &[
            ("dust_emission", on.clone()),
            ("boundary_dust", on.clone()),
            ("inside_emission", on.clone()),
            ("preserve_foam", on),
            ("generation_rate", float(0.5)),
            ("spray_speed", float(2.0)),
        ],
        budget,
    )
}



/// I1: the packed face grid and the axis adapters give the same tick
/// fingerprints. 32 cells: packing works face by face, whatever the lattice.
pub(super) fn packed_scene_fingerprints() {
    use manifold_nodes_water::presets::gpu_flip::with_whitewater_axes;
    let def = with_tick_probe(whitewater_render_def(WaterScene::dam_break(32)));
    let mut packed = Vec::new();
    let mut axes = Vec::new();
    run("packed_faces", def.clone(), &mut packed);
    run("packed_faces", with_whitewater_axes(def), &mut axes);
    assert_eq!(packed, axes, "I1 fingerprints: packed grid versus real axis adapters");
}



/// One tick of the stage over a constructed pool with no emission: three
/// particles that die this tick ahead of one that lives. The stage's sort
/// writes only an index (`order`, `sorted: None`), never the pool, so pool
/// order reaches the keep and compact passes unchanged. Compaction
/// must move the survivor from slot 3 to slot 0 (the compact pass's
/// `scan[i] - 1 != i` case). Returns the survivor's slot and the live count.
fn compaction_moves_a_survivor() -> (usize, u32) {
    let harness = Harness::new();
    let cells = [8u32; 3];
    let nodes = [13u32; 3];
    let shape = StepShape::new(nodes, nodes, cells, 1.0, Some(Transform { pos: [0.6; 3], scale: [1.2; 3], ..Transform::default() }), 256)
        .expect("shape");
    let shared = |bytes: &[u8]| {
        let b = harness.device.create_buffer_shared(bytes.len() as u64);
        // SAFETY: fresh shared storage, not submitted yet.
        unsafe { b.write(0, bytes) };
        b
    };
    let distance = shared(bytemuck::cast_slice(&vec![1.0f32; 512]));
    let solid = shared(bytemuck::cast_slice(&vec![10.0f32; 13 * 13 * 13]));
    let particles = shared(bytemuck::cast_slice(&vec![FluidParticle::default(); 256]));
    let faces: [GpuBuffer; 3] = std::array::from_fn(|a| shared(bytemuck::cast_slice(&vec![0.0f32; face_len(cells, a) as usize])));
    // Inside FLIP's boundary box (1.625 cells in), in air.
    let at = [0.825f32; 3];
    let particle = |lifetime: f32, id: u32| WhitewaterParticle {
        position_lifetime: [at[0], at[1], at[2], lifetime],
        kind: 2,
        id,
        ..Default::default()
    };
    let mut pool = vec![WhitewaterParticle { kind: WHITEWATER_EMPTY, ..Default::default() }; 256];
    pool[0] = particle(1.0e-4, 10);
    pool[1] = particle(1.0e-4, 11);
    pool[2] = particle(1.0e-4, 12);
    pool[3] = particle(5.0, 42);
    let pool = shared(bytemuck::cast_slice(&pool));
    let state = shared(bytemuck::cast_slice(&[4u32, 43, 0, 0, 0, 0, 0, 0]));
    let inputs = StepInputs {
        motion: None,
        obstacle_source: None,
        particles: &particles,
        solid: &solid,
        faces: manifold_water_whitewater::primitives::whitewater_step::FaceSource::Axes([&faces[0], &faces[1], &faces[2]]),
        level_set: &distance,
        distance: Some(&distance),
    };
    let settings = StepFrame {
        shape,
        count: Some(0),
        ticks: 1,
        dt: TICK as f32,
        epoch: 0,
        seed: 0.0,
        gravity: [0.0, -9.81, 0.0],
        wavecrest_emission: 0.0,
        turbulence_emission: 0.0,
        min_turbulence: 100.0,
        max_turbulence: 200.0,
        inside_emission: false,
        generation_rate: 0.0,
        spray_speed: 1.0,
        dust_emission: false,
        boundary_dust: false,
        dust_rate: 175.0,
        influence_base: 1.0,
        influence_decay: 2.0,
        min_energy: MIN_ENERGY,
        max_energy: MAX_ENERGY,
        preserve_foam: false,
    };
    let mut stage = Step::default();
    let out_pool = shared(&vec![0u8; pool.size as usize]);
    let out_state = shared(&[0u8; 32]);
    let mut native = harness.device.create_encoder("whitewater compaction case");
    stage.advance_tick(&mut GpuEncoder::new(&mut native, &harness.device), &settings, &inputs, &pool, &state, true).expect("tick");
    native.copy_buffer_to_buffer(stage.tick_output("pool_out").expect("pool_out"), &out_pool, out_pool.size);
    native.copy_buffer_to_buffer(stage.tick_output("state_out").expect("state_out"), &out_state, 32);
    native.commit_and_wait_completed();
    let after: Vec<WhitewaterParticle> = read(&out_pool, 256);
    let state: Vec<u32> = read(&out_state, 8);
    assert_eq!(state[3], 0, "the constructed case must not spawn: {state:?}");
    let slot = after.iter().position(|p| p.id == 42 && p.kind != WHITEWATER_EMPTY).expect("the survivor is kept");
    assert!(after[1..].iter().all(|p| p.kind == WHITEWATER_EMPTY), "only the survivor remains: {:?}", &after[..4]);
    (slot, state[0])
}

/// Every tick of the shipped Dam Break, the all-emitters fixture and the
/// all-emitters fixture at a 1000-particle budget (the overflow case), bit
/// for bit against the golden; and the constructed compaction case.
#[test]
fn whitewater_tick_state_matches_golden() {
    let (slot, live) = compaction_moves_a_survivor();
    assert_eq!((slot, live), (0, 1), "compaction moves the survivor from slot 3 to slot 0");
    println!("coverage constructed: compaction moved the survivor 3 -> {slot}");
    let mut lines = Vec::new();
    let fixtures = [
        ("shipped", variant(&[], None)),
        ("all_emitters", all_emitters(None)),
        ("all_emitters_budget_1000", all_emitters(Some(1000.0))),
    ];
    let mut coverage = Vec::new();
    for (label, def) in fixtures {
        coverage.push((label, run(label, def, &mut lines)));
    }
    for (label, seen) in &coverage {
        let row: Vec<String> = EVENTS.iter().zip(seen).map(|(e, t)| format!("{e} {}", t.map_or("-".into(), |t| t.to_string()))).collect();
        println!("coverage {label}: {}", row.join(", "));
    }
    for (i, event) in EVENTS.iter().enumerate() {
        assert!(coverage.iter().any(|(_, seen)| seen[i].is_some()), "no fixture reached {event} in {TICKS} ticks");
    }
    let path = format!("{}/tests/fixtures/{GOLDEN}", env!("CARGO_MANIFEST_DIR"));
    let golden = std::fs::read_to_string(&path).expect("golden fixture reads");
    let sha = golden.lines().find_map(|l| l.strip_prefix("# sha ")).expect("the golden names the commit it was recorded at");
    assert!(sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()), "the golden's sha line is malformed: {sha}");
    let expected: Vec<&str> = golden.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(expected.len(), lines.len(), "golden fingerprint count");
    let moved: Vec<String> =
        expected.iter().zip(&lines).filter(|(e, l)| **e != l.as_str()).map(|(e, l)| format!("want {e}\n got {l}")).collect();
    assert!(moved.is_empty(), "{} of {} fingerprints moved; first:\n{}", moved.len(), lines.len(), moved.iter().take(10).cloned().collect::<Vec<_>>().join("\n"));
}

use manifold_nodes_water::testkit::whitewater_fingerprints::*;
