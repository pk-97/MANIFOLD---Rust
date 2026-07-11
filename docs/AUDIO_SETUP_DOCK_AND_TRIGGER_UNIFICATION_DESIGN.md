# Audio Setup Dock & Trigger Unification — the panel becomes a workspace column; clip triggers become layer-owned audio mods

**Status:** **SHIPPED — WAVE 2 COMPLETE 2026-07-11 (P5–P8 all landed)** — Peter's morning-after rig review of Wave 1 found the D6 fire meter dead in every transport state; root-caused same session (BUG-109: P3c's capture reset wipes the step-3b trigger levels while playing; stopped ticks never evaluate clip triggers at all — so **BUG-082's closure below was false**, reopened via BUG-109). Wave 2 = P5 meter resurrection + peak-hold · P6 Sensitivity rename + Delta UI removal · P7 scope↔drawer linkage · P8 panel de-clutter (kick lane out, Cap chip out, Inputs authoring out, St/Mo collapsed into the channel dropdown). Executor: Sonnet orchestration against §7; land in two batches (P5+P6, P7+P8). **P5 shipped 2026-07-11:** top-of-tick `FireMeterCapture` reset, stopped-tick meter-only conditioning walk (`LiveTriggerState::evaluate_meter_only`), UI-side peak-hold in `MeterIds::update` — closes BUG-109 at the plumbing level (L1 unit tests + a synthetic-level PNG); BUG-082 re-closed the same way. **VD-025 (live rig confirmation) stays open — not claimable by this executor**, see §7.3 Phase 5. **P6 shipped 2026-07-11:** "Amount" → "Sensitivity" display rename; Delta toggle removed from the drawer (both `AudioModDrawerTarget`s, one shared builder) — `RowClick::AudioToggleRate` deleted, flat button-index math renumbered in all three click-routing sites; `AudioModShape::rate_of_change` + its `condition()` arm stay compiled dormant; a load migration (`Project::clear_legacy_rate_on_flags`) clears any saved `rate_on: true` on both carriers, counted + logged, proven by a save-reload round-trip test. **P7 shipped 2026-07-11:** expanding a fire-mode drawer re-taps the scope to its send (`UIRoot::open_fire_mode_drawer_send`, `app_render.rs`'s tap-follow block); a non-Full selected band dims the spectrum outside it — implemented in `spectrogram.wgsl`'s fragment shader (not UI-tree quads; see the phase's as-built correction) and proven by a real GPU readback test. VD-026 carries the L3-flow + live-PNG gap (same live-audio-device limitation as VD-025). **P8 shipped 2026-07-11:** kick lane out of the scope (`ScopeOnsets` drops the field outright, detector untouched); Cap chip + its click-to-reveal routings popup deleted (content lives in the now-read-only Inputs section); Inputs section authoring ("+ Layer", per-layer ×) deleted, the layer header's Send dropdown is the one surviving path; St/Mo toggle deleted, the channel dropdown enumerates stereo pairs and singles directly; Consumers clip-trigger rows reworded to "Clip trigger · Layer · Band"; AUDIO_SENDS_UX D2/D3 as-built notes added. — Wave 1 record: ✅ P1–P4 shipped 2026-07-10. P1 dock column + overlay deletion (`36a96791`, closes BUG-047) · P2 `LayerClipTrigger` model + migration + evaluator (`e4aa01bf`) · P3a Triggers-matrix deletion (`47f2a112`) · P3b inspector AUDIO TRIGGERS authoring section, default-collapsed (`5c4fbcca`) · P3c fire meter/D6 (`12fbc37d`, see BUG-109) · P4 readability + hygiene, BUG-070 FIXED (`a649f62a`). Per-phase detail in `docs/landings/2026-07-10-audio-dock-p{1,2,3a,3b,3c,4}.md`, `docs/landings/2026-07-11-audio-dock-p5p6.md`, and `docs/landings/2026-07-11-audio-dock-p7.md`. Owns closed: BUG-047, BUG-070, BUG-109. Owns open: BUG-082 (re-fixed, L4 confirmation pending). Open debt: VD-024 (P3b/P7 section unit tests), VD-025 (P5's live-crossing confirmation, Peter's rig), VD-026 (P7's L3 flow + live-dim PNG, same rig session). **Owed to Peter (L4 feel-pass):** narrow-width dock, dock-width persistence, live fire-meter crossing (P5 plumbing shipped, crossing itself unconfirmed), gain reset-gesture confirmation (shipped as right-click). **Deferred:** level-crossing detector, bulk trigger-tune view (chip tooltips: moot, chips deleted in P8). · design 2026-07-09 · wave 2 2026-07-11 · Fable
**Prerequisites:** none (runs against shipped AUDIO_SENDS_UX P1–P4 and LIVE_AUDIO_TRIGGERS §9 U-P1/U-P2 code)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter, opening the session: the panel "opens over the inspector panel and blocks you from
seeing params and tuning them with the audio settings open. I think it would be better if
it 'pushed out' from the left side of the inspector column, resizing the main preview
window and timeline so you can see everything at once." And on the trigger split: "we need
to unify how these triggers for auto firing 'ghost clips' [work] with the discrete trigger
steps that the audio modulation uses. Users learn one system for how to configure a
'trigger' from an audio source." Constraint, quoted: "As long as everything is still
easily readable if we're overlaying things on the spectrogram I am happy."

**The governing insight: audio calibration is a closed loop — play the track, watch the
spectrogram, watch the show respond, tune the consumer — and the current overlay breaks
the last arc of the loop by covering the inspector.** AUDIO_SENDS D6 moved the panel off
the show (un-dimmed, right-anchored); this design finishes the move by making it a real
workspace column so the preview, timeline, inspector, and panel are all visible at once.
The second half kills the last parallel trigger system: the ghost-clip `TriggerRoute`
matrix (send-owned, transient-hardcoded, bespoke sensitivity) becomes a layer-owned config
speaking the exact audio-mod vocabulary §9 already unified for param triggers.

**Supersessions.** This doc supersedes AUDIO_SENDS_UX_DESIGN D6/§3.3 (non-dimming overlay
— completed, not reversed: the reasoning that killed the dimming modal kills the overlay
too) and amends APP_SHELL_DESIGN's §7 classification of Audio Setup from T2 modal to a T1
workspace surface (APP_SHELL's own R3 — "tune-while-watching config is a T1 surface, not a
modal" — argues this side; the T2 row predates the trigger matrix and scope growing into
the panel). LIVE_AUDIO_TRIGGERS §9 U1–U6 stand unchanged; its send-owned `TriggerRoute`
model (§1–§7 of that doc) is superseded by D2 here.

Companion docs: `AUDIO_SENDS_UX_DESIGN.md` (the panel's current shape), `LIVE_AUDIO_TRIGGERS_DESIGN.md`
(§9 is the unification precedent this extends), `AUDIO_MODULATION_DESIGN.md` (drawer + shape
vocabulary), `APP_SHELL_DESIGN.md` (panel taxonomy).

---

## 1. Audit — what exists (verified 2026-07-09)

| Piece | Where | State |
|---|---|---|
| `ScreenLayout` — single source of truth for top-level rects; inspector = full-height right column bounding `content_area()` | `crates/manifold-ui/src/layout.rs:12` (struct), `:88` (`content_area`), `:152` (`inspector`) | Shipped. **`effect_browser_width` (`:20`, `:72`, `:163`) is the exact precedent: an optional column, 0.0 = closed, bounds content** |
| Inspector width drag + double-click snap-back tween | `layout.rs:282` (`reset_inspector_width`), `inspector_width_anim` | Shipped — the resize-handle pattern the new column copies |
| Audio Setup panel as modeless overlay: 38%×100% right-anchored | `audio_setup_panel.rs:117` (`PANEL_W_FRAC`), `:745` (`compute_overlay_rect`), `:2314` (`Modality::Modeless`), `:2355` (Escape self-close) | Shipped (AUDIO_SENDS P3). 3,269 lines; owns bespoke hit-testing (`panel_rect`, `:1842`), per-frame `update_meters`/scope mirror (`:1943`, `:2070`) |
| Overlay registration | `manifold-app/src/ui_root.rs:27` (`OverlayId::AudioSetup`), `:982` (dispatch arm), `:211` (panel field) | Shipped — the seam P1 rewires |
| Ghost-clip trigger routes: per-send `Vec<TriggerRoute>`, transient-only, bespoke sensitivity | `manifold-core/src/audio_setup.rs:146` (storage), `audio_trigger.rs:152` (struct), `:194-196` (**feature hardcoded to `AudioFeatureKind::Transients`**) | Shipped — the last parallel trigger config |
| Route evaluation: content thread walks `setup.sends[].triggers`, edge-detects, emits `FireRequest` | `manifold-playback/src/live_trigger.rs:56` (`LiveTriggerState::evaluate`), `FireRequest` `:33` (send_label auto-route + `one_shot_beats`) | Shipped. Fires snap to project quantize (same as MIDI clip-launch) |
| Unified param trigger (§9): trigger = `ParameterAudioMod` in fire mode; `shape.apply()` → `trigger_edge.advance(out_norm, 0.5)`; Amount is the tune knob against the fixed 0.5 edge | `manifold-playback/src/modulation.rs:519-556`, `audio_mod.rs:306` (`AudioModShape::apply`), `audio_trigger.rs:61` (`TransientEdge`) | Shipped 2026-07-07 (U-P1/U-P2). **Any feature is offered on trigger cards (U2)** |
| Standard audio-mod drawer (+ trailing Mode row for gate cards) | `param_slider_shared.rs:1518` (`build_audio_mod_drawer`), `:1625` (Feature row) | Shipped — the drawer D5 reuses |
| Layer-side clip-launch config precedent: flat MIDI fields on `Layer` | `manifold-core/src/layer.rs:140-155` (`midi_note`/`midi_channel`/`midi_device`/`midi_trigger_mode`) | Shipped — the home D2's config sits beside |
| Legacy-config load-migration precedent | `audio_trigger.rs:135` (`LegacyAudioTriggerMod`, deserialize-only), `effects::migrate_legacy_audio_trigger` (U5) | Shipped — the migration shape D2 copies |
| Consumers list = navigation (click selects owning layer) | AUDIO_SENDS D3, shipped P2 | Shipped — absorbs all trigger *display* once the matrix dies |
| Per-send analysis gating (consumed-set walkers) | AUDIO_SENDS D4, `audio_mod_runtime.rs` consumed-set cache; U4 deleted trigger-specific arms | Shipped — D2 adds back exactly ONE arm (layer configs), named in §3.4 |
| Open bugs this design owns | BUG-047 (panel overflow clips, LOW) · BUG-070 remainder (gain steppers + send fader lack double-click reset, LOW) · BUG-082 (fire-mode near-dead on level features, MED) | Open — each lands in a named phase below |

Extend, don't redesign. No new crates, no new threads, no new shared state, no new
widget kinds. The panel's internal row builders, scope, spectrogram, and crossover
drags are untouched except where a phase names them.

## 2. Decisions

- **D1 — Audio Setup becomes a `ScreenLayout` column between the content area and the
  inspector.** New input field `audio_setup_width: f32` (0.0 = closed) + computed
  `audio_setup()` rect; `content_area()` subtracts it exactly as it subtracts
  `inspector_width` (`layout.rs:91`). Shaped like `effect_browser_width`
  (`layout.rs:72`). The panel builds into that rect from the root build pass; the
  overlay registration dies. Preview and timeline shrink to make room — and on narrow
  screens they keep shrinking: **Peter's call (2026-07-09, AskUserQuestion): "Shrink
  preview + timeline further"** — one rule at every width, no fallback mode, no
  inspector collapse. *Consequences, stated honestly:* on a 13" laptop with the
  inspector at its 500 px default and the panel at ~460 px, the content column gets
  genuinely small; the mitigations are the panel's resize handle, double-click
  snap-back, and Escape-to-close — not a second layout mode. Rejected: keeping the
  overlay (occludes the inspector — the calibration loop's whole point; and the
  overlay's bespoke event/dirty/hit-test path is where this panel's one-off bugs live);
  a width-threshold fallback to the overlay (transitional-state design, two paths to
  maintain, resurrects the occlusion exactly where screens are tightest).
- **D2 — Clip triggers move to the layer; the send-owned matrix dies. Peter's call
  (2026-07-09, AskUserQuestion): "Layer side only."** A layer owns its audio
  clip-trigger configs (`Vec<LayerClipTrigger>`, §3.1), sitting beside its MIDI
  clip-launch fields (`layer.rs:140-155`) — both are "what launches clips on this
  layer." The Audio Setup Triggers matrix section is deleted; the existing Consumers
  rows (AUDIO_SENDS D3, navigational) display every trigger as "Low → Kick" and click
  through to the layer. *Consequences, stated honestly:* tuning every trigger of a song
  in one sitting now means visiting layers via the Consumers list instead of one
  matrix; if that proves clumsy in use, the answer is the Deferred bulk-tune view, not
  resurrecting the matrix. Rejected: authoring on both surfaces with one config
  (Peter's explicit non-pick; two editing surfaces for one config is the drift the
  panel just escaped); keeping the matrix as the editor (send-centric authoring is the
  split Peter is killing).
- **D3 — One trigger vocabulary: `LayerClipTrigger` embeds `AudioModSource` +
  `AudioModShape` and fires through the same chassis** — `shape.apply()` →
  `TransientEdge::advance(out_norm, 0.5)` — that param triggers use (U2 verbatim:
  Amount is the tune knob against the fixed 0.5 edge; what you audition on a slider is
  byte-identical to what fires the clip). The bespoke `sensitivity` and the transient
  hardcode die; any feature and band works, Kick included. Rationale: Peter, 2026-07-07
  (§9): "reuse the existing detectors so we don't have this stupid and dangerous
  split" — this is the same decision applied to the last holdout. Rejected: keeping a
  bespoke sensitivity/threshold pair on clip triggers (re-creates the two-vocabulary
  problem: users would learn "Amount vs 0.5" on params and "sensitivity" on clips);
  making the config literally a `ParameterAudioMod` (its `param_id`/`action`/
  `trigger_mode`/base-tracking fields are meaningless here — sharing the INNER types
  and evaluator is the unification; junk fields are not).
  - **AS-BUILT correction (P2, 2026-07-10):** the prose above says `shape.apply()` →
    `advance(out_norm, 0.5)`, but the actual param-trigger path
    (`modulation.rs`) edge-detects on `shape.condition()` (the pre-range-map signal),
    NOT `apply()`/`out_norm` — edge-detecting the range-mapped value reintroduces the
    documented range-trim firing bug ("range_min ≥ 0.5 fired once and never re-armed").
    P2's evaluator therefore fires on `trigger_edge.advance(shape.condition(...), 0.5)`,
    which IS byte-identical to param triggers (the property D3 actually asserts). The
    `apply()` wording was imprecise; `condition()` is the mechanism.
- **D4 — No Mode row on clip triggers.** `TriggerFireMode` (Clip/Audio/Both) exists to
  arbitrate between clip-edge and audio events on a *gate param*; a clip trigger IS the
  clip launcher — there is nothing to arbitrate. The drawer is Source/Feature/Band/
  Inv/Delta/Amount/Attack/Release + one **Length** row (`one_shot_beats`, the existing
  "1b"-style stepper labels). Fires keep snapping to the project quantize grid.
- **D5 — The drawer is the same `build_audio_mod_drawer`, parameterized, not forked**
  (`param_slider_shared.rs:1518`). It already grows a trailing Mode row for gate cards
  (U-P2's `trigger_mode_idx: Option<i32>`); it grows a trailing Length row the same
  way. One drawer builder, three callers (plain mod / gate mod / clip trigger).
  Rejected: a bespoke clip-trigger drawer (the §8-P3b lesson: the bespoke
  `AudioTriggerMod` drawer was deleted 24 hours after it shipped).
- **D6 — Fire legibility: a live level meter with the 0.5 fire threshold drawn as a
  line, rendered beside the Amount row of every fire-mode drawer** (param triggers and
  clip triggers alike — LIVE_AUDIO_TRIGGERS §9's "UPGRADE 2", now committed). Tuning
  becomes visual: crank Amount until the shaped signal visibly crosses the line. **This
  is the fix for BUG-082**: level features (Amplitude/Centroid/…) aren't near-dead
  because the engine can't honor them — they're a Schmitt trigger nobody can see. The
  meter makes them tunable; U2's "any feature" stands. Rejected: restricting the
  Feature row to Transients/Kick on fire-mode cards (walks back U2, and forecloses
  legitimate level-crossing triggers — "fire when the pad swells past mid"). Amend
  BUG-082's fix-shape line when this lands.
- **D7 — Readability package for the docked panel** (Peter's condition: "everything is
  still easily readable if we're overlaying things on the spectrogram"). Committed:
  band labels move out of the stacked corner ONTO their divider lines as small backed
  chips (they currently collide — see the 2026-07-09 screenshot); each source row gains
  an inline level meter (signal-present at a glance, no selection needed); the selected
  source row gets an explicit selection highlight (the master-detail scoping —
  "Spectrogram — DRUMS" — is currently invisible); the spectrogram gains nothing else.
  Fire-tuning feedback lives in the D6 drawer meter, NOT on the spectrogram.
- **D8 — Panel hygiene folded in, not left behind:** the docked panel body becomes a
  `ScrollContainer` (fixes BUG-047's clipped sections); gain steppers and the send
  fader get the intrinsic double-click reset (closes BUG-070's remainder); the
  "(missing layer)" row states what happened and offers the repair ("Input layer was
  deleted — choose a replacement"); "Cap+1"/"St"/"Mo" chips keep their width but gain
  hover tooltips spelling them out. No broader visual redesign — that's
  UI_SOTA_UPGRADE_PLAN's territory (scope fence).

## 3. Design body

### 3.1 Data model (manifold-core)

```rust
// crates/manifold-core/src/audio_trigger.rs — replaces TriggerRoute for authoring.
// Serialized on Layer; shaped like the flat MIDI fields' sibling block at
// layer.rs:140 and like PresetInstance.audio_mods for the Vec-of-configs pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerClipTrigger {
    pub enabled: bool,
    /// Send + feature + band — the SAME source type every audio mod uses.
    pub source: AudioModSource,
    /// Sensitivity/attack/release/curve/invert/rate-of-change — the SAME shape.
    /// Amount tunes the shaped signal against the fixed 0.5 fire edge (U2).
    pub shape: AudioModShape,
    /// How long the fired one-shot holds (no note-off exists for a transient).
    pub one_shot_beats: Beats,
    // Runtime edge/follower state (TransientEdge, smoothed, prev_raw) lives in
    // the evaluator keyed by (LayerId, index) — NOT serialized, matching
    // LiveTriggerState::armed (live_trigger.rs:47). Rationale: Layer is a pure
    // data model; ParameterAudioMod carries its edge inline only because the
    // mod struct already held follower state — do not copy that here.
}

// Layer (layer.rs, beside the MIDI block):
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub clip_triggers: Vec<LayerClipTrigger>,
```

`AudioSend.triggers: Vec<TriggerRoute>` (`audio_setup.rs:146`) becomes
**deserialize-only legacy** exactly like `LegacyAudioTriggerMod`
(`audio_trigger.rs:135`): kept as a `#[serde(skip_serializing)]` field (or a shadow
deserialize struct — executor's choice, both are house patterns), never written, drained
by the load migration. `TriggerRoute`'s public helpers (`threshold`,
`sensitivity_to_threshold` consumers outside the legacy path) go with it — §3.5 deletion
gate.

### 3.2 Migration (load-time, U5 precedent)

For each send, for each legacy `TriggerRoute`: resolve the target layer —
`target_layer` id if set, else auto-route by send label (the fire-time name match in
`live_trigger.rs`, run once at load). Resolved → push a `LayerClipTrigger` onto that
layer: `source` = (send id, `Transients`, route band), `shape` = default with
sensitivity approximated into Amount (the exact U5 mapping — "exact-feel fidelity
explicitly NOT owed", feature is weeks old), `one_shot_beats` preserved, `enabled`
preserved. Unresolvable (no such layer) → **dropped with a counted `eprintln`** naming
the send and band. *Consequences, stated honestly:* that drop is silent on-screen today
— acceptable only because the feature is weeks old and lives in Peter's own projects;
the entry joins BUG-079's "surface load notices in-app" scope rather than growing its
own UI. Round-trip gate in P2 covers: legacy project loads → triggers fire → save →
reload → triggers still fire (the BUG-036 lesson: create-path green is half a gate).

### 3.3 Evaluation (manifold-playback)

`LiveTriggerState::evaluate` (`live_trigger.rs:56`) changes its walk: layers with
non-empty `clip_triggers` instead of `setup.sends[].triggers`. Per enabled config:
extract `source.feature` from the send's `SendFeatures` (same
`AudioFeature::extract` the mod path uses, `modulation.rs:519`), run
`shape.apply(raw, dt, &mut smoothed, &mut prev_raw)`, fire on
`trigger_edge.advance(out_norm, 0.5)`. Follower + edge state keyed
`(LayerId, usize)` in the evaluator's map (extending `armed`,
`live_trigger.rs:47-48`). `FireRequest` simplifies: the target IS the owning layer —
`send_label` auto-routing dies (it was a workaround for send-side authoring not
knowing the layer). Quantize snap at the sink is untouched.

**Hot-path note:** the walk is per-analysis-block (not per-frame), same as today;
state map allocates only on first fire per key, same as today. No new allocation
class.

### 3.4 Analysis gating — the one new walker arm

AUDIO_SENDS D4's consumed-set rebuild (in `audio_mod_runtime.rs`) and
`AudioSetup::has_active_triggers`-style capture gating currently read
`send.triggers`. Both gain the layer walk: a send is consumed if any layer has an
enabled `clip_trigger` sourcing it. ⚠ VERIFY-AT-IMPL: enumerate every reader of
`send.triggers` / `has_active_triggers` / `trigger_for` before P2 —
`rg -n "\.triggers|has_active_triggers|trigger_for" crates/ -g '*.rs'` — and re-point
each; if the count differs from the session sweep baked into the P2 brief, stop and
list the new sites first. U4's lesson inverted: we deleted walker arms by deleting a
config type; moving a config type means every walker that knew the old home must
learn the new one — miss one and triggers fire but analysis never starts (or capture
never spins up). The P2 gate proves the arm end-to-end: a project whose ONLY audio
consumer is a clip trigger must start capture and fire.

### 3.5 The dock (manifold-ui + manifold-app)

`ScreenLayout` grows: `audio_setup_width: f32` input (0.0 closed; default-open width
~460 px, constant beside `DEFAULT_INSPECTOR_WIDTH`), `audio_setup()` rect (between
content and inspector: `x = screen_width - inspector_width - audio_setup_width`),
`content_area()` subtracts it, resize handle + double-click snap-back cloned from the
inspector pair (`layout.rs:282`, the `AnimF32` mirror-field pattern at `:39`). Unit
tests shaped like `inspector_shrinks_both_preview_and_timeline` (`layout.rs:412`).

The panel stops being an `Overlay`: `OverlayId::AudioSetup` and its dispatch arm
(`ui_root.rs:27`, `:982`) are deleted; the root build pass builds the panel into
`layout.audio_setup()` when open (⚠ VERIFY-AT-IMPL: the exact root build call
sequence — read `ui_root.rs` around the panel field `:211` and the existing
per-frame `update_meters`/scope calls, and re-anchor before P1). Open/close: the
header Audio button and Escape now toggle `audio_setup_width` between 0.0 and the
default (Escape handling moves from the overlay's `on_event` arm to the same
key-dispatch site the overlay driver used — one path, not both). The panel body
becomes a `ScrollContainer` (see `guide_scroll_and_clipping`; fixes BUG-047).
Per-frame meter/scope updates (`update_meters` `:1943`) are position-independent and
carry over unchanged.

**The plausible-wrong architectures, forbidden by name:**
- You will want to keep the overlay path alive "for small screens" or behind a flag —
  no. One layout rule at every width (D1, Peter's call); the overlay registration is
  deleted the same phase the column lands, with an rg-zero gate.
- You will want to invent a second trigger config type or keep `TriggerRoute` alive as
  a parallel authoring path — no. That is the exact §9 U1 bug class ("every gate,
  walker, drawer, and command must know about two things"); `LayerClipTrigger` is the
  only authorable clip-trigger shape, and the legacy field is deserialize-only.
- You will want the panel to read `Project` directly now that it's a "real panel" —
  no. `state_sync` remains the sole boundary (AUDIO_SENDS §3.1's rule, still binding);
  the panel renders view-model rows exactly as before.
- You will want to fix BUG-082 by restricting the Feature row — no (D6). The meter
  row is the fix; U2 stands.

## 4. Phasing

### Phase 1 — The dock (layout column + overlay-path deletion + scroll) — SHIPPED 2026-07-10 (`36a96791`)
- **Entry state:** `rg -n "audio_setup_width" crates/manifold-ui/src/layout.rs` → zero
  hits; `rg -n "OverlayId::AudioSetup" crates/manifold-app/src/ui_root.rs` hits `:27`
  region; anchors `layout.rs:88/:152`, `audio_setup_panel.rs:745/:2314` re-verified.
- **Read-back:** this doc D1/D7-scroll/§3.5; `layout.rs` whole; the overlay driver's
  dispatch for `OverlayId::AudioSetup`; `guide_scroll_and_clipping` memory. Restate:
  one layout rule at all widths, overlay path deleted not paralleled, state_sync
  boundary untouched.
- **Deliverables:** `audio_setup_width` + `audio_setup()` + `content_area()`
  subtraction + resize handle + snap-back + layout tests; panel built from root pass
  into the column rect; `OverlayId::AudioSetup` deleted; Escape + Audio-button toggle
  re-wired; panel body in a `ScrollContainer`.
- **Gate:** *Positive:* layout unit tests (column bounds content; zero-width = today's
  rects byte-identical); headless PNG with panel open — preview, timeline, inspector,
  and panel all visible, panel sections scrollable with a tall fixture (BUG-047's
  repro fixture). L3: a `scripts/ui-flows/` flow opens the panel via the header
  button, asserts an inspector param row is still clickable, closes via Escape.
  *Negative:* `rg "compute_overlay_rect|Modality" crates/manifold-ui/src/panels/audio_setup_panel.rs`
  → zero hits; `rg "OverlayId::AudioSetup" crates/` → zero hits.
- **Performer gesture:** drag a bound param's slider in the inspector while the
  spectrogram runs — both visibly live in one frame of screen.
- **Forbidden moves:** dual overlay/dock mode; width-threshold special cases; touching
  panel section content beyond the scroll wrap.
- **Test scope:** `cargo test -p manifold-ui --lib` + the flow; no workspace sweep.
- **Demo:** the PNG + flow above — L3.

### Phase 2 — LayerClipTrigger model + migration + evaluation (core/playback/io/app-runtime) — SHIPPED 2026-07-10 (`e4aa01bf`)
- **Entry state:** §3.4's `rg` sweep run fresh, count matches the brief's baked list
  (else stop and list); `audio_trigger.rs:152/:194`, `live_trigger.rs:56`,
  `layer.rs:140` re-verified.
- **Read-back:** D2/D3/D4, §3.1–§3.4; `live_trigger.rs` whole; U5's migration
  (`effects::migrate_legacy_audio_trigger`); the round-trip-gate rule
  (DESIGN_DOC_STANDARD §5). Restate: one config type, deserialize-only legacy,
  every `send.triggers` reader re-pointed.
- **Deliverables:** `LayerClipTrigger` + `Layer.clip_triggers`; legacy deserialize +
  load migration (label auto-route resolved at load; counted eprintln on drop);
  evaluator walk over layers; `FireRequest` simplification; the §3.4 walker arm;
  EditingService commands for add/remove/edit (shaped like the audio-mod command
  family, U-P2's `SetAudioModTriggerModeCommand` being the smallest member).
- **Gate:** *Positive:* named tests — legacy-JSON migration round-trip (load → fire →
  save → reload → fire); capture-gating test (clip-trigger-only project starts
  analysis); evaluator fire test ported from the route tests. `cargo test -p
  manifold-core -p manifold-playback -p manifold-io --lib` + `cargo test -p
  manifold-app --lib`. *Negative:* `rg "TriggerRoute" crates/ -g '*.rs'` → hits only
  in the legacy-deserialize path and its migration test;
  `rg "sensitivity_to_threshold"` → hits only in `TransientEdge` internals/tests (or
  zero if fully absorbed).
- **Performer gesture:** none UI-visible yet (model phase) — **Demo: none — L1**, and
  that is why P3 lands in the same wave.
- **Forbidden moves:** serializing the legacy field; a "both homes work" transition
  period; silently skipping an unresolvable route (must count + eprintln).
- **Test scope:** focused crates above; the wave's final phase runs the workspace sweep.

### Phase 3 — Layer-side authoring UI + drawer unification + fire meter (D5/D6)

> **SPLIT 2026-07-10 during execution.** **P3a SHIPPED (`47f2a112`):** the Triggers-matrix
> deletion (§ deliverable "matrix deleted") + the shared drawer's Length-row capability
> (D5) — both independent of the placement question below. Consumers rows re-pointed to
> `Project::clip_trigger_consumers` (the panel-side display of layer-owned triggers).
> **P3b BLOCKED — needs a placement decision.** The design says the authoring section sits
> "beside the layer's MIDI clip-launch block", but that block renders in the timeline track
> header (`layer_header.rs`), whose row height is the fixed `TRACK_HEIGHT` constant with a
> test forbidding per-type exceptions. A **variable-length** list of clip-trigger drawers
> cannot fit a fixed-height row without breaking the `single-source-y-layout` /
> `track-header-invariant` invariants. **Resolution (Peter, 2026-07-10, AskUserQuestion):**
> _"Inspector AUDIO TRIGGERS drawer... a single section that sits at the top of the inspector
> for the layer and is default collapsed."_ ONE collapsible section pinned at the top of the
> selected layer's inspector (not per-effect-card), default-collapsed so it doesn't eat space
> until opened; the inspector already hosts the identical `build_audio_mod_drawer` machinery
> and is variable-height/scrollable.
>
> **P3 further split into P3b + P3c** (the fire meter is net-new content-thread→UI live-value
> plumbing touching EVERY fire-mode drawer, a distinct work class with its own hot-path gate —
> keeping it out of the authoring phase):
> - **P3b (authoring):** the inspector AUDIO TRIGGERS section (single, top, default-collapsed)
>   + the clip-trigger drawer (D5 Length row `Some`, NO Mode row per D4) + a parallel additive
>   `PanelAction` command family (the existing `build_audio_mod_drawer` action vocabulary is
>   keyed on `(GraphParamTarget, ParamId)`, which `LayerClipTrigger` — addressed by `LayerId`
>   + index — cannot express; per P2, the commands are whole-value-replace) + the state_sync
>   view-model rows. Restores authoring, closing P3a's interim gap. UI-only, no thread crossing.
> - **P3c (fire meter, D6 + BUG-082):** the live level meter with the 0.5 threshold line beside
>   the Amount row of EVERY fire-mode drawer (param gate cards + clip triggers). Reads the
>   shaped `shape.condition()` signal (per the D3 AS-BUILT note). Net-new content-thread→UI
>   snapshot of the per-config shaped value — gated with `MANIFOLD_RENDER_TRACE=1` (no per-frame
>   content-thread allocation; the deleted `update_trigger_levels` per-frame pattern is the
>   precedent). This is BUG-082's fix; U2 stands (no Feature-row restriction).
- **Entry state:** P2 landed; `param_slider_shared.rs:1518` drawer builder re-anchored;
  U-P2's Mode-row parameterization read.
- **Read-back:** D4/D5/D6; `build_audio_mod_drawer` + the U-P2 landing notes in
  LIVE_AUDIO_TRIGGERS §9.2. Restate: one drawer builder, Length row not Mode row,
  meter reads the shaped signal not the raw feature.
- **Deliverables:** layer header/inspector "AUDIO" trigger section beside the MIDI
  block (add / remove / per-config drawer); Length row in the shared drawer builder;
  **fire meter with 0.5 threshold line beside Amount on every fire-mode drawer**
  (param gate cards + clip triggers); Audio Setup Triggers matrix deleted — Consumers
  rows (already navigational) are the panel-side display; state_sync view-model
  extensions for the new rows; BUG-082 entry amended (fix = this meter; Status
  FIXED @ sha).
- **Gate:** *Positive:* headless PNGs — layer with a clip trigger armed (drawer open,
  meter visible), a gate-card drawer showing the same meter; L3 flow — add a clip
  trigger via the layer UI, set band, assert the config row appears; affordance check
  per standard §5 (every clickable row reads as clickable in the static PNG).
  *Negative:* `rg "trigger" crates/manifold-ui/src/panels/audio_setup_panel.rs -i` →
  no matrix-row builders remain (Consumers rows only);
  `rg "build_audio_trigger|clip_trigger_drawer"` → zero hits (no forked drawer).
- **Performer gesture:** play a kick-heavy track, add a clip trigger on the Strobe
  layer, watch the meter cross the line, see the one-shot fire — without opening
  Audio Setup at all.
- **Forbidden moves:** a bespoke drawer; a Mode row on clip triggers; drawing fire
  feedback on the spectrogram (D7: it lives in the drawer).
- **Test scope:** focused ui/app libs; workspace sweep here if this is the wave's last
  code phase.

### Phase 4 — Readability + hygiene polish (D7/D8)
- **Entry state:** P1 landed (labels are positioned against the docked geometry).
- **Read-back:** D7/D8; the screenshot-documented label collision (this doc's intro);
  BUG-070's remaining-surface list.
- **Deliverables:** band labels as chips on their divider lines; per-source-row level
  meters; selected-source highlight; gain-stepper + send-fader double-click reset
  (BUG-070 remainder → Status update); "(missing layer)" copy + repair affordance;
  tooltips on Cap+N/St/Mo chips.
- **Gate:** *Positive:* headless PNG at the default width AND at minimum panel width —
  labels legible, non-overlapping in both (the readability condition, checked at the
  width where it's hardest); L3 flow double-clicks a gain stepper, asserts reset.
  *Negative:* none beyond clippy.
- **Performer gesture:** glance test — with four sources playing, name which stems
  have signal without clicking anything.
- **Forbidden moves:** widening into the UI_SOTA visual pass; new spectrogram overlays.
- **Test scope:** focused ui lib + workspace sweep + `cargo clippy --workspace -- -D
  warnings` (wave close).

**Phasing-completeness walk:** dock/resize/toggle (P1) · scroll/BUG-047 (P1) · model +
migration + evaluation + gating arm (P2) · layer authoring UI + shared drawer + Length
row (P3) · fire meter/BUG-082 (P3) · matrix deletion + Consumers-as-display (P3) ·
band labels, source meters, selection highlight, resets/BUG-070, copy fixes (P4).
Every §2/§3 commitment appears above or in Deferred.

## 5. Decided — do not reopen

1. One layout rule at every window width: content shrinks; no overlay fallback, no
   inspector collapse (Peter, 2026-07-09).
2. Clip triggers are authored on the layer ONLY; Audio Setup displays them via the
   navigational Consumers rows (Peter, 2026-07-09).
3. One trigger vocabulary: `AudioModSource` + `AudioModShape` + `TransientEdge` at the
   fixed 0.5 edge; Amount is the tune knob. No bespoke sensitivity anywhere.
4. U2 stands: any feature may drive a fire-mode config. BUG-082's fix is the D6 meter,
   never a feature restriction.
5. One drawer builder for all three audio-config callers; trailing rows parameterize it.
6. Fire-tuning feedback lives in the drawer meter, never on the spectrogram.
7. The panel stays one surface (AUDIO_SENDS "no Devices/Tuning split" carries over to
   the dock).
8. `state_sync` remains the panel's sole data boundary.

## 6. Deferred

- **Level-crossing fire detector as first-class config** (explicit threshold +
  hysteresis knobs, per-feature defaults) — Peter, 2026-07-09: "'fire when amplitude
  crosses a level' is a real future widening but it's a detector question, separable
  from the config unification" — noted here by his direction. Revive when: the D6
  meter + Amount tuning proves insufficient for level features in real use (the
  observable: Peter reaching for rate-of-change as a workaround on a level feature
  more than once).
- **Bulk trigger-tune view** (all of a song's clip triggers in one list). Revive when:
  layer-by-layer calibration via Consumers navigation proves clumsy in a real set-prep
  session (D2's recorded cost).
- **In-app load-notice surface for dropped/unresolvable migrated routes** — owned by
  BUG-079's fix, not this design.
- **Panel visual redesign beyond D7/D8** — UI_SOTA_UPGRADE_PLAN's territory.
- **Cap+N/St/Mo chip hover tooltips** (D8, escalated in P4): `manifold-ui` has no retained-mode
  tooltip primitive (only an immediate-mode one bespoke to `graph_canvas`), and building one
  crosses this design's own "no new widget kinds" audit + needs per-frame cursor plumbing.
  Revive when a shared tooltip primitive exists — `UI_WIDGET_UNIFICATION_DESIGN` (landed
  2026-07-10) is the likely home; this is a consumer, not the builder.
  **[2026-07-11: MOOT — P8 deletes all three chips; the panel keeps nothing that needs a
  tooltip. The primitive gap itself still belongs to UI_WIDGET_UNIFICATION.]**

## 7. Wave 2 (2026-07-11) — as-built correction + P5–P8

Authored the morning after Wave 1 landed, from Peter's rig review of the shipped panel
(Fable session, decisions Peter's). Two inputs: a false fix (the D6 meter never worked —
§7.1), and a usability verdict on the panel Wave 1 built ("I don't really understand how
to read the settings panel anymore").

### 7.1 As-built correction — the D6 fire meter has never displayed a clip-trigger level

BUG-109. Three independent breaks, all must land together (P5):

1. **Playing: capture wiped after triggers write.** P3c (`12fbc37d`) placed the per-tick
   `FireMeterCapture` reset inside `tick_playing`'s modulation step
   ([`engine.rs:901`](../crates/manifold-playback/src/engine.rs#L901)) with a comment
   asserting clip triggers evaluate "below". They evaluate at step 3b
   ([`engine.rs:844`](../crates/manifold-playback/src/engine.rs#L844)) — *above* —
   placed there by `d285663f` so a fired clip starts the same frame
   (`sync_clips_to_time` runs at step 4). Every playing tick: triggers push at 3b,
   step 7 resets, `evaluate_modulation` re-pushes only param-gate levels. The worker
   trusted `tick_audio_triggers`' stale doc comment ("called after modulation") over the
   call site 60 lines up — and over CORE_ENGINE_MAP's frame diagram, which shows the
   true order. Lesson recorded for the daemon corpus (comment-vs-callsite).
2. **Stopped: never evaluated.** `tick_non_playing` doesn't call `tick_audio_triggers`
   at all (correct for *firing* — one-shot expiry is beat-based) — so the meter is zero
   exactly when a performer tunes a trigger at soundcheck: transport stopped, music
   playing through the tap, spectrogram alive.
3. **Even fixed, it would blink.** `condition()`'s transient signal decays in
   milliseconds; an instantaneous fill at ContentState snapshot cadence is invisible.
   The meter needs UI-side peak-hold.

VD-025 recorded the honest gap ("live render-trace never run — no audio device in the
sandbox") and the wave still closed BUG-082 on a headless PNG. The PNG proved the meter
*renders*; only a live look proves it *moves*. P5's gate is written accordingly.

### 7.2 Decisions (Peter, 2026-07-11) — do not reopen

1. **Kick lane: removed from the scope outright** — tick lane and legend chip both. Not
   conditional display ("confusing if it's sometimes there and sometimes not" — elements
   are always present or absent, never state-toggled). The kick *detector* and the
   drawer's Kick feature button are untouched (KICK_SWEEP_EVENT stands).
2. **Delta (`rate_on`) leaves the UI everywhere** — both drawer targets, param mods and
   clip triggers ("not very useful and adds a lot of clutter"). Runtime field +
   conditioning arm stay dormant for a possible future re-wire; saved `rate_on` flags
   are cleared at load so no config carries invisible behavior the UI can't show.
3. **"Amount" renamed "Sensitivity"** (display label only — §5 item 3's one-knob
   vocabulary stands; this renames the knob, it does not add a threshold control).
4. **Manual test-fire button: rejected** ("don't need"). Do not re-propose.
5. **Scope↔drawer linkage: approved enthusiastically** (P7).
6. **AUDIO TRIGGERS section title stays as-is.** Hz readouts on the crossover dividers:
   not now (fine to leave). Floor stepper as-built confirmed OK (shows dB once engaged).
7. **Cap chip, Inputs authoring ("+ Layer" + per-layer ×), St/Mo toggle: removed from
   the panel** (P8) — the panel's job is *device in → sends → scope → who's listening*;
   authoring lives layer-side (extends Decision 2's matrix precedent to the last
   panel-side authoring surface).

### 7.3 Phasing

### Phase 5 — Fire meter resurrection (BUG-109; re-closes BUG-082) — SHIPPED 2026-07-11
- **As-shipped correction:** the brief's own heading said this phase "closes VD-025" —
  it doesn't, by design (§7.3's own "Explicitly not claimable" clause below). VD-025
  stays open until Peter's rig look confirms the live crossing; only the plumbing
  ships here (L1 unit tests + a synthetic-level PNG, per the brief's own gate).
- **Entry state:** D6 plumbing exists end-to-end (capture → snapshot → `update_fire_meters`)
  and is key-consistent on both sides; it has never displayed a clip-trigger level (§7.1).
- **Read-back:** §7.1; `engine.rs` steps 3b/7 in both tick branches; CORE_ENGINE_MAP frame
  diagram; `drawer.rs` `MeterIds::update`.
- **Deliverables:** (a) ONE `FireMeterCapture` reset per tick, at the top of `engine.tick`
  before any evaluator, both branches — delete the two mid-branch resets; (b) stopped-tick
  conditioning walk: `LiveTriggerState::evaluate` grows a `fire: bool` (or equivalent
  split) — `tick_non_playing` runs the same `condition()` walk pushing meter levels,
  keeping follower envelopes warm, but never calls `edge.advance` and returns no fires;
  (c) UI-side peak-hold in `MeterIds::update`: instant attack, ≥250 ms hold, then decay
  (a 5 ms conditioned spike must stay visible ≥250 ms; the ≥0.5 "firing" bright accent
  latches for the same hold); (d) fix the stale `tick_audio_triggers` doc comment (says
  "after modulation"; it runs at 3b *before* `sync_clips_to_time`, and why); (e)
  CORE_ENGINE_MAP: non-playing branch line gains the conditioning walk, both branches note
  the top-of-tick reset; (f) BUG_BACKLOG: BUG-109 → FIXED, BUG-082 annotation updated.
- **Gate:** *Positive:* unit tests — a playing tick with an enabled trigger + synthetic
  snapshot leaves `fire_meters.get(key)` `Some` at tick end (the regression that was
  impossible before); a stopped tick likewise `Some` AND returns no `FireRequest`;
  headless PNG with a synthetic level shows fill + bright-over-0.5 state. *Negative:*
  no fires while stopped; param-gate meter keys still present after a playing tick
  (the reset move must not starve `evaluate_modulation`'s own capture).
- **Explicitly not claimable by the executor:** the live look on a real device. The phase
  ships as "plumbing proven, awaiting Peter's rig"; VD-025 and the BUG-109 closure notes
  say so until his feel-pass confirms. (The exact overclaim that produced §7.1.)
- **Performer gesture:** transport stopped at soundcheck, track through the tap — the
  Sensitivity meter breathes with the music and the tick shows where the fire line sits.
- **Forbidden moves:** touching fire semantics while playing (step-3b placement stands);
  content-side smoothing of the meter signal (hold is a UI concern; the capture stays
  the raw conditioned value the edge reads).
- **Test scope:** `-p manifold-playback --lib` focused + `-p manifold-ui --lib`; sweep at
  landing.

### Phase 6 — Drawer cleanup: Sensitivity, Delta removal, Invert — SHIPPED 2026-07-11
- **Entry state:** P5 landed (the meter the renamed slider tunes is alive).
- **Read-back:** §7.2 items 2–3; `param_slider_shared.rs` drawer builder; the U5/P2
  migration precedent (`migrate_legacy_clip_triggers`); AUDIO_MODULATION_DESIGN.md's
  Invert/Delta row (§ around line 211).
- **Deliverables:** "Amount" label → "Sensitivity" (all fire-mode + mod drawers;
  `AudioShapeParam::Sensitivity` is already the internal name — display strings only);
  Delta button removed from the shared toggle row for BOTH `AudioModDrawerTarget`s (one
  builder stays one builder); "Inv" → "Invert" in the freed space; load-time migration
  clears `rate_on` on every `AudioModShape` carrier (param mods + clip triggers),
  counted + `eprintln!`'d per the P2 pattern; runtime `rate_on` + its `condition()` arm
  stay compiled (if the UI removal strands helpers into dead-code warnings, the `allow`
  names its un-suppression trigger: "re-wire per AUDIO_SETUP_DOCK §7.2 item 2");
  AUDIO_MODULATION_DESIGN.md gets an as-built note (Delta UI removed 2026-07-11,
  runtime dormant).
- **Gate:** *Positive:* migration unit test — fixture with `rate_on: true` on both a
  param mod and a clip trigger loads cleared with count logged, and stays cleared on
  round-trip; drawer PNG shows Sensitivity + Invert and no Delta on both drawer kinds;
  Liveschool fixture loads green. *Negative:* a project saved post-migration contains no
  `rate_on: true` anywhere.
- **Performer gesture:** the drawer reads top-to-bottom as plain language: what I listen
  to, how sensitive, how fast, how long.
- **Forbidden moves:** deleting the runtime arm (Peter wants the re-wire option);
  renaming `AudioShapeParam` variants or serde fields (display-only rename).
- **Test scope:** `-p manifold-ui --lib` + `-p manifold-io --lib` (migration) focused;
  sweep at landing (P5+P6 batch).

### Phase 7 — Scope follows the trigger drawer — SHIPPED 2026-07-11
- **Entry state:** P5/P6 landed. The scope-tap mechanism exists (analysis gating's
  "scope-tapped send"); the Audio Setup panel owns tap selection today.
- **Read-back:** §7.2 item 5; §5 item 6 (the boundary this phase must respect);
  analysis-gating arm in `audio_mod_runtime.rs`; drawer expand/collapse flow in
  `audio_trigger_section.rs` / `param_card.rs`.
- **Deliverables:** expanding any fire-mode drawer (clip trigger or param gate card)
  re-taps the scope to that config's send; collapsing restores the Audio Setup panel's
  own selected send; while a non-Full band is selected, the scope dims the spectrum
  outside that band's crossover range (two translucent overlay quads derived from the
  existing divider positions — Full = no dim). The dimming shows *what the config
  listens to*; fire feedback stays in the drawer meter (§5 item 6 is not reopened).
  Tap-follow state is session-only, never persisted.
- **As-built correction:** the "two translucent overlay quads" are NOT UI-tree nodes.
  Reading `ui_frame.rs`'s render order found the VQT waterfall blit is the LAST GPU
  draw call every frame (`spectrogram.render()` → `blit_texture_pane`, after the panel
  atlas AND the overlay flush) — a UI-tree quad positioned inside `scope_rect` would be
  painted over by that blit on every live-data frame (the existing per-band meters are
  deliberately kept OUTSIDE `scope_rect`, in the reserved margin, for exactly this
  reason — `audio_setup_panel.rs`'s own comment: "the blit would otherwise cover
  them"). The dim is instead computed IN the same shader pass that already draws the
  band-divider lines (`spectrogram.wgsl`): `Params` gained `dim_lo_y`/`dim_hi_y` (the
  KEPT y-range, same bottom-up 0..1 convention as `band_lo_y`/`band_hi_y`; `< 0`
  disables), `Spectrogram::render` gained a `dim_range: Option<(f32, f32)>` parameter,
  and the fragment shader darkens (`× 0.28`) the colour-mapped magnitude outside the
  kept range, before drawing dividers/centroids/onsets/cursor so those stay
  full-brightness. `VqtPassState` gained `band_dim`, resolved once per frame from
  `UIRoot::open_fire_mode_drawer_band()`.
- **Gate:** *Positive:* `open_fire_mode_drawer_send`/`_band` unit tests
  (`param_card.rs` — armed trigger-gate row reads its send+band; disarmed → `None`)
  proving the tap-follow/dim-selection LOGIC; a real GPU readback test
  (`spectrogram::gpu_tests::dim_range_darkens_outside_the_kept_band_only`, `gpu-proofs`)
  proving the SHADER — a uniform white fill darkens outside `dim_range` and stays full
  brightness inside it, on the actual Metal pipeline, not a value-level stand-in.
  *Negative:* `open_fire_mode_drawer_send_is_none_for_a_plain_continuous_mod` proves an
  armed non-trigger-gate drawer never re-taps; no new UI widget kind was added (the dim
  lives in the existing shader, not a new overlay primitive).
- **Not claimable by this executor (mirrors P5's §7.1 lesson):** an L3 ui-flow driving
  a real drawer-expand click, and a live full-pipeline PNG showing "open a trigger
  drawer in the actual app → the spectrogram visibly dims" — both need either a new
  harness interact verb (none exists for arming/expanding an audio-mod drawer) or a
  live VQT column feed (`content_num_bins > 0`, which needs real audio, unavailable
  headless — the exact gap P5's VD-025 already names for the waterfall itself). Carried
  as verification debt, not claimed here.
- **Performer gesture:** open a kick trigger and the spectrogram immediately shows the
  band it hears, dimming everything it doesn't — tune Sensitivity while watching both.
- **Forbidden moves:** fire-level display on the spectrogram (§5 item 6); persisting the
  follow-tap; widening into multi-scope.
- **Test scope:** `-p manifold-ui --lib` + `-p manifold-app --lib` + `-p manifold-spectral
  --features gpu-proofs` (shader) focused; sweep at landing (P7+P8 batch).

### Phase 8 — Panel de-clutter (the cuts) — SHIPPED 2026-07-11
- **Entry state:** P5–P7 landed (the panel's remaining job is legibility).
- **Read-back:** §7.2 items 1 + 7; AUDIO_SENDS_UX_DESIGN D1/D2 (the layer↔send binding
  this phase re-scopes to layer-side-only authoring); `state_sync.rs`'s AudioSendRow
  builder; `manifold_spectral::ScopeOnsets` (lane definition).
- **Deliverables:**
  (a) kick lane out of the scope: lane definition, tick emission into `ScopeColumn`,
  legend plumbing — detector + Feature button untouched;
  (b) Cap chip off the send row; `source_label`/`layer_fed` dropped from `AudioSendRow`
  if unconsumed after; device-vs-layer-fed detail lives in the routings dropdown only;
  (c) Inputs section authoring removed ("+ Layer" row, per-layer ×,
  `AudioSendAddLayerClicked` and friends): the section becomes read-only routing
  display — device line + one line per feeding layer, absorbing the D8
  "(missing layer)" repair copy, which now points at the layer header's Send dropdown
  (the surviving authoring surface; same command, one owner — the matrix precedent
  finished);
  (d) St/Mo toggle deleted (`AudioSendStereoToggle` action + handler + button): the
  channel dropdown enumerates stereo pairs AND single channels ("Left + Right", "Left",
  "Ch 3+4", "Ch 3"…) — mono falls out of picking one channel;
  `SetAudioSendChannelsCommand` already carries any channel vec, no model change;
  (e) Consumers trigger rows reworded: "Clip trigger · <Layer> · <Band>" (mod rows keep
  "Layer · Effect · Param");
  (f) design cross-refs: AUDIO_SENDS_UX D2 as-built note (panel-side layer routing
  authoring removed 2026-07-11 — layer header owns it).
- **Gate:** *Positive:* panel unit tests updated to the new row set; headless PNG open +
  closed at default and minimum width (P4's readability condition re-checked after the
  cuts); Liveschool fixture loads and its routed sends display correctly read-only; L3
  flow — layer header Send dropdown still routes (the one remaining authoring path).
  *Negative:* `rg`-zero on `AudioSendStereoToggle` and the removed panel actions; scope
  renders no kick lane and no orphaned legend gap.
- **Performer gesture:** a first-time reader answers "what feeds this send, and who's
  listening to it" from the panel alone, no tooltips needed.
- **Forbidden moves:** touching send *creation* ("+ Add Source" stays); the UI_SOTA
  visual pass; new widget kinds; conditional visibility anywhere (§7.2 item 1's rule).
- **Test scope:** `-p manifold-ui --lib` + `-p manifold-app --lib` (state_sync) +
  `-p manifold-renderer --lib` if `ScopeOnsets` lives there; full workspace sweep +
  `cargo clippy --workspace -- -D warnings` at wave close.
- **As-built correction:** `ScopeOnsets` lives in `manifold-spectral`, not
  `manifold-renderer` — the brief's own entry-state anchor was imprecise; verified
  at execution time (§5's re-verification rule) and the test-scope line above
  followed the real crate. Removing the `kick: f32` field (not a flag) shrinks
  `ScopeOnsets::COUNT` 4→3 and `ScopeColumn::STRIDE` 8→7 at compile time — every
  consumer (`LANE_COLORS`/`LANE_LABELS`/`lanes()`, the WGSL uniform, the
  `mod_harness` CPU port's lane loop) is generic over the count already, so the
  removal cascaded with a one-line push-site edit
  (`analysis.rs`'s `ScopeColumn` construction) and one obsolete end-to-end test
  deletion (`streaming_analyzer_scope_reports_kick_fires`) — no shader change.
- **As-built correction:** the "Cap" chip's click-to-reveal routings dropdown
  (`AudioSendRoutingsClicked`, `send_routings()`, `DropdownContext::AudioSendRoutings`)
  is deleted along with the chip, not kept as an alternate access path — its
  entire content (device + feeding-layer lines) already lives in the Inputs
  section post-(c), permanently visible, so the click-through became pure
  redundancy. `feeding_layer_ids()` and the `audio_layers` cache (state_sync →
  `UIRoot::set_audio_layers`) are deleted with it — both existed only to feed
  the deleted "+ Layer" dropdown.
- **As-built correction:** `AudioSendRow::feeding_layers` (id + name pairs) is
  KEPT, not dropped alongside `source_label`/`layer_fed` — it's real project
  data (not row-level chrome) and removing it would have meant touching every
  fixture/test construction site for no behavioral gain; only its one caller
  (`feeding_layer_ids`) is gone.

**Phasing-completeness walk (wave 2):** meter reset order + stopped-tick walk +
peak-hold + stale comment + map lines (P5) · Sensitivity + Delta-out + Invert +
`rate_on` migration + mod-design note (P6) · tap-follow + band dimming + restore (P7) ·
kick lane, Cap chip, Inputs authoring, St/Mo, Consumers copy, sends-design note (P8).
Every §7.2 decision lands in exactly one phase; §7.2 items 4/6 are recorded rejections,
no phase.

**Landing:** two batches per §2c — P5+P6, then P7+P8. One warm worktree for the
workstream; base-verification guard on every brief; landing protocol per
`.claude/GIT_TREE_DISCIPLINE.md` §2.
