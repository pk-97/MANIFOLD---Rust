# Audio Modulation — Design Doc

Drive any effect-card slider from live audio. A new modulation source that sits beside the existing envelope and driver (beat-synced LFO) sources on every parameter — same drawer pattern, same per-param binding model. The instrument goal: route a bassline into Manifold and have its **pitch movement** push a visual parameter, so wobbles, glides, pitch-shifts and risers map straight onto the visuals — not just "the music got louder."

Status: **design only.** The audio-input plumbing already exists (recording uses it). The modulation plumbing and UI are to be built. The "intelligent" analysis (pitch/ridge tracking) is deliberately **not built yet** — but the architecture below leaves the seam open so it drops in without rework.

---

## 1. What it is, for the instrument

Each slider on an effect card already has buttons that open a downward drawer to modulate it with an **envelope** or a **driver** (beat-synced LFO). This adds a third: **Audio**. Open the drawer, pick an audio source, shape it, and the slider tracks the audio in real time.

The headline use is **pitch as motion**: with a monophonic source (a bassline on its own channel) the system tracks the fundamental and exposes its *rate of change*. A wobble bass becomes an oscillating control signal; a riser becomes a sustained climb; a pitch-shift becomes a step. This is a better fit for tonal material than the usual "FFT band energy → brightness" mapping, though energy still plays a role (see §6).

## 2. Scope

**In scope (v1 — plumbing):**
- An **Audio Setup**: the central place to route audio in and define **named sends** ("Kick / Bass / Vocals") that sliders reference.
- Always-on audio capture, gated on whether any audio mod is active (independent of recording).
- An off-RT audio worker thread that turns sample blocks into per-send **feature frames**.
- A new per-parameter modulation source (`Audio`) stored on `PresetInstance`, evaluated each content-thread tick.
- The UI: the Audio Setup panel + the per-slider drawer (send picker + shaping controls: attack/release/range/curve).
- A trivial first feature set so the whole path is exercised end-to-end: **band energy** + **onset trigger**.

**In scope (v2 — intelligence, designed-for but not built):**
- Synchrosqueeze-style ridge tracking → instantaneous pitch and **pitch delta** (df/dt).
- Onset detection as note segmentation for the pitch tracker.
- Energy as a confidence/normalization signal for the pitch tracker.

**Out of scope:**
- No VST/plugin dependency. Manifold opens its own audio device. The user is responsible for routing audio in correctly (aggregate device, virtual device, hardware input). This is a deliberate decision — see §3.
- No source separation inside Manifold. Separation is the user's routing problem: route the kick to its own channel and label it a send (§3.2).

## 3. Audio input & the Audio Setup layer

### 3.1 Input — already solved

`manifold-audio::capture::AudioCaptureDevice` ([crates/manifold-audio/src/capture.rs](../crates/manifold-audio/src/capture.rs)) already:
- Enumerates CoreAudio input devices (`AudioDeviceInfo`, default flag) — a device picker is free.
- Opens a device at its **native sample rate and channel count**.
- Streams **Float32 interleaved** samples into a lock-free SPSC ring buffer (2 s capacity).
- Runs an RT-disciplined callback: no alloc, no lock, no log — only `push_slice`.

Today the ring's single consumer is handed to the recording thread to mux into the `.mov` — capture lives and dies with a recording session ([crates/manifold-recording/src/session.rs](../crates/manifold-recording/src/session.rs)). For modulation, capture must be **always-on while any audio mod is active**, independent of recording. That lifecycle move is the foundation (§4).

> **Decision (2026-06-15): no VST as audio source.** Manifold must be able to route audio in on its own for the live-recording path regardless, so the modulation feature reuses that path. A plugin-as-sensor design was rejected — it couples a perform-critical feature to a separate product and a feature-data transport. The cost is that any analysis the VST's analyzer already does (e.g. synchrosqueezing) must be **ported into Manifold's worker** rather than received over a wire.

### 3.2 The Audio Setup — one place to route and label

The raw stream is just numbered interleaved channels — meaningless to a performer and fragile to re-patching. The **Audio Setup** is the central layer between the device and the sliders: the user routes audio in *once*, defines **named sends**, and every slider's drawer picks from those names.

```rust
pub struct AudioSetup {
    pub device_name: Option<String>,   // chosen input device; None = system default
    pub sends: Vec<AudioSend>,
}

pub struct AudioSend {
    pub id: AudioSendId,               // stable; what a slider stores
    pub label: String,                 // "Kick", "Bass", "Vocals" — what the user sees
    pub channels: SmallVec<[u16; 2]>,  // one or more input channels, downmixed to mono for analysis
    pub gain_db: f32,                  // per-send trim
    pub analysis: SendAnalysisConfig,  // which extractors run for this send (see below)
}
```

Why this is the right layer:
- **Sliders reference a send by `AudioSendId`, never a raw channel.** Relabel or re-patch a send in one place and every slider that uses it follows. A device swap at a venue re-points the setup, not 40 sliders.
- **Sends carry per-send analysis config.** "Kick" needs only energy + onset; "Bass" opts into the expensive v2 pitch tracker. Expensive analysis is opt-in per send, which bounds worker cost by design rather than paying it on every channel.
- **A send is mono for analysis.** Pick one channel, or downmix a few. Pitch tracking wants a clean monophonic source anyway (§6).

**Where it lives:** project-level, saved with the project (the labels are show content). `device_name` is stored but remappable — if the saved device is absent at load, the setup loads with sends intact and capture disabled until the user re-points it, same spirit as a missing MIDI port. (Open: whether device selection should instead be a global/rig preference — see §12.)

## 4. Capture lifecycle & the worker tap

Capture must run whenever the Audio Setup has ≥1 send *and* ≥1 slider references one (the gate), regardless of recording. `ContentPipeline` owns the always-on capture device (it already owns the recording session), starts/stops it on the gate, and configures the worker from the `AudioSetup`.

**v1 (recommended): independent capture for modulation.** Modulation owns its own `AudioCaptureDevice`; recording keeps its own. Zero changes to perform-critical recording code. The only risk is two cpal streams on one physical device when recording and modulating at once — macOS CoreAudio generally allows multiple input clients; validated in step 1.

**Later (optional): unify.** One shared capture feeds both, with a producer-side fan-out (recording ring + analysis ring off the one RT callback, each gated). Cleaner long-term, but it refactors recording, so it is not a v1 blocker.

```
cpal RT callback ─push→ analysis ring ─drain→ worker (per-send features) ─frames→ content thread
   (capture)            (SPSC, gated)         (downmix per send, extract)    (param eval)
```

Whichever path, the RT callback stays RT-safe: pre-allocated ring(s), `push_slice` only, no alloc/lock/log.

## 5. The audio worker — feature extraction off the RT thread

Feature extraction cannot run in the cpal callback (FFT/analysis is not RT-safe). A dedicated **audio worker thread** drains the analysis ring in blocks and produces **feature frames** at a sane rate (per analysis block, not per sample).

This worker is the home of the **feature seam** — the single most important architectural commitment in this doc:

> The worker produces feature frames; the drawer picks among features. A feature is an output of a pluggable extractor, **not** a hardcoded scalar. v1 plumbing must not assume "the feature is band energy."

Feature frames are keyed by **send** (`AudioSendId`), not raw channel — the worker downmixes each send's channels to mono, then runs only that send's configured extractors. A frame is a small, fixed-shape struct per send — cheap to publish. Per send, v1 carries at least:

| Feature | v1? | Meaning |
|---|---|---|
| `band_energy[lo/mid/hi]` | yes | perceptual (log-spaced, loudness-weighted) band energy |
| `onset` | yes | transient trigger (impulse, decays) |
| `pitch_hz` | v2 | tracked fundamental (instantaneous frequency of the dominant ridge) |
| `pitch_delta_st` | v2 | df/dt in **semitones per second** — the headline "motion" feature |
| `pitch_confidence` | v2 | 0–1 trust signal from ridge magnitude / energy |

Extractors are independent and composable. Band energy and onset are simple DSP. The pitch features all come from one ridge tracker (§6) added later as a single drop-in extractor.

## 6. The intelligence (v2 — designed-for, not built)

The target analysis is **ridge tracking on a synchrosqueezed time-frequency representation**, using the ridge's rate of change as the control signal. Manifold has implemented synchrosqueezing before (the Analyzer's energy-conservation work), so this is a port of known code, not research.

Four design commitments, recorded now so v2 doesn't relearn them:

1. **Track in log space.** `pitch_delta` is in **semitones/sec**, not Hz/sec. A ±1-semitone wobble at 80 Hz and at 400 Hz must produce the same slider motion; in Hz they differ ~5×.
2. **Onset = segmentation, not a rival feature.** Within a note, `pitch_delta` is the glide/wobble. At a new note the pitch jumps; a naive derivative spikes to a meaningless value. The onset detector tells the tracker "new note — re-acquire the ridge, suppress the jump," cleanly separating "the line is gliding" from "a new note hit."
3. **Energy is confidence, not the modulator.** When there is no tonal content (silence, pure percussion), the ridge chases noise. `pitch_confidence` (from ridge magnitude / band energy) gates trust so the slider goes still instead of twitching. Energy normalizes the pitch path; it does not drive it.
4. **Ridge extraction is the whole game, and it is far easier per-send.** A full mix has many ridges and "the frequency" is ambiguous. A separated bassline is near-monophonic — one dominant ridge, easy to follow. This is why the Audio Setup matters twice: the instrument move is "make a mono 'Bass' send and enable pitch tracking on it." Only sends that opt in pay the ridge-tracker cost.

Cost: synchrosqueezing is heavier than a plain FFT (FFT + reassignment), but it runs per-block on the worker thread, off the RT callback, so latency is fine.

## 7. Modulation model — slots beside drivers and envelopes

`PresetInstance` ([crates/manifold-core/src/effects.rs](../crates/manifold-core/src/effects.rs)) already stores per-parameter modulation sources as parallel vectors, **all keyed by `ParamId`**:

```rust
pub param_values: Vec<ParamSlot>,                              // base + effective per slot
pub drivers: Option<Vec<ParameterDriver>>,                     // beat-synced LFOs
pub envelopes: Option<Vec<ParamEnvelope>>,                     // ADSR / Random
pub ableton_mappings: Option<Vec<AbletonParamMapping>>,        // macro mappings
```

Audio modulation adds a fourth parallel vector — no new architecture, just another source in the same shape:

```rust
pub audio_mods: Option<Vec<ParameterAudioMod>>,                // NEW
```

A `ParameterAudioMod` mirrors the shape of `ParameterDriver`:

```rust
pub struct ParameterAudioMod {
    pub param_id: ParamId,        // stable addressing, same convention as ParameterDriver
    pub enabled: bool,
    pub source: AudioModSource,   // which send + which feature
    pub shape: AudioModShape,     // attack/release/range/curve — see §8
    // runtime-only state (envelope-follower accumulator) — not serialized
}
```

`AudioModSource` is `{ send_id: AudioSendId, feature: AudioFeature }` — it references a **named send** in the project's `AudioSetup` (§3.2), never a raw channel. `AudioFeature` is the enum that grows with §5's table — v1 ships `BandEnergy(Band)` and `Onset`; `PitchDelta`/`Pitch` land with v2 without touching this struct.

**Evaluation.** Like drivers and envelopes, `audio_mods` is evaluated each content-thread tick: read the latest feature frame for the source's `send_id`, run it through the shaper, write the parameter's effective value on top of its `base` (`ParamSlot.base`). It participates in the same per-tick parameter evaluation and composition order as the existing sources — audio modulation is not special-cased in the render path. (Composition order with drivers/envelopes/Ableton on the same param follows the existing modulation-stacking convention; spell it out when implementing.)

**Persistence.** `audio_mods` serializes alongside `drivers`/`envelopes`, `None` when unused (skipped on serialize), keyed by `param_id` so it survives the same legacy-resolution and orphan policy. A saved project that referenced a since-removed param drops the orphaned audio mod, same as drivers.

## 8. Shaping — what makes it an instrument

Raw audio features are jittery driving a slider. The shaper is what makes audio modulation feel musical rather than noisy, and it applies to **every** feature regardless of intelligence level:

- **Attack / release** — envelope-follower smoothing. Slow release on band energy gives a pumping pad; fast attack on onset gives a snappy trigger.
- **Range** — map the feature's working range onto a slider sub-range (min/max), so the audio drives just part of the parameter's travel.
- **Curve** — response shaping (lin / exp / log) for feel.

`AudioModShape` holds these. They are deliberately the same controls a performer expects on any modulation source.

## 9. Threading & realtime path

```
cpal RT thread ─push→ analysis ring ─drain→ audio worker ─frames→ playback realtime input ─→ content thread
   (capture)            (gated)              (FFT/features)        (alongside MIDI/OSC)        (param eval)
```

- Feature frames enter the content thread through the **same realtime-input path MIDI and OSC already use** in `manifold-playback`. Audio-feature ingestion is a sibling to OSC ingestion — no new shared-mutable-state pattern, the two-thread model is preserved.
- The content thread owns the `Project`; it reads the latest feature frame and writes effective param values during its tick. No UI-thread involvement in evaluation.
- Frames are published latest-wins (a bounded channel or a seqlock-style single-slot), not queued — modulation wants the freshest value, not a backlog.

## 10. UI — the drawer

The drawer parallels the existing envelope/driver drawers built on `param_slider_shared` ([crates/manifold-ui/src/panels/param_slider_shared.rs](../crates/manifold-ui/src/panels/param_slider_shared.rs)):

- A third modulation button on the slider opens the Audio drawer downward.
- **Source row:** send picker (the named sends from the project's `AudioSetup` — "Kick / Bass / Vocals") + feature picker (`BandEnergy`, `Onset` in v1; `Pitch`, `PitchDelta` appear when v2 lands and only for sends that enabled pitch tracking).
- **Shape rows:** attack, release, range (min/max), curve.
- A small live meter of the post-shape value so the performer sees it move while assigning.
- Mutations route through `EditingService` like every other param change — a command that edits `audio_mods` on the target `PresetInstance`. No direct model writes from the UI.

The drawer is authoring UI. It does not need to be perform-time minimal; it needs to be clear.

### 10.1 The Audio Setup panel

Separate from the per-slider drawer, a central **Audio Setup** panel edits the `AudioSetup` (§3.2): pick the input device, see its channels, create/label/delete sends, assign each send's channel(s) and gain, and toggle per-send analysis (e.g. "enable pitch tracking"). This is the one place audio enters Manifold; the slider drawers only consume the sends it defines. Edits route through `EditingService` like any other project mutation. A live per-send meter here helps the user confirm signal is arriving before they assign anything.

**No new UI infra required — this is composition of what ships:**
- **The modal shell** follows [browser_popup.rs](../crates/manifold-ui/src/panels/browser_popup.rs) — a floating `UITree` modal over the main UI with a search bar and a `ScrollContainer` list. The Settings panel is the same skeleton; Audio Setup is one section/tab inside it.
- **Send-label text entry** uses the existing [text_input.rs](../crates/manifold-app/src/text_input.rs) session editor (caret/selection/blink/anchored overlay) — the same path as layer/macro/group renames. The work is one `TextInputField::AudioSendLabel(send_id)` variant, one commit arm in `app.rs`, and a `begin()` on label click.
- **Device/channel pickers and per-send analysis toggles** are the existing dropdown/button widgets.

The earlier `mapping_popover` "no text field on this surface" note is specific to the immediate-mode graph canvas and does not apply here — `UITree` panels have full text editing.

## 11. Build order

1. **Analysis core (manifold-audio, isolated + unit-testable).** A `FeatureFrame` (keyed by send), an `AudioFeatureWorker` that takes the ring consumer + a send→channel map, drains blocks, downmixes per send, computes band energy, and publishes latest-wins frames through a second `ringbuf` SPSC channel. Touches no app/recording/GPU code; tested with synthetic samples. This is the foundation and the first commit.
2. **Audio Setup model.** `AudioSetup` / `AudioSend` / `AudioSendId` / `SendAnalysisConfig` on the project; serialization; missing-device load policy; `EditingService` commands to add/label/route/delete sends.
3. **Capture lifecycle.** `ContentPipeline` owns the always-on capture device + worker, gated on (sends exist ∧ a slider references one), configured from `AudioSetup`. Content-thread reads latest frame each tick — temporary log to prove frames arrive end-to-end.
4. **Modulation model + evaluation.** `ParameterAudioMod` / `AudioModSource` / `AudioModShape` / `AudioFeature` on `PresetInstance`; content-thread evaluation writing effective values; serialization + legacy/orphan policy; `EditingService` command.
5. **UI.** Audio Setup panel (§10.1), then the per-slider drawer (§10) with send + shape rows and a live meter.
6. **Onset feature.** Add the onset extractor and `Onset` feature; exercise trigger-style mapping.
7. **(v2) Ridge tracker.** Port synchrosqueezing into a per-send pitch extractor producing `pitch_hz` / `pitch_delta_st` / `pitch_confidence`, gated by `SendAnalysisConfig`; add the features to the enum and drawer. No changes to the model or UI plumbing — that is the point of the seam.

## 12. Open questions

- **Composition order** when a param carries an audio mod *and* a driver/envelope/Ableton mapping. Inherit the existing stacking convention; confirm it reads sensibly with audio in the mix.
- **Feature-frame rate** and the smoothing relationship between the worker's block rate and the content thread's 60 fps tick. The shaper's attack/release should be expressed in time, not frames, so it is rate-independent.
- **Where device selection lives.** The Audio Setup (sends + labels) is project content, but the physical input device is rig/venue-specific. Default: store `device_name` in the setup, remappable on load if absent (like a missing MIDI port). Alternative: device choice is a global/rig preference and only the logical sends are per-project. Decide before the setup model ships (step 2).
- **Send with a stale channel** after a device swap to fewer channels — disable that send (and the mods referencing it) until re-pointed, same spirit as a missing param. (Sends already absorb most of this: a slider references a `send_id`, not a channel, so only the setup needs fixing, not the sliders.)
- **Per-send cost ceiling.** How many sends can run the v2 ridge tracker simultaneously before the worker falls behind. Bounded by per-send opt-in (§3.2), but document the practical limit rather than failing silently.

## 13. Invariants to respect

- **No VST dependency.** Manifold owns its audio input. (§3)
- **RT callback stays RT-safe.** No alloc/lock/log; gated second `push_slice` only. (§4)
- **No analysis in the RT thread.** All feature extraction on the worker. (§5)
- **Two-thread model intact.** Features enter via the playback realtime path; the content thread owns evaluation; no new `Arc<Mutex>`. (§9)
- **All mutations through `EditingService`.** The drawer issues commands; no direct model writes. (§10)
- **The feature seam holds.** A feature is a pluggable extractor output; nothing in v1 plumbing may assume the feature is band energy. (§5)

---

_Related: [VSYNC_AND_FRAME_PACING.md](VSYNC_AND_FRAME_PACING.md) (timing discipline), [CONTENT_THREAD / two-thread model](../CLAUDE.md). Recording's use of the capture ring: [crates/manifold-recording/src/lib.rs](../crates/manifold-recording/src/lib.rs)._
