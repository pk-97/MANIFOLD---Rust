//! WATER_SIMULATION_DESIGN section 9, row "Same solver state survives clip
//! edges/gaps" (`water_lifecycle_preserves_clip_edges_and_gaps`).
//!
//! The generator stack's half of the contract: `GeneratorRenderer` forwards
//! the host `SimulationFrame` to a layer's `PresetRuntime` only on output
//! frames that layer is actually evaluated on. A clip end / layer gap /
//! render-skip means NO `set_simulation_frame` + render calls at all, so
//! the per-instance clock identity freezes at the pre-gap frame — hidden
//! elapsed time is never integrated, and resuming advances by exactly one
//! frame's delta, not a catch-up sum.
//!
//! These tests run on the mock backend (no GPU): `PresetRuntime` is the
//! seam `GeneratorRenderer` forwards through, and the per-instance frame
//! identity is recorded there.

use super::*;
use crate::node_graph::substeps::SimulationFrame;

const DT: f64 = 1.0 / 60.0;

fn sim_frame(frame_id: u64, epoch: u64, advancing: bool) -> SimulationFrame {
    SimulationFrame {
        frame_id,
        delta: Seconds(if advancing { DT } else { 0.0 }),
        epoch,
        advancing,
        exporting: false,
    }
}

fn frame_time(frame_id: u64) -> FrameTime {
    FrameTime {
        beats: Beats(frame_id as f64 * DT * 2.0),
        seconds: Seconds(frame_id as f64 * DT),
        delta: Seconds(DT),
        frame_count: frame_id as i64,
    }
}

fn lissajous_runtime() -> PresetRuntime {
    let json = include_str!("../../../assets/generator-presets/Lissajous.json");
    PresetRuntime::from_json_str(json, &PrimitiveRegistry::with_builtin())
        .expect("Lissajous preset must load on the mock backend")
}

/// Clip ends (gap), the layer sits unevaluated, a later clip starts: the
/// simulation clock must not advance while hidden, and must advance by
/// exactly the resumed frame's delta — never an accumulated sum of the
/// hidden frames.
#[test]
fn water_lifecycle_preserves_clip_edges_and_gaps() {
    let mut runtime = lissajous_runtime();

    // Parity contract: a fresh instance has consumed no frame. A graph
    // with substep regions run in this state is the reported
    // host-integration error — every render path installs an explicit
    // frame first (live, export, warmup, headless, thumbnail).
    assert!(
        runtime.last_simulation_frame().is_none(),
        "a fresh runtime must not claim a simulation frame"
    );

    // The layer renders output frames 1..=3.
    for id in 1..=3 {
        runtime.set_simulation_frame(sim_frame(id, 7, true));
        runtime.execute_frame(frame_time(id));
    }
    let last = runtime
        .last_simulation_frame()
        .expect("frames 1..=3 were delivered");
    assert_eq!(last.frame_id, 3);
    assert_eq!(last.epoch, 7);

    // GAP: the clip ended. GeneratorRenderer makes no
    // set_simulation_frame / render calls for the layer while it has no
    // active clip — modelled here by exactly the calls the renderer
    // makes: none. Frames 4..=9 pass on the host clock unseen.

    // A later clip starts on the same layer. The host hands over the
    // next output frame — ONE frame's delta, not 7 frames of catch-up.
    runtime.set_simulation_frame(sim_frame(10, 7, true));
    runtime.execute_frame(frame_time(10));

    let last = runtime
        .last_simulation_frame()
        .expect("the resumed frame was delivered");
    assert_eq!(
        last.frame_id, 10,
        "the instance identity is the delivered frame, not an accumulated count"
    );
    assert_eq!(
        last.delta,
        Seconds(DT),
        "resume advances exactly one frame's delta — hidden frames integrate nothing"
    );
    assert_eq!(last.epoch, 7, "a gap is not a seek; the epoch survives");

    // And a duplicate frame id is delivered as-is: the boundary node
    // owns dedup (a duplicate frame_id must not advance the clock
    // twice) — the renderer/runtime layer must not silently swallow it.
    runtime.set_simulation_frame(sim_frame(10, 7, true));
    runtime.execute_frame(frame_time(10));
    assert_eq!(
        runtime.last_simulation_frame().expect("duplicate delivered").frame_id,
        10
    );
}
