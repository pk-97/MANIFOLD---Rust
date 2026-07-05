# Live Audio Triggers — Design & Phase Tracker

Status: **SHIPPED — phases 0–7 done, fires + renders end-to-end (verified live
2026-06-19 per §0).** The only outstanding item is Peter's live feel-check on real
stems (an L4 check). Header corrected 2026-07-05 (it still read "IN PROGRESS").
Branch `live-audio-triggers` (off `audio-clip-detection`). Created 2026-06-18.

> **This doc is the cross-compaction tracker.** A fresh session reads §0 first, works
> the §Phase checklist, ticks boxes + commits as it goes, and updates §0 at the end.

## 0. CURRENT POSITION (read first, update last)

- **Status: FIRES + RENDERS end-to-end (verified live 2026-06-19).** Phases 0–6 done. The
  render bug is fixed (see the §3.4 note + `[[live-audio-triggers]]` memory): a one-shot now
  snaps on the **beat clock** (`beat_stamp = current_beat`, `event_absolute_tick = -1`), not
  `get_current_absolute_tick()` — that returns a frame counter with no external MIDI clock, so
  the slot's window looked long-expired and `start_clip` never ran (black viewport). Regression
  test `fire_oneshot_starts_at_playhead_when_abs_tick_is_frame_based`.
- **Phase 7 (legibility & tuning upgrades, §7) — DONE 2026-06-19.** All four shipped: per-row
  firing flash + level/threshold meter (7.1–7.3), per-send dB input floor/squelch (7.4), per-route
  one-shot length + Whole grouping + dropped arrow (7.5). The panel now reads as an instrument:
  you can see triggers fire, tune by watching the level cross the line, and squelch quiet bleed.
  Floor control is a dB stepper (the freq-axis line in the first sketch was wrong — see §7 U3).
  Remaining: Peter's live feel check (flash latency, floor sweep, length range).
- **Detector upgraded to SuperFlux 2026-06-19.** The shared transient detector (`reduce_send`/
  `band_reduce`) was energy-over-running-mean, which fired on amplitude wobble in busy mixes
  ("rapid/overly sensitive", worst on Whole). Replaced with **SuperFlux** — spectral flux + a
  frequency max-filter that suppresses vibrato/pitch-slide false positives. Same `bands[].transients`
  field, so triggers + Transient modulation + scope all inherit it. Per-route min-gap idea dropped
  (SuperFlux's built-in ~32 ms refractory covers it). PENDING Peter's A/B on real stems. Full
  rationale + rejected-approach history: `[[audio-onset-detector]]`.
- **Deferred (documented, not blocking):** stopped-transport live triggering (v1 fires in
  `tick_playing`). Per-route one-shot length is now IN scope (§7 upgrade 4).

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
4. **Quantize = the project quantize_mode**, reused from the MIDI clip-launch path
   (Off/¼/Beat/Bar). **CORRECTED 2026-06-19:** a live audio fire has NO musical tick (it fires
   in real time at the playhead), so it passes `beat_stamp = current_beat` + `event_absolute_tick
   = -1` + `midi_note = -1` into the *same* `trigger_live_clip` path MIDI uses — routing through
   the beat-domain snap. The earlier `event_absolute_tick = get_current_absolute_tick()` was the
   render bug: that resolver returns a frame counter without an external MIDI clock, producing a
   start_beat unrelated to the playhead (a timing bug became the show). The per-route `quantize`
   field stays dropped. Stopped-transport live triggering is deferred (beat-based expiry needs a
   running clock); v1 fires in `tick_playing` — which is exactly when you perform (transport follows
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

## 7. Legibility & tuning upgrades (Phase 7)

The panel fires and renders, but it reads as a config form. Four upgrades turn it into an
instrument you can tune **by eye while the track plays**. One goal: *what you see is what you
detect on, and you can see it fire.*

### The signal path (where each upgrade lives)

```
 PER SEND  (once — "what you see = what you detect")
 ─────────────────────────────────────────────
   capture + layer taps
        │
        ▼
   [ input gain ]                                         (exists: send gain)
        │
        ▼
   [ floor / gate ] ◄── draggable line on spectrogram     UPGRADE 3  (per-bin spectral floor)
        │
        ▼
   [ 4096-pt VQT ] ──┬──► SPECTROGRAM  (what you see)
                     │
                     └──► Low · Mid · High · Whole  (SLICES of the same gated column)
                                   │
 PER ROW  (post — the firing decision)
 ─────────────────────────────────────
                                   ▼
                          [ sensitivity threshold ] ◄── live level meter   UPGRADE 2
                                   │ crosses → FIRE   ──► row flash         UPGRADE 1
                                   ▼
                          one-shot (length) ──► target layer                UPGRADE 4
```

The floor is **one control per send**, applied to the single VQT column before display AND
before band slicing AND before features — it is NOT per band (there is one 4096-pt VQT per
send; bands are reductions of it; `[[audio-vqt-feature-unification]]`). The per-row sensitivity
is the only *post*-analysis control: it does not change the spectrogram, only the fire decision.

### Upgrade 1 — Firing flash (do first; cheapest, highest leverage)
Each trigger row pulses in its band colour the instant it fires. Proves the loop is alive and
lets you confirm a band is catching the hits without looking away at the output. Needs: the
engine surfaces *which routes fired this tick* to the UI (a per-send, per-band fire pulse in
the `ContentState` snapshot, decaying like the transient impulse), and the panel row draws a
colour flash driven by it. No model change — pure runtime/UI.

### Upgrade 2 — Level + threshold meter per row
Replace the blind `50%` with a live horizontal meter: the band's current transient level, with
the sensitivity **threshold marked as a line**. Tuning becomes visual — "kick peaks clearly
cross the line, snare bleed doesn't." The `%` stepper stays (sets the threshold line); the meter
just shows level-vs-line. Needs: per-send per-band level in the snapshot (same plumbing as the
flash). No model change.

```
 NOW:        [■] Low    - 50% +   →    Kick ▼

 UPGRADED:        level ▕▆▆▆▅▃▁··········┊······▏   ┊ = threshold (the % sets it)
             [■]⚡ Low    ▕▔▔ 50% ▔▏   1 beat ▼    Kick ▼
                  │                     │
               flash on fire       one-shot length
```

### Upgrade 3 — Input floor / spectral gate (most code — touches the analyzer) — SHIPPED
A per-send **floor** (dB): VQT bins below it are zeroed *before* the column is displayed, sliced,
or feature-extracted. Acts as a squelch — quiet bleed/noise between hits can't trigger and
doesn't clutter the view. NOTE: onset detection keys on *change*, not absolute level, so the
floor is a squelch (mute-below), not "only loud things have onsets."

**Control = a dB stepper, NOT a draggable freq line.** The first sketch (a draggable horizontal
line on the spectrogram) was wrong: the spectrogram's vertical axis is **frequency**, so a
horizontal line sets a frequency, not a loudness floor. Loudness is the *colour* axis, which has
no spatial handle. So the floor is a **"Floor [−] val [＋]" stepper in the scope title** (reads
"Off" by default); raise it and the quiet wash blacks out (bins gate to ‑inf dB) and the onset
ticks thin to just the hits — same outcome, honest gesture.

Implemented: `AudioSend.floor_db` (default OFF sentinel, skip-serializes); `SetAudioSendFloorCommand`
(live, like gain); `StreamingSendAnalyzer` gate after `form_tilted_column` on raw magnitude vs
`10^(dB/20)`, zeroing both `vqt_raw` (scope) and `state.col` (features) — one gate, both consumers.

```
 BEFORE (Off):                          AFTER (Floor −48 dB):
 20k ┤ ░░░  ░░   ░░░░   ░░ ░░░          20k ┤
  1k ┤ ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓           1k ┤   ▓        ▓▓
 100 ┤ ▒▒▒▒▒▒ wash ▒▒▒▒▒▒▒▒           100 ┤ ▓   ▓  ▓      ▓   (only the loud hits survive)
     └────────────────────► t             └────────────────────► t
        ▎▎▎▎▎▎▎▎▎▎▎▎ ticks                    ▎    ▎    ▎  ticks thinned
```

### Upgrade 4 — Clarify "Whole" + expose one-shot length, drop the `→`
"Whole" is the *parent* (full-mix transient), not a peer of L/M/H — separate or relabel
("Full mix") so the relationship reads. Expose **per-route one-shot length** (model already has
`one_shot_beats`, defaulted 1 beat) as a small stepper/dropdown — flash-vs-sustain matters live.
Drop the decorative `→` between sensitivity and target.

### Phase 7 checklist (tick + commit as you go)
- [x] **7.1–7.3 Row meter + firing flash (Upgrades 1+2).** Done as one pure-UI slice: no new
      engine channel — driven by the selected send's per-band transient impulses already on
      `ContentState.spectrogram_features` (what-you-detect-on). `TriggerRouteRow.threshold`;
      per-row meter nodes + flash via `update_trigger_levels`, fed in `app_render`.
- [x] **7.4 Input floor (Upgrade 3).** `AudioSend.floor_db` (default OFF, skip-serialize, no
      migration needed) + `SetAudioSendFloorCommand` + analyzer per-bin gate (pre-display/slice/
      features) + **dB stepper** in the scope title (not the freq-line first sketched — see above).
- [x] **7.5 Whole/length/arrow (Upgrade 4).** One-shot length stepper per row
      (`AudioTriggerLengthStep`, musical halve/double 1/4..16); faint group divider after the
      Whole row; dropped the `→`.
- [x] **7.6 Ship.** Builds + clippy clean; core/io/ui/audio/editing tests green; floor
      serde round-trip + analyzer gate tests; committed + pushed; §0 + memory updated.
