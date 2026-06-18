# Live Audio Triggers — Design & Phase Tracker

Status: **IN PROGRESS**. Branch `live-audio-triggers` (off `audio-clip-detection`).
Created 2026-06-18.

> **This doc is the cross-compaction tracker.** A fresh session reads §0 first, works
> the §Phase checklist, ticks boxes + commits as it goes, and updates §0 at the end.

## 0. CURRENT POSITION (read first, update last)

- **Done:** Phase 0 (setup). Phase 1 (core `TriggerRoute` + `AudioSend.triggers`).
- **Next action:** Phase 2 — engine evaluator + `LiveClipManager` one-shot fire.

## 1. What this is

Live audio input triggers visual clips, **no lookahead**. Feed audio in — separated
stems (kick/snare/bass on their own sends) or a full mix — and onsets fire fixed-length
one-shot clips on chosen layers, in real time, tuned live. A send→layer routing matrix.

Distinct from per-clip percussion detection (`audio-clip-detection`, offline, stem-
separated, BPM-aware). This is the **realtime** sibling: no Python, no stems separation,
no BPM — just edge-detect the transient that's already computed and fire a clip.

## 2. Why it's small (what already exists)

- **The detector already runs.** `SendFeatures.bands[band].transients` (0..1 decaying
  impulse) is produced per send, per analysis block, for audio modulation. `Full` band
  = whole-signal transient (the "Whole" source); `Low/Mid/High` = mix split. No new DSP.
- **The fires already reach the content thread.** `AudioModRuntime::update` assembles the
  `AudioFeatureSnapshot` (indexed by `AudioSetup::sends` order) and hands it to the engine
  each tick. The trigger evaluator reads the same snapshot — no new thread/channel.
- **The sink exists.** `LiveClipManager::trigger_live_clip` creates phantom clips on a
  layer (the MIDI NoteOn path). A transient has no NoteOff, so we add a **one-shot**
  variant that auto-commits after a fixed beat length, sharing clip-creation internals
  (refactor, do not copy-paste).
- **The widgets exist.** `BitmapSlider`+`SliderDragState`, `build_dropdown_trigger`,
  `DropdownPanel`/`DropdownContext` — the same controls the clip-detection inspector uses.

## 3. Settled decisions

1. **"Whole" = `AudioBand::Full` transient.** No dedicated detector — `Full` already runs it.
   Source is just `AudioBand` (Full = Whole; Low/Mid/High = mix split).
2. **A fire creates a fixed-length one-shot clip** on the target layer (no NoteOff exists).
3. **Routes are per-send, edited under the scope** in the Audio Setup modal (not a global
   table). The modal is the right home — the scope already draws the transient ticks you
   trigger on. A `⚡` on each send row lights when it has active routes.
4. **Quantize is opt-in, off by default** (off = tightest ~85 ms latency; on = snap to grid,
   adds up to one grid step of lag). Reuse the quantize-grid options from clip detection.
5. **Auto-route by name** — a send named "Kick" routes to a layer named "Kick" (reuse the
   name-match idea from `percussion` auto-route). Explicit routes override.

## 4. Architecture (by crate)

```
core      TriggerRoute type + Vec<TriggerRoute> on AudioSend       NEW (serialized)
playback  per-tick trigger evaluator (edge-detect + refractory)    NEW (the only real logic)
playback  LiveClipManager one-shot fire path                       EXTEND (share internals)
editing   SetAudioSendTriggersCommand                              NEW (mirrors SetClipDetectionConfig)
app       ContentCommand + dispatch + auto-route + state_sync view NEW/WIRE
ui        audio_setup_panel "Triggers" section + PanelActions      NEW (reuse widgets)
app       DropdownContext::AudioTrigger{Layer,Quantize}            NEW (mirror ClipDetect*)
```

### Core type (shape, refine in Phase 1)
```rust
pub struct TriggerRoute {
    pub enabled: bool,
    pub source: AudioBand,            // Full = "Whole"; Low/Mid/High = mix split
    pub target_layer: Option<LayerId>,// None = auto-route by name
    pub sensitivity: f32,             // 0..1 → transient fire threshold
    pub quantize: Option<Beats>,      // None = off (default)
    pub one_shot_beats: Beats,        // fire length
}
```
Reuse `AudioFeature{Transients, band}.extract(&SendFeatures)` to read the impulse — do not
re-index `bands` by hand.

### Evaluator (Phase 2) — the only real risk
Edge-detect on the decaying impulse with a per-route refractory. Lesson from
`[[audio-onset-detector]]`: you MUST fall below a re-arm floor (+ a min-gap) before firing
again, or a single decaying impulse machine-guns. The detector is already solved upstream;
this is just arm/fire/re-arm on its output. State (armed flag, last-fire beat) lives in the
evaluator (runtime, content thread), keyed by (send_id, source) — NOT in the serialized model.

## 5. Phase checklist (tick + commit as you go)

- [x] **Phase 0 — Setup.** Branch `live-audio-triggers`; this doc; memory `project_live_audio_triggers`.
- [x] **Phase 1 — Core.** `TriggerRoute` (`audio_trigger.rs`) + `AudioSend.triggers`
      (serde default/skip-empty) + `has_active_triggers`; sensitivity→threshold mapping;
      reuse `AudioFeature::extract` to read the impulse; 4 unit tests pass; clippy clean.
- [ ] **Phase 2 — Engine path.** Trigger evaluator (arm/fire/re-arm + refractory) in
      manifold-playback reading the per-send snapshot; `LiveClipManager` one-shot fire
      (refactor shared clip-creation out of `trigger_live_clip`). Prove with a hardcoded
      route + `println!` on a real stem before any UI. `cargo test -p manifold-playback --lib`.
- [ ] **Phase 3 — Editing command.** `SetAudioSendTriggersCommand` through EditingService
      (mirror `SetClipDetectionConfigCommand`). Test.
- [ ] **Phase 4 — App wiring.** `ContentCommand` variant + dispatch; auto-route-by-name on
      add/edit; `state_sync` builds the per-send route view + `⚡` flag.
- [ ] **Phase 5 — UI.** `audio_setup_panel` "Triggers — <send>" section: route rows
      (`[enable] source [sensitivity slider] → [layer ▼] [quantize ▼]`) reusing BitmapSlider
      + build_dropdown_trigger; new PanelActions; `DropdownContext::AudioTrigger{Layer,Quantize}`
      in ui_root; ui_bridge dispatch. `⚡` per send row.
- [ ] **Phase 6 — Polish + ship.** Auto-route UX, edge cases (no layers, send delete with
      routes), clippy `-D warnings` (core/playback/editing/ui/app), focused tests, commit/push.

## 6. Invariants / guardrails

- Audio stays on the **perform surface**, NOT graph nodes (`[[audio-stays-on-perform-surface]]`).
- All model mutations through `EditingService` — UI sends a command, never writes the model.
- No new `Arc<Mutex>` shared state; evaluator state is owned by the content thread.
- No per-frame allocation on the engine tick — the evaluator runs every content tick.
- Refactor for reuse; **do not copy-paste** `trigger_live_clip` for the one-shot path.
- Don't build this on a future UI API — current widgets only; Phase 2b of the UI overhaul
  will migrate this panel with the rest (see `docs/UI_ARCHITECTURE_OVERHAUL.md`).
