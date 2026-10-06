//! Per-tick whitewater fingerprints on the shipped GPU FLIP Dam Break
//! (docs/WHITEWATER_STAGE_FUSION_DESIGN.md P0, invariant I1). Every
//! simulation tick's `pool_out`, `state_out`, `counts_out` and four
//! populations are hashed whole and held to a golden recorded on main before
//! any stage change. `MANIFOLD_RECORD_GOLDEN=1` rewrites the fixture from
//! this build; only ever do that on a base with no stage change.

use manifold_core::effect_graph_def::EffectGraphDef;
use serde_json::{Value, json};

use super::gpu_flip_preset::WaterScene;
use super::whitewater_scene_tests::{Show, whitewater_render_def, with_tick_probe};
use crate::node_graph::whitewater::WhitewaterParticle;

const GOLDEN: &str = "whitewater_tick_golden.txt";
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
const EMPTY: u32 = 3;

/// The events the fused passes must reproduce; each has to happen in the
/// recorded run or the golden proves nothing about it.
const EVENTS: [&str; 6] = ["spawn", "death", "compaction", "capacity overflow", "id wrap", "dust spawn"];

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// The shipped def with these whitewater params and, when given, this
/// whitewater budget (the card's node, which sizes stage and boundary alike).
fn variant(params: &[(&str, Value)], budget: Option<f64>) -> EffectGraphDef {
    let mut def = serde_json::to_value(whitewater_render_def(WaterScene::dam_break(64))).expect("def serialises");
    for node in def["nodes"].as_array_mut().expect("nodes") {
        if node["nodeId"] == WHITEWATER {
            assert_eq!(node["typeId"], "node.whitewater_step");
            for (name, value) in params {
                node["params"][*name] = value.clone();
            }
        }
        if let Some(budget) = budget
            && node["nodeId"] == "whitewater_budget"
        {
            node["params"]["value"] = json!({"type": "Float", "value": budget});
        }
    }
    with_tick_probe(serde_json::from_value(def).expect("variant def"))
}

fn all_emitters(budget: Option<f64>) -> EffectGraphDef {
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
    let mut seen = [None; EVENTS.len()];
    let mut previous: Option<(Vec<u32>, Vec<u32>)> = None;
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
        let [due] = show.probes(["ticks"]);
        dues.push(due);
        assert!(due == 0.0 || due == 1.0, "{label}: frame {frames} ran {due} ticks; dues {dues:?}");
        if due == 0.0 {
            continue;
        }
        tick += 1;
        let bytes: Vec<Vec<u8>> = PORTS.iter().map(|(_, capture)| show.provided_all_bytes(BOUNDARY, capture)).collect();
        for ((port, _), b) in PORTS.iter().zip(&bytes) {
            lines.push(format!("{label} tick {tick} {port} {} {:016x}", b.len(), fnv(b)));
        }
        let state = words(&bytes[1])[..8].to_vec();
        let counts = words(&bytes[2])[..9].to_vec();
        if let Some((was, was_counts)) = &previous {
            let spawned = state[3].wrapping_sub(was[3]);
            let full = state[2].wrapping_sub(was[2]);
            let placed = spawned - full;
            let deaths = (was[0] + placed).saturating_sub(state[0]);
            let pool: &[WhitewaterParticle] = bytemuck::cast_slice(&bytes[0]);
            let tail_empty = pool[state[0] as usize..].iter().all(|p| p.kind == EMPTY);
            let happened = [
                spawned > 0,
                deaths > 0,
                deaths > 0 && state[0] > 0 && tail_empty,
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

/// Every tick of the shipped Dam Break, the all-emitters fixture and the
/// all-emitters fixture at a 1000-particle budget (the overflow case), bit
/// for bit against the golden.
#[test]
fn whitewater_tick_state_matches_golden() {
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
    if std::env::var("MANIFOLD_RECORD_GOLDEN").is_ok_and(|v| v == "1") {
        let sha = std::process::Command::new("git")
            .args(["-C", env!("CARGO_MANIFEST_DIR"), "rev-parse", "HEAD"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let header = format!(
            "# Whitewater per-tick golden (whitewater_tick_state_matches_golden)\n# sha {sha}\n# fixtures: shipped GPU FLIP Dam Break 64; all emitters; all emitters at budget 1000; {TICKS} ticks each after restart\n# line: fixture tick N port bytes FNV-1a-64 over the whole buffer\n"
        );
        std::fs::write(&path, header + &lines.join("\n") + "\n").expect("golden writes");
        println!("recorded {} fingerprints at {sha}", lines.len());
        return;
    }
    let golden = std::fs::read_to_string(&path).expect("golden fixture reads");
    let expected: Vec<&str> = golden.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(expected.len(), lines.len(), "golden fingerprint count");
    let moved: Vec<String> =
        expected.iter().zip(&lines).filter(|(e, l)| **e != l.as_str()).map(|(e, l)| format!("want {e}\n got {l}")).collect();
    assert!(moved.is_empty(), "{} of {} fingerprints moved; first:\n{}", moved.len(), lines.len(), moved.iter().take(10).cloned().collect::<Vec<_>>().join("\n"));
}
