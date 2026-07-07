# Live Audio Triggers — Design & Phase Tracker

Status: **SHIPPED — phases 0–7 done, fires + renders end-to-end (verified live
2026-06-19 per §0).** The only outstanding item is Peter's live feel-check on real
stems (an L4 check). Header corrected 2026-07-05 (it still read "IN PROGRESS").
Branch `live-audio-triggers` (off `audio-clip-detection`). Created 2026-06-18.

**§8 addendum (2026-07-07): Param triggers — audio fires the Trigger controls.
P1+P2+P3a LANDED on main 2026-07-07 @ `3089e0a3`** (merged from
`wave/param-triggers`, full workspace + gpu-proofs + clippy gate green pre- and
post-merge): the engine fires, the renderer feeds generators AND effect chains,
Strobe proves the effect-side reachability at L1. **P3b (the UI drawer to
configure it) is SCOPED, not built** — see §8.4 P3b for why it's bigger than the
original brief and what a follow-up session needs to read first. Same evaluator
machinery, new target: instead of firing one-shot clips, a transient pulses a
playing generator's trigger response (and `is_trigger` cards on effects).
Peter's ask, verbatim: *"if Trigger is enabled we can choose if we want rising
clip edge (default) OR the transient trigger OR both."*

> **This doc is the cross-compaction tracker.** A fresh session reads §0 first, works
> the §Phase checklist, ticks boxes + commits as it goes, and updates §0 at the end.

## 0. CURRENT POSITION (read first, update last)

- **§9 unification U-P1 + U-P2 SHIPPED 2026-07-07** (worktree
  `.claude/worktrees/unified-trigger-mods`): the trigger IS an audio mod now, top
  to bottom. `AudioTriggerMod`/`PresetInstance.audio_trigger` are gone; a
  trigger-gate card's config is a `ParameterAudioMod` with `trigger_mode:
  Option<TriggerFireMode>`, reachable through the exact same "A" drawer every
  other audio mod uses, plus one trailing Mode row. See §9.2 for the full
  file-by-file account. **Owed:** Peter's live L4 feel-pass (carries over from
  §8, unchanged by this refactor since the DSP path is identical) — headless
  PNG proof only goes to L1.
- **§8 feel-pass round 1 (2026-07-07, same evening): Peter's first live pass found two
  real engine bugs, both root-fixed the same night.** (1) A config left DISARMED from
  the drawer kept its mode, and the `acquire_clip` clip-edge gate read
  `mode.wants_clip_edge()` without checking `enabled` — a disarmed Transient config
  silently killed clip-launch triggering for its layer, surviving save/reload with no
  badge (the badge correctly hides when disarmed; the engine didn't apply the same
  rule). Fixed @ `62a75cee`: disabled-means-absent now has one owner,
  `AudioTriggerMod::clip_edge_enabled()`. (2) The audio-analysis gate was blind to §8
  configs: `has_active_audio_mods` (capture on/off) and `analysis_consumed_sends` (D4
  per-send gate) counted per-param audio mods and §1–§7 send routes but not
  `PresetInstance.audio_trigger` — so a project whose only audio consumer is an armed
  trigger drawer never captured, and even with capture running the trigger's send was
  skipped: armed audio triggers never fired. Fixed this landing: single owner
  `PresetInstance::active_audio_trigger()`, wired into both gates plus
  `audio_send_usage_count` (delete-send warning) and `audio_mod_consumers` (Consumers
  section, listed as "… • Trigger"). Lesson for future §-additions: a new audio
  consumer type must register with the ANALYSIS GATE walkers in `project.rs`, not just
  the evaluator — the evaluator can be perfect and never see a single sample.
- **§8 P3b BUILT 2026-07-07 (follow-up session, two PRs on `wave/param-triggers-p3b`) —
  the whole §8 feature is now UI-reachable.** PR1: effect cards gained the toggle/trigger
  row branch they never had (root cause was deeper than the missing branch —
  `state_sync.rs::preset_to_config` hardcoded `is_toggle/is_trigger: false` for every
  effect param, so Strobe's P3a card rendered as a raw slider; fixed at the registry-read
  level, shared `build_toggle_trigger_row` now serves both card builders, and
  `GenParamToggle/Fire` generalized to `ParamToggle/Fire(GraphParamTarget, ..)`), plus
  D5b reachability: `is_trigger` cards now reach the standard audio-mod "A" drawer
  (no mode row). PR2: the D6 `AudioTriggerMod` drawer (Dropdown send · Segmented band ·
  Slider sensitivity · Segmented mode) on `isTriggerGate` cards, new
  `SetAudioTriggerModCommand` (whole-field capture, undoable), PanelAction dispatch +
  state_sync card view, and the collapsed-row mode badge (§8.2 consequences). Second
  root fix en route: `is_trigger_gate` lived only on graph-metadata `ParamSpecDef`,
  unreachable for stock (never-forked) instances — added to registry `ParamDef` and
  threaded through both resolution paths. Verified headless to PNG on both a generator
  (Plasma) and Strobe. Still owed: Peter's live L4 feel-pass (whole §8 + §1–§7 debt).
- **§8 execution note (2026-07-07, P1+P2 session): one interpretive call made mid-flight,
  flagged for Peter's review.** D1 says "per layer the renderer keeps `clip_count`... and
  `audio_count`" (singular, per-layer) while D2 says the `audio_trigger` config is
  per-INSTANCE (any generator or effect on that layer). The doc doesn't spell out what
  happens when multiple instances on one layer each carry their own config. Read
  literally ("gates each increment when the event happens... so switching mode live
  never jumps the effective count"), I implemented: `clip_count` increments
  unconditionally at `acquire_clip`, gated only by the layer's OWN GENERATOR's
  `audio_trigger.mode` (default true = old behavior, unaffected by config absence);
  `audio_count` is a single per-layer accumulator that ANY instance on that layer
  (generator or effect) can independently bump via its own `audio_trigger` fire,
  gated by THAT instance's own mode wanting `Transient`. This reconciles "one shared
  count per layer" with "config lives per-instance" and matches the P3 acceptance demo
  (Strobe, an EFFECT, needs its own audio_trigger to make "kick fires Strobe" true even
  when the layer's generator has no config of its own). Not re-litigated exhaustively
  against every possible multi-instance interaction — flag if the live feel-pass finds
  this wrong.
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

## 8. Param triggers — audio fires the Trigger controls (designed 2026-07-07, NOT BUILT)

§1–§7 fire **clips**. This section makes transients fire the **trigger response of an
already-playing generator or effect chain** (plus `is_trigger` cards) — the kick pulses the
burst/reset/jump the generator already performs on clip retrigger, without touching clip
scheduling. On stage: point the Kick send at a playing FluidSim and every kick injects;
the same generator still responds to clip launches in "both" mode. Peter's founding
directive, verbatim: *"if Trigger is enabled we can choose if we want rising clip edge
(default) OR the transient trigger OR both."*

### 8.1 Audit — what the trigger surface actually is (verified 2026-07-07)

- **Triggers are counts, not pulses.** `ParamConvert::Trigger` passes a monotonic count
  through; every consuming primitive edge-detects with the `last_count` cold-start
  pattern (`node_graph/param_binding.rs:181-184`). Counts compose by addition — "both"
  is summing two counters.
- **The generator "Trigger" control is the `clip_trigger` toggle card.** All 11
  trigger-responsive generator presets (BasicShapes, ConcentricTunnel, FluidSim2D/3D,
  Lissajous, MriVolume, NestedCubes, ParticleText, Plasma, StrangeAttractor, Wireframe)
  ship a `clip_trigger` **toggle** (`isToggle: true`) that gates the response
  (e.g. FluidSim2D: `trig_gate_env.enable`); the event source is separate — always-present
  wires from `generator_input.trigger_count` into consuming ports.
- **The clip edge is `acquire_clip`.** `trigger_count` is per-layer runtime state in
  `GeneratorRenderer`, incremented when a new clip becomes active on the layer
  (`generator_renderer.rs:370-372`). That is the "rising clip edge".
- **Effects never see the clip edge.** `trigger_count`/`anim_progress` are clip-side
  concepts that stay 0 for effect chains (`preset_runtime.rs:1918-1924`). An effect's
  only trigger surface is an `is_trigger` fire-button card (`param_card.rs:123-126`,
  user bindings with `ParamConvert::Trigger`); zero shipped presets set `isTrigger: true`
  (searched 2026-07-07), and `RegisteredParam::trigger()` has zero callers.
- **The audio half already ships.** Sensitivity→threshold mapping
  (`core/audio_trigger.rs:71-74`), armed-flag edge detection with `REARM_RATIO = 0.6`
  hysteresis (`playback/live_trigger.rs:79-88`), transient extraction via
  `AudioFeature{Transients, band}`, and per-instance audio-mod evaluation each tick
  (`playback/modulation.rs:375-414`, runs for effect AND generator instances).
- **Port-shadows-param kills graph-level summing.** A wired port shadows its param, so
  clip-edge wire + card binding on the same port select, not add. The sum must happen
  at the count **source** (engine/renderer side), not in the graph.

Classification: the edge detection, threshold math, transient feature, per-instance
config storage, and drawer UI all *exist*. Genuinely new: one config type, one
evaluation arm, the count-combination seam, and ~15 lines of drawer spec.

### 8.2 Decisions

- **D1 — Two counters, gated at event time, summed at read.** Per layer the renderer
  keeps `clip_count` (existing, incremented in `acquire_clip`) and `audio_count` (new,
  incremented by transient fires). Mode `ClipEdge` (default) / `Transient` / `Both`
  gates each increment **when the event happens**, not retroactively at read — so
  switching mode live never jumps the effective count and never emits a phantom
  trigger. Effective `trigger_count` = `clip_count + audio_count`.
- **D2 — Config is per-instance, beside `audio_mods`.**
  `PresetInstance.audio_trigger: Option<AudioTriggerMod>` where
  `AudioTriggerMod { enabled, source: AudioModSource /* send + Transients×band */,
  sensitivity: f32, mode: TriggerFireMode }`. Reuses `AudioModSource` (send-id
  addressing survives relabel/re-patch); serde skip-none, old projects byte-identical.
  It is the performance surface, saved with the show, and travels with the generator
  instance like the `clip_trigger` toggle it sits beside.
- **D3 — Fires are immediate.** No launch-quantize: a visual transient quantized to the
  grid reads as latency on stage. Latency = detector latency + ≤1 content frame — same
  as §1–§7 routes with quantize Off. (Clip-launch TriggerRoutes keep honoring the
  project quantize mode; that behavior is correct for *launches* and unchanged.)
- **D4 — Reuse the edge detector, refactored not copied.** Extract the sensitivity→
  threshold mapping (already pure in `TriggerRoute::threshold`) into a shared helper in
  `core::audio_trigger`, and the armed/re-arm hysteresis into a small pure
  `TransientEdge` struct usable by both `LiveTriggerState` (keyed by send×band) and the
  new param evaluator (keyed by instance). Runtime state, never serialized. Audit
  finding: `LiveTriggerState::clear()` documents "call on transport stop" but has zero
  call sites (BUG-051) — P1 wires BOTH edge-state holders into the transport-stop reset
  rather than copying the omission.
- **D5 — Effect chains receive the clip edge too (Peter, 2026-07-07: "I would like
  triggers to be possible with effects too").** `set_frame_context` currently pins
  `trigger_count` to 0 for effect slots (`preset_runtime.rs:1918-1924`); P2 feeds the
  owning layer's effective count instead, so an effect graph consumes
  `generator_input.trigger_count` exactly like a generator graph and the instance-level
  `audio_trigger` config (D2) applies to effects with the full mode choice — a Strobe
  on the Kick layer can flash on clip launches, on kicks, or both. Master/global chains
  have no layer: clip contribution is 0 there, audio fires still work. Honest gap: no
  shipped effect preset consumes `trigger_count` yet, so day one this is reachable via
  graph-editor override wiring (the P2 demo); effect presets adopt trigger-gate cards
  as individual preset upgrades later, not in this wave.
- **D5b — `is_trigger` fire-button cards ride `ParameterAudioMod`, audio-only.** When
  an audio mod's target param `is_trigger`, evaluation switches from continuous
  overwrite (`p.value = min + (max-min)*out_norm`) to edge detection: a runtime
  fire-counter on the mod, `p.value = base + count`. Downstream `last_count` edge
  detection consumes it unchanged. No mode row on these — a button has no clip edge,
  and the chain-level stream (D5) is where clip/audio mixing lives; per-param mode
  would be config sprawl.
- **D6 — UI home: the audio drawer on the trigger card.** For generators, the "A"
  drawer on the `clip_trigger` toggle card configures it: Dropdown(send) ·
  Segmented(band: Whole/Low/Mid/High) · Slider(sensitivity) · Segmented(mode:
  Clip/Audio/Both). The card is identified by an explicit `isTriggerGate` flag on the
  outer-card ParamDef (one-line edit in each of the 11 presets), NOT by matching the id
  string `"clip_trigger"` (`feedback_hidden_field_dependencies`). `is_trigger` cards
  get the same drawer minus the mode row (D5b). **Reachability rule (dead-LANES
  lesson):** an effect's instance-level config needs a gate card to host the drawer, so
  P3 upgrades ONE effect preset — Strobe — with a `clip_trigger` toggle card and a
  minimal trigger→flash response (executor does the §2.5-style read of Strobe's graph
  first; wiring is theirs, the card + behavior is committed here). Without this the
  effect half ships UI-unreachable. All edits through `EditingService` commands like
  every other audio-mod edit.

Consequences, stated honestly:
- Fires arrive at analysis-block rate on the content tick — a transient between blocks
  lands on the next one. Identical to the shipped clip-trigger routes; nobody has felt
  it, but Peter's L4 feel-pass on §1–§7 is still owed and covers both.
- `Transient` mode silently ignores clip launches for that generator's trigger response.
  That is the point, but it's a mode a user can forget — the drawer must show the mode
  on the collapsed card row (the toggle card already shows its state).
- A generator whose graph consumes `trigger_count` through custom override wiring gets
  the summed count like any preset — but an override that *re-purposes* `trigger_count`
  semantically (e.g. as a free counter) will see audio increments too. Accepted; the
  count has always meant "times this layer was triggered".

Rejected (do not re-propose):
- **R1 — Fire a one-shot clip on the same layer** (works today via §1–§7 and does
  increment `trigger_count`): churns clip state, interrupts the playing clip, and the
  one-shot length is meaningless for a pulse. Clip routes stay for firing *clips*.
- **R2 — Continuous audio mod on the `clip_trigger` toggle** (BoolThreshold flapping):
  gates the response on/off instead of firing it; no refractory; wrong semantics.
- **R3 — An audio-transient node inside the graph**: audio stays on the perform
  surface, not graph nodes (`[[audio-stays-on-perform-surface]]`, §6).
- **R4 — A routing table in the Audio Setup modal**: splits a param's audio config
  across two surfaces; per-param drawers are where mod config lives (§10 of
  AUDIO_MODULATION_DESIGN). The Audio Setup table stays clip-routing only.

### 8.3 Architecture (by crate)

```
core      AudioTriggerMod + TriggerFireMode + shared threshold fn      NEW (serialized)
core      TransientEdge (pure armed/re-arm hysteresis)                 NEW (runtime-only)
core      ParamDef.is_trigger_gate flag (+ 11 preset JSON edits)       NEW
playback  param-trigger arm in the audio-mod pass: instance fires →    NEW
          per-layer pulse list; is_trigger mods → count-add semantics
renderer  GeneratorRenderer: audio_count per layer; mode gate in       EXTEND
          acquire_clip; effective count = clip_count + audio_count
renderer  effect chains: set_frame_context feeds the layer's           EXTEND (D5)
          effective count into generator_input.trigger_count
          (currently pinned 0.0); master chains: clip part = 0
editing   SetAudioTriggerModCommand (mirrors audio-mod commands)       NEW
ui        drawer rows on trigger cards (DrawerSpec — §10.2 of          NEW (small)
          AUDIO_MODULATION_DESIGN did the hard part)
app       PanelAction + dispatch + state_sync card view                WIRE
```

### 8.4 Phase checklist (tick + commit as you go)

- [x] **P1 — Core model + engine evaluation.** `AudioTriggerMod`/`TriggerFireMode`
      (`core/audio_trigger.rs`); `audio_trigger` on `PresetInstance` (skip-none, serde
      round-trip test, both effect + generator wire paths); `TransientEdge` extracted
      and `LiveTriggerState` re-based on it (its 5 tests stay green — the refactor
      proof); trigger-aware arm in `evaluate_instance_audio_mods` for `is_trigger`
      targets (`ParameterAudioMod.trigger_edge`/`fire_count`, D5b); generator/effect
      fires surfaced from the modulation pass as a per-layer (or master, D5) pulse list
      (`evaluate_all_param_triggers` → `Vec<TriggerPulse>`, drained via
      `PlaybackEngine::take_trigger_pulses`, P2 plumbs it into the renderer); BUG-051
      fixed — `engine.stop()` now calls `live_trigger_state.clear()` +
      `modulation::clear_all_trigger_edges` (covers both the §1-7 route edges and the
      new §8 holders). Gate: 6 new core tests (`audio_trigger::tests`) + 6 new playback
      tests (`modulation::tests::param_trigger_*`, `clear_all_trigger_edges_*`,
      `is_trigger_audio_mod_*`) all green; full existing suites green (core 309+9,
      playback 158+6 incl. the 5 `live_trigger` refactor-proof tests, editing 97+67,
      io `load_project` 15 incl. the Liveschool canonical fixture); clippy clean on
      core/playback/editing/io.
- [x] **P2 — Renderer seam + vertical proof.** `LayerGeneratorState.trigger_count`
      split into `clip_count` + `audio_count` (`generator_renderer.rs`); mode gate at
      `acquire_clip`'s increment site (`clip_edge_enabled`, computed from the
      generator's own `audio_trigger.mode`, default true = unchanged old-project
      behavior); `effective_trigger_count()` = `clip_count + audio_count`, read at the
      render_info_scratch site. `bump_audio_count`/`effective_trigger_count_for_layer`
      public accessors. Pulse list plumbed content-pipeline (`ContentPipeline::
      apply_trigger_pulses`, called each tick right after `take_trigger_pulses`) →
      renderer: `CompositeLayerDescriptor.trigger_count` (per-layer) +
      `CompositorFrame.master_trigger_count` (new, session-scoped counter on
      `ContentPipeline`, D5: master has no layer so clip part is always 0) →
      `layer_compositor.rs`'s 2 layer-effect-chain `PresetContext` sites +2 master
      sites now read the real count instead of a hardcoded 0 (the 2 group-chain sites
      are UNCHANGED/deferred — D5 doesn't define a group-scoped count). D5 fix in
      `preset_runtime.rs::run`: the `generator_input` frame-context block now also
      pushes `ctx.trigger_count` (previously always 0.0 for effect chains).
      Gate: 2 new `gpu-proofs` tests —
      `generator_renderer::tests::effective_trigger_count_sums_clip_and_audio_and_respects_clip_edge_mode`
      (generator half: clip_count+audio_count sum, mode gate) and
      `preset_runtime::generator_input_tests::run_feeds_nonzero_trigger_count_into_generator_input_effect_slot`
      (effect-chain half: a nonzero `ctx.trigger_count` reaches the
      `generator_input` node) — both pass, proving the SAME effective count reaches
      a generator graph AND an effect-chain slot on the same layer. Full
      `gpu-proofs` suite (1244 tests) + default workspace sweep + full workspace
      clippy all green (one PRE-EXISTING unrelated failure excluded:
      `manifold-core`'s `docs_index_sync` — `docs/README.md` was already stale
      against `ABLETON_TRANSPORT_SYNC_DESIGN.md`/`BOX3D_PHYSICS_DESIGN.md` at this
      branch's base tip `a52860e7`, before this wave touched anything — out of
      scope for this wave, not fixed here).
      **Honest gap — L4 owed:** the real proof (app run, stem playing, transient
      visibly fires a playing FluidSim burst) could not be run in this headless
      session — no audio device, no interactive GPU output to observe. Verified
      only to L1 (tests green) for this phase; Peter's live feel-pass must cover
      this alongside the existing §7 feel-pass debt. The effect-side look lands
      with P3's Strobe card, also L4-owed.
- [x] **P3a — Model + Strobe reachability (SHIPPED).** `is_trigger_gate` flag
      (`ParamSpecDef.is_trigger_gate`) + all 11 generator preset edits
      (`isTriggerGate: true` on each `clip_trigger` card) + Strobe upgraded with a
      `clip_trigger` toggle card wired to a minimal trigger→flash response
      (D6 reachability rule; §2.5 audit of Strobe's graph done first — composed
      entirely from existing primitives: `system.generator_input` →
      `node.trigger_gate` (enabled by the toggle) → `node.envelope_decay` (the same
      atom FluidSim2D's clip-trigger state machine uses) → `node.math` (Max) combined
      with the existing beat-gate square wave → `flash.amount`). Gate: `check-presets`
      (46/46) + a real `gpu-proofs` test that builds and RUNS the bundled Strobe
      preset end-to-end and proves clip_trigger ON flashes on a trigger_count jump
      while OFF doesn't (`preset_runtime::generator_input_tests::
      strobe_clip_trigger_card_flashes_on_trigger_count_jump_when_enabled`) — the
      concrete effect-side "kick fires Strobe" proof at the graph-value (L1) level.
      `docs/node_catalog.json` regenerated (Strobe is now a usage example for 3
      primitives). Full gpu-proofs suite (1245) + default workspace sweep + clippy
      all green.
- [x] **P3b — UI drawer + dispatch (BUILT 2026-07-07, follow-up session, two PRs:
      `b333d855` reachability + `b71c7dc8` drawer — see §0 for the two root-cause
      fixes found en route; scoping notes below kept for the record).**
      Investigation found this is substantially
      bigger than the original phase brief implied — a genuinely new UI feature, not
      a drawer-config tweak. Findings, so a follow-up session doesn't re-derive them:
      - **D5b (`is_trigger` cards reuse the existing per-param audio-mod "A"
        drawer)** requires touching the SAME gate at 6 call sites, not 1:
        `param_slider_shared.rs:1838` (click resolution, already found) PLUS
        `param_card.rs:1304`, `:2266`, `:3289`, `:3662` (height computation for both
        generator and effect card variants, and row-building). The row-building
        site at `param_card.rs:2266` is NOT a boolean gate — the toggle/trigger
        branch allocates NO D/E/A lane space at all ("A toggle can't be modulated,
        so the D/E/A lane to its right is correctly left empty"), so reaching
        `is_trigger` cards means restructuring that branch to reserve lane space
        conditionally, not flipping a flag.
      - **D6 (`is_trigger_gate` cards get a NEW drawer: Dropdown(send) ·
        Segmented(band) · Slider(sensitivity) · Segmented(mode))** cannot reuse the
        existing per-param audio-mod drawer's dispatch as-is: `ParameterAudioMod`
        lives in a per-param `Vec` (`PresetInstance.audio_mods`), while
        `AudioTriggerMod` (D2) is a single `Option` field
        (`PresetInstance.audio_trigger`) — the click→edit dispatch, the
        `DrawerIds`/config struct, and the `EditingService` command (new
        `SetAudioTriggerModCommand`, mirroring `SetAudioSendTriggersCommand`'s
        whole-field-capture shape, not the per-param audio-mod command) are all new,
        not reused. The `DrawerSpec`/`drawer::build` MECHANISM (§10.2 of
        AUDIO_MODULATION_DESIGN) is reusable — only the model binding and the extra
        mode row are new.
      - **Collapsed-row mode indicator** (show the mode on the collapsed card row
        so `Transient`-only isn't a silent trap) has no existing precedent to
        extend from; small, but genuinely new.
      Brief for the follow-up session: read `param_slider_shared.rs`'s
      `check_row_click` (~1830-1924) and `param_card.rs`'s `build_param_row` +
      `compute_height_effect`/`compute_height_generator` end to end first (the
      §2.5-style read this phase skipped due to running out of session budget);
      then split into two PRs — (1) `is_trigger` reachability on the existing
      drawer (small, mechanical once the layout branch is understood), (2) the new
      `AudioTriggerMod` drawer + command + dispatch + state_sync + collapsed-row
      indicator (the real build). Gate: ui tests + clippy + manual drawer pass +
      the effect-side look already proven at L1 above should also be exercised via
      the new UI at L3/L4 once it exists.
- [x] **P4 — Ship (P1/P2/P3a only — P3b not in this landing).** Full workspace
      gate rerun twice — once pre-merge in the `wave/param-triggers` worktree,
      once more in the main checkout after merging a concurrent BUG-052 landing
      (`216549e2`/`6e0e8988`) that arrived while this was landing — both green
      (workspace suite, `manifold-core` 318, `gpu-proofs` 1245, workspace clippy).
      Merged `--no-ff` into `main` @ `3089e0a3`, pushed, rejected once (someone
      else landed first — the sample-rate-invariance fix), re-fetched/merged/
      re-gated/pushed successfully @ `a8993dbc`. `wave/param-triggers` confirmed
      an ancestor of `origin/main` before deleting the branch + worktree.
      **Explicitly owed, logged, not done here:** Peter's live feel-pass (L4) on
      the whole feature — no audio device, no interactive GPU output in this
      session; P3b's UI drawer (see P3b above) — a follow-up session's job, with
      the brief already written.

## 9. Unification — the trigger IS an audio mod (Peter, 2026-07-07: "reuse the existing detectors so we don't have this stupid and dangerous split")

Decided in-session after feel-pass round 1. The §8 D2 shape (`AudioTriggerMod`, a
second per-instance config beside `audio_mods`) was a design mistake: the DSP was
always shared, but the parallel CONFIG type forced every gate, walker, drawer, and
command to know about two things, and the same night it shipped, two real bugs came
from plumbing that knew about only one (§0). The §8.2 D2/D6 decisions are SUPERSEDED
by this section; D1 (two counters, event-time gating), D3 (immediate fires), D5
(effect chains get the count) stand unchanged.

### 9.1 Decisions

- **U1 — One config type.** `AudioTriggerMod` and `PresetInstance.audio_trigger` are
  DELETED. A trigger-gate card's audio config is a normal `ParameterAudioMod` in
  `audio_mods`, `param_id` = the gate card's param. The D5b fire chassis is the
  evaluator: `shape.apply()` → `trigger_edge.advance(out_norm, 0.5)` — but for an
  `is_trigger_gate` target the fire emits a `TriggerPulse` (bumping the layer's
  `audio_count` exactly as before) and NEVER writes the toggle's value (R2's
  flapping stays dead — the toggle remains a user control).
- **U2 — Any feature, standard drawer.** UX: the trigger drawer IS the audio-mod
  drawer — Source/Feature/Band/Inv/Delta/Amount/Attack/Release — plus one
  trigger-only Mode row (Clip/Audio/Both). Kick can fire a trigger now.
  What you audition on a slider is byte-identical to what fires the trigger.
  The bespoke Sensitivity slider dies; Amount is the tune knob (scales the shaped
  signal against the fixed 0.5 edge threshold, same knob-feel, one vocabulary).
- **U3 — Mode lives on the mod.** `ParameterAudioMod.trigger_mode:
  Option<TriggerFireMode>` (serde skip-none; `None` on non-gate targets). Arm-time
  default **Both** (adding audio to a trigger must not silently kill clip launches —
  the §8 ClipEdge default made arming a no-op, bad instrument feel). The clip-edge
  gate helper becomes `PresetInstance::clip_edge_enabled()`: no enabled gate mod →
  true; else its mode's `wants_clip_edge()`. Disabled-means-absent (today's
  regression semantics) carries over verbatim.
- **U4 — Walkers shrink back.** `PresetInstance::active_audio_trigger()` and the
  four §0 walker arms are DELETED — a fire-mode mod is just an audio mod, so
  `has_active_audio_mods`/`analysis_consumed_sends`/usage/consumers cover it with
  zero trigger-specific code. This deletes the "walker forgets the second config
  type" bug class by deleting the second config type.
- **U5 — Migration.** Load-time: a serialized `audioTrigger` field converts to a
  `ParameterAudioMod` on the instance's `is_trigger_gate` param (feature/band/send
  preserved; `trigger_mode` preserved; enabled preserved; sensitivity approximated
  into Amount — field is one day old, exists in ~one project, exact-feel fidelity
  explicitly NOT owed). Deserialize-only legacy struct, no version bump needed
  (field was skip-none; absent stays absent).
- **U6 — Pulse plumbing unchanged.** `evaluate_all_param_triggers` and its second
  walk DIE; the audio-mod walk collects pulses. `PlaybackEngine::take_trigger_pulses`,
  `ContentPipeline::apply_trigger_pulses`, renderer `audio_count`, D5 effect-chain
  feed: all untouched.

### 9.2 Phases

- [x] **U-P1 (core+playback+renderer+io):** model change + migration + evaluator
      merge + `clip_edge_enabled()` move + walker-arm removal + BUG-051 clear path
      covers `trigger_mode` mods' edges (already does — same `trigger_edge` field).
      Tests: port today's two §0 regression tests to the unified shape (disarmed
      gate mod → clip edge on + gates off; armed fire mod → gates on, send claimed,
      pulse emitted on threshold crossing; mode row gates clip/audio at event time);
      migration round-trip from a legacy `audioTrigger` JSON blob.
- [x] **U-P2 (ui+app+editing, 2026-07-07):** trigger-gate cards open the standard
      audio-mod drawer + a trailing Mode row (`build_audio_mod_drawer`'s new
      `trigger_mode_idx: Option<i32>` param), reusing the SAME `audio_btn`/
      `audio_config` slot `is_trigger` (D5b) already proved reaches both effect
      and generator targets — `build_toggle_trigger_row`'s `is_trigger` and
      `is_trigger_gate` branches are now one branch (`param_slider_shared.rs`).
      Deleted: `AudioTriggerCardState`, `build_audio_trigger_mod_drawer`,
      `ModTab::AudioTrigger`, the 6 `AudioTriggerMod*` PanelActions +
      `audio_trigger_snapshot` (`app.rs`/`ui_bridge/mod.rs`/`inspector.rs`/
      `ui_snapshot/script.rs`), `build_audio_trigger_card_state`,
      `SetAudioTriggerModCommand`, `audio_trigger_band_labels`,
      `dragging_audio_trigger_sensitivity`. Added: one new command
      (`SetAudioModTriggerModeCommand`, mirrors `SetAudioModSourceCommand`'s
      whole-old/new-capture shape) + one new PanelAction
      (`AudioModSetTriggerMode(GraphParamTarget, ParamId, usize)`) — the
      smallest existing audio-mod command family member, not a new mechanism.
      `AudioCardState`/`ParamModState` grew one field each
      (`trigger_mode_idx`/`audio_mode_idx`), populated by the SAME
      `build_audio_card_state` walk every other per-param field already used
      (no `is_trigger_gate` awareness needed there — only the UI's Mode row
      and collapsed badge care which row it is). Collapsed-row mode badge
      reads the gate mod's `trigger_mode` (unchanged mechanism, new field
      source). Headless PNG proof on a generator (Plasma, mode Both) and
      Strobe (mode Transient/"Audio"), both drawers open + badges, plus a
      plain slider's (Bloom's) own armed audio-mod drawer alongside for the
      regression look — see `[[live-audio-triggers]]` memory for the
      screenshots. Gate: `cargo test --workspace` (2 new manifold-editing
      tests + 3 rewritten manifold-app state_sync tests + 2 rewritten
      manifold-ui param_card tests, full suite green) + `cargo clippy
      --workspace -- -D warnings` clean + `cargo clippy -p manifold-app
      --features ui-snapshot` clean except the pre-existing unrelated
      BUG-057 (`make_blit_pipeline` dead code, landed in an earlier commit,
      logged not fixed — out of scope).
