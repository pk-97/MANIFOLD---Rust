//! Every fire of the shipped trained detectors (kick, snare) on one audio file,
//! as JSON lines:
//!
//! ```text
//! {"family":"kick","onset_s":1.237333,"emit_s":1.301021}
//! ```
//!
//! `onset_s` is the end of the candidate's onset hop on the 48 kHz / 256 model
//! grid (the lab's `onset_s`); `emit_s` is the input sample where the app
//! fires. Detectors are built from the embedded models and paired exactly as
//! `send_detectors` pairs them (a model reading the kick's verdicts runs fed by
//! the kick through `KickFed`); the run is checked against `send_detectors`
//! itself and aborts if any fire differs (`--skip-check` halves the run time).
//! Input is decoded and mean-downmixed at its own rate, like `causal_events`
//! and `mod_harness`.
//!
//! A `.mkick` golden clip (`tests/fixtures/detectors/<set>/clip_*.mkick`) runs
//! its stored audio and reports, on stderr, the golden's fire count beside
//! ours for the family that wrote it.
//!
//! Sibling of `causal_events`, which dumps the analyzer-driven live trigger
//! path; this one is the trained-detector path.
//!
//! ```text
//! cargo run --release -p manifold-audio --example detector_fires -- \
//!     <clip.(wav|aiff|mp3|flac)|clip.mkick> [--skip-check]
//! ```

use manifold_audio::detectors::{EventKind, detector, runners, send_detectors};
use manifold_audio::trained::container::Container;
use manifold_audio::trained::spec::{MODEL_HOP, MODEL_RATE};

/// The live detector worker's step.
const BLOCK: usize = 1024;

struct Hit {
    kind: EventKind,
    cand_hop: u64,
    sample: u64,
}

fn run(mono: &[f32], rate: u32) -> Vec<Hit> {
    let mut hits = Vec::new();
    for mut r in runners(rate) {
        for block in mono.chunks(BLOCK) {
            r.push_traced(block, |kind, _, _, fire| {
                hits.extend(fire.map(|f| Hit {
                    kind,
                    cand_hop: f.cand_hop,
                    sample: f.sample,
                }));
            });
        }
    }
    hits.sort_by_key(|h| (h.sample, h.kind.name()));
    hits
}

/// The app's own detector set on the same input, as (family, sample).
fn app_fires(mono: &[f32], rate: u32) -> Vec<(&'static str, u64)> {
    let mut dets = send_detectors(rate);
    let mut events = Vec::new();
    for block in mono.chunks(BLOCK) {
        for d in &mut dets {
            d.push(block, &mut events);
        }
    }
    let mut out: Vec<_> = events.iter().map(|e| (e.kind.name(), e.sample)).collect();
    out.sort_unstable_by_key(|&(k, s)| (s, k));
    out
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let check = !args.iter().any(|a| a == "--skip-check");
    let Some(input) = args.iter().find(|a| !a.starts_with("--")).cloned() else {
        eprintln!("usage: detector_fires <clip.(wav|aiff|mp3|flac)|clip.mkick> [--skip-check]");
        std::process::exit(2);
    };
    let golden = input
        .ends_with(".mkick")
        .then(|| {
            std::fs::read(&input)
                .map_err(|e| e.to_string())
                .and_then(|b| Container::parse(&b).map_err(|e| format!("{e:?}")))
        })
        .map(|g| g.unwrap_or_else(|e| fail(format!("{input}: {e}"))));
    let (mono, rate) = match &golden {
        Some(g) => {
            let n = g
                .shape("audio_i16")
                .unwrap_or_else(|e| fail(format!("{e:?}")))[0];
            let pcm = g
                .i16s("audio_i16", &[n])
                .unwrap_or_else(|e| fail(format!("{e:?}")));
            (
                pcm.iter()
                    .map(|&v| v as f32 / 32768.0)
                    .collect::<Vec<f32>>(),
                MODEL_RATE,
            )
        }
        None => {
            let d = manifold_playback::audio_decoder::decode_audio_to_pcm(&input)
                .unwrap_or_else(|e| fail(e));
            (d.mono(), d.sample_rate)
        }
    };

    let hits = run(&mono, rate);
    let ours: Vec<(&str, u64)> = hits.iter().map(|h| (h.kind.name(), h.sample)).collect();
    if check && ours != app_fires(&mono, rate) {
        fail("fires differ from send_detectors; this dump no longer runs the app's detector path");
    }
    for h in &hits {
        let onset_s = ((h.cand_hop + 1) * MODEL_HOP) as f64 / MODEL_RATE as f64;
        let emit_s = h.sample as f64 / rate as f64;
        println!(
            "{{\"family\":\"{}\",\"onset_s\":{onset_s:.6},\"emit_s\":{emit_s:.6}}}",
            h.kind.name()
        );
    }
    let count = |k: EventKind| hits.iter().filter(|h| h.kind == k).count();
    eprintln!(
        "{input}: {:.2}s @ {rate} Hz; kick {} snare {}{}",
        mono.len() as f64 / rate as f64,
        count(EventKind::Kick),
        count(EventKind::Snare),
        if check {
            " (matches send_detectors)"
        } else {
            " (send_detectors check skipped)"
        }
    );

    if let Some(g) = &golden {
        let id = g
            .text("train_id")
            .unwrap_or_else(|e| fail(format!("{e:?}")));
        let Some(kind) = EventKind::ALL.into_iter().find(|&k| build_id(k) == id) else {
            fail(format!("golden train_id {id} is no shipped model"));
        };
        let nf = g.shape("fires").unwrap_or_else(|e| fail(format!("{e:?}")))[0];
        let k = g
            .shape("cand_hop")
            .unwrap_or_else(|e| fail(format!("{e:?}")))[0];
        let cand = g
            .i64s("cand_hop", &[k])
            .unwrap_or_else(|e| fail(format!("{e:?}")));
        let onset = g
            .f64s("onset_s", &[k])
            .unwrap_or_else(|e| fail(format!("{e:?}")));
        let worst = hits
            .iter()
            .filter(|h| h.kind == kind)
            .filter_map(|h| {
                cand.iter().position(|&c| c as u64 == h.cand_hop).map(|i| {
                    (onset[i] - ((h.cand_hop + 1) * MODEL_HOP) as f64 / MODEL_RATE as f64).abs()
                })
            })
            .fold(0.0f64, f64::max);
        eprintln!(
            "golden ({}): {nf} fires expected, {} here; max onset_s difference {worst:.2e} s",
            kind.name(),
            count(kind)
        );
    }
}

fn build_id(kind: EventKind) -> String {
    detector(kind, MODEL_RATE)
        .map(|d| d.train_id().to_owned())
        .unwrap_or_default()
}
