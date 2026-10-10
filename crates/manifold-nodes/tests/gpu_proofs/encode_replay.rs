//! Encode replay in the executor (`docs/ENCODE_REPLAY_DESIGN.md` P1b): an
//! outermost substep region replays its recorded dispatches, and nothing a
//! host can read changes. Every proof runs the same graph twice, replay off
//! and on, and compares bytes.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuReplayStats, GpuTextureFormat};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::water::matter::MatterTickStats;
use manifold_node_engine::{exec::backend::Backend, persistence::EffectGraphDefExt, exec::execution::Executor, exec::effect_node::FrameTime, graph::Graph, exec::metal_backend::MetalBackend, parameters::ParamValue, state_store::StateStore, exec::execution_plan::compile, load::graph_loader::pre_allocate_resources};
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;

use crate::substeps::{
    N, copy_chains_def, def, forces, node_of, registry, resource, seed_particles,
};

const FRAMES: u32 = 30;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Dump {
    None,
    /// The editor's atlas dump over every node: it pins textures and holds
    /// them past the frame; replay stays on.
    Every,
    /// The Cmd+D dump, which also copies every array: replay stays off.
    All,
}

struct Outcome {
    /// Per frame: the boundary state, what the sink read, and every array
    /// the Cmd+D dump copied, concatenated.
    frames: Vec<Vec<u8>>,
    stats: GpuReplayStats,
    /// Stats after each frame.
    stats_by_frame: Vec<GpuReplayStats>,
}

/// Append `bytes` of `buffer`, through a shared copy when it is private.
fn read_shared(buffer: &manifold_gpu::GpuBuffer, bytes: usize, out: &mut Vec<u8>) {
    let staging;
    let source = if buffer.mapped_ptr().is_some() {
        buffer
    } else {
        let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
        staging = device.create_buffer_shared(bytes as u64);
        let mut enc = device.create_encoder("encode-replay-readback");
        enc.copy_buffer_to_buffer(buffer, &staging, bytes as u64);
        enc.commit_and_wait_completed();
        &staging
    };
    let ptr = source.mapped_ptr().expect("shared buffer");
    // SAFETY: every command buffer that wrote it completed; `bytes` is within it.
    out.extend_from_slice(unsafe { std::slice::from_raw_parts(ptr.cast::<u8>().cast_const(), bytes) });
}

/// Run `def` for `frames` frames with replay on or off. `change` edits the
/// graph before a frame.
fn run_graph(
    def: EffectGraphDef,
    replay: bool,
    dump: Dump,
    profile: bool,
    frames: u32,
    change: impl Fn(u32, &mut Graph),
) -> Outcome {
    let harness = manifold_node_engine::testkit::gpu_harness::shared();
    let device = &harness.device;
    let mut graph = def.into_graph(&registry(), &Default::default()).expect("proof def builds");
    let plan = compile(&graph).expect("proof def compiles");
    let mut backend = MetalBackend::new(device.clone(), 64, 64, GpuTextureFormat::Rgba16Float);
    pre_allocate_resources(&mut graph, &plan, device, &mut backend).expect("pre-allocate");
    let boundary = node_of(&graph, "test.particle_boundary");
    let state_res = resource(&plan, boundary, "out", true);
    let sink_res = resource(&plan, node_of(&graph, "test.particle_sink"), "particles", false);
    let seed_res = resource(&plan, node_of(&graph, "test.particle_source"), "out", true);
    let force_res = resource(&plan, node_of(&graph, "test.force_source"), "out", true);
    for (res, bytes) in [
        (seed_res, bytemuck::cast_slice::<_, u8>(&seed_particles()).to_vec()),
        (force_res, bytemuck::cast_slice::<[f32; 3], u8>(&forces()).to_vec()),
    ] {
        let slot = backend.slot_for(res).expect("array bound");
        let buffer = Backend::array_buffer(&backend, slot).expect("array buffer");
        // SAFETY: shared-storage buffer, no GPU work in flight yet.
        unsafe { buffer.write(0, &bytes) };
    }
    let mut exec = Executor::new(Box::new(backend));
    exec.set_encode_replay(replay);
    match dump {
        Dump::None => {}
        Dump::Every => exec.set_dump_set(Some(graph.nodes().map(|n| n.id).collect())),
        Dump::All => exec.set_dump_all(true),
    }
    let mut state = StateStore::new();
    let particle_bytes = N * std::mem::size_of::<manifold_node_engine::particles::Particle>();
    let mut outcome = Outcome { frames: Vec::new(), stats: GpuReplayStats::default(), stats_by_frame: Vec::new() };
    for frame in 0..frames {
        change(frame, &mut graph);
        let time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(f64::from(frame) / 60.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: i64::from(frame),
        };
        let mut enc = device.create_encoder("encode-replay-proof");
        if profile {
            let sampler = device.create_timestamp_sampler(4096).expect("timestamp counters");
            enc.enable_dispatch_profiling(sampler, device);
        }
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(&mut graph, &plan, time, &mut gpu, &mut state, 0);
        }
        let profile = enc.commit_and_wait_profiled(device);
        assert_eq!(profile.failed_command_buffers, 0, "frame {frame} failed on the GPU");

        let mut bytes = Vec::new();
        for res in [state_res, sink_res] {
            let buffer = exec.host_array_buffer(&graph, &plan, res).expect("array holds its own contents");
            read_shared(buffer, particle_bytes, &mut bytes);
        }
        // Independent steps may run in another order in another build.
        let mut dumped = exec.dump_array_resources().to_vec();
        dumped.sort_by_key(|&(node, port, _)| (node.0, port));
        for (node, port, res) in dumped {
            bytes.extend_from_slice(&node.0.to_le_bytes());
            bytes.extend_from_slice(port.as_bytes());
            let buffer = exec.dump_array_buffer(res).expect("dumped array readable");
            read_shared(buffer, buffer.size as usize, &mut bytes);
        }
        outcome.frames.push(bytes);
        outcome.stats_by_frame.push(exec.replay_stats());
    }
    outcome.stats = exec.replay_stats();
    outcome
}

fn assert_same(what: &str, off: &[Vec<u8>], on: &[Vec<u8>]) {
    assert_eq!(off.len(), on.len());
    for (frame, (a, b)) in off.iter().zip(on).enumerate() {
        assert_eq!(a.len(), b.len(), "{what}: frame {frame} read back a different amount");
        if a != b {
            let first = a.iter().zip(b).position(|(x, y)| x != y).expect("differs");
            panic!("{what}: replay changed frame {frame} (first differing byte {first} of {})", a.len());
        }
    }
}

/// Replay on against off, byte for byte, over a substep region, and the same
/// region with copy chains before, inside and after it; each with and
/// without the editor's atlas dump.
#[test]
fn encode_replay_parity() {
    for (name, def) in [("region", def as fn() -> EffectGraphDef), ("copy chains", copy_chains_def)] {
        for dump in [Dump::None, Dump::Every] {
            let what = format!("{name}, dump {dump:?}");
            let off = run_graph(def(), false, dump, false, FRAMES, |_, _| {});
            let on = run_graph(def(), true, dump, false, FRAMES, |_, _| {});
            assert_eq!(off.stats, GpuReplayStats::default(), "{what}: replay off opened a span");
            assert!(on.stats.replayed > 0 && on.stats.executes > 0, "{what}: nothing replayed: {:?}", on.stats);
            let warm = on.stats_by_frame[1];
            assert_eq!(on.stats.recorded, warm.recorded, "{what}: a steady graph re-recorded: {:?}", on.stats);
            eprintln!("encode replay parity, {what}: {:?}", on.stats);
            assert_same(&what, &off.frames, &on.frames);
        }
    }
    let off = dam_break(false, FRAMES, |_, _| {});
    let again = dam_break(false, FRAMES, |_, _| {});
    let on = dam_break(true, FRAMES, |_, _| {});
    assert!(on.stats.replayed > 0, "Dam Break replayed nothing: {:?}", on.stats);
    eprintln!("encode replay parity, Dam Break: {:?}", on.stats);
    assert_within_spread("Dam Break", &off, &again, &on);
}

/// Mean |Δ| over an Rgba16Float image.
fn image_delta(a: &[u8], b: &[u8]) -> f64 {
    let decode = |c: &[u8]| f64::from(half::f16::from_le_bytes([c[0], c[1]]).to_f32());
    let sum: f64 = a.chunks_exact(2).zip(b.chunks_exact(2)).map(|(x, y)| (decode(x) - decode(y)).abs()).sum();
    sum / (a.len() / 2) as f64
}

/// Parity where direct runs themselves differ (`docs/ENCODE_REPLAY_DESIGN.md`
/// P1b, Defaulted). Two direct runs of the Dam Break in one process do not
/// always agree: some pairs match in every byte, others differ from frame 2
/// on, in the Matter frame and everything downstream, by the same few
/// discrete amounts (BUG-4n2g). A byte comparison can't tell replay from that, so the
/// Dam Break gate is what the solver conserves: the dump holds the same
/// arrays at the same sizes, no point went non-finite, the live point count
/// is exact, and mass holds. The byte spreads are printed. The bit-for-bit
/// proofs are the deterministic graphs above and the SWASH scenes.
fn assert_within_spread(what: &str, off: &PresetOutcome, again: &PresetOutcome, on: &PresetOutcome) {
    let shape = |o: &PresetOutcome| o.arrays.iter().map(|(n, b)| (n.clone(), b.len())).collect::<Vec<_>>();
    assert_eq!(shape(off), shape(on), "{what}: replay changed what the dump holds");
    let stats = |o: &PresetOutcome| {
        let (_, bytes) = o.arrays.iter().find(|(n, _)| n == "matter_state.stats").expect("Matter tick stats dumped");
        MatterTickStats::from_words(bytemuck::cast_slice(bytes))
    };
    let (direct, replay) = (stats(off), stats(on));
    assert_eq!(replay.nonfinite, 0, "{what}: replay left non-finite points: {replay:?}");
    assert_eq!(replay.live, direct.live, "{what}: replay changed the live point count");
    assert!((replay.mass - direct.mass).abs() <= 1e-4 * direct.mass.abs(), "{what}: mass {} under replay, {} direct", replay.mass, direct.mass);
    let differ = |a: &PresetOutcome, b: &PresetOutcome| {
        a.arrays.iter().zip(&b.arrays).filter(|((_, x), (_, y))| x != y).count()
    };
    let frames = |a: &PresetOutcome, b: &PresetOutcome| {
        a.frames.iter().zip(&b.frames).map(|(x, y)| image_delta(x, y)).fold(0.0, f64::max)
    };
    eprintln!(
        "{what}: of {} dumped arrays, {} differ between direct runs and {} under replay; worst frame mean |Δ| {:.3e} direct, {:.3e} replay; live {} mass {:.4}",
        off.arrays.len(),
        differ(off, again),
        differ(off, on),
        frames(off, again),
        frames(off, on),
        replay.live,
        replay.mass,
    );
}

/// Compile every pipeline the Dam Break needs before a compared run: a cold
/// first run renders its early frames while pipelines still compile, so it
/// differs from any warm run.
fn warm_dam_break() {
    static WARM: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARM.get_or_init(|| {
        run_preset(DAM_BREAK, true, 16, false, |_, _| {});
    });
}

/// A changed iteration count and grid on the region, and a changed speed and
/// resolution (grid, array capacity and storage) on the Dam Break, re-record
/// and still match replay off.
#[test]
fn encode_replay_survives_changes() {
    let region_change = |frame: u32, graph: &mut Graph| {
        if frame == 10 {
            let boundary = node_of(graph, "test.particle_boundary");
            graph.set_param(boundary, "iterations", ParamValue::Float(5.0)).expect("iterations");
        }
        if frame == 20 {
            let movers: Vec<_> = graph
                .nodes()
                .filter(|n| n.node.type_id().as_str() == "node.move_particles_3d")
                .map(|n| n.id)
                .collect();
            for mover in movers {
                graph.set_param(mover, "active_count", ParamValue::Float(600.0)).expect("active_count");
            }
        }
    };
    let off = run_graph(def(), false, Dump::None, false, FRAMES, region_change);
    let on = run_graph(def(), true, Dump::None, false, FRAMES, region_change);
    let recorded = |frame: usize| on.stats_by_frame[frame].recorded;
    eprintln!("encode replay changes, region: recorded {} → {} → {}", recorded(9), recorded(19), recorded(29));
    assert!(recorded(10) > recorded(9), "more iterations recorded nothing new");
    assert!(recorded(20) > recorded(19), "a new grid recorded nothing new");
    assert_eq!(recorded(29), recorded(21), "the changed region kept re-recording");
    assert_same("region with changes", &off.frames, &on.frames);

    let dam_change = |frame: u32, manifest: &mut ParamManifest| {
        let mut set = |id: &str, value: f32| manifest.get_mut(id).expect("Dam Break param").value = value;
        if frame == 10 {
            set("speed", 1.5);
        }
        if frame == 20 {
            set("resolution", 48.0);
        }
    };
    let off = dam_break(false, FRAMES, dam_change);
    let again = dam_break(false, FRAMES, dam_change);
    let on = dam_break(true, FRAMES, dam_change);
    let recorded = |frame: usize| on.stats_by_frame[frame].recorded;
    eprintln!("encode replay changes, Dam Break: recorded {} → {} → {}", recorded(9), recorded(19), recorded(29));
    assert!(recorded(19) > recorded(9), "a faster Dam Break recorded nothing new");
    assert!(recorded(29) > recorded(19), "a new resolution recorded nothing new");
    assert_within_spread("Dam Break with changes", &off, &again, &on);
}

/// Replay stays off under per-dispatch GPU profiling and the Cmd+D dump.
#[test]
fn replay_off_under_profiling_and_dump() {
    let profiled = run_graph(def(), true, Dump::None, true, 3, |_, _| {});
    assert_eq!(profiled.stats.replayed + profiled.stats.recorded + profiled.stats.executes, 0, "{:?}", profiled.stats);
    assert!(profiled.stats.direct > 0, "the profiled span saw no dispatches");
    let dumped = run_graph(def(), true, Dump::All, false, 3, |_, _| {});
    assert_eq!(dumped.stats, GpuReplayStats::default(), "a span opened under the Cmd+D dump");
    let direct = run_graph(def(), false, Dump::All, false, 3, |_, _| {});
    assert_same("Cmd+D dump", &direct.frames, &dumped.frames);
}

struct PresetOutcome {
    /// Per frame: the output texture.
    frames: Vec<Vec<u8>>,
    /// Every array the Cmd+D dump reads after the last frame.
    arrays: Vec<(String, Vec<u8>)>,
    stats: GpuReplayStats,
    stats_by_frame: Vec<GpuReplayStats>,
    cpu_ms: Vec<f64>,
    gpu_ms: Vec<f64>,
}

const DAM_BREAK: &str = "WaterDamBreakMatter";

fn dam_break(replay: bool, frames: u32, change: impl Fn(u32, &mut ParamManifest)) -> PresetOutcome {
    warm_dam_break();
    run_preset(DAM_BREAK, replay, frames, true, change)
}

fn run_preset(
    id: &'static str,
    replay: bool,
    frames: u32,
    read: bool,
    change: impl Fn(u32, &mut ParamManifest),
) -> PresetOutcome {
    let h = manifold_node_engine::testkit::gpu_harness::shared();
    let json = manifold_nodes::bundled_presets::bundled_preset_json(&manifold_core::PresetTypeId::new(id))
        .expect("bundled preset");
    let def: EffectGraphDef = serde_json::from_str(&json).expect("preset parses");
    let mut manifest = ParamManifest::from_params(
        def.preset_metadata.as_ref().map(|m| m.params.iter().cloned().map(Param::bundled).collect()).unwrap_or_default(),
    );
    let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        h.format,
        None,
    )
    .expect("preset builds");
    runtime.set_encode_replay(replay);
    let target = h.make_target("encode-replay-preset");
    let mut outcome = PresetOutcome {
        frames: Vec::new(),
        arrays: Vec::new(),
        stats: GpuReplayStats::default(),
        stats_by_frame: Vec::new(),
        cpu_ms: Vec::new(),
        gpu_ms: Vec::new(),
    };
    for frame in 0..frames {
        change(frame, &mut manifest);
        let last = read && frame + 1 == frames;
        runtime.set_dump_all(last);
        let ctx = PresetContext {
            time: f64::from(frame) / 60.0,
            beat: f64::from(frame) / 30.0,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: i64::from(frame),
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("encode-replay-preset");
        let start = std::time::Instant::now();
        {
            let mut gpu = GpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifest);
        }
        outcome.cpu_ms.push(start.elapsed().as_secs_f64() * 1e3);
        let profile = enc.commit_and_wait_profiled(&h.device);
        assert_eq!(profile.failed_command_buffers, 0, "{id} frame {frame} failed on the GPU");
        outcome.gpu_ms.push(profile.total_ms);
        outcome.stats_by_frame.push(runtime.replay_stats());
        if !read {
            continue;
        }
        outcome.frames.push(h.readback(&target.texture));
        if last {
            for dump in runtime.dump_arrays_all() {
                let mut bytes = Vec::new();
                read_shared(dump.buffer, dump.buffer.size as usize, &mut bytes);
                outcome.arrays.push((format!("{}.{}", dump.name, dump.port), bytes));
            }
            outcome.arrays.sort_by(|a, b| a.0.cmp(&b.0));
        }
    }
    runtime.set_dump_all(false);
    outcome.stats = runtime.replay_stats();
    outcome
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// Frame CPU and GPU ms for the bundled Dam Break, replay off against on,
/// alternating runs so drift hits both sides; and the first visit's cost.
#[test]
fn encode_replay_probe() {
    const WARM: usize = 8;
    const MEASURED: u32 = 48;
    let mut rows: [Vec<(f64, f64)>; 2] = [Vec::new(), Vec::new()];
    // The frame whose region recorded first, and its CPU on each side.
    let mut first_visit = (0usize, [0.0; 2]);
    warm_dam_break();
    for round in 0..3 {
        let runs = [false, true].map(|replay| run_preset(DAM_BREAK, replay, WARM as u32 + MEASURED, false, |_, _| {}));
        if round == 0 {
            let k = runs[1].stats_by_frame.iter().position(|s| s.recorded > 0).expect("the region recorded");
            first_visit = (k, [runs[0].cpu_ms[k], runs[1].cpu_ms[k]]);
        }
        for (side, run) in runs.iter().enumerate() {
            rows[side].push((mean(&run.cpu_ms[WARM..]), mean(&run.gpu_ms[WARM..])));
        }
    }
    for (side, name) in ["replay off", "replay on"].into_iter().enumerate() {
        let cpu: Vec<String> = rows[side].iter().map(|r| format!("{:.3}", r.0)).collect();
        let gpu: Vec<String> = rows[side].iter().map(|r| format!("{:.3}", r.1)).collect();
        eprintln!(
            "ENCODE REPLAY PROBE {DAM_BREAK} {name}: frame CPU ms [{}], GPU ms [{}], recording frame {} CPU {:.2} ms",
            cpu.join(", "),
            gpu.join(", "),
            first_visit.0,
            first_visit.1[side],
        );
    }
}
