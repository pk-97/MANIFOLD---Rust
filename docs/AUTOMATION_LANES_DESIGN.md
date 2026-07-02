# Automation Lanes — Design

**Status: APPROVED (Peter, 2026-07-02). Not implemented.** Sonnet-executable; phases in §10.

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
