<!-- index: A timeline audio layer: drag an audio file onto a layer, it plays through the existing kira playback subsystem, draws a waveform, and feeds a send for audio modulation. The architectural spine is offline analysis — a decoded file is analyzed once on import into a per-send feature curve sampled at the playhead (deterministic, look-ahead, no realtime glitch risk). The playback half (decode, kira output/mixing, sample-accurate transport sync, multi-stem) ALREADY EXISTS, bolted to the percussion-import pipeline; the feature mostly promotes it to a first-class LayerType::Audio and adds send-routing for modulation + warp. Covers the data model, the offline modulation curve, the kira-based playback reuse, warp (Signalsmith first-class / varispeed via kira playback_rate), export, and the recon-anchored phase plan (§12). -->

# Audio Layer — Design Doc

Drag an audio file onto a layer and it becomes a track: it plays through Manifold's audio output, draws its waveform on the lane, and routes to a send for audio modulation. No effects, no compositing — an audio track sits in the same lane list as video and generator layers, the way audio, MIDI, and return tracks coexist in an Ableton arrangement. This is the studio half of "Visual DAW": compose the modulation *into* the arrangement, deterministically, instead of riding live capture.

Status: **mostly SHIPPED** per the §13 build ledger (P0/P1/P3/§3R realtime tap/P4 varispeed + the Signalsmith seam all landed; remaining: P5 export, P6 hardening, the audible Signalsmith swap). Original recon note: most of the playback half already existed pre-design. A recon pass (2026-06-18) found a working **kira**-based subsystem already wired and running, bolted to the percussion-import pipeline: [audio_decoder.rs](../crates/manifold-playback/src/audio_decoder.rs) decodes any format to f32 PCM (symphonia); [audio_sync.rs](../crates/manifold-playback/src/audio_sync.rs) (`ImportedAudioSyncController`) plays an imported track through kira **sample-accurately synced to the transport** (seek-on-drift, replay-on-stop, encoder-delay, volume); [stem_audio.rs](../crates/manifold-playback/src/stem_audio.rs) does the same for *multiple stems*; both are driven each tick from [content_thread.rs](../crates/manifold-app/src/content_thread.rs) via `update_sync`. So **kira is already the output backend and the mixer**, and the follow-the-transport sync policy is already built. What's genuinely new: the layer/clip data model, **send-routing for modulation** (the offline curve), and **warp**. Build order is §12.

> **Correction (2026-06-18):** an earlier draft of this doc claimed "Manifold has no realtime audio output" and scoped a from-scratch cpal output backend + mixer + sync as the risky new work (§4, §9). That was wrong — kira already provides all three. §4 and §9 are rewritten around reusing it; the from-scratch framing is struck.

---

## 0. Why a file is not a microphone

> **⚠ SUPERSEDED 2026-06-18 — see §3R.** This section and §3 argued for an
> *offline* modulation curve (analyze the whole file once, sample at the
> playhead). That approach shipped, then was reversed by Peter: the audio layer
> is to behave like an Ableton audio track — hold the clip, play it, and stream
> the **played** signal to the send for **realtime** analysis, exactly like a
> live input. The determinism §0 sells is a studio nicety, not wanted for a live
> instrument, and the offline decode froze the app the first time a send was
> bound. §0/§3 are kept for history; **§3R is the shipping design.** The
> realtime tap was validated against kira 0.9.6 on real hardware (the `tap_spike`
> test in `audio_layer_playback.rs`).

The instinct is to treat an audio layer like another capture source: stream its samples into a send ring, let the live analysis worker chew on them, done. That works, and it's wrong — it throws away the one thing a file has that a microphone never will: **the future is already on disk.**

A live capture source has no choice but to analyze reactively. Samples arrive as they arrive; the FFT runs on whatever just landed; the modulation is always a window late. A decoded file has no such constraint. You can run the entire feature analysis **once, on import**, across the whole file, and store a per-send feature curve indexed by time. At playback you don't analyze anything — you *sample the curve* at the playhead. Three consequences, all of them upgrades:

1. **Deterministic.** The modulation is identical every show. The curve is computed from the samples, not from whatever the realtime worker happened to catch this pass. A timing bug can't become the show because there is no realtime timing in the modulation path.
2. **Look-ahead.** Because the curve is fully known, modulation can read *ahead* of the playhead — anticipate the kick instead of reacting 20 ms after it. A riser can start swelling the visual before the drop lands. The live path can never do this; the file path gets it for free.
3. **No glitch risk.** The content thread runs at 60 fps. Feeding a continuous realtime analysis off a 60 fps producer means a content-thread hitch becomes a modulation dropout. Sampling a precomputed curve is a table lookup — a stall just reads a slightly stale index, inaudible and invisible. The audio *output* is realtime, but that's kira's own audio thread (it buffers ahead of the content tick), not a ring the content thread has to feed sample-by-sample — see §4.

So the modulation half of this feature is built **offline**, not by reusing the live ring. The live analysis worker stays exactly as it is for microphones and taps; the audio layer is a different, simpler source that hands the modulation system a curve instead of a stream. This is the spine of the whole design.

---

## 1. Data model (`manifold-core`)

The layer/clip model already discriminates kinds by `LayerType` and by which clip field is populated. Audio extends both the same way.

- **`LayerType::Audio = 3`.** Today the enum is `Video=0, Generator=1, Group=2`; Audio is the next variant. Extend the int and string match arms in both `Serialize`/`Deserialize` paths at [crates/manifold-core/src/types.rs](../crates/manifold-core/src/types.rs) (~L116). Default stays `Video`.
- **Audio clip.** `TimelineClip` is a flat struct discriminated by populated field — `video_clip_id` for video, `generator_type` for generators. Add an audio variant the same way: an `audio_file_path` (plus the offline-analysis artifact handle, §3) populated when the owning layer is `Audio`. `in_point` is already `Seconds` (the established convention for player time); duration stays `Beats`.
- **Send source becomes a sum.** *(Shipped 2026-06-19.)* An `AudioSend` carries `source: AudioSendSource { layers: Vec<LayerId> }` — a **struct**, not an enum, so a send can sum capture channels (`AudioSend.channels`) **and** audio layers at once (a capture+layer mix), not one-or-the-other. A layer-fed send reads the layer's **realtime post-fader tap** (the §3R model), not a precomputed curve.
- **Per-layer audio fields.** Which send the layer drives (`Option<AudioSendId>`), a gain, and the **analysis-only** flag (the third output state, §5). Solo/mute already exist on `Layer` (`is_solo` / `is_muted`) and now carry audible meaning — see §5. The analysis-only flag is a new serialized bool on `Layer` (default false = Live); stem lanes from Detect and Group default it true.

Audio layers ignore the visual half of `Layer` (opacity, effects, blend, compositing). That's fine and already true of the existing types — Video and Generator don't use every field either.

---

## 2. Build scope and the one fork

Split by what exists versus what's genuinely new. The recon (status line) moved a lot from "new" to "reuse."

**Reuse (already built):**
- **Decode** — `audio_decoder::decode_audio_to_pcm(path) -> DecodedAudio { samples, sample_rate, channels }` (symphonia; WAV/AIFF/FLAC/MP3/AAC/OGG).
- **Playback output + mixing** — **kira** (`audio_sync::ImportedAudioSyncController`, `stem_audio`). Plays one track + N stems, summed, with volume. kira owns the realtime audio thread.
- **Transport sync** — `ImportedAudioSyncController::update_sync(engine)` / the stem controller's `update_sync(master, engine)`: sample-accurate seek-on-drift, replay-on-stop, encoder-delay compensation. This *is* the follow-the-transport policy.
- **Waveform** — `WaveformRenderer::set_audio_data(&samples, channels, sample_rate)` + `waveform_painter` + the `waveform_lane` panel.
- **Per-layer solo/mute fields** — `Layer.is_solo` / `is_muted`.
- **Modulation downstream of a send** — `AudioFeatureSnapshot` → `evaluate_all_audio_mods`, features/onset/`AudioSendId` — entirely source-agnostic.

**New — data model (§1):** `LayerType::Audio = 3`, the audio clip field, the send-source enum, per-layer send + gain.

**New — offline modulation (§3):** analyze the decoded file once on import into a per-send feature curve; sample it at the playhead into `AudioFeatureSnapshot`. Simpler than the live path — no ring, no worker, no glitch budget.

**New — promote playback to layers (§4):** the existing kira controllers play a *single global* imported track + its stems (the percussion model). The work is generalizing that to **per-layer, multi-clip** playback driven by `LayerType::Audio` clips, and giving mute/solo audible meaning. Not a from-scratch output backend — a refactor of what exists.

**New — warp (§4.1):** kira's `playback_rate` gives varispeed nearly free; pitch-preserving (Signalsmith) is the real new build.

**New — UI (§6) and serialization (§7).**

**Nearly free — export (§8):** render the kira master to a temp WAV, mux via the existing `audio_muxer`.

**The fork:** modulation is offline (§3); playback is kira-realtime (§4). Independent subsystems sharing a decoded buffer. A first cut can ship *either* — silent-but-modulating (offline only, skips the layer-playback refactor) or audible-but-unanalyzed (playback only). Recommended order: offline modulation first (cheaper, the studio payoff, zero playback risk), layer-playback promotion second.

---

## 3. Offline modulation

> **⚠ SUPERSEDED 2026-06-18 — see §3R.** Kept for history. The shipping design
> streams the played signal to the send in realtime; it does not precompute a
> curve.

On import, after decode, run the same feature extractors the live worker uses (band energy, RMS, Centroid, onset) across the whole file at a fixed hop, producing a per-send **feature curve**: a time-indexed array of `SendFeatures`. Store it as the clip's analysis artifact (§7).

At each content tick, for each audio layer with a send assignment, find the active clip, convert the playhead to a curve index (through the clip's warp ratio, §4.1, so a warped clip's features stay aligned to what's heard), and publish that `SendFeatures` into the same latest-wins slot the live worker would have written. **Downstream is byte-identical to live capture** — the modulation system never learns whether its features came from a ring or a table.

- **Look-ahead** is a per-binding offset added to the sample index — optional, defaults to zero, exposed later if wanted.
- **Reanalyze** is cheap and offline; changing analysis settings re-runs on the decoded buffer with no playback consequence.
- **Hop rate** matches the live worker's effective feature rate so a curve and a live send are interchangeable on the same modulation target.

This reuses the *extractors* (the feature seam in `analysis.rs`) without reusing the *realtime plumbing* around them — the extractors must stay callable on an arbitrary sample buffer, not only on the ring consumer. That's the one refactor the offline path asks of the existing audio crate.

---

## 3R. Realtime modulation — tap kira's output (THE SHIPPING DESIGN)

**Decision (2026-06-18, Peter):** an audio layer is an Ableton-style audio track.
It holds the clip, plays it through kira, and streams the **played** signal to
its send for the *same realtime analysis a live mic/tap already uses*. No offline
curve. Determinism (§0) is traded away on purpose — for a live instrument it's a
studio nicety, and the offline decode froze the whole app the first time a send
was bound (a full-song analyze ran synchronously on the content tick).

**Why it must come from kira, not the decoded buffer.** Warp changes the played
signal: varispeed (`set_playback_rate`) resamples live so the pitch shifts;
Signalsmith plays a *different* (stretched) buffer. The raw file samples at the
playhead don't match what's heard under either. kira's track output is the only
signal that's correct by construction — post-warp, post-gain, post-mute.

**Validated.** kira 0.9.6 supports exactly this: a custom pass-through
`Effect` on a sub-track sees each played `Frame`, and a sound routes there via
`StaticSoundData::output_destination(&track_handle)`. The sound applies rate +
volume *before* the track effect (kira `sound.rs`), so the tap is **post-fader**:
gain/mute affect modulation. Proven on real hardware by the `tap_spike` ignored
test in [audio_layer_playback.rs](../crates/manifold-playback/src/audio_layer_playback.rs)
(played a 0.2 sine, tap caught peak 0.2000).

**The shape.** Each audio layer gets its own kira sub-track with a tap effect
that copies played frames into a lock-free ring; the existing CQT/band DSP
(`form_tilted_column`/`reduce_send`, already free functions) runs on that ring
and writes `SendFeatures` into the snapshot at the layer's send index — the same
latest-wins slot the live worker fills. Downstream is byte-identical to a live
send. The send routing itself (which send a layer feeds) is the layer-header
Send dropdown, one mutation through `SetLayerAudioSendCommand` (§6).

**Pre/post-fader:** post (mute/gain kill modulation), matching a normal mixer
send. Revisit only if a muted layer should still drive visuals — which is exactly
the **analysis-only** output state (§5): silent to master, tap still hot. That
state is the planned "revisit," still to build.

**Build order — SHIPPED 2026-06-18 (steps 1–6 done; step 7 partial):**

1. ✓ Confirm kira sub-track + pass-through tap effect (runtime spike).
2. ✓ Route each audio layer through its own kira sub-track. `AudioLayerPlayback`
   creates a sub-track per audio layer (`ensure_layer_track`); each clip voice
   routes there via `StaticSoundData::output_destination(&track)`.
3. ✓ Lock-free tap on each layer track → ring. `LayerTap` (a kira `Effect`)
   copies post-fader mono into a `ringbuf` SPSC. kira requires `Effect: Sync`
   but the producer is `Send`-only, so it's wrapped in an uncontended `Mutex`
   (only the audio thread touches it; the content thread drains the consumer).
   Sample rate is learned from `Effect::init`.
4. ✓ Analyze the tapped stream. `StreamingSendAnalyzer` (manifold-audio) runs
   the same `form_tilted_column`/`reduce_send` DSP as the live worker → `SendFeatures`.
5. ✓ Write features into the snapshot at the layer's send index.
   `audio_mod_runtime` drains each layer-fed send's tap and overwrites its slot.
6. ✓ Deleted the offline system: `OfflineSendAnalyzer`, `FeatureCurve`,
   `AudioLayerCurves`, the background-decode fix, and the curve-sampling.
7. ◑ Verify. DSP + real-hardware route proven by tests
   (`streaming_analyzer_*`, the ignored `layer_tap_streams_post_fader_samples`:
   routed tone tapped post-fader at peak 0.2000, decay-to-silence confirmed).
   **In-app warp/gain/mute on a live audio layer still needs a real session.**

**Scope unification (2026-06-18 follow-up).** A layer-fed send is now a *real*
input: the inline analyzer also produces the Audio Setup **spectrogram** columns
+ overlay scalars (same data the capture worker pushes), so selecting a layer-fed
send draws the scope and per-band meters exactly like a mic/system input. The
runtime routes the scope drains (`drain_spectrogram_columns`/`_scalars`,
`spectrogram_num_bins`/`_freq_range`) to the tapped send's analyzer when it's
layer-fed, and mutes the capture worker's tap so it doesn't produce columns
nobody reads. The per-band meters already worked (they read the snapshot slot,
which the analyzer fills). Before this, a layer-fed send drove modulation but the
scope stayed black — it only ever listened to the capture worker.

**Source is an input SET, summed and analyzed once (SUPERSEDES the exclusive
model — 2026-06-19).** A send's input is its capture **channels** (the device,
when channels are assigned) **plus** any number of audio **layers**, **mixed to
one mono stream and analyzed once** so a send reacts to live input *and* layers
together ("what you hear is what modulates"). The model: `AudioSendSource {
layers: Vec<LayerId> }` on top of the existing `AudioSend.channels`; a send taps
the device iff it has channels (`has_capture()` is derived, NOT a stored flag —
there is no per-send device on/off). The unification that makes this clean: the
capture worker stopped analyzing and now only **downmixes** the device to per-send
mono (`MonoReader`); **all** analysis runs on the content thread, one
`StreamingSendAnalyzer` per send, fed `capture_mono + Σ layer_taps` (layer taps
resampled to the device rate via `LinearResampler` when they differ). That
collapsed the old dual analysis paths (worker VQT + inline) into one, and with it
the scope's capture-vs-layer routing (`SpectrogramTap`, `tapped_layer_send`) — the
scope just reads the tapped send's analyzer. Migration `v1.9.0 → v1.10.0` rewrites
the old `{"layer":"x"}` → `{layers:["x"]}` and drops the old unit `"capture"`.

Routing stays split by concern: an audio layer picks its send from the **layer
header** Send control (`SetLayerAudioSendCommand`, layer-centric, additive — point
several layers at one send and it sums them); the Audio Setup per-send source
**chip is READ-ONLY** — click it to open a dropdown listing the send's routings
("Capture · <channels>", "Layer · <name>" …). No disable-at-this-level; channels
are edited from the channel control, layers from the layer header. The gate that
runs analysis now also counts **active live triggers**, so triggers fire with the
scope closed during a show.

Original §3R note (history): a send did not pick its source; the chip was a
read-only indicator and the cycle-the-source button was removed as the wrong
direction (DAW model: track → send).

**Architecture choice (step 4) — resolved (a), inline.** The live worker is *one
ring → many sends*; layer taps are *one ring per layer → one send each*. Chose a
`StreamingSendAnalyzer` per layer-fed send run **inline on the content thread** —
NOT a worker thread per layer. The analysis is a handful of VQT hops per tick
with pre-allocated scratch (no per-frame alloc); a content hitch only delays
analysis a frame, it can't glitch audio (kira plays on its own thread). This
keeps the proven live-capture path untouched and adds zero threads.

---

## 4. Playback (via kira — already built, needs promotion to layers)

**Manifold already plays audio.** kira is a workspace dependency and owns a realtime audio thread that decodes, mixes, and outputs to the default device. The percussion pipeline drives it today through two controllers; the work here is generalizing them from "one global imported track + its stems" to "per-layer, multi-clip."

What exists and is reused as-is:
- **Output + mixing** — kira's `AudioManager` plays multiple `StaticSoundData` sounds summed to the default output, each with its own volume `Tween`. That is the mixer; no cpal output backend is needed.
- **Transport sync** — `ImportedAudioSyncController::update_sync(engine)` already does the whole policy: it computes the expected sample position from the playhead beat (`beat_to_timeline_time`), and on drift beyond a threshold calls `handle.seek_to(expected)`; on a stopped source mid-play it re-plays from `StaticSoundData` to get a fresh handle; it compensates an encoder delay. Transport stays master; audio follows. Done.
- **Stems** — `stem_audio` already syncs N stems sample-perfectly to the master's position with its own `update_sync` and `reset_stems`. The multi-source machinery exists.

What's new (the promotion):
- **Per-layer / multi-clip generalization.** Today there is one `Option<ImportedAudioSyncController>` on the content thread (`audio_sync`). Generalize to one playing handle per active audio clip (keyed by `ClipId`, per the pool-keyed-by-identity invariant), driven from the same `update_sync` each tick. A clip becoming active under the playhead `play()`s its `StaticSoundData`; becoming inactive stops it. This is the bulk of the work and it's a refactor of `audio_sync` + `stem_audio`, not new realtime code.
- **Mute / solo audible meaning** — §5. kira volume tweens make mute/solo a per-handle volume ramp (which also gives free declick — see below).
- **Gain** — per-layer gain → the handle's volume (in dB, matching `AudioSend::gain_db`'s convention).

- **Declick.** Audio clicks where video doesn't — a hard cut at a clip start/end, pause, loop-wrap, or scrub-stop pops. kira's volume `Tween` (already used for `set_volume`) covers most of it: ramp in/out over a few ms instead of hard start/stop. Verify kira's seek/replay paths don't click; add short fades where they do.
- **Warp** — each clip carries a **clip BPM** (`TimelineClip::recorded_bpm` already exists, clamped 20–300); warp ratio = `project_tempo / clip_bpm`. Both the player and the offline-curve index (§3) go through this ratio. See §4.1.

### 4.1 Time-stretch — Signalsmith first-class, varispeed (kira playback_rate) fallback

Pitch-preserving stretch is **first-class**, because the material that warps worst — drums, percussive transients — is exactly what drives the modulation and what the audience hears.

- **Varispeed (fallback / preview)** — kira exposes `playback_rate`; setting it to the warp ratio stretches by resampling, **nearly free**, but pitch moves with tempo. Inaudible near ratio 1.0. This is the quickest path to *any* warp and validates the clip-BPM → ratio wiring end-to-end before Signalsmith lands.
- **Signalsmith Stretch (primary)** — MIT-licensed, single-header C++, good transient handling, easy FFI. Pitch-preserving. Because kira plays decoded `StaticSoundData`, the Signalsmith path stretches the **decoded buffer offline/ahead** (or in a streaming wrapper) and hands kira the warped samples at `playback_rate = 1.0` — i.e. Signalsmith replaces the resample, it doesn't fight kira's rate. 

Both are driven by the *same* clip-BPM ratio, interchangeable behind a `warp(samples, ratio)` seam. **Don't roll your own** phase vocoder: the naive version smears the transients you care about; Signalsmith already solves transient handling + phase locking.

---

## 5. Output state (mute / analysis) and solo semantics

Audio layers reuse `is_solo` / `is_muted` but they mean audible things now, parallel to how they mean visible things on video layers.

**Three output states, two toggles (locked with Peter 2026-06-19).** Every audio lane is one of Live / Analysis-only / Muted, driven by an independent **Mute** and **Analysis** toggle. The UX/visual spec lives in [LAYER_CONTROLS_DESIGN §5.3](LAYER_CONTROLS_DESIGN.md); this section owns the **routing**.

| State | → master | → send |
|---|---|---|
| **Live** | audible | feeds |
| **Analysis-only** | silent | feeds |
| **Muted** | silent | none |

- **The send tap is post-fader, and shipped** ([audio_layer_playback.rs:226](../crates/manifold-playback/src/audio_layer_playback.rs#L226)): a layer-fed send drains the sub-track *after* the layer fader, and the mute path sets that fader to `0.0`. So **mute already silences the send** — this is the live behavior, not a plan.
- **Analysis-only** (not yet built) therefore needs a **routing split**, not a fader move: cut the sub-track → master contribution while leaving the post-fader tap hot. In kira terms, separate "audible to the main bus" from "present on the sub-track that the tap reads." This split is the one non-trivial bit of the output-state work, and the only part of the three-state model still to build.
- **Muted** is the fully-off state: silent to master *and* the tap sees nothing. **This is what mute already does** (post-fader, volume `0.0`).
- **Solo** — an audio-solo bus independent of the visual solo bus. Soloing an audio layer must not blank the video, and soloing a video layer must not silence audio. Two buses, same field name, disjoint membership by `layer_type`. (Shipped: `audible = !is_muted && (!any_solo || is_solo)`.)

> ✓ **Settled by the shipped code** (was flagged as a reversal). An earlier draft of this section wanted "mute does *not* stop feeding its send." The shipped infra does the opposite: the post-fader tap zeroes on mute, so **mute is the fully-off state** and the silent-but-still-modulating case is the new **Analysis-only** state. No decision owed — the code already chose. Also noted in LAYER_CONTROLS §5.3 and AUDIO_CLIP_DETECTION §8.6.

---

## 6. UI

**Adding audio — two gestures, locked with Peter 2026-06-19:**

- **Right-click → Add Audio Layer** in the add-layer menu, like the other layer types. (Today the menu omits Audio.)
- **Drag-drop with a target-aware affordance.** Dropping is *not* always "make a new lane" (the current behavior in [project_io.rs](../crates/manifold-app/src/project_io.rs), which appends a fresh audio track per file):
  - drop **onto an existing audio lane** → the clip **joins that lane** at the drop beat;
  - drop **onto empty timeline space** → a **new** audio lane + clip.
  - While dragging, the affordance shows which you'll get: the target audio lane **highlights** ("joins here"), empty space shows an **insertion line** ("new lane here"). One gesture, two outcomes, no modifier keys.
- Import UX partly exists from percussion.

- **Layer header:** mute + **analysis** toggles (§5), send dropdown ("which send this layer drives"), gain. Solo already renders.
- **Lane:** audio clips draw the waveform (`waveform_painter`) instead of a video thumbnail. Compositor skips `Audio` layers entirely — no visual output, no render cost.
- A **generic import+waveform path** decoupled from the percussion *trigger* pipeline: today decode/waveform are wired to onset→clip-trigger analysis. The audio layer wants "drop a file, get samples + a waveform + a feature curve" without the trigger-binding baggage.

---

## 7. Serialization (`manifold-io`)

- Persist the audio layer + clip: file path, `in_point`, duration, send assignment, gain. Relative-path handling mirrors `video_folder_path` / `relative_video_folder_path`.
- **Bundle decision — DECIDED (Peter, 2026-07-05, baseline review):** reference by
  path, never embed ("audio should be path not embedded that will bloat project
  files"). Relative-path handling mirrors the video-folder pattern above. A
  **Collect All and Save** action (Ableton precedent: copy every externally-
  referenced file into the project and repoint the paths) is in scope as an
  explicit user action — design it when P5/P6 rank; it is the portability answer,
  not default embedding. Rejected: embed-in-ZIP-by-default, because stems bloat
  the project file.
- **Feature curve (§3):** cache the offline analysis artifact so a project reopens without re-decoding + re-analyzing every audio clip. Invalidate on file change or analysis-setting change.

---

## 8. Export integration

Audio layers feed the export's audio track through the `audio_muxer` (`manifold-media/src/audio_muxer.rs`) that already muxes AAC into exported video with `-itsoffset` alignment. Rendering a show to video *with its audio* becomes nearly free — a benefit to include in scope, not a cost. The mixer's master output is the export's audio bed.

---

## 9. The realtime risk (largely retired by kira)

The earlier draft worried that the content thread (60 fps) would *feed* a continuous audio stream, so a frame hitch would become an audible glitch. **kira makes that mostly moot**: kira owns its own realtime audio thread and buffers ahead of the content tick. The content thread only sends it *control* messages (`play`/`seek_to`/`set_volume`) via `update_sync` — it does not hand kira samples frame-by-frame. A content-thread hitch delays the next sync correction by a frame; it doesn't starve the output.

What remains worth watching:
- **Seek frequency.** `update_sync` seeks on drift. If the per-clip generalization (§4) causes many simultaneous seeks (e.g. a transport jump re-seeking every active clip at once), that's a burst of kira work — check it stays smooth with several active audio clips. The existing single-controller path doesn't exercise this.
- **Signalsmith on the content thread.** Pitch-preserving stretch must not run synchronously on the content tick for a long clip — do it on import/ahead (offline), like the feature curve, or in kira's streaming path. Varispeed has no such cost (it's a rate set).

The offline modulation path (§3) is **immune** regardless — a table lookup, not a realtime producer.

---

## 10. Decisions to make up front

1. **Decode strategy** — *settled by kira:* `StaticSoundData` is whole-file-in-RAM, which is already how the percussion path loads. Keep it for v1 (a 5-min stereo stem ≈ 50 MB). Streaming is a kira option only if RAM becomes a problem with many clips.
2. **Output device** — kira plays to the system default output. A device picker means threading kira's `AudioManagerSettings`/backend config through; defer — default first.
3. **Bundle audio into the project ZIP** vs. reference by path — *decided
   2026-07-05:* reference by path; Collect All and Save as the explicit
   portability action (§7).
4. **Warp / time-stretch** — *decided:* warp is in scope, pitch-preserving (Signalsmith) first-class, varispeed fallback, clip-BPM ratio behind `warp(samples, ratio)` (§4.1). Not an open question; recorded here for visibility.
5. **Tap feedback loop** — playing audio out *while* tapping system audio re-enters the tapped mix → feedback. Needs a guard or a documented "don't route these together."
6. **Doubling the same stem** — two apps mixing is fine (macOS sums them like any players); the *only* hazard is Manifold and Ableton playing the **same material** expecting tight lock, because transport sync (OSC/MIDI) isn't sample-accurate and the two copies flam. Manifold audio stays sample-tight to Manifold's *visuals* (one playhead) and only needs rough alignment with Ableton, which transport sync gives. **Constraint: don't double the same sound in both.** This is a usage rule, not a subsystem cost.

---

## 11. Smaller, don't forget

- **Resampling + mono/stereo** — file rate ≠ output device rate ≠ analysis hop rate; resample on decode, downmix/upmix to the output channel count.
- **Output metering** — mirror the per-send capture meters for the master out.
- **Scrub audio** — do you hear grains while scrubbing (DAW-style) or silence? Recommend silent scrub for v1 (with declick on stop).
- **Curve/send slot pressure** — `MAX_SENDS = 16` is shared between live capture and audio layers; many audio layers each claiming a send competes with live sources.

---

## 12. Implementation plan (recon-anchored, 2026-06-18)

Phases ordered so each ships something usable. Anchors are real file:line from the recon. Crate in brackets.

### P0 — Data model + plumbing *(foundation; nothing audible yet)*
- `LayerType::Audio = 3` — extend the enum + **both** match arms (int *and* string) in `Serialize`/`Deserialize` at [types.rs:116](../crates/manifold-core/src/types.rs#L116). Note `Group=2` already exists, so Audio is `3`. Then find every exhaustive `match` on `layer_type` (compositor skip, lane rendering) and add the `Audio` arm. [core, renderer, ui]
- Audio clip field on `TimelineClip` — flat struct, discriminated by populated field (`video_clip_id` / `generator_type`); add `audio_file_path` + an analysis-artifact handle, and a `new_audio(...)` constructor mirroring `new_video` at [clip.rs:190](../crates/manifold-core/src/clip.rs#L190). **`recorded_bpm` already exists** ([clip.rs:27](../crates/manifold-core/src/clip.rs#L27), clamped 20–300) — that's the clip-BPM for warp, no new field. [core]
- Send source enum on `AudioSend` — today `{ id, label, channels, gain_db, analysis }` at [audio_setup.rs:79](../crates/manifold-core/src/audio_setup.rs#L79). Add a source discriminator: capture-channels (current) | audio-layer(`LayerId`). [core]
- Per-layer: `Option<AudioSendId>` send target + gain on `Layer`. `is_solo`/`is_muted` already on `Layer` (§5). [core]
- Command: `AddLayerCommand` already takes `LayerType` ([commands/layer.rs:39](../crates/manifold-editing/src/commands/layer.rs#L39)) → `Layer::new(name, LayerType::Audio, idx)` once the variant exists. Add assign-send / set-gain commands following the same `Command` impl pattern. [editing]
- Serialize/deserialize roundtrip. [io]

### P1 — Import + waveform *(file on a lane, silent)* — **mostly exists**
- Decode: reuse `audio_decoder::decode_audio_to_pcm` [playback]. Waveform: reuse `WaveformRenderer::set_audio_data` + `waveform_lane`. The single-global-import path is at [app_lifecycle.rs `poll_pending_audio_load`](../crates/manifold-app/src/app_lifecycle.rs#L483) — generalize it from one `waveform_lane` to per-audio-layer.
- Drag-drop an audio file → `AddLayerCommand(Audio)` + audio clip. [ui/app]
- Lane draws waveform; compositor skips `Audio` layers. [renderer/ui]

### P2 — Offline modulation *(studio payoff; silent; SHIPPABLE; zero playback risk)*
- Make the `analysis.rs` extractors callable on an arbitrary buffer. The DSP is already free functions over a VQT column — `band_reduce` / `reduce_send` / `band_edges` / `tilt_weights` / `relative_flux` ([analysis.rs:805+](../crates/manifold-audio/src/analysis.rs#L805)) — with sequential state in `SendState`. Offline analyzer = a sequential pass over decoded samples driving a `SendState` through `CqtTransform`, emitting `Vec<SendFeatures>` (the curve). Identical math → "what you see is what modulates" holds. [audio]
- Sample the curve at the playhead and write into `AudioFeatureSnapshot.sends[i]` at [audio_mod_runtime.rs:203](../crates/manifold-app/src/audio_mod_runtime.rs#L203). Downstream (`evaluate_all_audio_mods`, [modulation.rs:355](../crates/manifold-playback/src/modulation.rs#L355)) is unchanged. [app/playback]
- **The one real P2 decision — index alignment.** `AudioFeatureSnapshot.sends` is indexed by *position* in `AudioSetup::sends`, and the worker only produces device-fed sends. Two options: (a) pass layer-fed sends to the worker with empty `channels` (silent), then overwrite their slots from the curve — trivial alignment, wastes a VQT slot on silence; (b) keep a project-send-index → worker-send-index map, fill device slots from the worker frame and layer slots from the curve — no wasted compute, needs the map. Recommend (b) given `MAX_SENDS=16` pressure. [app]
- Send dropdown on the layer header; reanalyze action; look-ahead offset (stub). [ui/audio]

### P3 — Promote playback to layers *(makes it audible)* — **kira refactor, not from-scratch**
- Generalize `audio_sync::ImportedAudioSyncController` + `stem_audio` from one global track+stems to one kira handle per active audio clip (keyed by `ClipId`), driven from `update_sync` each tick in [content_thread.rs](../crates/manifold-app/src/content_thread.rs). Active-under-playhead → `play()`; inactive → stop. [playback/app]
- Mute/solo → per-handle kira volume tween (§5); per-layer gain → handle volume (dB). [playback]
- Declick: confirm kira seek/replay don't click; add short volume ramps where they do. [playback]
- Watch simultaneous-seek burst on transport jumps with many active clips (§9). [playback]

### P4 — Warp
- Clip-BPM (`recorded_bpm`) → ratio `project_tempo / clip_bpm`; feed both the player and the offline-curve index (§3). [core/playback/audio]
- Varispeed first via kira `playback_rate` (proves the wiring). Then Signalsmith (MIT FFI) stretching the decoded buffer offline, handed to kira at `playback_rate = 1.0`. Behind one `warp(samples, ratio)` seam. [media/playback]
- Set-clip-BPM UI. [ui]

### P5 — Export *(nearly free)*
- Render kira master → temp WAV → `AudioMuxer::mux(ffmpeg, video, audio_wav, out, offset)` ([audio_muxer.rs:52](../crates/manifold-media/src/audio_muxer.rs#L52), already `-itsoffset`-aligned). [media]

### P6 — Decisions + hardening
- Bundle audio into V2 ZIP vs path-reference (§7, decision 3) [io]; tap-feedback guard (§10.5) [audio]; silent scrub v1 [playback]; `MAX_SENDS=16` slot pressure (§11) [audio]; master metering [ui].

**Checkpoints:** end of **P2** = working file-driven modulation, silent, zero playback risk — a real shippable milestone. End of **P3** = audible. P4–6 layer on quality and integration.

### Cross-phase coupling
- The send-source enum (P0) is the hinge: P2's curve path and P3's playback both read which clip/layer owns which send.
- `recorded_bpm` (existing) couples P0, P3, P4 — it's the clip BPM everywhere.
- The offline analyzer (P2) and Signalsmith warp (P4) both want an "operate on a decoded buffer ahead of playback" home; build P2's buffer-analysis path so P4 can sit beside it.

---

## 13. Build status (2026-06-18, branch `audio-layer`)

**Landed — the functional pipeline is end-to-end and tested.**
- **P0 data model** ✓ — `LayerType::Audio`, `TimelineClip.audio_file_path` + `new_audio`/`is_audio`, `AudioSendSource` struct (`{ layers: Vec<LayerId> }`, capture+layer mix) with `bind_send_to_layer`/`send_for_layer`/`unbind_layer` on `AudioSetup`, `Layer.audio_gain_db` + `is_audio`/`active_audio_clip_at`/`audio_gain_linear`, commands `SetLayerAudioSendCommand` (layer→send routing) + `SetLayerAudioGainCommand`. Roundtrip + undo tests.
- **P1 compositor** ✓ — audio layers skipped from compositing and excluded from the visual solo bus (§5).
- **P1 drag-drop** ✓ — dropping a `wav/mp3/flac/aif/aiff/ogg/m4a/aac` file appends an audio layer + clip at the drop beat (one undo step). `is_supported_audio_extension` + `audio_duration_beats`. The OS file-drop dispatcher in `app.rs` routes audio through the same `process_dropped_files` path as MIDI (was previously a "not yet implemented" stub that never reached the import — fixed 2026-06-18).
- **P2 offline modulation** ✗ **REMOVED 2026-06-18 — replaced by §3R realtime tap.** Was `OfflineSendAnalyzer` + `FeatureCurve` (manifold-audio) + `AudioLayerCurves` cache (manifold-app). It decoded+analyzed the whole file synchronously on the content tick, freezing the app when a send was bound. Deleted in full (analyzer, curve, cache, and the `audio_mod_runtime` curve sampling) and replaced by the realtime kira tap below.
- **§3R realtime modulation tap** ✓ — each audio layer gets a kira sub-track (`AudioLayerPlayback::ensure_layer_track`); a `LayerTap` `Effect` copies post-fader mono into a lock-free `ringbuf`; `StreamingSendAnalyzer` (manifold-audio) runs the live worker's exact CQT/band DSP on the drained stream and `audio_mod_runtime` writes the result into the snapshot at the layer's send index. Post-fader, so warp/gain/mute are baked into the modulation. Proven by `streaming_analyzer_*` unit tests + the ignored real-hardware `layer_tap_streams_post_fader_samples` (routed tone tapped at peak 0.2000); in-app warp/gain/mute behaviour still wants a live session (§3R step 7).
- **P3 playback** ✓ — `AudioLayerPlayback` (manifold-playback): one kira voice per active audio clip, transport-following (seek-on-drift/replay-on-stop), mute/solo (audio bus)/gain via per-voice volume tween, 5 ms declick. Driven from the content tick; decode reuses `audio_sync::preload_audio`.

**Landed — UI (P1/P2 complete):**
- **Send-source selector UI** ✓ — a per-send source button in the Audio Setup panel cycles capture → each audio layer → capture, committing `SetLayerAudioSendCommand`. `state_sync` resolves the label/`layer_fed`; the inspector handler cycles and dispatches.
- **In-clip waveform painting** ✓ — `AudioWaveformCache` background-decodes each audio clip into a `WaveformRenderer` (the same zoom-aware MIP + spectral-color engine the audio-import lanes use), cached by `ClipId` (lazy, self-evicting), attached to `ViewportClip.waveform` each sync. The bitmap renderer selects the MIP level for the current zoom and paints via `waveform_painter::draw_waveform` across the clip's **full** pixel rect (clamped to visible columns) — so the waveform stays locked to the audio under zoom/scroll. (Earlier flat-peak `draw_clip_waveform` path replaced 2026-06-18: it mapped buckets across the clamped on-screen rect, which broke zoom tracking.)

**Landed — P4 warp (varispeed half):**
- **Warp ratio** ✓ — `TimelineClip::warp_ratio(project_bpm)` = `project_bpm / recorded_bpm` (1.0 when `recorded_bpm == 0`, i.e. warp off). The single source for both paths.
- **Varispeed playback** ✓ — `AudioLayerPlayback` sets the kira voice `playback_rate` to the ratio (declicked) and scales the transport-elapsed term by it; pitch moves with tempo (Signalsmith replaces this next).
- **Curve alignment** ✓ — `audio_mod_runtime` scales the playhead→curve-seconds by the same ratio, so a warped clip's modulation features track what's heard.
- **Set-clip-BPM UI** ✓ — audio clips get a "Clip BPM" button in the clip chrome (new `mode_audio` section) reusing the existing `ClipBpmClicked` → `ChangeClipRecordedBpmCommand` path; "Auto" = no warp. Tests for `warp_ratio` + the audio chrome section.

**Landed — P4 warp (Signalsmith seam):**
- **Pitch-preserving stretch** ✓ — Signalsmith Stretch (MIT) vendored under `manifold-playback/vendor/signalsmith` (3 headers, terminate at the C++ stdlib); a thin `extern "C"` wrapper (`native/signalsmith_stretch.cpp`, built by a new `build.rs`) calls `SignalsmithStretch::exact()` (the library's documented whole-buffer offline recipe). `audio_warp::warp_interleaved(samples, channels, sample_rate, ratio)` is the `warp(samples, ratio)` seam: stretches the decoded buffer offline so it plays at kira `playback_rate = 1.0`, pitch preserved. Returns `None` (→ varispeed fallback) for no-warp / degenerate / too-short input. Tests verify pitch preservation + length (2× halves at constant 440 Hz, 0.5× doubles at constant 330 Hz) without needing audio.

**DEFERRED — P4 wiring (the audible Signalsmith swap):**
- Varispeed warp is the shipping default; Signalsmith stays the verified-but-unwired seam until revisited. The plan when picked up: an **async stretch cache** in `AudioLayerPlayback` keyed by `(ClipId, ratio)` — kick the offline stretch off the content thread, keep **varispeed playing as the instant preview/fallback**, and **swap the voice to the stretched buffer** (rate 1.0, position = `in_point/ratio + elapsed`) once ready; re-stretch on clip-BPM or project-tempo change. Build the kira `StaticSoundData` directly from `frames: Arc<[Frame]>` (public field — no WAV round-trip; confirmed in kira 0.9.6). Needs a runtime ear check (swap click? alignment across the swap?).
- Coupled clip-content mapping — **both done 2026-06-18**: the waveform is now trim/warp-aware (a window onto the file, not stretched to fit), and changing clip BPM rescales the clip's *timeline length* (holding the played source span constant) via `ChangeClipRecordedBpmCommand`. A **Warp on/off toggle** alongside the existing Clip-BPM value lives in the audio clip's inspector chrome.

**Remaining — later phases:**
- **Export (P5), hardening (P6)** — not started.
