//! The warm start's carry (docs/FFT_WATER_SOLVER_DESIGN.md P3): every solve
//! starts from the last one's collar sources, carried across frames by
//! node.field_feedback. The carry is zero on the first frame, after a
//! restart and after a relaunch, so the same start gives the same bits: an
//! export is reproducible with the carry on. All at 64³, after
//! `fft_water_scenes_cover_every_dispatch` proved the warm scenes' arrays.

use serde_json::{Value, json};

use super::swash_preset::{WaterScene, water_def};
use super::swash_scene_tests::Run;

const FRAMES: usize = 12;

fn scene() -> WaterScene {
    WaterScene::dam_break(64).with_warm()
}

/// FNV-1a over the particles' bytes: one frame's state in a word.
fn fingerprint(run: &Run) -> u64 {
    let particles = run.particles();
    bytemuck::cast_slice::<_, u8>(&particles)
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn frames(run: &mut Run, count: usize) -> Vec<u64> {
    (0..count)
        .map(|_| {
            run.frame();
            fingerprint(run)
        })
        .collect()
}

/// The warm Dam Break with a `node.value` named `reset` wired into the
/// reset of each node in `targets`, as the generator's trigger count is in
/// the app.
fn with_reset(targets: &[&str]) -> Run {
    let scene = scene();
    let mut def = serde_json::to_value(water_def(scene)).expect("water def serialises");
    let nodes = def["nodes"].as_array().expect("nodes");
    let id = nodes.iter().filter_map(|n| n["id"].as_u64()).max().expect("nodes") + 1;
    let wires: Vec<Value> = nodes
        .iter()
        .filter(|n| targets.iter().any(|name| n["nodeId"] == *name))
        .map(|n| json!({"fromNode": id, "fromPort": "out", "toNode": n["id"], "toPort": "reset_trigger"}))
        .collect();
    assert_eq!(wires.len(), targets.len(), "every reset target is in the graph");
    def["nodes"].as_array_mut().expect("nodes").push(json!({
        "id": id, "nodeId": "reset", "typeId": "node.value", "params": {"value": {"type": "Float", "value": 0.0}}
    }));
    def["wires"].as_array_mut().expect("wires").extend(wires);
    Run::from_def(scene, serde_json::from_value(def).expect("def with reset"))
}

/// Two runs from the same start agree bit for bit on every frame, so an
/// export with the carry on is reproducible; and the carry changes the bits,
/// so the checks below would see a stale one.
#[test]
fn fft_water_warm_start_is_bit_deterministic() {
    let first = frames(&mut Run::new(scene()), FRAMES);
    let second = frames(&mut Run::new(scene()), FRAMES);
    assert_eq!(first, second, "two warm runs from the same start differ");
    let cold = frames(&mut Run::new(WaterScene::dam_break(64)), FRAMES);
    assert_ne!(first[0], cold[0], "the carry must reach the particles for the reset checks to mean anything");
}

/// A restart (the state store cleared, as seek, project load and restart
/// do) zeroes the carry with the particles: the frames after it are the
/// fresh run's frames, bit for bit.
#[test]
fn fft_water_warm_start_restart_zeroes_the_carry() {
    let fresh = frames(&mut Run::new(scene()), FRAMES);
    let mut run = Run::new(scene());
    frames(&mut run, 7);
    run.restart();
    assert_eq!(frames(&mut run, FRAMES), fresh, "a restarted warm run is not the fresh run");
}

/// A relaunch (the reset trigger's value changes) zeroes the carry with the
/// particles; resetting the particles alone leaves a stale carry, which the
/// frames show.
#[test]
fn fft_water_warm_start_relaunch_zeroes_the_carry() {
    let fresh = frames(&mut Run::new(scene()), FRAMES);
    let mut run = with_reset(&["state", "pressure_carry", "density_carry"]);
    // The first frame arms the trigger; a change after it fires.
    assert_eq!(frames(&mut run, 7)[0], fresh[0], "the armed first frame is the fresh one");
    run.set_value("reset", 1.0);
    assert_eq!(frames(&mut run, FRAMES), fresh, "a relaunched warm run is not the fresh run");

    let mut stale = with_reset(&["state"]);
    frames(&mut stale, 7);
    stale.set_value("reset", 1.0);
    assert_ne!(frames(&mut stale, 1)[0], fresh[0], "a stale carry left no trace, so the check above proves nothing");
}
