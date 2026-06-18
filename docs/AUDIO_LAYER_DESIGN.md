<!-- index: A timeline audio layer: drag an audio file onto a layer, it plays through a real output stream, draws a waveform, and feeds a send for audio modulation. The architectural spine is offline analysis — a decoded file is analyzed once on import into a per-send feature curve sampled at the playhead (deterministic, look-ahead, no realtime glitch risk), so only the audio *output* stays realtime. Covers the data model, the offline-vs-live modulation fork, the output backend + mixer + declick + warp (Signalsmith pitch-preserving first-class, varispeed fallback) + transport-follow sync, export integration, and the doubling-the-same-stem usage rule with Ableton. -->

# Audio Layer — Design Doc

Drag an audio file onto a layer and it becomes a track: it plays through a real output stream, draws its waveform on the lane, and routes to a send for audio modulation. No effects, no compositing — an audio track sits in the same lane list as video and generator layers, the way audio, MIDI, and return tracks coexist in an Ableton arrangement. This is the studio half of "Visual DAW": compose the modulation *into* the arrangement, deterministically, instead of riding live capture.

Status: **design only.** Decode + waveform rendering already exist (the percussion import pipeline). The modulation path downstream of a send already exists (the analysis worker, features, onset, `AudioSendId`). What's new is realtime audio *output* — Manifold has none today — plus the layer/clip model and a sync policy. Build order and the one architectural fork are in §2.

---

## 0. Why a file is not a microphone

The instinct is to treat an audio layer like another capture source: stream its samples into a send ring, let the live analysis worker chew on them, done. That works, and it's wrong — it throws away the one thing a file has that a microphone never will: **the future is already on disk.**

A live capture source has no choice but to analyze reactively. Samples arrive as they arrive; the FFT runs on whatever just landed; the modulation is always a window late. A decoded file has no such constraint. You can run the entire feature analysis **once, on import**, across the whole file, and store a per-send feature curve indexed by time. At playback you don't analyze anything — you *sample the curve* at the playhead. Three consequences, all of them upgrades:

1. **Deterministic.** The modulation is identical every show. The curve is computed from the samples, not from whatever the realtime worker happened to catch this pass. A timing bug can't become the show because there is no realtime timing in the modulation path.
2. **Look-ahead.** Because the curve is fully known, modulation can read *ahead* of the playhead — anticipate the kick instead of reacting 20 ms after it. A riser can start swelling the visual before the drop lands. The live path can never do this; the file path gets it for free.
3. **No glitch risk.** The content thread runs at 60 fps. Feeding a continuous realtime analysis off a 60 fps producer means a content-thread hitch becomes a modulation dropout. Sampling a precomputed curve is a table lookup — a stall just reads a slightly stale index, inaudible and invisible. **Only the audio output stays realtime**, and that's isolated behind its own ring (§4).

So the modulation half of this feature is built **offline**, not by reusing the live ring. The live analysis worker stays exactly as it is for microphones and taps; the audio layer is a different, simpler source that hands the modulation system a curve instead of a stream. This is the spine of the whole design.

---

## 1. Data model (`manifold-core`)

The layer/clip model already discriminates kinds by `LayerType` and by which clip field is populated. Audio extends both the same way.

- **`LayerType::Audio = 3`.** Today the enum is `Video=0, Generator=1, Group=2`; Audio is the next variant. Extend the int and string match arms in both `Serialize`/`Deserialize` paths at [crates/manifold-core/src/types.rs](../crates/manifold-core/src/types.rs) (~L116). Default stays `Video`.
- **Audio clip.** `TimelineClip` is a flat struct discriminated by populated field — `video_clip_id` for video, `generator_type` for generators. Add an audio variant the same way: an `audio_file_path` (plus the offline-analysis artifact handle, §3) populated when the owning layer is `Audio`. `in_point` is already `Seconds` (the established convention for player time); duration stays `Beats`.
- **Send source becomes a sum.** An `AudioSend` today means "channels on a capture device." Extend its source to an enum — capture-channels **or** audio-layer(`LayerId`). This is the one model change that touches the existing audio crate: the analysis/modulation wiring must know that a send fed by a layer reads a precomputed curve, not a live ring.
- **Per-layer audio fields.** Which send the layer drives (`Option<AudioSendId>`) and a gain. Solo/mute already exist on `Layer` (`is_solo` / `is_muted`) and now carry audible meaning — see §5.

Audio layers ignore the visual half of `Layer` (opacity, effects, blend, compositing). That's fine and already true of the existing types — Video and Generator don't use every field either.

---

## 2. Build scope and the one fork

Split by what exists versus what's genuinely new.

**Reuse:**
- File import + decode to samples — the percussion pipeline (`percussion_analysis` / `percussion_settings`).
- Waveform rendering on a lane — `waveform_painter` / `WaveformRenderer`.
- Per-layer solo/mute fields — `Layer.is_solo` / `is_muted`.
- The modulation path downstream of a send — analysis worker, features, onset, `AudioSendId`.

**New — data model (§1):** `LayerType::Audio`, the audio clip field, the send-source enum, per-layer send + gain.

**New — offline modulation (§3):** analyze the decoded file once on import into a per-send feature curve; sample it at the playhead. This is *simpler* than the live path, not harder — no ring, no realtime worker, no glitch budget.

**New — realtime output (§4):** the output backend (the missing twin of `CaptureBackend`), the mixer, declicking, warp/time-stretch (Signalsmith first-class, varispeed fallback — §4.1), and the transport-follow sync policy. This is the only genuinely new realtime surface, and the only place the glitch risk lives.

**New — UI (§6) and serialization (§7).**

**Nearly free — export (§8):** audio layers feed the export's audio track through the `audio_muxer` that already exists.

**The fork:** modulation is offline (§3); output is realtime (§4). They are independent subsystems that happen to share a decoded buffer. A first cut could ship *either* alone — silent-but-modulating (offline only) or audible-but-unanalyzed (output only). Recommended order: offline modulation first (it's cheaper and it's the studio payoff), output second.

---

## 3. Offline modulation

On import, after decode, run the same feature extractors the live worker uses (band energy, RMS, Centroid, onset) across the whole file at a fixed hop, producing a per-send **feature curve**: a time-indexed array of `SendFeatures`. Store it as the clip's analysis artifact (§7).

At each content tick, for each audio layer with a send assignment, find the active clip, convert the playhead to a curve index (through the clip's warp ratio, §4.1, so a warped clip's features stay aligned to what's heard), and publish that `SendFeatures` into the same latest-wins slot the live worker would have written. **Downstream is byte-identical to live capture** — the modulation system never learns whether its features came from a ring or a table.

- **Look-ahead** is a per-binding offset added to the sample index — optional, defaults to zero, exposed later if wanted.
- **Reanalyze** is cheap and offline; changing analysis settings re-runs on the decoded buffer with no playback consequence.
- **Hop rate** matches the live worker's effective feature rate so a curve and a live send are interchangeable on the same modulation target.

This reuses the *extractors* (the feature seam in `analysis.rs`) without reusing the *realtime plumbing* around them — the extractors must stay callable on an arbitrary sample buffer, not only on the ring consumer. That's the one refactor the offline path asks of the existing audio crate.

---

## 4. Realtime output

The genuinely new subsystem. Manifold opens no output device today.

- **Output backend.** The twin of `CaptureBackend`: open an output device, RT callback **drains** a ring under the same contract capture obeys — no alloc, no lock, no log, no panic. Backend-neutral seam, same shape as capture, so a future Linux/Windows output drops in where capture's does.
- **Feeder.** The content thread, each tick, decodes/reads the active clip's sample window for every audible audio layer and pushes into the mixer. This is the realtime producer; its depth is the glitch budget (§9).
- **Mixer.** Sum audible layers → master, per-layer gain, mute (drop from mix), solo (audio-solo bus). Optional master gain/limiter. Feeds the output ring.
- **Declick.** Audio is unforgiving where video is not. Short fade ramps at every clip start/end, on pause, on loop-wrap, and on scrub-stop — a hard cut anywhere produces an audible click. This is not polish; it's correctness for audio.
- **Sync.** Transport stays master (it already is, and in a live show Ableton is master above it via OSC/MIDI). Audio *follows*: reseek the sample position on jump/loop/scrub, tolerate sub-frame drift. **Audio is never the master clock** — that would fight the Ableton-sync reality the whole instrument is built around.
- **Warp.** Each audio clip carries a **clip BPM**; the warp ratio is `project_tempo / clip_bpm`, so the clip tracks tempo and stays on the beat grid (Ableton's warp-marker model, minus the markers). Both the player and the offline analysis index through this ratio. The stretch sits behind a single `warp(samples, ratio)` seam with two implementations (§4.1).

### 4.1 Time-stretch — Signalsmith first-class, varispeed fallback

Pitch-preserving stretch is **first-class**, because the material that warps worst — drums, percussive transients — is exactly what drives the modulation and what the audience hears.

- **Signalsmith Stretch** (primary): MIT-licensed, modern, single-header C++ with good transient handling, designed for easy integration; wrapped via a Rust binding / thin FFI. MIT keeps it clean for a product. This is the default warp path.
- **Varispeed** (fallback): resample at the ratio — trivial, but pitch moves with tempo. Used only where Signalsmith is unavailable or as a cheap preview; inaudible near ratio 1.0.

Both take the *same* input — the clip-BPM ratio — so they're interchangeable behind `warp(samples, ratio)`. **Don't roll your own** phase vocoder: the naive version is ~200 lines and smears transients (the kicks you care about); the difficulty is transient handling and phase locking, which Signalsmith already solves.

---

## 5. Solo / mute semantics

Audio layers reuse `is_solo` / `is_muted` but they mean audible things now, parallel to how they mean visible things on video layers:

- **Mute** — drop this layer from the master mix. (Does it also stop feeding its send? Decision: no — muting the speakers shouldn't silence the modulation. Mute is an output-mix concept; a separate gesture, if wanted, disables the send.)
- **Solo** — an audio-solo bus independent of the visual solo bus. Soloing an audio layer must not blank the video, and soloing a video layer must not silence audio. Two buses, same field name, disjoint membership by `layer_type`.

---

## 6. UI

- **Drag-drop** an audio file onto a layer (or onto empty lane space) creates an `Audio` layer + clip. Import UX partly exists from percussion.
- **Layer header:** send dropdown ("which send this layer drives") + gain. Solo/mute already render.
- **Lane:** audio clips draw the waveform (`waveform_painter`) instead of a video thumbnail. Compositor skips `Audio` layers entirely — no visual output, no render cost.
- A **generic import+waveform path** decoupled from the percussion *trigger* pipeline: today decode/waveform are wired to onset→clip-trigger analysis. The audio layer wants "drop a file, get samples + a waveform + a feature curve" without the trigger-binding baggage.

---

## 7. Serialization (`manifold-io`)

- Persist the audio layer + clip: file path, `in_point`, duration, send assignment, gain. Relative-path handling mirrors `video_folder_path` / `relative_video_folder_path`.
- **Bundle decision:** embed the audio file in the V2 ZIP like video assets, or reference by path? (§10.)
- **Feature curve (§3):** cache the offline analysis artifact so a project reopens without re-decoding + re-analyzing every audio clip. Invalidate on file change or analysis-setting change.

---

## 8. Export integration

Audio layers feed the export's audio track through the `audio_muxer` (`manifold-media/src/audio_muxer.rs`) that already muxes AAC into exported video with `-itsoffset` alignment. Rendering a show to video *with its audio* becomes nearly free — a benefit to include in scope, not a cost. The mixer's master output is the export's audio bed.

---

## 9. The realtime risk

The content thread runs at 60 fps and, in the audible path (§4), *feeds* a continuous audio stream. A content-thread hitch that is invisible in video becomes an **audible glitch** — audio is far less forgiving of frame stutter than the compositor. The output ring between content and the RT callback needs enough decode-ahead depth to ride out a hitch, deeper than intuition suggests.

Note the offline modulation path (§3) is **immune** to this — it's a table lookup, not a realtime producer. The risk is confined to audio output. This is the strongest reason to treat the two halves as separable subsystems.

---

## 10. Decisions to make up front

1. **Decode strategy** — whole-file-to-RAM (simplest; a 5-min stereo stem ≈ 50 MB; percussion already decodes the whole file) vs. streaming. Recommend RAM for v1.
2. **Output device** — default output only, or a picker? Recommend default first.
3. **Bundle audio into the project ZIP** vs. reference by path (§7).
4. **Warp / time-stretch** — *decided:* warp is in scope, pitch-preserving (Signalsmith) first-class, varispeed fallback, clip-BPM ratio behind `warp(samples, ratio)` (§4.1). Not an open question; recorded here for visibility.
5. **Tap feedback loop** — playing audio out *while* tapping system audio re-enters the tapped mix → feedback. Needs a guard or a documented "don't route these together."
6. **Doubling the same stem** — two apps mixing is fine (macOS sums them like any players); the *only* hazard is Manifold and Ableton playing the **same material** expecting tight lock, because transport sync (OSC/MIDI) isn't sample-accurate and the two copies flam. Manifold audio stays sample-tight to Manifold's *visuals* (one playhead) and only needs rough alignment with Ableton, which transport sync gives. **Constraint: don't double the same sound in both.** This is a usage rule, not a subsystem cost.

---

## 11. Smaller, don't forget

- **Resampling + mono/stereo** — file rate ≠ output device rate ≠ analysis hop rate; resample on decode, downmix/upmix to the output channel count.
- **Output metering** — mirror the per-send capture meters for the master out.
- **Scrub audio** — do you hear grains while scrubbing (DAW-style) or silence? Recommend silent scrub for v1 (with declick on stop).
- **Curve/send slot pressure** — `MAX_SENDS = 16` is shared between live capture and audio layers; many audio layers each claiming a send competes with live sources.
