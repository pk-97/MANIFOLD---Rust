# Audio Modulation — Design Doc

Drive any effect-card slider from live audio. A new modulation source that sits beside the existing envelope and driver (beat-synced LFO) sources on every parameter — same drawer pattern, same per-param binding model. The instrument goal: route a bassline into Manifold and have its **pitch movement** push a visual parameter, so wobbles, glides, pitch-shifts and risers map straight onto the visuals — not just "the music got louder."

Status: **v1 SHIPPED 2026-06-17** (§11 steps 1–5 ✅: modulation plumbing, drawer UI, feature×band matrix, spectrogram overlays). Step 6 (onset) shipped as the SuperFlux per-band transients detector. **Step 7 (pitch/ridge tracking) is now designed: [AUDIO_OBJECT_TRACKING_DESIGN.md](AUDIO_OBJECT_TRACKING_DESIGN.md) (2026-07-06)** — it inherits §6's commitments; note its D2 supersedes §6's synchrosqueezing choice (harmonic-sum salience on the shared VQT column instead, synchro kept as a named precision fallback). Known stale labeled question: the §12 device-selection default was due "before step 2 ships" — step 2 shipped without closing it in-doc.

---

## 0. Why this exists — the audio carries what the DAW can't

Manifold already drives visuals from Ableton: note data, macros, automation (the OSC bridge). So the first thing this feature has to justify is what audio analysis adds that reading the session doesn't.

The answer is the gap between the **control layer** and the **signal layer**, and that the map between them is not invertible. The DAW holds *intent* — note, velocity, macro position, automation curve. The audio holds the *result*, and the result is a nonlinear, emergent function of that intent that cannot be recovered from the control side: FM index moving sidebands, a filter tipping into self-oscillation, unison voices beating, wavefolding, granular scrubbing, resonance ringing after note-off. There is no automation lane for any of it because it was never authored — it emerged from the patch. This holds **even for a synth sitting inside the Ableton session**: the DAW knows what was asked for; only the audio knows what came out. Structure, timing, and simple modulation can come from the DAW. Complex modulation that lives in the audio can only come from the audio.

**The payoff is glue.** DAW-data driving couples visuals to music at the *event* level — note fires, visual fires — but the motion between events is synthetic, an independent LFO that merely got co-triggered. Audio driving couples them at the *continuous* level: the visual rides the same contour the ear is tracking, micro-detail and all. That continuous co-variation is what reads as "these are one thing" rather than "visuals reacting to music" — perception fuses two senses into one event when they share both timing *and* shape, and only audio gives you the shape. The glue is perceptual, not metaphorical.

**Division of labor: the DAW drives the skeleton, the audio drives the flesh.** Structure, scene changes, gross automation, discrete hits — symbolic, slow, choreographable, reliable — from the DAW. The continuous expressive life on top — wobble, brightness breathing, the contour between hits — from the audio. They are not rival sources for one job; they work at different timescales and levels of abstraction. This also settles the composition-order question (§12): on a shared param, DAW and audio mods stack as **base-plus-detail** — automation sets the structural sweep, audio adds the emergent motion riding on top of it — rather than one overriding the other.

**The glue is cheap.** Because the value is in measuring the *result*, even crude features inherit the emergent motion: centroid rises when FM index rises, energy and flux ride every gesture, the pitch contour falls out of a clean mono source on its own. You don't model the patch; you measure its output. The cheap descriptor set (§5) already carries most of the glue. Pitch tracking and the rest don't *create* the coupling — they create specific, nameable axes ("this slider is the bassline's pitch") on top of coupling you'd get from centroid and energy alone. The sophistication is about legibility and control, not about whether it feels bound. This is the case for shipping the cheap features first.

**It is source-dependent, by design.** The glue's strength tracks how much emergent content a source has. A heavily modulated synth, a resonant filter, an FM or live-played patch — huge payoff, the audio is full of life the DAW can't see. A clean piano sample, a static pad, a metronomic kick — the audio is nearly redundant with the note, and audio-driving there only buys latency and jitter over reading the MIDI. So the feature is a **per-send opt-in** (`SendAnalysisConfig`, §3.2) pointed at the expressive sources, leaving the DAW to handle what's already ground truth.

**Where the field sits.** Resolume and Notch ship band-energy-with-smoothing as the default audio path — the "just energy" paradigm this feature exists to get past. TouchDesigner has a high ceiling (spectral centroid, custom DSP, even ML) but you build it yourself, and common practice is still band-split → lag → map. Serious live-AV rigs mostly drive visuals from the DAW's symbolic data and skip audio analysis entirely, because they already have the data. Almost no tool ships *both* paths well. Manifold has the DAW-data path (the Ableton bridge) and adds the audio path aimed precisely at the emergent content the data path structurally can't see. That pairing is the differentiator, not the analysis sophistication on its own.

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

Feature frames are keyed by **send** (`AudioSendId`), not raw channel — the worker downmixes each send's channels to mono, runs one FFT, and reduces it. A frame is a small, fixed-shape struct per send — cheap to publish.

**The feature × band matrix (shipped 2026-06-17).** Rather than a flat feature list, the worker runs the *same five detectors over four frequency bands*. The drawer picks one cell of the matrix — a detector (`kind`) × a band — so e.g. `Transients × Low` is a kick detector, `Brightness × High` is the brightness of just the top end. Every cell is normalized **0..1**.

| Detector (`kind`) | Meaning | Full | Low | Mid | High |
|---|---|:-:|:-:|:-:|:-:|
| `Amplitude` | band loudness (per-bin RMS, dB-normalized) | ✓ | ✓ | ✓ | ✓ |
| `Brightness` | spectral centroid, log-mapped across the band's edges | ✓ | ✓ | ✓ | ✓ |
| `Noisiness` | spectral flatness (tonal→noisy) | ✓ | ✓ | ✓ | ✓ |
| `Liveliness` | **relative** spectral flux (change ÷ band energy) — self-scales, doesn't pin | ✓ | ✓ | ✓ | ✓ |
| `Transients` | onset trigger (impulse, decays) from the band's flux | ✓ | ✓ | ✓ | ✓ |

`SendFeatures` is `[BandFeatures; 4]` (Full/Low/Mid/High) — 20 cheap scalars per send. `pitch_hz` / `pitch_delta_st` / `pitch_confidence` remain per-send v2 fields (the ridge tracker), not part of the matrix.

**One perceptual tilt, applied once.** Before *any* reduction the magnitude spectrum is multiplied by a fixed pink (+3 dB/oct amplitude) tilt — the analysis counterpart of the spectrogram's pink-noise tilt — so highs aren't buried by the natural 1/f slope. Every detector then sees identical data: amplitude/brightness track perceived balance, and noisiness measures flatness relative to *pink* (the right reference for music) rather than white. The tilt is an analysis-side fixed curve, deliberately **not** wired to the display's tilt knob — a cosmetic control must never move the modulation.

**Normalization.** `Amplitude` is dB-mapped (−60 dB floor) against a fixed reference; the per-send input gain is the calibration knob (no auto-range, so the same kick reads the same every night). `Liveliness` is relative flux (flux ÷ energy), naturally 0..1, so it self-scales with density instead of pinning on dense material. `Brightness`/`Noisiness` are intrinsically bounded.

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

`AudioModSource` is `{ send_id: AudioSendId, feature: AudioFeature }` — it references a **named send** in the project's `AudioSetup` (§3.2), never a raw channel. `AudioFeature` is `{ kind: AudioFeatureKind, band: AudioBand }` — one cell of the §5 matrix. Deserialization migrates the pre-matrix flat enum (`Amplitude`/`BandEnergy(b)`/`Centroid`/…) onto the new shape, so saved projects keep working.

**Evaluation.** Like drivers and envelopes, `audio_mods` is evaluated each content-thread tick: read the latest feature frame for the source's `send_id`, run it through the shaper, write the parameter's effective value on top of its `base` (`ParamSlot.base`). It participates in the same per-tick parameter evaluation and composition order as the existing sources — audio modulation is not special-cased in the render path. (Composition order with drivers/envelopes/Ableton on the same param follows the existing modulation-stacking convention; spell it out when implementing.)

**Persistence.** `audio_mods` serializes alongside `drivers`/`envelopes`, `None` when unused (skipped on serialize), keyed by `param_id` so it survives the same legacy-resolution and orphan policy. A saved project that referenced a since-removed param drops the orphaned audio mod, same as drivers.

## 8. Shaping — what makes it an instrument

Raw audio features are jittery driving a slider. The shaper is what makes audio modulation feel musical rather than noisy, and it applies to **every** feature regardless of intelligence level:

- **Attack / release** — envelope-follower smoothing. Slow release on band energy gives a pumping pad; fast attack on onset gives a snappy trigger.
- **Range** — the trim handles define the zone of the parameter's travel the audio drives, in **every** action mode (2026-07-10): Continuous maps its output into the zone; Step wraps/bounces/clamps against the zone's rails; Random jumps within the zone. Fire detection (Step/Random/trigger targets) runs on the conditioned, *pre*-range-map signal, so trimming the zone never distorts or kills firing. See PARAM_STEP_ACTIONS_DESIGN.md §5.9.
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
- Rows (top to bottom), each with a leading label that lines up with the sliders:
  - **Source** — send picker (the named sends, tinted with each send's identity color).
  - **Feature** — the detector: Amplitude / Brightness / Noisiness / Liveliness / Transients.
  - **Band** — Full / Low / Mid / High. Feature × Band = the cell that drives the slider.
  - **Invert** toggle (loud→low). **As-built note (2026-07-11):** the Delta
    (drive-on-rate-of-change) toggle shown here was removed from the drawer —
    `AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §7.2 item 2, Peter's
    call: "not very useful and adds a lot of clutter." The runtime
    `AudioModShape::rate_of_change` field and the `condition()` arm that
    reads it stay compiled — dormant, not deleted — for a possible future
    re-wire; no UI button can set or clear it anymore, so a load migration
    clears any `rate_of_change: true` a project saved before the removal.
  - **Sensitivity / Attack / Release** shaping sliders (sensitivity + envelope-follower smoothing; the slider's display label was "Amount" before the same 2026-07-11 rename — §7.2 item 3, "Amount" read as a generic gain knob, "Sensitivity" says what it tunes). Range is the green trim handles on the slider itself.
- The main slider moves as the mod drives it, doubling as the live meter; a dedicated post-shape meter and the spectrogram feature-overlays are the next step.
- Mutations route through `EditingService` like every other param change — a command that edits `audio_mods` on the target `PresetInstance`. No direct model writes from the UI.

The drawer is authoring UI. It does not need to be perform-time minimal; it needs to be clear.

### 10.0 Spectrogram feature overlays + draggable bands (shipped 2026-06-17)

The matrix (§5) shipped, but the analysis was assigned blind. Three pieces made it legible, all on the **Audio Setup spectrogram** (per tapped send). All decoupled from the modulation hot path. Built draggable-bands first (it also positions the per-band meters), then meters, then the scrolling traces.

1. **Draggable band boundaries.** The Low/Mid/High crossovers were `LOW_HZ`/`MID_HZ` **consts** in `analysis.rs`; they're now `low_hz`/`mid_hz` on `AudioSetup` (serde-default 250/2000, global to all sends), threaded to the worker via a lock-free `CrossoverBank` (mirrors `GainBank`) that re-splits `band_bins` live each drain — no capture restart. The shader draws the two lines from the editable values; a press near a line in the scope arms a drag (`AudioSetupPanel::on_event`), live-edits via `MutateProjectLive`, and commits one `SetAudioCrossoversCommand`. `AudioSetup::clamp_crossovers` keeps low<mid in 20..18k.

2. **Per-band level meters.** Low/Mid/High amplitude bars in a reserved right margin, each at the geometric centre of its band slab so it lines up with the frequency axis and follows the crossovers as they drag. The tapped send's `SendFeatures` ride `ContentState` (the runtime exposes the resolved tapped-send index); the panel repositions + fills the bars in place each frame.

3. **Scrolling centroid trace + transient ticks.** Per-column overlays computed in the worker from the CQT column (centroid as height-from-bottom — VQT bins are geometric, so it's already the log-freq centre; onset from column-to-column flux), streamed on a lock-step scalar ring (2 floats/column) alongside the magnitude columns, carried in `ContentState`, and drawn in the spectrogram shader from a third storage buffer keyed by the same 1:1 column slot — so they scroll locked to the waterfall with no scroll math. A `-1` centroid sentinel hides the trace on empty columns; the WGSL is guarded by a naga parse/validate unit test.

The hover readout (freq + pink-weighted dB at the cursor) is a separate, parallel addition (see commits `ce932caa`/`627b4438`).

### 10.1 The Audio Setup panel

Separate from the per-slider drawer, a central **Audio Setup** panel edits the `AudioSetup` (§3.2): pick the input device, see its channels, create/label/delete sends, assign each send's channel(s) and gain, and toggle per-send analysis (e.g. "enable pitch tracking"). This is the one place audio enters Manifold; the slider drawers only consume the sends it defines. Edits route through `EditingService` like any other project mutation. A live per-send meter here helps the user confirm signal is arriving before they assign anything.

**No new UI infra required — this is composition of what ships:**
- **The modal shell** follows [browser_popup.rs](../crates/manifold-ui/src/panels/browser_popup.rs) — a floating `UITree` modal over the main UI with a search bar and a `ScrollContainer` list. The Settings panel is the same skeleton; Audio Setup is one section/tab inside it.
- **Send-label text entry** uses the existing [text_input.rs](../crates/manifold-app/src/text_input.rs) session editor (caret/selection/blink/anchored overlay) — the same path as layer/macro/group renames. The work is one `TextInputField::AudioSendLabel(send_id)` variant, one commit arm in `app.rs`, and a `begin()` on label click.
- **Device/channel pickers and per-send analysis toggles** are the existing dropdown/button widgets.

The earlier `mapping_popover` "no text field on this surface" note is specific to the immediate-mode graph canvas and does not apply here — `UITree` panels have full text editing.

### 10.2 Drawer API — abstract before adding the fourth drawer (decided 2026-06-15)

The per-slider audio drawer is **not** built as a fourth hand-rolled copy of the driver/envelope/Ableton machinery. Each existing drawer today re-implements id allocation, layout math, hit-testing, draw, a `RowClick` variant, and the effect-vs-generator action fork (in `param_slider_shared.rs` + the 3112-line `param_card.rs`). A fourth copy is the wrong direction, and an API only the audio drawer uses (leaving three bespoke) is worse — five patterns.

**Design.** A drawer is a declarative list of controls under a slider. Every control in the three existing drawers is one of a small set:
- **Segmented** — N buttons, one active (driver beat-div grid, waveform grid)
- **Toggle** — (reverse / dot / triplet / Ableton invert)
- **Slider** — (envelope decay; audio attack / release / range / sensitivity)
- **Dropdown** — dynamic option list (new: the audio send picker)

```rust
enum DrawerControl { Segmented{labels, selected}, Toggle{on}, Slider{range, value}, Dropdown{options, selected} }
struct DrawerSpec { rows: Vec<Vec<DrawerControl>> }
// builder: DrawerSpec -> UITree nodes + laid-out rects (the UITree-coupled part)
// hit-test: click -> (row, control_index, sub-value)  (pure, unit-testable)
```

The drawer owns layout + hit-testing generically; the caller maps `(control_index, value)` onto a `ParameterAudioMod` / driver / envelope edit. This collapses `DriverConfigIds` / `EnvelopeConfigIds` / `AbletonConfigIds` and the parallel `ParamModState` vectors into one mechanism; the audio drawer is then just another `DrawerSpec`.

**Sequence (each step compiles and is independently verifiable):**
1. Drawer API core: control specs → layout → hit-test, with unit tests. New module, no regression risk.
2. Migrate **envelope** (one slider — simplest) onto it as proof.
3. Migrate **Ableton** (status + invert toggle).
4. Migrate **driver** (the segmented grids — hardest).
5. Audio drawer as a `DrawerSpec` (send Dropdown + feature Segmented/Dropdown + shaping Sliders), wired through the unified action path.

Pixel-identical render of the migrated driver/envelope drawers must be confirmed with the app running (the layout/hit-test core is unit-tested, but the UITree/scissor integration is not).

## 11. Build order

1. **Analysis core (manifold-audio, isolated + unit-testable).** A `FeatureFrame` (keyed by send), an `AudioFeatureWorker` that takes the ring consumer + a send→channel map, drains blocks, downmixes per send, computes band energy, and publishes latest-wins frames through a second `ringbuf` SPSC channel. Touches no app/recording/GPU code; tested with synthetic samples. This is the foundation and the first commit.
2. **Audio Setup model.** `AudioSetup` / `AudioSend` / `AudioSendId` / `SendAnalysisConfig` on the project; serialization; missing-device load policy; `EditingService` commands to add/label/route/delete sends.
3. **Capture lifecycle.** `ContentPipeline` owns the always-on capture device + worker, gated on (sends exist ∧ a slider references one), configured from `AudioSetup`. Content-thread reads latest frame each tick — temporary log to prove frames arrive end-to-end.
4. **Modulation model + evaluation.** `ParameterAudioMod` / `AudioModSource` / `AudioModShape` / `AudioFeature` on `PresetInstance`; content-thread evaluation writing effective values; serialization + legacy/orphan policy; `EditingService` command.
5. **UI.** First the **drawer API** (§10.2), then the per-slider audio drawer as a `DrawerSpec`, then the Audio Setup panel (§10.1).
   - ✅ **Drawer API** — built + unit-tested (`panels/drawer.rs`). Now the single builder for **all four** drawers: it grew `ButtonWidth` (proportional vs uniform — the driver waveform row is uniform) and a generic `Status` row (leading dot + audit label + right-aligned action button — the Ableton mapping strip). Each migrated drawer reconstructs its typed ids from the flat button list `drawer::build` returns, so the existing hit-test/drag paths are unchanged. Node creation order is preserved, so first-node/node-count bookkeeping is byte-identical.
   - ✅ **Legacy drawer migration (done 2026-06-16).** `build_envelope_config`, `build_driver_config`, and `build_ableton_config` now construct a `DrawerSpec` and call `drawer::build` instead of hand-rolling layout. The five duplicated layout constants (`DRIVER_ROW_HEIGHT`/`BEAT_DIV_SPACING`/`DRIVER_PAD_H`/`ENV_ROW_HEIGHT`/`ENV_PAD_H`) were deleted — those metrics live once in `drawer.rs`. The "five patterns" risk §10.2 named is closed: one drawer mechanism, no bespoke copies.
   - ✅ **Per-slider audio drawer** — the "A" button + drawer (send selector with ＋new, feature selector) on every effect/generator slider, wired through `PanelAction` → `ui_bridge` → the audio-mod EditingService commands. With no sends defined, the "A" button opens the Audio Setup panel (it used to be a silent no-op) so the user lands where they can create one.
   - ✅ **Audio Setup panel** (§10.1) — `panels/audio_setup_panel.rs`, a centered modal. Reachable from a visible **Audio** button in the header (next to Monitor/Perform) **or** ⌘⇧A; Escape closes. Input-device cycle + one row per send (channel stepper, gain stepper, delete) + Add-Send. Clicks resolve to the project-level audio-setup `PanelAction`s → tested EditingService commands; `state_sync` configures it (device enumeration + sends) each frame it's open. Multi-channel routing now works: route kick to channel 3, bass to channel 4, etc.
   - **Deferred polish (not blocking):** per-send text-field rename (sends auto-label "Audio N"); the v2 per-send analysis toggles (pitch tracking); multi-channel downmix *in the panel* (the worker already downmixes a `Vec<channel>`, but the panel edits one channel per send in v1).
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
