//! Causal hit-event dumper for the LIVE audio-trigger path.
//!
//! Feeds a decoded audio file through [`StreamingSendAnalyzer`] one hop at a
//! time (exactly like `mod_harness` — the analyzer never sees future samples)
//! and runs the REAL discrete "a hit happened" decision on the features each
//! hop: [`LiveTriggerState::evaluate`], the same evaluator
//! `PlaybackEngine` ticks at `manifold-playback/src/engine.rs` (the
//! `live_trigger_state.evaluate(...)` call), with five enabled
//! [`LayerClipTrigger`] configs — Transients on Full/Low/Mid/High plus the
//! Low-band Kick ridge — each on its own layer so the fire carries its band
//! identity. Fires are dumped as JSON:
//!
//! ```text
//! [{"time_sec": 1.234, "kind": "transients_low"}, ...]
//! ```
//!
//! Shapes default to sensitivity 1.0, attack 5 ms, release 120 ms (the
//! out-of-box live tuning) unless overridden by `--sensitivity`, `--attack-ms`,
//! or `--release-ms`, so this measures the causal path as-shipped or at a
//! chosen tuning. The one deliberate difference from the stage: evaluation runs
//! at the analyzer's hop cadence (~5.33 ms) rather than the engine's 60 fps
//! tick — strictly finer than live, and the same convention `mod_harness`
//! already uses.
//!
//! ```text
//! cargo run -p manifold-audio --example causal_events -- <clip.(wav|aiff|mp3|flac)> \
//!     [--out events.json] [--start s] [--dur s] \
//!     [--sensitivity 1.0] [--attack-ms 5.0] [--release-ms 120.0]
//! ```

use std::collections::HashMap;

use manifold_audio::analysis::StreamingSendAnalyzer;
use manifold_core::audio_features::AudioFeatureSnapshot;
use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource};
use manifold_core::audio_setup::{AudioSend, AudioSetup, DEFAULT_LOW_HZ, DEFAULT_MID_HZ};
use manifold_core::audio_trigger::{FireMeterCapture, LayerClipTrigger};
use manifold_core::layer::Layer;
use manifold_core::types::LayerType;
use manifold_core::units::Seconds;
use manifold_playback::live_trigger::LiveTriggerState;

/// The five routes, as (kind label, feature). Order is the layer order.
const ROUTES: [(&str, AudioFeatureKind, AudioBand); 5] = [
    ("transients_full", AudioFeatureKind::Transients, AudioBand::Full),
    ("transients_low", AudioFeatureKind::Transients, AudioBand::Low),
    ("transients_mid", AudioFeatureKind::Transients, AudioBand::Mid),
    ("transients_high", AudioFeatureKind::Transients, AudioBand::High),
    ("kick_low", AudioFeatureKind::Kick, AudioBand::Low),
];

struct Args {
    input: String,
    out: String,
    start_s: f32,
    dur_s: f32,
    sensitivity: f32,
    attack_ms: f32,
    release_ms: f32,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        input: String::new(),
        out: String::new(),
        start_s: 0.0,
        dur_s: f32::INFINITY,
        sensitivity: 1.0,
        attack_ms: 5.0,
        release_ms: 120.0,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let next = |i: &mut usize| -> Result<String, String> {
        *i += 1;
        argv.get(*i).cloned().ok_or_else(|| format!("missing value after {}", argv[*i - 1]))
    };
    while i < argv.len() {
        match argv[i].as_str() {
            "--out" => args.out = next(&mut i)?,
            "--start" => args.start_s = next(&mut i)?.parse().map_err(|e| format!("--start: {e}"))?,
            "--dur" => args.dur_s = next(&mut i)?.parse().map_err(|e| format!("--dur: {e}"))?,
            "--sensitivity" => {
                args.sensitivity = next(&mut i)?.parse().map_err(|e| format!("--sensitivity: {e}"))?
            }
            "--attack-ms" => {
                args.attack_ms = next(&mut i)?.parse().map_err(|e| format!("--attack-ms: {e}"))?
            }
            "--release-ms" => {
                args.release_ms = next(&mut i)?.parse().map_err(|e| format!("--release-ms: {e}"))?
            }
            s if s.starts_with("--") => return Err(format!("unknown flag {s}")),
            s => args.input = s.to_string(),
        }
        i += 1;
    }
    if args.input.is_empty() {
        return Err(
            "usage: causal_events <clip> [--out events.json] [--start s] [--dur s] \
             [--sensitivity 1.0] [--attack-ms 5.0] [--release-ms 120.0]".into(),
        );
    }
    if args.out.is_empty() {
        let stem = args.input.trim_end_matches(|c| c != '.').trim_end_matches('.');
        args.out = format!(
            "{stem}.s{:.2}.a{:.1}.r{:.1}.causal_events.json",
            args.sensitivity, args.attack_ms, args.release_ms
        );
    }
    Ok(args)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    // Decode + mean-downmix, identical to mod_harness's file path.
    let decoded = match manifold_playback::audio_decoder::decode_audio_to_pcm(&args.input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let ch = decoded.channels.max(1);
    let mono: Vec<f32> = decoded
        .samples
        .chunks_exact(ch)
        .map(|f| f.iter().sum::<f32>() / ch as f32)
        .collect();
    let sr = decoded.sample_rate;
    let start = ((args.start_s.max(0.0) * sr as f32) as usize).min(mono.len());
    let len = if args.dur_s.is_finite() {
        ((args.dur_s.max(0.0) * sr as f32) as usize).min(mono.len() - start)
    } else {
        mono.len() - start
    };
    if len == 0 {
        eprintln!("empty excerpt (start/dur out of range)");
        std::process::exit(1);
    }
    let mono = &mono[start..start + len];

    // The causal analyzer, defaults untouched (floor off, pitch tracking off,
    // scope off) — the same state an untouched project's live send runs in.
    let mut an = StreamingSendAnalyzer::new(sr, DEFAULT_LOW_HZ, DEFAULT_MID_HZ);
    let hop = an.hop().max(1);
    let dt = hop as f32 / sr as f32;

    // One send; one layer per route, each holding one enabled default-shape
    // clip trigger — the layer id maps a FireRequest back to its band kind.
    let send = AudioSend::new("harness");
    let send_id = send.id.clone();
    let mut setup = AudioSetup::default();
    setup.sends.push(send);
    let mut layers: Vec<Layer> = Vec::new();
    let mut layer_kind: HashMap<String, &'static str> = HashMap::new();
    for (kind, feature_kind, band) in ROUTES {
        let mut layer = Layer::new(kind.to_string(), LayerType::Video, layers.len() as i32);
        let mut cfg = LayerClipTrigger::new(AudioModSource {
            send_id: send_id.clone(),
            feature: AudioFeature::new(feature_kind, band),
        });
        cfg.enabled = true;
        cfg.shape.sensitivity = args.sensitivity;
        cfg.shape.attack_ms = args.attack_ms;
        cfg.shape.release_ms = args.release_ms;
        layer.clip_triggers.push(cfg);
        layer_kind.insert(layer.layer_id.as_str().to_string(), kind);
        layers.push(layer);
    }

    let mut trigger = LiveTriggerState::default();
    let mut events: Vec<(f32, &'static str)> = Vec::new();
    let mut hop_index = 0usize;
    for chunk in mono.chunks(hop) {
        an.push(chunk);
        // A short final chunk completes no VQT hop; skip its evaluation so
        // events only ever land on real analysis hops.
        if chunk.len() < hop {
            break;
        }
        let snapshot = AudioFeatureSnapshot { sends: vec![an.latest()] };
        let fires = trigger.evaluate(
            &snapshot,
            &setup,
            &layers,
            Seconds(dt as f64),
            &mut FireMeterCapture::default(),
        );
        let t = hop_index as f32 * dt;
        for fire in fires {
            let kind = layer_kind
                .get(fire.target_layer.as_str())
                .copied()
                .unwrap_or("unknown");
            events.push((t, kind));
        }
        hop_index += 1;
    }

    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for (_, kind) in &events {
        *counts.entry(kind).or_default() += 1;
    }
    println!(
        "{}: {:.2}s @ {sr} Hz, {hop_index} hops of {hop} samples ({:.2} ms), {} events",
        args.input,
        mono.len() as f32 / sr as f32,
        1000.0 * dt,
        events.len(),
    );
    for (kind, _, _) in ROUTES {
        println!("  {kind:<16} {}", counts.get(kind).copied().unwrap_or(0));
    }

    // Hand-rolled JSON — no new dependency for a two-field record.
    let mut json = String::from("[\n");
    for (i, (t, kind)) in events.iter().enumerate() {
        let comma = if i + 1 < events.len() { "," } else { "" };
        json.push_str(&format!("  {{\"time_sec\": {t:.6}, \"kind\": \"{kind}\"}}{comma}\n"));
    }
    json.push_str("]\n");
    std::fs::write(&args.out, &json).unwrap_or_else(|e| {
        eprintln!("failed to write {}: {e}", args.out);
        std::process::exit(1);
    });
    println!("wrote {}", args.out);
}
