use crate::testkit::whitewater_scene::Show;
use manifold_core::effect_graph_def::EffectGraphDef;
use crate::water::fluid::TICK;
pub(crate) const WHITEWATER: &str = "whitewater";
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
pub(crate) const TICKS: u32 = 120;
/// `WHITEWATER_ID_LIMIT`: ids are taken modulo this.
const ID_LIMIT: u32 = 256;

/// The events the fused passes must reproduce; each has to happen in the
/// recorded run or the golden proves nothing about it. Compaction moving a
/// survivor is not visible in the outputs alone: [`compaction_moves_a_survivor`]
/// proves it on a constructed pool.
pub(crate) const EVENTS: [&str; 6] = ["spawn candidate", "spawn placed", "removal", "capacity overflow", "id wrap", "dust spawn"];

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}
fn words(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}
/// One fixture's fingerprint lines and the first tick each event was seen.
pub(crate) fn run(label: &str, def: EffectGraphDef, lines: &mut Vec<String>) -> [Option<u32>; EVENTS.len()] {
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