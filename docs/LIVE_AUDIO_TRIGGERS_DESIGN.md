# Live Audio Triggers — Design & Phase Tracker

Status: **IN PROGRESS**. Branch `live-audio-triggers` (off `audio-clip-detection`).
Created 2026-06-18.

> **This doc is the cross-compaction tracker.** A fresh session reads §0 first, works
> the §Phase checklist, ticks boxes + commits as it goes, and updates §0 at the end.

## 0. CURRENT POSITION (read first, update last)

- **Status: FEATURE COMPLETE (pending runtime verification).** Phases 0–6 all done. Builds +
  clippy clean across touched crates; core (275), io (17), ui (293), playback (103+18),
  editing (7) tests green. Amber send-label cue marks sends with active routes.
- **The ONE remaining task is Peter's:** run the app, point a real stem at a send, enable a
  route, confirm onsets fire clips on the target layer and the latency/feel is right. Watch
  for: octave/feel of sensitivity steps, layout/spacing of the Triggers rows at the modal
  width, and whether the ~85 ms detector latency feels tight enough.
- **Deferred (documented, not blocking):** per-route one-shot length control (model supports
  it, defaulted to 1 beat); stopped-transport live triggering (v1 fires in `tick_playing`).

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
4. **Quantize = the project quantize_mode**, reused verbatim from the MIDI clip-launch path
   (Off/¼/Beat/Bar). REVISED from a per-route grid: audio fires pass
   `event_absolute_tick = get_current_absolute_tick()` + `midi_note = -1` into the *same*
   proven `trigger_live_clip` path MIDI uses, so there is zero new timing math and no
   tick-domain risk (a timing bug becomes the show). The per-route `quantize` field was
   dropped. Stopped-transport live triggering is deferred (beat-based expiry needs a running
   clock); v1 fires in `tick_playing` — which is exactly when you perform (transport follows
   Link/MIDI clock from the incoming music).
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
    pub one_shot_beats: Beats,        // fire length (quantize = project quantize_mode)
}
```
Reuse `AudioFeature{Transients, band}.extract(&SendFeatures)` to read the impulse — do not
re-index `bands` by hand.

### Evaluator (Phase 2) — `live_trigger.rs`, `LiveTriggerState`
Pure edge-detection on the impulse: fire on the rising edge above the route threshold, then
re-arm only once the impulse falls below `threshold * REARM_RATIO`. The upstream detector
already enforces one-impulse-per-onset (its own ~106 ms refractory, `[[audio-onset-detector]]`),
so the evaluator needs no time/beat refractory — just the arm flag prevents multi-firing on a
single impulse's plateau. Tempo-independent. State (armed flag) is runtime, content-thread,
keyed by `(send_id, source)` — NOT serialized. `evaluate(&snapshot, &audio_setup) -> Vec<FireRequest>`
is a pure decision (unit-tested without the engine); the engine resolves each `FireRequest`'s
layer + calls the fire sink.

### Sink (Phase 2) — `LiveClipManager::fire_layer_oneshot`
Resolves the target layer's content (`resolve_layer_live_content`: generator vs first
`source_clip_id`, shared with the MIDI from-layer path — no copy-paste) and calls the existing
`trigger_live_{clip,generator_clip}`. A new per-clip expiry map (`end_beat`, layer) ends the
one-shot when `current_beat` passes its end — the only state MIDI doesn't already have, since a
transient has no NoteOff. Engine runs expiry + fire in `tick_playing` after modulation eval.

## 5. Phase checklist (tick + commit as you go)

- [x] **Phase 0 — Setup.** Branch `live-audio-triggers`; this doc; memory `project_live_audio_triggers`.
- [x] **Phase 1 — Core.** `TriggerRoute` (`audio_trigger.rs`) + `AudioSend.triggers`
      (serde default/skip-empty) + `has_active_triggers`; sensitivity→threshold mapping;
      reuse `AudioFeature::extract` to read the impulse; 4 unit tests pass; clippy clean.
- [x] **Phase 2 — Engine path.** `live_trigger.rs` `LiveTriggerState::evaluate` (pure
      edge-detect → `FireRequest`); `LiveClipManager::fire_layer_oneshot` (reuses the MIDI
      trigger primitives via shared `resolve_layer_live_content`, also refactored into the
      MIDI from-layer path — no copy-paste) + `expire_due_oneshots`; engine
      `tick_audio_triggers` (borrow-split fire + expiry) wired into `tick_playing` step 3b;
      `resolve_trigger_layer` (explicit + auto-route-by-name). 5 evaluator + 4 sink tests;
      full playback suite (103+18) + clippy clean. **Runtime verification on a real stem is
      still pending** (needs the app; can't run headless here).
- [x] **Phase 3 — Editing command.** `SetAudioSendTriggersCommand` in editing's
      `commands::audio_setup` (mirrors `SetAudioSendAnalysisCommand`; captures the whole
      route vec → one undo step). Round-trip test; clippy clean. (Also fixed a pre-existing
      `AudioClipDetection` literal missing `last_counts` in `clip_detection.rs`.)
- [ ] **Phase 4 — App wiring.** `ContentCommand` variant + dispatch; auto-route-by-name on
      add/edit; `state_sync` builds the per-send route view + `⚡` flag.
- [x] **Phase 4+5 — UI + app wiring.** `audio_setup_panel` "Triggers — <send>" section under
      the scope: four band rows `[enable swatch][band][−] sens% [＋] -> [layer ▼]`, using the
      panel's native idioms (gain-style stepper, channel-style dropdown) — no drag plumbing,
      no new framework, glyphs proven in-atlas. `TriggerRouteRow` on `AudioSendRow`; new
      `PanelAction::AudioTrigger{Toggled,SensitivityStep,LayerClicked,SetLayer}`;
      `DropdownContext::AudioTriggerLayer` + `audio_trigger_layers` cache in ui_root;
      `AudioSend::triggers_with_route` find-or-create helper drives the dispatch →
      `SetAudioSendTriggersCommand`; state_sync builds the rows + caches candidate layers.
      ui (293) + editing (7) tests green; workspace clippy clean. **Deferred:** per-route
      one-shot length control (model supports it, defaulted 1 beat); the `⚡` send-row badge.
- [x] **Phase 6 — Polish + ship.** Amber send-label cue for sends with active routes
      (glyph-free, no layout churn). Edge cases handled: no candidate layers → dropdown is
      Auto-only; missing/orphaned target layer → reads "Auto"; send delete drops routes with
      the send (RemoveAudioSendCommand). Clippy clean (core/editing/playback/ui/app); io +
      core serialization round-trips green (empty triggers skip-serialize, old projects
      byte-identical). Committed + pushed; memory updated.

## 6. Invariants / guardrails

- Audio stays on the **perform surface**, NOT graph nodes (`[[audio-stays-on-perform-surface]]`).
- All model mutations through `EditingService` — UI sends a command, never writes the model.
- No new `Arc<Mutex>` shared state; evaluator state is owned by the content thread.
- No per-frame allocation on the engine tick — the evaluator runs every content tick.
- Refactor for reuse; **do not copy-paste** `trigger_live_clip` for the one-shot path.
- Don't build this on a future UI API — current widgets only; Phase 2b of the UI overhaul
  will migrate this panel with the rest (see `docs/UI_ARCHITECTURE_OVERHAUL.md`).
