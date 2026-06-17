# Real-Time Audio Infrastructure — Design Doc

<!-- index: The real-time audio stack: capture (cpal), the off-RT analysis worker, the planned native CoreAudio device directory (channel names, stable UIDs, hot-plug), and the audio-settings UX. Threading, data flow, perf budget, and the phased build plan. -->

The end-to-end real-time audio stack that feeds the instrument: how samples get in, how they become control signals, and how the device/channel metadata around them is surfaced and kept honest. This is the *infrastructure* layer. The feature that consumes it — driving effect sliders from audio — lives in [Audio Modulation — Design Doc](AUDIO_MODULATION_DESIGN.md); read that for the modulation source, the per-slider drawer, and the v2 pitch-tracking intelligence. This doc owns capture, analysis, the device directory, threading, and performance.

Status: **implemented (2026-06-17).** Phases 1–6 and the Phase 7 cross-platform fallback have all shipped on `audio-modulation`: the native CoreAudio device directory (channel names, UIDs, liveness, subdevice grouping, hot-plug), UID-based identity + legacy migration, names/grouping in the UI, stage reliability (hot-plug rebuild, mic TCC, offline indicators), perf hygiene, and the full audio-settings UX (rename, identity color, mono/stereo, per-send meter, delete-in-use confirm). Still future by design: native Linux/Windows directory backends (7.1/7.2) and the output-channel/subdevice tree view (7.3). The §9 plan below records what was built; "planned" wording in earlier sections is historical design context.

---

## 1. The whole stack, for the instrument

You route audio into Manifold — a kick on its own channel, a bassline on another — and those signals drive visuals in real time. For that to feel like an instrument rather than a config screen, three things have to be true:

1. **The routing speaks your language.** You pick "BlackHole ▸ BH_IN_L," not "Channel 1," and you name the result "Kick," not "Audio 1."
2. **It survives the stage.** A device that vanishes mid-set is *noticed*, not silently dead. A saved show reopens on the right device even after you've replugged everything.
3. **It costs almost nothing.** Audio analysis never competes with the render. The show's frame budget is sacred.

The infrastructure below exists to make those three true. Everything is a consequence of one of them.

## 2. Architecture at a glance

```text
 CoreAudio device  ── cpal stream ──▶  capture ring  ──drain──▶  analysis worker  ──frames──▶  content thread
 (HAL: names, UID,      (RT OS thread,     (SPSC f32,            (own OS thread:              (60fps tick:
  hot-plug events)       lock-free push)    interleaved)          deinterleave→downmix→FFT)    FeatureReader.latest())
        │                                                                                            │
        └────────── AudioDeviceDirectory (native query + listeners) ──────────▶ UI / save format ───┘
```

Two independent data paths:

- **The sample path** (left-to-right, top): real audio samples flow RT thread → ring → worker → content thread. This is the hot path and it is already built.
- **The metadata path** (bottom): device list, channel names, stable IDs, and hot-plug events flow from the native directory to the UI and the save format. This is the new work.

The split matters: the sample path stays on cpal (portable, proven). Only the metadata path drops to native CoreAudio, because that is the only place the information we want actually lives.

## 3. The sample path (built)

### 3.1 Capture — `manifold-audio::capture`

[crates/manifold-audio/src/capture.rs](../crates/manifold-audio/src/capture.rs). Opens a system input device via cpal at its **native sample rate and channel count** (no format conversion), and streams Float32 interleaved samples into a lock-free SPSC ring (`ringbuf`, 2 seconds deep).

The cpal callback runs on a real-time OS thread and obeys the RT contract: **no alloc, no lock, no log, no panic** — only a `push_slice` into the ring, with an atomic overflow counter on the rare full ring. This is correct and should not be touched.

What capture exposes today: `list_devices()` (name + default flag), `device_channels()` (a bare count), `sample_rate()`, `channels()`. The limitation that drives Phase 1: **cpal's device model is counts and formats only — it has no concept of a channel name or a stable device identity.**

### 3.2 Analysis worker — `manifold-audio::analysis`

[crates/manifold-audio/src/analysis.rs](../crates/manifold-audio/src/analysis.rs). A dedicated OS thread (`manifold-audio-analysis`) owns the capture ring's consumer and turns samples into per-send **feature frames**:

```text
drain ring → deinterleave → downmix each send to mono → accumulate to FFT_SIZE → FFT → band energy + RMS
```

- **Window:** 1024 samples, non-overlapping (~21ms at 48kHz — finer than the 16ms content tick).
- **Per send:** selected channels are averaged to mono (`downmix`), accumulated, and once a full window is ready, run through a Hann-windowed FFT into three perceptual bands (low/mid/high) plus an overall RMS amplitude.
- **Output:** a `Copy`, fixed-size `FeatureFrame` (`[SendFeatures; MAX_SENDS=16]`) published latest-wins through a second SPSC ring. No `Arc<Mutex>`, no locks on the read path.
- **Send identity** is by *index* (position in the `sends` slice), not `AudioSendId` — the id↔index mapping is the wiring layer's job, keeping this crate free of `manifold-core` types and unit-testable in isolation.

The frame struct is built around a **feature seam**: adding onset/pitch (v2) is a new field on `SendFeatures` plus an extractor in the worker loop — and it must reuse the *same* FFT buffer, never run a second transform.

### 3.3 The read end — content thread

`FeatureReader::latest()` drains the output ring keeping the newest frame, and caches it so a tick with no new frame still reports the last value (modulation holds, doesn't drop to zero). This is all the 60fps tick does for audio: one ring drain + one ~256-byte struct copy.

## 4. The metadata path — `AudioDeviceDirectory` (planned)

The new abstraction. cpal can't express channel names, stable identity, or liveness, so the metadata path drops to the native HAL behind a trait — leaving the sample path on cpal and keeping OS-specific code quarantined for future Linux/Windows backends (the same backend-neutral discipline as `manifold-gpu`).

```rust
// manifold-audio
pub struct ChannelInfo {
    pub index: u16,
    pub name: Option<String>,   // None → display "Channel N"
}

pub struct DeviceInfo {
    pub uid: String,            // stable identity — persist this, not the name
    pub name: String,           // display + fallback match
    pub is_default: bool,
    pub is_alive: bool,
    pub channels: Vec<ChannelInfo>,
    // (optional, Phase 2.3) subdevice grouping for the channel dropdown
}

pub trait AudioDeviceDirectory {
    fn list_input_devices(&self) -> Vec<DeviceInfo>;
    fn subscribe(&self, on_change: Box<dyn Fn() + Send>);  // hot-plug → UI refresh
}
```

### 4.1 macOS implementation — `CoreAudioDirectory`

CoreAudio is Apple's native audio API (the HAL — Hardware Abstraction Layer); cpal is a thin wrapper over it on macOS. We query it directly via `objc2`/`core-foundation` (already dependencies of this crate):

| Information | CoreAudio property |
|---|---|
| Stable device identity | `kAudioDevicePropertyDeviceUID` |
| Liveness | `kAudioDevicePropertyDeviceIsAlive` |
| True input channel count | `kAudioDevicePropertyStreamConfiguration` (input scope) |
| Per-channel name | `kAudioObjectPropertyElementName` (input scope, per element) |
| Subdevices of an aggregate | `kAudioAggregateDevicePropertyFullSubDeviceList` |
| Hot-plug / default-change | listeners on `kAudioHardwarePropertyDevices`, `…DeviceIsAlive`, `…DefaultInputDevice` |

The channel names are **the same labels Audio MIDI Setup shows**, because that is where they are stored — you set them there, CoreAudio holds them, and we read them. Missing name → `None` → "Channel N".

### 4.2 Why not "mirror Ableton"

Ableton's input/output routing labels are partly these same CoreAudio names and partly Ableton's *own* relabels, stored privately in Live's preferences and Set, with no public API. We can't mirror Ableton's labels. We can mirror their *source* — Audio MIDI Setup — which matches Ableton for any channel not manually renamed inside Live (i.e. most of them). Chasing Ableton's private labels would be reverse-engineering a moving target for marginal gain.

### 4.3 True channel count fixes a real bug

cpal's `default_input_config().channels()` can **under-report** on aggregate devices, so today's dropdown can be short channels you actually have. `kAudioDevicePropertyStreamConfiguration` gives the real count. So the directory isn't only cosmetic — it makes the routing list *correct*.

## 5. Stable identity & persistence (planned)

We currently persist `device_name: Option<String>`. Names are neither stable nor unique (two "BlackHole 2ch," renamed aggregates, reorder on reconnect), so a saved show can reopen pointing at the wrong device or none.

Fix: persist the **UID**, keep the name as display + fallback match. On load, resolve UID → live device; if absent, fall back to name match, and if still unresolved, keep the routing *intent* and mark it unresolved rather than dropping it. This touches the save format (`manifold-io`) and needs a versioned migration (name-only → resolve to UID on load), so the decision is locked **before** shows are saved with the new routing.

## 6. Reliability on stage (planned)

- **Hot-plug / device loss.** `subscribe()` wires CoreAudio listeners; on change we refresh the device list live, mark a dead routed device, and apply a defined fallback policy (hold last / drop to default / surface error). This is the strongest reason to go native beyond names — a vanished device should never be a silent failure mid-set.
- **Mic permission (TCC).** Capturing the built-in mic needs `NSMicrophoneUsageDescription` in the plist and a runtime grant; BlackHole and most virtual devices don't. Without it the mic returns silent zeros. Surface a clear "mic blocked" state.
- **Device-state in the UI.** Dead/offline indicator on the device row and any affected sends.

## 7. The audio-settings UX

The Audio Setup panel ([crates/manifold-ui/src/panels/audio_setup_panel.rs](../crates/manifold-ui/src/panels/audio_setup_panel.rs)) is a **modal** — and stays modal. This is settings, configured deliberately, not a live-performance surface; dimming the show behind it is correct. The dropdown machinery is in [crates/manifold-app/src/ui_root.rs](../crates/manifold-app/src/ui_root.rs#L1236-L1268).

The organizing idea: **a send is the vocabulary the rest of the instrument speaks in.** Anything that makes a send legible pays off everywhere it's referenced (notably the modulation drawer).

Planned UX (all post-Phase-2):

- **Rename + color sends** — "Kick," not the auto-assigned "Audio N"; a per-send color carried into the modulation drawer so a driven slider is visibly tied to its source.
- **Channel dropdown shows names, grouped by subdevice** — `BlackHole ▸ BH_IN_L / BH_IN_R`, `MacBook ▸ Mic`. A flat 64-item list on a big aggregate is unusable.
- **Stereo / paired channels** — a mono/stereo toggle per send. `SendSpec.channels` already supports multiple channels (`downmix` averages them); this is just the UI affordance plus a 2-channel default.
- **Per-send meter** — reads the existing `FeatureFrame.amplitude`, shipped via the normal `ContentState` snapshot (no new path, no GPU). Paired with gain trim it becomes the calibration surface: set a send so it actually swings 0–1 on your material.
- **Delete-in-use warning** — show "drives N params" on the row and confirm before deleting, so a bound send isn't silently severed.

Explicitly **not** doing: dockable/non-dim modality (it's settings); per-send smoothing/attack-release (deferred — and if added, it likely belongs in the drawer, not here).

## 8. Performance — the budget is sacred

The audio subsystem is low-impact **by construction**, and the metadata work doesn't change that (it's all device-open / panel-build, never per-frame). Verdict:

- **GPU: zero.** Nothing in the audio path touches the GPU. It never competes with the 4.5–5.5ms frame budget.
- **Content thread (60fps tick): microseconds.** One SPSC drain + one ~256-byte `FeatureFrame` copy. No FFT, no alloc, no lock on the read path.
- **Analysis worker: ~1% of one core** at the 16-send cap (one 1024-pt FFT per send per ~21ms window ≈ 750 FFTs/sec total). The right thread to spend it on, isolated from content and render.

Discipline to keep it there:

- The meter **must** reuse `FeatureFrame.amplitude` — never add a separate readback path.
- Don't run the worker with zero sends / no device — it currently idle-wakes ~500×/sec regardless ([analysis.rs run loop](../crates/manifold-audio/src/analysis.rs)). Gate its existence on "device open AND ≥1 send."
- Minor cleanup (worker thread, off the hot path): `drain_and_analyze` takes `carry` out leaving it zero-capacity, then `extend_from_slice` reallocates it every drain — a buffer swap removes the churn.
- Cap channel count sanely: the capture ring is `2s × SR × channels`; a 64-ch aggregate at 96kHz is ~49MB. One-time alloc, but an exotic device shouldn't surprise us.
- v2 onset/pitch extracts from the existing FFT buffer — no second transform.

## 9. Build plan — phases, steps, tasks

**Status: Phases 1–6 ✓ and Phase 7's fallback ✓ shipped 2026-06-17. Remaining: 7.1 / 7.2 / 7.3 (future, by design).**

Sequenced by dependency. **Critical path: 1 → 2 → (3 ∥ 4).** Phase 3's save-format decision must land before shows are saved with the new routing. Phase 5 items are independent once Phase 2 lands; Phase 6 can go anytime.

### Phase 1 — CoreAudio device directory (foundation)
- **1.1 Define the trait** (`manifold-audio`): `ChannelInfo`, `DeviceInfo`, `AudioDeviceDirectory`; home in `directory.rs`; stay `manifold-core`-free.
- **1.2 macOS impl** `CoreAudioDirectory`: device list + UID + liveness; per-channel names; true channel count (replaces `device_channels`); name fallback.
- **1.3 Verify**: unit-test decode helpers; manually diff output against your aggregate in Audio MIDI Setup.

### Phase 2 — Names into the UI
- **2.1** Plumb `DeviceInfo` to the app; replace the `list_devices`/`device_channels` call sites in [ui_root.rs:1236-1268](../crates/manifold-app/src/ui_root.rs#L1236-L1268); cache the selected device's info.
- **2.2** Channel dropdown + send row show channel **names** (fallback "Channel N").
- **2.3** Group the dropdown by subdevice (needs subdevice list in `DeviceInfo`; defer if scope tightens).

### Phase 3 — Stable identity & persistence
- **3.1** Route on UID, not name (`manifold-core` `AudioSend`/device selection); resolve UID → device at open, name fallback.
- **3.2** Save-format migration (`manifold-io`): add UID, versioned load migration, defined behavior when the device is absent.

### Phase 4 — Reliability
- **4.1** Hot-plug listeners (`subscribe()`): live list refresh, mark dead device, fallback policy.
- **4.2** Mic permission (TCC): plist key, runtime check, "mic blocked" state.
- **4.3** Device-state indicators in the panel.

### Phase 5 — UX
- **5.1** Rename + color sends (carry into the modulation drawer).
- **5.2** Stereo / paired channels (UI toggle over existing multi-channel `downmix`).
- **5.3** Per-send meter (reuse `FeatureFrame.amplitude` via `ContentState`).
- **5.4** Delete-in-use warning ("drives N params").

### Phase 6 — Perf hygiene
- **6.1** Don't run the worker with zero sends / no device.
- **6.2** `carry` buffer swap (remove per-drain realloc).
- **6.3** Sane channel cap for exotic aggregates.
- **6.4** v2 onset/pitch reuse the existing FFT buffer.

### Phase 7 — Future / cross-platform
- **7.1** `PipeWireDirectory` / JACK (port names map well).
- **7.2** `WasapiDirectory` (endpoint names; leans on the `None → "Channel N"` fallback).
- **7.3** Output-channel view + full subdevice tree — only if ever needed (we consume input only; likely never).

## 10. Related docs

- [Audio Modulation — Design Doc](AUDIO_MODULATION_DESIGN.md) — the feature this infrastructure feeds: modulation source, per-slider drawer, v2 pitch tracking.
- [Overlay System Design](OVERLAY_SYSTEM_DESIGN.md) — the overlay stack the Audio Setup modal lives in.
- [Content Thread](../CLAUDE.md) / two-thread model — where `FeatureReader` is read and `ContentState` snapshots ship to the UI.
