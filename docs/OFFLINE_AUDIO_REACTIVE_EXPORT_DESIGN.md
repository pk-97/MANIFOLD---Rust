<!-- index: Offline audio-reactive export — feed export-rendered audio through the live SendAnalyzer chain per frame so audio-bound params move in rendered video; deterministic, live path untouched. Approved for build 2026-07-04. -->

# Offline Audio-Reactive Export — Design & Implementation Contract

**Status: DESIGNED (Fable, 2026-07-04). Sonnet-executable. Blocks nothing;
unblocked by nothing — runs in its own lane.**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 + §8 first.
Anchors are a 2026-07-04 snapshot — re-verify before each phase.**

## The problem

Export renders faster-than-realtime with synthetic timestamps
(`crates/manifold-app/src/content_export.rs:132`, `realtime_now` from frame
index at `:385`). Audio modulation features come exclusively from live capture
and live layer taps (`crates/manifold-app/src/audio_mod_runtime.rs:182`
`update()`, called per live tick). During export neither exists, so
`engine.audio_snapshot_mut().sends` stays default and every audio-bound
parameter renders frozen — while the mixdown WAV
(`manifold-playback/src/audio_mixdown.rs` `render_export_mix`, wired at
`content_export.rs:105-121`) is muxed into the file. The show's most expressive
layer silently dies in the deliverable.

For the release-content workflow (single-track master + MIDI mockups), audio
reactivity carries most of the motion. This feature makes exports reproduce it
— deterministically, which live capture can never be.

## What already exists (audit 2026-07-04)

| Piece | Where | Role here |
|---|---|---|
| `SendAnalyzer` — `new(rate, low_hz, mid_hz)`, `set_floor_db`, `set_crossovers`, `push(&[f32])`, `latest() → SendFeatures` | `manifold-audio/src/analysis.rs:916-1138` | The entire DSP, reused as-is. Pure push-driven: same samples in → same features out. Zero capture coupling. |
| Engine feed point | `audio_mod_runtime.rs:374-384` — `engine.audio_snapshot_mut().sends[i] = features` | The exact write the offline driver replicates per frame. |
| Send source model | `send.has_capture()`, `send.layers()` (`audio_mod_runtime.rs:273-274`) | Decides each send's offline source (D2). |
| Export audio render | `render_export_mix(project, start, end, bpm, tempo_map, out_wav) → Ok(bool)` (`audio_mixdown.rs:40`) — mirrors live warp/gain/solo, 48k, decode-failure = honest silence | Produces the very samples the analyzers should hear. Refactored (seam below), behavior unchanged. |
| Export frame loop | `content_export.rs` — per-frame engine tick with synthetic time | Where the driver's per-frame push + snapshot write is inserted, before each tick. |

`AudioModRuntime` itself is NOT reused offline — it drags CoreAudio directory
subscriptions, hot-plug listeners, and capture lifecycle. The offline driver is
a sibling consumer of the same `SendAnalyzer`, not a mode of the live runtime.
The live path is untouched by this design.

## Decisions

- **D1 — Analysis runs on export-rendered audio, per frame, in the export
  loop.** New `OfflineAudioModDriver` (manifold-app, alongside the export
  driver): per send, one `SendAnalyzer` at the mixdown sample rate + one
  precomputed mono buffer for the export range + a sample cursor. Before frame
  *f*'s engine tick: push samples `[floor(f·rate/fps), floor((f+1)·rate/fps))`
  (integer boundaries from the frame index — no cumulative drift), then write
  `latest()` into `engine.audio_snapshot_mut()` exactly as
  `audio_mod_runtime.rs:374-384` does. Same `floor_db`/crossover setters, same
  values as live.
- **D2 — Send sources map to export audio.** Layer-fed sends get the sum of
  *their* layers' rendered mono (post warp/gain/solo — same rules as the
  mixdown). Capture-fed sends get the full export mix mono: live capture is
  front-of-house audio (the show's sound), and the timeline mix is that sound's
  export-time equivalent. This substitution is LOGGED per send in the export
  log — visible, never silent (per `no-silent-fallbacks`). No audio in range →
  features stay default and the log says so; same visual result as today, now
  with a stated reason.
- **D3 — 1-second pre-roll.** Analyzers start cold; live ones carry history.
  Before frame 0, push up to 1s of audio preceding the range start (silence
  where the timeline has none) so envelopes/decays are settled at first frame.
- **D4 — Determinism is a feature and a test.** Same project + range + fps →
  bit-identical `SendFeatures` sequences across runs. No wall-clock, no
  threads in the analysis path.
- **D5 — Scope/spectrogram UI is not driven offline** (`set_scope` stays off).
  Meters and calibration are live-UX concerns.
- **D6 — Allocation discipline:** all send buffers sized once at export init;
  the per-frame work is slice + push + snapshot write. (Export is not the live
  hot path, but there is no reason to churn.)

## Seam brief (old → new), per DESIGN_DOC_STANDARD §6

`audio_mixdown.rs` currently renders straight to a WAV:

```rust
// OLD (public, kept working verbatim as a thin wrapper):
pub fn render_export_mix(project, start_beat, end_beat, bpm, tempo_map, out_wav_path)
    -> Result<bool, String>

// NEW (extracted core, same crate):
pub struct ExportAudio {
    pub sample_rate: u32,            // OUT_SAMPLE_RATE
    pub master_mono: Vec<f32>,       // full mix, mono, whole range incl. 1s pre-roll
    pub per_layer_mono: AHashMap<LayerId, Vec<f32>>, // only layers referenced by any send
    pub pre_roll_samples: usize,
}
pub fn render_export_audio(project, start_beat, end_beat, bpm, tempo_map,
                           tapped_layers: &[LayerId]) -> Result<ExportAudio, String>
```

`render_export_mix` becomes: call `render_export_audio` (empty `tapped_layers`),
write the WAV from the stereo path exactly as today, return the same `Ok(bool)`.
Existing callers unchanged. The WAV written for muxing must remain byte-identical
for a fixture project (gate, P1). The mono used for analysis is a downmix of the
same rendered frames — one render, two consumers, no drift between what is heard
and what is analyzed.

## Phases

- **P1 — Mixdown seam.** Extract `render_export_audio`; wrapper preserves
  `render_export_mix` behavior. Gates: `-p manifold-playback --lib`; fixture
  WAV byte-identical pre/post refactor (hash both in the test).
- **P2 — Driver + loop wiring.** `OfflineAudioModDriver` in manifold-app;
  per-frame push + snapshot write in the export loop before each tick; D3
  pre-roll; D2 source mapping + log lines. Gates: `-p manifold-app --lib` unit
  tests — synthetic sine fixture produces nonzero band features at the right
  frames; two runs produce identical feature sequences (D4); frame/sample
  boundary math property test (no drift over 10k frames).
- **P3 — Prove it end-to-end.** Export a fixture project with a param bound to
  a band envelope over a click-track audio layer; extract frames from the
  output video; assert the bound region changes across beat boundaries and is
  static between them. This is the render-path proof (per
  `prove-render-path-before-claiming-visual-win`) — a green unit test is not a
  moving export. Milestone: Peter exports a real track and sees the pump.

Full workspace sweep at P3 (export + serialization-adjacent = infrastructure).

## Forbidden shortcuts

Reusing `AudioModRuntime` with capture stubbed (drags CoreAudio lifecycle into
export); decoding the muxed WAV back off disk instead of the seam (double
decode, rate/offset drift risk); wall-clock or thread timing anywhere in the
analysis path (kills D4); silently feeding capture-fed sends nothing (must log);
per-frame `Vec` churn (D6).

## Deferred

- Per-send offline gain staging beyond the live-mirroring rules.
- Driving the spectrogram scope offline (D5).
- Session-recording integration (SESSION P5) — same driver slots in when P5 lands.
