# Automation Lanes — Design

**Status: SHIPPED — P1–P4 on main 2026-07-04; P5 (section 7 (UI/UX) addendum) partially shipped and merged 2026-07-07 — the P5 status block after section 10 (Phasing) is exact on landed vs. remaining. Owed: VD-001 (LANES live confirm) — Peter confirms LANES lights live and ARM-records a first lane.**
**Prerequisites: none. SESSION_MODE_DESIGN section 2 (Hard dependency edges) reserves a serde-optional field slot on `ClipSequence` for this feature — fill that slot, don't invent a second home.**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` section 5 (Phase briefs)–section 6 (Seam briefs) and section 8 (Execution protocol) before any phase; audit claims are a 2026-07-02 snapshot — run the section 8.3 (pre-flight) check first.**

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
deferred, section 11). No base-relative driver mode in v1.

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
    CurvedRange { bend, start, end }, // exact clipped power-curve subrange
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
uses the standard power-curve bend. `CurvedRange` preserves clipped and nested
power-curve subranges. Deterministic, allocation-free.

## 3. Runtime sampling

A new function `evaluate_all_automation(project, current_beat, latches) -> bool`
in `manifold-playback` (own module, `automation.rs`), called in the content
tick **before `evaluate_modulation`** — it is a hand, not a modulator, so it
must land before the base→value reset. Walk shape is a copy of
`evaluate_all_audio_mods`: master effects + layer effects + gen params, skip
disabled instances, resolve via `resolve_param_in`, write via
`set_base_param`. No per-frame allocations: reuse the two-pass
resolve-then-write pattern with a scratch `Vec`.

Sampling runs whenever the transport is playing and during offline export at
exported-frame beats. Export uses arrangement sampling without consuming live
touches, latches, or recording gestures. When stopped, lanes sample for
inspection while manual overrides remain latched.

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
`touched` since last frame → latch (or record, section 5), clear the flag, skip the
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
  Exact representable-beat guards preserve dense samples outside the take;
  curved boundaries use `CurvedRange`.
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
- `CommitRecordedGestureCommand` — the section 5 single-entry commit.
- `BackToArrangement` is NOT a command (it mutates runtime latch state, not
  the project) — it's a `ContentCommand` variant handled on the content
  thread, no undo entry.

Addressing follows the unified card-target shape (Effect | Generator | Master
target + `param_id`), not indices — same discipline as
`EditUserParamBindingCommand`.

## 7. UI / UX (decided: copy Ableton's model — Peter, 2026-07-02)

Interaction feedback contract (2026-09-14): lane chrome identifies layer-owned
arrangement automation. Default lanes are 160 px; resizing retains a 64 px
minimum. Header, plot, and footer geometry is shared by rendering and input;
chrome consumes clicks without creating points. Automation uses a cyan accent,
with selected/hovered points and segments highlighted and overridden lanes dimmed.
Hover feedback identifies the operation before pressing; active feedback remains
bound to the captured gesture until release or cancellation. Point values and
insertion previews use the same beat snap, parameter range, and integer rounding
as editing. One click in empty plot space or on a segment inserts a point;
Shift-click deselects and a bare drag selects a region. Segment clicks insert,
segment drags move vertically, and Alt-drag bends continuous parameters. Integer
segments remain stepped. Draw mode takes precedence over point/segment drags.
The corresponding inspector parameter receives a selection outline. Modifier
changes and lane geometry changes refresh feedback without requiring mouse motion.

Gesture continuity contract (2026-09-26): pressing an empty plot or segment shows
the prospective point in that input frame; releasing commits one undoable edit.
Starting a drag restores the pre-press envelope before marquee/segment routing,
and Escape or an abandoned press restores it without a history entry. Content
snapshots cannot replace that provisional envelope. Adding a handle on a curve
samples the snapped beat and splits the original shape, preserving both sides.
Bending starts from the captured shape and follows vertical pointer movement
for both rising and falling ramps. Fine-adjustment changes remain continuous.
Segment movement uses a shared boundary clamp, preserving its slope. Pencil
strokes replace the swept interval, including skipped pointer samples, retain
outside automation, and honor Cmd-unsnap. The first and final automation drag
positions are applied before committing. Alt-click straightens a curved segment;
the lane menu also offers Straighten, Hold, Ease In, and Ease Out.

Phrase selection contract (2026-09-26): dragging empty plot space selects a beat
interval across the intersected lanes, independent of the points' vertical values.
The selection stays visible after release and can contain no interior points.
Copy includes sampled boundary values and clipped segment shapes. Paste replaces
the destination interval, retaining the exact curve outside it; a single-lane
phrase can map into another parameter's range. Cut and Delete hold the entry value
through the selected interval. Duplicate places the phrase at the selection's end
(or one grid step later for a single point). Each edit is one undo step. Keyboard
paste uses the automation selection start or the most recent lane insertion beat;
Paste Here uses the menu's clicked beat. A context menu in a selected lane retains
a multi-lane selection.

The AUTOMATION and DRAW buttons expose mode state. Beat/bar/subdivision grid lines
share one grid policy with the layer and ruler, including subdivision visibility
and physical pixel widths. Curves include every breakpoint and draw Hold and
same-time transitions vertically; curved segments use adaptive screen-space
sampling and antialiased strokes. Drag readouts follow the point in an
edge-clamped tooltip, and numeric editors open beside the addressed point.
Readouts and exact time entry use one-based
`bar.beat.fraction`, with a three-digit fraction in thousandths of a beat.
The lane menu exposes Cut, Copy, Paste Here, Duplicate, Select All, Delete,
exact point value/time entry, and Insert Shape. Shapes replace the selected
point span, or one bar at the snapped click when no span is selected: ramp up,
ramp down, triangle, sine, square, hold low, and hold high. Edits retain the
existing command/undo and clipboard range-conversion paths. Per-lane Restore
Automation clears only that parameter's override; RESTORE ALL clears all latches.
Authoring the first curve clears an earlier slider touch so it begins active.

**Placement — automation lives on the layer.** Expanding a layer (the
existing layer-expand affordance) reveals the advanced layer controls,
including automation. **Choose parameter…** in the layer or lane menu opens a
searchable picker grouped by effect/generator instance. Choosing a parameter
reveals its existing lane or a flat placeholder without changing its value.
Each automated parameter gets a separate strip. Lane menus hide, pin, and reorder
strips independently of their envelopes; Show All restores hidden strips. These
view choices last for the session. A placeholder retains its position when its
first point creates a real envelope.

**Touch-to-select.** Touching any param on the layer (card slider, inspector
knob) auto-selects that param in the lane's chooser — Live's behavior; it
makes "wiggle the knob, then draw" the zero-friction path to a new lane.

**Interaction vocabulary — same shortcuts and controls as Live:**

- **Automation mode toggle** (Live's `A`): show/hide automation across the
  timeline; lanes draw as a cyan breakpoint line beneath the layer.
- **Click on the line** adds a breakpoint (dot); **drag** moves it (snapped
  to the timeline grid); **Delete** removes the selection. Repeated clicks
  select the dot without deleting it; Shift-click toggles its selection.
- Every lane uses single-click insertion, including empty placeholders.
  Shift-click away from the line deselects without changing the curve.
  Selected dots draw larger and white. A moved point remains selected at its new
  beat and value; hiding automation or undo/redo clears point selections.
  Point dragging preserves the grab offset and Shift scales value movement to
  one quarter. Different values may share one beat to create an instant step.
  Equal-beat order is preserved; playback takes the last point at the exact beat.
  Only an identical beat/value position replaces a breakpoint, with exact lane
  restoration on undo. Moving past another point during preview restores it.
  Segments remain editable when either endpoint is outside the viewport.
- **Show Automation** in an effect/generator parameter's context menu reveals
  its lane without touching the parameter, arming recording, or creating points.
  It expands the owning track and folded parents through the content command
  path. Layer-owned parameter rows also expose an AUTO button. Revealing a lane
  scrolls it into view and opens enough timeline space to edit; an existing
  session lane height is kept.
  Right-clicking AUTO opens the same parameter menu as its label. **Clear
  Automation** removes the parameter's complete lane in one undoable command,
  clears stale selection/reveal state, and closes its placeholder. The lane
  menu also offers **Clear points** to keep an empty lane open.
  Master and group automation editors remain deferred.
- **Drag the grip at the bottom-left of a lane** to resize it from 64–240px.
  Heights are session-only UI state keyed by the existing target/parameter
  address. Mapper row totals and visible strips use the same resolved heights;
  folding a track preserves its size without reserving hidden space.
  Lane labels use manifest parameter names.
- **Stopped and paused inspection** samples automation at the current playhead
  before modulation, using the same curve sampler as playback. Manual overrides
  remain latched until Back to Arrangement. Stopped seeks neither record nor
  finalize a pending recording gesture during inspection. Lifecycle owners flush
  recording on stop, pause, disarm, seek, save, and export.
- **Live envelope preview** sends point, segment, bend, group and pencil edits to
  the content thread while dragging. Runtime envelopes sample at the playhead
  before modulation in playing, paused and stopped states; authoritative lane
  points stay untouched until the undoable release command. Escape restores the
  original envelope without adding an undo step. Preview completion restores the
  previous parameter base before normal sampling resumes; manual override latches
  and recording gestures retain their existing ownership.
- **Drag a segment** vertically to move it; **modifier-drag a segment**
  (Alt/Option, Live 11 style) bends it into a curve — this is the
  `Curved(f32)` shape in section 2.
- **Cmd-drag** bypasses grid snap for fine placement (Live's convention);
  **Shift-drag** for fine value adjustment.
- **Drag-select time** across one or more lanes; all points in the interval are
  selected regardless of vertical position. Selected points can be moved together.
  Group drags move points in time and value, using the grabbed point as the
  snap anchor and preserving beat spacing. Cmd bypasses snap; Shift scales
  value movement. A shared boundary clamp keeps the group at or after beat
  zero and preserves its normalized value shape within parameter ranges.
  Selected points outside the visible beat range in shown lanes remain part
  of the move. Each preview
  rebuilds from the complete original lanes, so crossing another point and
  moving past it restores that point. Release replaces exact destination
  collisions atomically per lane, with one undo step for the whole group;
  Escape restores the original lanes. Clipboard cut/copy/paste and duplication
  are undoable; time-stretch and
  simplification remain unfinished.
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
- Param cards show a small cyan "automated" indicator on params with an
  enabled lane; the indicator grays when overridden.

Headless-PNG self-verification for the visual pass, per the standing UI
workflow.

**Addendum 2026-07-07 (Peter, discussion) — the exposure half, settled:**

- **Strips-under, not overlay-on-track.** Live draws the selected envelope
  over the track's clips; MANIFOLD keeps the shipped strips-below-the-layer
  model (Peter: "strips under is better for Manifold"). The September 2026
  searchable chooser uses the layer/lane menus; automation stays in strips.
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

## 10. Phasing

- **P1 — model + runtime:** `AutomationLane`/`AutomationPoint`/`SegmentShape`
  in core; serde; pruning-row integration; curve eval; `automation.rs`
  sampling pass wired into the content tick before `evaluate_modulation`;
  `touched` flag + latch map + Back to Arrangement `ContentCommand`. Full
  workspace sweep (core types touched).
- **P2 — editing:** the section 6 command set + state_sync exposure (lane data +
  latch/arm state to UI snapshots).
- **P3 — recording:** arm toggle, gesture capture, punch boundaries,
  single-undo commit.
- **P4 — timeline UI:** automation mode, lane strips, breakpoint editing,
  override graying, transport-bar buttons.

P1 ships value on its own only via P2/P4 editing — but P1+P2 land as one
reviewable arc; P3/P4 independent after.

- **P5 — exposure (added 2026-07-07; = TIMELINE_UX_AUDIT item #1):** the section 7
  addendum. `A` keybinding; param chooser + "+" on the expanded layer;
  touch-to-select; flat-line render + first-click lane birth. P1–P4 SHIPPED
  2026-07-04; P5 status below (partial ship, 2026-07-07).

### P5 status

**Shipped and headless-PNG-verified:**

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
  existed. The September 2026 first-click flow also observes the held press,
  undo, and redo; the point-drag flow asserts the exact release position.

- **Chooser follow-up (2026-09-26):** the shared `PickerCore` now backs
  **Choose parameter…** in the layer and lane menus. This supersedes the
  originally proposed pair of dropdowns and "+" button. Search and keyboard
  navigation resolve a captured target/parameter address. Independent hide,
  pin, and ordering are session view state; they do not remove automation.

**Descoped, not silently dropped:**

- **The two-tier header height reconciliation** (TIMELINE_UX_AUDIT item #2,
  the unused `TrackHeight::Tall` stop) turned out NOT to be a hard
  dependency for the chooser's home, on inspection: the chooser/placeholder
  strip stacks below the layer using the SAME additive-height pattern real
  lane strips already use (`layer.automation_lane_count` →
  `CoordinateMapper::layer_height`), which lives entirely within the
  existing non-collapsed ("expanded" in section 7's original 2026-07-02 language)
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
5. Lanes store param-range values; beat-indexed; `Linear | Hold | Curved | CurvedRange`.
6. Recording = Automation Arm, gesture punch-over, one undo entry per
   gesture, records the smoothed/applied value.
7. Per-frame sampling bypasses undo and never bumps `DataVersion`.
8. UI = Ableton's model: lanes live in the expanded layer's advanced
   controls, one lane per automated parameter, searchable Choose parameter,
   touch-to-select, Live's gesture vocabulary (click-to-dot, modifier-drag
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
