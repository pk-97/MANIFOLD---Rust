# Automation Lanes — Design

**Status: SHIPPED — P1–P4 on main @ `8b306de0` (2026-07-04); P5 (§7 addendum) partially shipped 2026-07-07 on `lane/automation-exposure` — see the P5 status block after §10 for exactly what landed vs. what remains.** The original gap (param-chooser + "+" + touch-to-select never shipped, so lanes could only be born via ARM recording) was the root cause of Peter's 2026-07-05 "lane-visibility issues" / dead-LANES report — the 2026-07-07 timeline-ux audit proved the LANES toggle functional end-to-end headless (real dispatch, strips off/on, PNGs; `scripts/ui-flows/toggle-lanes.json`) and root-caused the symptom as unreachability, not wiring: see `docs/TIMELINE_UX_AUDIT_2026-07-07.md` §1. Status previously corrected in the 2026-07-05 baseline review (the canonical stale-status escape, `DESIGN_DOC_STANDARD.md` §10). Open verification debt: VD-001 — Peter's L4 residue narrowed to confirming LANES lights live + ARM-recording a first lane.
**Prerequisites: none. Sequencing: `docs/DESIGN_BUILD_ORDER.md` wave 3. Note: SESSION_MODE_DESIGN §2 reserves a serde-optional field slot on `ClipSequence` for this feature — fill that slot, don't invent a second home.**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any
phase. Conformance-hardened: audit claims are a 2026-07-02 snapshot — run the §8.3
pre-flight before each phase.**

Timeline automation for effect/generator params, modeled on Ableton arrangement
automation. One sentence: **a lane is a beat-indexed base writer** — it records
or draws the user's hand over arrangement time, and the existing modulation
pipeline rides on top unchanged.

---

## 1. The model (decided — don't reopen)

MANIFOLD already has a two-tier param stack (`ParamSlot { base, value }`,
`crates/manifold-core/src/effects.rs`):

- **Tier 1 — hands** write `base` (persisted, last-writer-wins): UI slider
  commands, Ableton macros (`ableton_bridge.rs` → `set_base_param`), OSC param
  router, macro bank. `set_base_param` writes `base` AND `value` so the write
  is visible before the next modulation pass.
- **Tier 2 — modulators** recompute `value` from `base` every tick
  (`modulation.rs::evaluate_modulation`): reset base→value, LFO drivers
  (absolute set), audio mods (absolute set), envelopes (additive pull).
  Per-instance `ParamMapping` reshape applies downstream at the renderer
  boundary and never touches the slot.

**Automation lanes are a tier-1 hand, sampled from the arrangement.** Each
frame, for every non-overridden lane, sample the curve at `current_beat` and
`set_base_param`. The modulation pipeline is not touched — no new phase, no
reordering, no fifth silo.

This is exactly Ableton's semantics, which is the requirement ("mostly copy
how Ableton manages automation"):

| Ableton | MANIFOLD | Composes with automation? |
|---|---|---|
| Arrangement automation | lanes (this doc) | — |
| Clip modulation envelopes (relative) | envelopes (additive pull) | yes, already |
| M4L LFO (absolute, fights automation) | LFO drivers + audio mods | no — the modulator owns the param |

**Decided: drivers/audio-mods stay exclusive.** A lane on a param that has an
enabled LFO driver or audio mod does nothing (the modulator's absolute set
overwrites `value` regardless of base) — same as mapping an M4L LFO onto an
automated param in Live. The move, as in Live, is to automate the *driver's*
rate/trim instead (drivers are addressable state; automating driver fields is
deferred, §11). No base-relative driver mode in v1.

What this is on stage: the arc of the set gets drawn/recorded in the
arrangement — a slow filter sweep over 32 bars, a strobe-rate ramp into the
drop — and audio-reactive envelopes still breathe on top of it, exactly like
automating a macro under modulation in Live.

## 2. Data model

Lanes live **on the `PresetInstance`**, keyed by `param_id` — the exact
pattern of the four existing per-param automation rows (`drivers`,
`envelopes`, `audio_mods`, `ableton_mappings`). That buys for free: serde
with the instance, moving with the layer, param addressing via
`resolve_param_in` (registry + user-binding tail, so user-exposed graph
params work automatically), and orphan pruning
(`prune_orphaned_automation` / `prune_automation_by_ids` gain one more row
type).

```rust
// manifold-core/src/effects.rs (new, alongside ParamEnvelope etc.)

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AutomationLane {
    pub param_id: ParamId,
    pub enabled: bool,               // lane on/off (Ableton: deactivated lane)
    pub points: Vec<AutomationPoint>, // sorted by beat, ascending
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct AutomationPoint {
    pub beat: Beats,                 // arrangement beat, absolute
    pub value: f32,                  // param-range value (not normalized)
    pub shape: SegmentShape,         // shape of the segment LEAVING this point
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum SegmentShape {
    Linear,
    Hold,                            // step — required for enum/int-backed params
    Curved(f32),                     // -1..1 bend, Ableton-style segment drag
}
```

- On `PresetInstance`: `pub automation_lanes: Option<Vec<AutomationLane>>`,
  skip-when-empty serde per the existing convention (byte-identical projects
  when absent — no save-file migration needed; additive optional field in both
  V1 JSON and V2 ZIP).
- `points` sorted invariant enforced at write time (commands sort on insert),
  mirroring `TempoMap::ensure_sorted` (`tempo.rs` is the existing
  beat-anchored-curve precedent).
- `value` is stored in param range, not normalized — lanes survive nothing;
  they are resolved against `resolve_param_in`'s min/max only for clamping at
  write and sample time.
- Master effects (`project.settings.master_effects`) are `PresetInstance`s —
  they get lanes for free.

**Curve evaluation** (pure function, `manifold-core`):
binary-search the segment containing `beat`; before the first point → first
point's value (Ableton behavior); after the last → last value; `Curved(c)`
uses the standard power-curve bend. Deterministic, allocation-free.

## 3. Runtime sampling

A new function `evaluate_all_automation(project, current_beat, latches) -> bool`
in `manifold-playback` (own module, `automation.rs`), called in the content
tick **before `evaluate_modulation`** — it is a hand, not a modulator, so it
must land before the base→value reset. Walk shape is a copy of
`evaluate_all_audio_mods`: master effects + layer effects + gen params, skip
disabled instances, resolve via `resolve_param_in`, write via
`set_base_param`. No per-frame allocations: reuse the two-pass
resolve-then-write pattern with a scratch `Vec`.

Sampling runs whenever the transport is playing (and during offline export at
exported-frame beats — automation is a pure function of beat, so export is
deterministic by construction). When stopped, lanes don't write; params hold.

**`base` becomes derived state for automated params.** The per-frame
`set_base_param` bypasses undo and the editing service entirely (same as
Ableton-macro and OSC writes today). Saving mid-arrangement persists whatever
base the playhead last wrote — harmless, playback re-derives it. Dirty
tracking: lane sampling must NOT bump the project `DataVersion` per frame; it
returns `any_wrote` and folds into the existing `modulation_active`
compositor-dirty path (`content_thread.rs`).

## 4. Override latch (the precedence rule)

Ableton semantics, copied:

- A live touch on an automated param — slider grab, Ableton macro move, OSC
  write — **wins immediately and latches that param "overridden."** The lane
  stops writing. The lane data is untouched.
- **Back to Arrangement** clears latches and resumes lanes: one global action
  (transport-bar button, lights up red when any latch is set, exactly like
  Live) + per-lane re-enable in the lane UI.
- Latches are **runtime-only** (never serialized). Stored as an
  `AHashMap<(PresetId, ParamId), ()>` (or set) owned by the playback side, not
  the `Project`. Cleared on project load and on transport... no — Ableton does
  NOT clear overrides on transport stop/start; only Back to Arrangement (or
  re-record) clears. Copy that: latches persist across play/stop within a
  session.

**Touch detection — single funnel, no per-path hooks.** Add a runtime-only
`touched: bool` to `ParamSlot` (not serialized, `#[serde(skip)]`):
`set_base_param` sets it. The automation evaluator, per lane: if
`touched` since last frame → latch (or record, §5), clear the flag, skip the
write. Because every hand funnels through `set_base_param`, this catches UI
commands, Ableton, OSC, and macro bank with zero call-site changes. The
evaluator's own writes go through a private path that doesn't set `touched`
(or clears it after writing) — pin this with a test, it's the one
self-trigger footgun.

Ordering note: hands run earlier in the tick (OSC router, Ableton apply) than
the engine tick that samples automation. The `touched` flag makes first-touch
frames correct regardless of order — the evaluator sees the flag and yields
instead of clobbering.

## 5. Recording

Ableton's model: an **Automation Arm** toggle (global, transport bar). While
playing with arm on, touching a control writes *into the lane* instead of
latching an override.

- Armed + playing + `touched` → append/replace points at `current_beat` with
  the current `base` (the post-One-Euro smoothed value for Ableton sources —
  record what was applied, not the raw wire).
- While a touch is "held" (touched again within a short window, ~2 beats of
  inactivity ends the gesture), successive frames overwrite the beat range
  being passed — i.e. punch-over, Ableton overwrite behavior. On gesture end,
  the recorded segment joins the existing curve with boundary points at the
  punch-in/out beats (so the old curve resumes exactly — Live's behavior).
- If no lane exists for the touched param, arm creates one (this is how lanes
  are born from performance; drawing in the UI is the other way).
- **Undo:** per-frame writes during recording bypass undo; on gesture end the
  whole gesture commits as ONE undo entry carrying the pre-gesture point set
  (the mapping-drawer drag pattern — explicit reverse captured at gesture
  start, `new_with_reverse` precedent, see binding-unification drag-undo fix).
- Recording a param that has an exclusive modulator (driver/audio mod) records
  base movements that are invisible in the output — allowed, harmless, same
  as Live.

## 6. Editing & undo

All lane edits go through `EditingService` commands (`manifold-editing`):

- `AddAutomationPointCommand` / `MoveAutomationPointCommand` /
  `RemoveAutomationPointCommand` — point-level, drag preview + commit with
  explicit reverse (drag-undo pattern).
- `SetLaneEnabledCommand`, `ClearLaneCommand`, `RemoveLaneCommand`.
- `CommitRecordedGestureCommand` — the §5 single-entry commit.
- `BackToArrangement` is NOT a command (it mutates runtime latch state, not
  the project) — it's a `ContentCommand` variant handled on the content
  thread, no undo entry.

Addressing follows the unified card-target shape (Effect | Generator | Master
target + `param_id`), not indices — same discipline as
`EditUserParamBindingCommand`.

## 7. UI / UX (decided: copy Ableton's model — Peter, 2026-07-02)

**Placement — automation lives on the layer.** Expanding a layer (the
existing layer-expand affordance) reveals the advanced layer controls,
including automation: a param-chooser lane (device/instance dropdown +
param dropdown, Live's exact pair) with a **"+"** button that breaks out
additional lanes stacked under the layer. Each automated param gets its own
lane. Lane rows ride the shipped timeline redesign.

**Touch-to-select.** Touching any param on the layer (card slider, inspector
knob) auto-selects that param in the lane's chooser — Live's behavior; it
makes "wiggle the knob, then draw" the zero-friction path to a new lane.

**Interaction vocabulary — same shortcuts and controls as Live:**

- **Automation mode toggle** (Live's `A`): show/hide automation across the
  timeline; lanes draw as the red breakpoint line over the layer.
- **Click on the line** adds a breakpoint (dot); **drag** moves it (snapped
  to the timeline grid); **double-click a dot** deletes it; **Delete** removes
  the selection.
- **Drag a segment** vertically to move it; **modifier-drag a segment**
  (Alt/Option, Live 11 style) bends it into a curve — this is the
  `Curved(f32)` shape in §2.
- **Cmd-drag** bypasses grid snap for fine placement (Live's convention);
  **Shift-drag** for fine value adjustment.
- **Marquee-select** multiple dots and drag/delete them together.
- **Draw mode** (Live's `B`): pencil freehand/steps following the grid.
- Grid snapping follows the existing timeline grid settings.
- Exact keybindings ride MANIFOLD's shortcut system; where a Live default
  conflicts with an existing MANIFOLD binding, keep MANIFOLD's and note the
  remap — the *gestures* are the contract, the letters are configurable.

**State affordances:**

- Overridden lane = **grayed line** (Live's exact affordance); per-lane
  re-enable click on the lane header.
- Global **Back to Arrangement** button in the transport bar, lit red when
  any latch is set; **Automation Arm** toggle next to it.
- Param cards show a small red "automated" indicator on params with an
  enabled lane (Live's red dot); the indicator grays when overridden.

Headless-PNG self-verification for the visual pass, per the standing UI
workflow.

**Addendum 2026-07-07 (Peter, discussion) — the exposure half, settled:**

- **Strips-under, not overlay-on-track.** Live draws the selected envelope
  over the track's clips; MANIFOLD keeps the shipped strips-below-the-layer
  model (Peter: "strips under is better for Manifold"). The chooser + "+"
  spec above is unchanged; don't build the overlay.
- **`A` binds to the automation-mode toggle** (same as the transport LANES
  button). Plain `a` is currently unbound (`input_handler.rs`); Cmd+A
  stays select-all. `B` draw-mode already ships.
- **First-draw path (no arm, no playback):** a param chosen in the chooser
  with no lane yet renders as a flat line at its current base value —
  Live's "every param has an implicit envelope" feel. The first click
  births the real lane via `AddAutomationPointCommand`'s existing
  `created_lane` semantics. Recording stops being the only birth path.
- **Chooser home:** the expanded layer tier per this section — which means
  the two-height header contract reconciliation
  (TIMELINE_UX_AUDIT_2026-07-07 item #2) rides along with this work.

## 8. Interactions & edge cases

- **Ableton macro on an automated param:** macro move = touch = override (or
  record if armed). This is the correct Live-side behavior too — an external
  controller fighting arrangement automation should override, not average.
- **Enum/int-backed params:** author with `Hold` segments; sampler clamps and
  the existing param write path handles rounding exactly as slider writes do.
- **Tempo map changes:** lanes are beat-indexed, so they stretch with tempo
  automatically — correct by construction, matches Live.
- **Clip-relative envelopes** (lanes that move with a clip): explicitly out of
  scope — that's Ableton's *other* automation system (clip envelopes); the
  additive `ParamEnvelope` decay family already covers the per-clip use case.
  Revisit only if a real show need appears.
- **Reset/seek/loop:** nothing to do — sampling is a pure function of beat;
  no state to invalidate on seek (unlike the ML workers' generation counter).
- **Hot path:** lanes walk only instances where `automation_lanes` is
  `Some` and non-empty; binary search per lane; zero allocations post-warmup.
  Typical scale (53 layers / 128 effects) → tens of lanes, negligible.

## 9. Testing

- `manifold-core`: curve eval unit tests — segment shapes, before-first /
  after-last, sorted-invariant, clamping.
- `manifold-playback` (`--lib`): sampling writes base before modulation reset;
  latch on touch (each hand's funnel); evaluator self-write does NOT latch;
  Back to Arrangement resumes; armed recording produces the punch-in/out
  boundary points; gesture commits one undo entry with correct reverse.
- Serde: skip-when-empty roundtrip — project without lanes is byte-identical
  (the binding-unification proof pattern).
- Scope per the testing discipline: per-crate `--lib` runs; this touches
  `manifold-core` effects types, so the finishing commit runs the full
  workspace sweep.

## 10. Phasing (Sonnet-executable)

- **P1 — model + runtime:** `AutomationLane`/`AutomationPoint`/`SegmentShape`
  in core; serde; pruning-row integration; curve eval; `automation.rs`
  sampling pass wired into the content tick before `evaluate_modulation`;
  `touched` flag + latch map + Back to Arrangement `ContentCommand`. Full
  workspace sweep (core types touched).
- **P2 — editing:** the §6 command set + state_sync exposure (lane data +
  latch/arm state to UI snapshots).
- **P3 — recording:** arm toggle, gesture capture, punch boundaries,
  single-undo commit.
- **P4 — timeline UI:** automation mode, lane strips, breakpoint editing,
  override graying, transport-bar buttons.

P1 ships value on its own only via P2/P4 editing — but P1+P2 land as one
reviewable arc; P3/P4 independent after.

- **P5 — exposure (added 2026-07-07; = TIMELINE_UX_AUDIT item #1):** the §7
  addendum. `A` keybinding; param chooser + "+" on the expanded layer;
  touch-to-select; flat-line render + first-click lane birth. P1–P4 SHIPPED
  2026-07-04; P5 status below (partial ship, 2026-07-07).

### P5 status (2026-07-07, Sonnet, `lane/automation-exposure`)

**Shipped and headless-PNG-verified** — Peter's actual complaint (ARM
recording was the ONLY way to birth a lane) is fixed:

- `A` keybinding, real unit test (`input_handler.rs`'s
  `bare_a_toggles_automation_mode_visible_regardless_of_current_state`) —
  toggles from either state, doesn't shadow Cmd+A select-all.
- Touch-to-select: any param drag (`PanelAction::ParamSnapshot`'s handler,
  `ui_bridge/inspector.rs`) records the layer's active chosen param
  (`UIState::chosen_automation_params`, layer-scoped, one entry per layer).
- First-draw path: a chosen param with no backing `AutomationLane` renders
  as a flat line at its current base value, no dot
  (`ui_translate::push_chosen_placeholder_lane`, `UiAutomationLane::placeholder`,
  `viewport.rs` skips dot emission for placeholders). The first click on
  that line creates the REAL lane via the pre-existing
  `AddAutomationPointCommand`/`add_automation_point` path — unmodified, it
  already creates a lane on demand. Proven end-to-end through the real
  hit-test + dispatch path (not a mock): `scripts/ui-flows/
  automation-placeholder-first-click.json` against the new
  `automationplaceholder` ui-snap scene — strip exists with 0 points before,
  a synthesized click, 1 point after; PNGs show the dot appearing where none
  existed. The two pre-existing automation scripts (`toggle-lanes.json`,
  `drag-automation-point.json`) still pass unchanged — no regression on the
  shared lane pipeline.

**Descoped, not silently dropped** — flagged back rather than faked:

- **The "+" button / full param-picker popover** (device dropdown + param
  dropdown search list) is NOT built. Building a real one (vs. another
  "read-only stand-in," the exact anti-pattern that caused the original dead-
  LANES report) means a new popover consumer of `PickerCore`
  (`panels/picker_core.rs` — the existing reusable pick-from-a-list model
  the preset browser already uses; it does the filter/keyboard-nav, drawing
  stays per-surface) with its own render + dismiss-on-outside-click wiring —
  a scoped follow-up, not a two-line addition. Touch-to-select already covers
  the zero-friction path the design doc calls out ("wiggle the knob, then
  draw") without it.
- **The two-tier header height reconciliation** (TIMELINE_UX_AUDIT item #2,
  the unused `TrackHeight::Tall` stop) turned out NOT to be a hard
  dependency for the chooser's home, on inspection: the chooser/placeholder
  strip stacks below the layer using the SAME additive-height pattern real
  lane strips already use (`layer.automation_lane_count` →
  `CoordinateMapper::layer_height`), which lives entirely within the
  existing non-collapsed ("expanded" in §7's original 2026-07-02 language)
  tier — no new tier needed. Item #2's actual complaint (the routing form
  showing unconditionally whenever a layer isn't collapsed, wasting vertical
  space) is a separate, real UX question — whether "expanded" should become
  a deliberate third state distinct from "just not collapsed" — that changes
  existing on-stage layer-header behavior and deserves its own sign-off, not
  a silent redefinition as a side effect of this build. Still open;
  unblocked from P5 either way.
- **Live-drag E2E proof of touch-to-select itself** (a real slider drag
  through the ui-snap harness) is unverified — the harness has no `HitTargets`
  surface for param sliders (only automation lanes, clips, graph canvas), so
  proving it needs a `Query`-selector approach to an uncertain node-type tag,
  not the `Surface` selector the automation-lane assertions above use. The
  wiring itself (`ui_bridge/inspector.rs`'s `ParamSnapshot` arm) is verified
  by code review, not an end-to-end drag render — flagged, not claimed.

## 11. Decided (don't reopen)

1. Lanes are **tier-1 base writers**; modulation pipeline untouched.
2. **Override latch** on live touch; Back to Arrangement (global +
   per-lane); latches runtime-only, survive play/stop, never serialized.
3. **Layer-scoped arrangement lanes** on `PresetInstance` keyed by
   `param_id`; clip-relative envelopes out of scope.
4. **Drivers/audio-mods stay exclusive** (M4L-LFO semantics). No
   base-relative driver mode in v1.
5. Lanes store param-range values; beat-indexed; `Linear | Hold | Curved`.
6. Recording = Automation Arm, gesture punch-over, one undo entry per
   gesture, records the smoothed/applied value.
7. Per-frame sampling bypasses undo and never bumps `DataVersion`.
8. UI = Ableton's model: lanes live in the expanded layer's advanced
   controls, one lane per automated param + "+" to add, touch-to-select in
   the chooser, Live's gesture vocabulary (click-to-dot, modifier-drag
   curves, draw mode, grid snap w/ Cmd bypass).

## 12. Deferred / rejected

- **Deferred:** automating driver/envelope/audio-mod fields themselves
  (rate, trim, depth — "automate the LFO's rate knob"); clip-relative
  envelopes; automation shapes beyond curvature (S-curves, steps-with-slew);
  lane consolidation/simplify (point-thinning on record exists implicitly via
  gesture overwrite — a Douglas-Peucker pass is polish).
- **Rejected:** base-relative (bipolar-depth) driver mode as part of this
  work — it changes existing project behavior and duplicates what automating
  driver fields will do better; a fifth modulation phase in
  `evaluate_modulation` — automation is a hand, not a modulator.
