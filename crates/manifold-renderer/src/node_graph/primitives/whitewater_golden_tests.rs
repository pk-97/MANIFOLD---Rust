//! Per-tick whitewater fingerprints on the shipped GPU FLIP Dam Break
//! (docs/WHITEWATER_STAGE_FUSION_DESIGN.md P0, invariant I1). Every
//! simulation tick's `pool_out`, `state_out`, `counts_out` and four
//! populations are hashed whole and held to a golden recorded before any
//! stage change. Comparison never writes. `MANIFOLD_RECORD_GOLDEN=1` records
//! a candidate next to the golden instead, only from a clean tree whose
//! renderer sources match [`BASE`]; promoting it is a manual rename.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_gpu::GpuBuffer;
use serde_json::{Value, json};

use super::energy_potential::{MAX_ENERGY, MIN_ENERGY};
use super::gpu_flip_preset::WaterScene;
use super::liquid_surface_tests::{Harness, read};
use super::whitewater_scene_tests::{Show, whitewater_render_def, with_tick_probe};
use super::whitewater_step::{Step, StepFrame, StepInputs, StepShape};
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::fluid::TICK;
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::liquid::grid::face_len;
use crate::node_graph::transform::Transform;
use crate::node_graph::whitewater::{WHITEWATER_EMPTY, WhitewaterParticle};

const GOLDEN: &str = "whitewater_tick_golden.txt";
const CANDIDATE: &str = "whitewater_tick_golden.candidate.txt";
/// The commit the golden belongs to: main before any whitewater stage
/// change. Recording refuses when the renderer's non-test sources differ.
const BASE: &str = "1a7fe1437";
const WHITEWATER: &str = "whitewater";
/// The liquid boundary that captures each tick's whitewater results.
const BOUNDARY: &str = "state";
/// Each whitewater output and the boundary port that holds its capture,
/// closed after every tick.
const PORTS: [(&str, &str); 7] = [
    ("pool_out", "whitewater_pool"),
    ("state_out", "whitewater_state"),
    ("counts_out", "whitewater_counts"),
    ("foam_particles", "foam_particles"),
    ("bubble_particles", "bubble_particles"),
    ("spray_particles", "spray_particles"),
    ("dust_particles", "dust_particles"),
];
const TICKS: u32 = 120;
/// `WHITEWATER_ID_LIMIT`: ids are taken modulo this.
const ID_LIMIT: u32 = 256;

/// The events the fused passes must reproduce; each has to happen in the
/// recorded run or the golden proves nothing about it. Compaction moving a
/// survivor is not visible in the outputs alone: [`compaction_moves_a_survivor`]
/// proves it on a constructed pool.
const EVENTS: [&str; 6] = ["spawn candidate", "spawn placed", "removal", "capacity overflow", "id wrap", "dust spawn"];

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

use crate::node_graph::liquid::conformance::json_node_mut;

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

/// One fixture's fingerprint lines and the first tick each event was seen.
fn run(label: &str, def: EffectGraphDef, lines: &mut Vec<String>) -> [Option<u32>; EVENTS.len()] {
    let mut show = Show::new(def, (96, 54), true, &[]);
    show.restart();
    assert!(!show.warmup_pending(), "{label}: warmup did not finish inside its frame bound");
    let mut seen = [None; EVENTS.len()];
    let mut previous: Option<(Vec<u32>, Vec<u32>)> = None;
    let accepted = |show: &Show| {
        let [epoch, time] = show.probes(["epoch", "simulation_time"]);
        (epoch, (f64::from(time) / TICK).round() as i64)
    };
    // The baseline is the restart's own frame, so frame 1 is checked too.
    // `restart` rewinds the harness clock to zero after its trigger frame,
    // and a backwards seek restarts the liquid clock: frame 1 is that
    // restart, exactly one epoch on with no tick.
    let (mut was_epoch, mut was_step) = accepted(&show);
    let mut tick = 0;
    let mut frames = 0;
    let mut dues = Vec::new();
    while tick < TICKS {
        show.frame(false);
        frames += 1;
        assert!(frames <= 2 * TICKS, "{label}: {tick} ticks in {frames} frames; dues {dues:?}");
        // At most one tick per frame, so the captures are that tick's and no
        // earlier tick of the frame goes unseen. A frame with no tick due
        // publishes nothing new and is not a tick.
        let [due, dropped] = show.probes(["ticks", "dropped_seconds"]);
        dues.push(due);
        assert!(due == 0.0 || due == 1.0, "{label}: frame {frames} ran {due} ticks; dues {dues:?}");
        assert_eq!(dropped, 0.0, "{label}: frame {frames} dropped simulation time");
        // The clock accepted exactly the ticks it scheduled, in one epoch.
        let (epoch, step) = accepted(&show);
        if frames == 1 {
            let [time] = show.probes(["simulation_time"]);
            assert_eq!(epoch, was_epoch + 1.0, "{label}: frame 1 is not the rewind's restart");
            assert_eq!(due, 0.0, "{label}: the rewind's restart ran a tick");
            assert_eq!(time, 0.0, "{label}: the rewind's restart accepted time");
        } else {
            assert_eq!(epoch, was_epoch, "{label}: frame {frames} changed epoch");
            assert_eq!(step, was_step + due as i64, "{label}: frame {frames} clock at tick {step}, was {was_step}, due {due}");
        }
        (was_epoch, was_step) = (epoch, step);
        if due == 0.0 {
            continue;
        }
        tick += 1;
        // The stage's own outputs, which the boundary's captures must equal
        // whole: a skipped or truncated capture fails here.
        let bytes: Vec<Vec<u8>> = PORTS.iter().map(|(port, _)| show.provided_all_bytes(WHITEWATER, port)).collect();
        for ((port, capture), stage) in PORTS.iter().zip(&bytes) {
            let captured = show.provided_all_bytes(BOUNDARY, capture);
            assert_eq!(stage.len(), captured.len(), "{label} tick {tick}: {port} and its capture differ in length");
            assert!(*stage == captured, "{label} tick {tick}: {port} and its capture differ");
            lines.push(format!("{label} tick {tick} {port} {} {:016x}", stage.len(), fnv(stage)));
        }
        let state = words(&bytes[1])[..8].to_vec();
        let counts = words(&bytes[2])[..9].to_vec();
        if let Some((was, was_counts)) = &previous {
            let spawned = state[3].wrapping_sub(was[3]);
            let full = state[2].wrapping_sub(was[2]);
            let placed = spawned - full;
            // Pool balance: lifetime deaths and keep-pass removals alike.
            let removed = (was[0] + placed).saturating_sub(state[0]);
            let happened = [
                spawned > 0,
                placed > 0,
                removed > 0,
                full > 0,
                placed > 0 && (state[1] < was[1] || placed >= ID_LIMIT),
                counts[8] > was_counts[8],
            ];
            for (first, now) in seen.iter_mut().zip(happened) {
                if now && first.is_none() {
                    *first = Some(tick);
                }
            }
        }
        previous = Some((state, counts));
    }
    let errors = show.errors();
    assert!(errors.is_empty(), "{label} ran with errors: {errors:#?}");
    seen
}

pub(super) fn packed_scene_fingerprints() {
    use super::whitewater_scene_tests::with_whitewater_axes;
    let def = with_tick_probe(whitewater_render_def(WaterScene::dam_break(64)));
    let mut packed = Vec::new();
    let mut axes = Vec::new();
    run("packed_faces", def.clone(), &mut packed);
    run("packed_faces", with_whitewater_axes(def), &mut axes);
    assert_eq!(packed, axes, "I1 fingerprints: packed grid versus real axis adapters");
}

#[test]
fn whitewater_packed_preset_save_load_matches_fingerprints() {
    use manifold_core::{project::Project, types::LayerType, preset_type_id::PresetTypeId};
    use super::whitewater_scene_tests::with_whitewater_reports;
    let def = super::gpu_flip_preset::render_def(WaterScene::dam_break(64));
    let mut project = Project::default();
    let index = project.timeline.add_layer("Dam Break", LayerType::Generator, PresetTypeId::new("WaterDamBreakGpuFlip"));
    project.timeline.layers[index].gen_params_or_init().graph = Some(def.clone());
    let path = std::env::temp_dir().join(format!("whitewater_round_trip_{}_{}.manifold", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    manifold_io::saver::save_project(&mut project, &path, Some("Whitewater round trip"), false).expect("save actual project archive");
    let reloaded = manifold_io::loader::load_project(&path).expect("load actual project archive");
    let loaded = reloaded.timeline.layers[index].generator_graph().expect("saved generator graph").clone();
    std::fs::remove_file(&path).expect("remove round-trip archive");
    let mut before = Vec::new();
    let mut after = Vec::new();
    run("save_load", with_tick_probe(with_whitewater_reports(def)), &mut before);
    run("save_load", with_tick_probe(with_whitewater_reports(loaded)), &mut after);
    assert_eq!(before, after, "I11: I1 fingerprints after manifold-io save/load");
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
        faces: super::whitewater_step::FaceSource::Axes([&faces[0], &faces[1], &faces[2]]),
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

fn git(args: &[&str]) -> std::process::Output {
    std::process::Command::new("git").args(["-C", env!("CARGO_MANIFEST_DIR")]).args(args).output().expect("git runs")
}

/// Writes the candidate golden, refusing a dirty tree, an unavailable HEAD
/// or renderer sources that differ from [`BASE`].
fn record_candidate(lines: &[String]) {
    let head = git(&["rev-parse", "HEAD"]);
    assert!(head.status.success(), "HEAD unavailable; refusing to record");
    let sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
    assert_eq!(sha.len(), 40, "HEAD unavailable; refusing to record");
    let status = git(&["status", "--porcelain", "--untracked-files=all"]);
    assert!(status.status.success(), "git status failed; refusing to record");
    let dirty: Vec<String> =
        String::from_utf8_lossy(&status.stdout).lines().filter(|l| !l.ends_with(CANDIDATE)).map(str::to_string).collect();
    assert!(dirty.is_empty(), "dirty tree; refusing to record:\n{}", dirty.join("\n"));
    let base = git(&["rev-parse", "--verify", &format!("{BASE}^{{commit}}")]);
    assert!(base.status.success(), "pinned base {BASE} unavailable; refusing to record");
    const MODULES: &str = "src/node_graph/primitives/mod.rs";
    let changed = git(&["diff", "--quiet", BASE, "HEAD", "--", "src", ":(exclude)*_tests.rs", ":(exclude)**/tests/**", &format!(":(exclude){MODULES}")]);
    assert!(changed.status.success(), "renderer sources differ from the pinned base {BASE}; refusing to record");
    // The module list may differ only by this test's own registration, at
    // one fixed place after a complete declaration, byte for byte: the
    // allowed cfg can neither land on another module nor take over an
    // existing attribute.
    let file_at = |rev: &str| {
        let shown = git(&["show", &format!("{rev}:crates/manifold-renderer/{MODULES}")]);
        assert!(shown.status.success(), "git show {rev}:{MODULES} failed; refusing to record");
        shown.stdout
    };
    let base_modules = String::from_utf8(file_at(BASE)).expect("utf-8 module list");
    let anchor = "\nmod whitewater_scene_tests;\n";
    assert_eq!(base_modules.matches(anchor).count(), 1, "the registration anchor moved; refusing to record");
    let registered = base_modules.replacen(
        anchor, &format!("{anchor}#[cfg(all(test, feature = \"gpu-proofs\"))]\nmod whitewater_golden_tests;\n"), 1);
    assert!(file_at("HEAD") == registered.as_bytes(), "{MODULES} differs from {BASE} beyond this test's registration; refusing to record");
    let header = format!(
        "# Whitewater per-tick golden (whitewater_tick_state_matches_golden)\n# base {BASE}\n# sha {sha}\n# fixtures: shipped GPU FLIP Dam Break 64; all emitters; all emitters at budget 1000; {TICKS} ticks each after restart\n# line: fixture tick N port bytes FNV-1a-64 over the whole buffer\n"
    );
    let path = format!("{}/tests/fixtures/{CANDIDATE}", env!("CARGO_MANIFEST_DIR"));
    std::fs::write(&path, header + &lines.join("\n") + "\n").expect("candidate writes");
    println!("recorded {} fingerprints at {sha} into {CANDIDATE}; promote by renaming it to {GOLDEN}", lines.len());
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
    if std::env::var("MANIFOLD_RECORD_GOLDEN").is_ok_and(|v| v == "1") {
        record_candidate(&lines);
        return;
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
