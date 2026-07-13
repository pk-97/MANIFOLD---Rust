# UI Widget Unification — one widget vocabulary, two hosts

**Status: IN PROGRESS · 2026-07-10 · Fable · AMENDED 2026-07-13 (Peter): opportunistic conversion replaced by scheduled sweep — P4–P7 added, D6 superseded · AMENDED 2026-07-13 (Fable, P7 design pass): P7 expanded to P7.1–P7.6 with D8–D12, per Peter's mandate to "unify all of the graph and timeline widgets, UI, and interaction surfaces even if they have not had any bugs raised previously". P7.0 (AudioTriggerSection) LANDED on main `6917c0ea`; P1–P6 execution in flight on `feat/widget-unification`.**
**Prerequisites:** none
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any phase.

Peter, 2026-07-10: *"The 2 different UI architectures we use between the main window and
graph editor concerns me honestly"* → after the assessment: *"it's no longer theoretical.
I think this is a valuable upgrade we should make to Manifold."*

The governing insight: **the two-surface split is correct and stays; the defect is one
layer down.** The retained chrome (UITree + intent registry) and the immediate-mode graph
canvas (camera + `Painter`) exist for real reasons — but widget *look* is already shared
across them (`BitmapSlider` has both a tree builder and a painter twin) while widget
*behavior* (what a gesture on a zone means) is hand-implemented per host. Every gesture
contract is therefore maintained by discipline, and the discipline leaks: BUG-105 (node
sliders have no right-click reset), BUG-102 (canvas has no text entry), BUG-070 (steppers
and the send-fader missed intrinsic reset). This design moves gesture semantics INTO the
widget — one surface-agnostic contract per widget, translated by each host into its own
action type — so a gesture added once exists everywhere **by construction**.

Binding constraints (per DESIGN_AUTHORING §1): hot path — the canvas calls widget
geometry per frame per visible row, so the contract functions are pure and allocation-free;
thread — UI thread only, no new state crosses the content boundary; persistence — none;
performance surface — the graph editor is authoring-not-perform, but gesture muscle
memory is a live-workflow concern: a performer mid-set-build right-clicks a node slider
expecting the reset every card slider taught them.

Companions: `docs/NODE_INTENT_DISPATCH.md` (the retained-side registry this derives
into), `GRAPH_EDITOR_REDESIGN.md` (owns canvas visuals; untouched here). Sibling
unifications surfaced in the same discussion (graph-snapshot twin resolvers,
param-surface dual source of truth) are **out of scope** — different subsystems, tracked
in the `ui-widget-unification` memory.

---

## 1. Audit — what exists (verified 2026-07-10)

Extend, don't redesign. Every piece below is load-bearing.

| Piece | Where | State |
|---|---|---|
| `BitmapSlider::build` — retained builder, 5 UITree nodes | `manifold-ui/src/slider.rs:113` | Returns `Slider { ids: SliderNodeIds, reset: PanelAction }` — "you cannot build a slider without stating its reset" (slider.rs:52–54) |
| `BitmapSlider::draw` — the immediate-mode twin | `slider.rs:300` | Painter-based; the canvas node faces call it (`graph_canvas/render.rs:879`) — **look is already unified** |
| `SliderNodeIds.default_normalized` | `slider.rs:48` | The widget's own visual default, for reset snap |
| `PanelAction::SliderReset { snapshot, changed, commit }` | `panels/mod.rs:150` | Reset expressed as the drag trio → undo == a drag to default. Replaced per-panel `*RightClick` actions (BUG-061) |
| `IntentRegistry<A>` + `Gesture { Click, DoubleClick, RightClick }` | `intent.rs:84`, `:26` | Node-id-keyed, fold-up dispatch, **already generic over action type** (graph-editor sidebar uses `IntentRegistry<GraphEditCommand>`). Drags stay in the stateful path by design (intent.rs:23–24) |
| Hand registration of track-RightClick → reset | `chrome/diff.rs:294` (`register_slider_resets`, `:292`) · `panels/layer_header.rs:2152` · `panels/audio_trigger_section.rs:609` | **Three hand-maintained sites** — the per-host duplication this design deletes |
| Canvas param-row input | `graph_canvas/interaction.rs` | Right-click → mapping popover or nothing (`:307`); left-press → `DragMode::ParamScrub` (`:766`); wire-driven rows read-only (`:762`); its own parity rule: "Every branch emits the same command the sidebar did (parity); only where you click moves" (`:744–748`) |
| Canvas command emission | `graph_canvas/mod.rs:444` (`pending_actions: Vec<GraphEditCommand>`) · `graph_edit.rs:98` (`SetGraphNodeParam`), `:116` (`SetOuterParam`) | Group-face mirror rows emit `SetOuterParam` instead (interaction.rs:801–804) |
| `ParamSnapshot.default_value` | `graph_view.rs:124` | The default is already in the node snapshot — reset needs no new plumbing |
| Node-face metrics diverge from card metrics **by design** | `graph_canvas/mod.rs:157–179` | Value box 72 vs card 56, label 84 vs 60 — zone geometry must be parameterized, not constant-shared |
| `MappingPopover` — surface-agnostic precedent | `graph_canvas/mapping_popover.rs:1–29` | Painter-drawn, host-agnostic, emits the same `PanelAction` triads. The pattern this design generalizes |
| UITree invariants | `tree.rs:40–43` | Structural ops build-phase only; interaction frames are `set_*`-only — **why the canvas can never sit on the UITree** |

Classification: the contract layer is the only *genuinely new* piece — two small enums
and two pure functions. Everything else is rewiring what exists. The design shrank in the
audit, as it should.

## 2. Decisions

**D1 — The two-surface split stays.** Retained chrome + immediate canvas, exactly as
today.
*Rejected: canvas-on-UITree* — a camera transform makes every rect a per-frame function;
the tree's own invariant (structural ops build-phase-only, tree.rs:40–43) forbids it.
*Rejected: full-immediate app (egui-style)* — the UI shares the machine with the show
renderer; re-solving a 53-layer / 2928-clip timeline per frame spends the GPU headroom
the show needs (`ui-present-content-gpu-contention`). The cached chrome is load-bearing.

**D2 — Gesture semantics move into the widget.** Each widget owns, alongside its
existing build/draw: **zone geometry** (pure, from a rect + metrics) and a **gesture
contract** (pure function `(zone, gesture) → Option<WidgetIntent>`). The contract speaks
widget language ("reset to default"), never host language (`PanelAction`,
`GraphEditCommand`).
*Rejected: a widget trait / framework* — three concrete widgets don't earn an
abstraction; per-widget inherent fns, same as `BitmapSlider` today.
*Rejected: a shared gesture registry object* — the contract is data + pure functions;
no state, no `Arc<Mutex>`, nothing to synchronize.

**D3 — Hosts translate intents into their own action types.** Chrome resolves an intent
to a `PanelAction` and registers it in the `IntentRegistry` at build time (derivation
replaces hand registration). The canvas resolves the same intent at input time and pushes
a `GraphEditCommand`. A host MAY translate an intent to nothing when its surface lacks
the target (explicit dead stop, same semantics as `claims_area` with no gesture,
intent.rs:41–48) — that translation is written in the host, visible, greppable; never a
silent skip inside the widget.

**D4 — Reset emission parity is the command shape, not a new command.** On the canvas,
`ResetToDefault` emits exactly what the scrub-commit path emits — an absolute
`SetGraphNodeParam` carrying `ParamSnapshot.default_value` (or `SetOuterParam` for a
group-face mirror row, per interaction.rs:801–804) — so undo equals a drag to default,
mirroring `PanelAction::SliderReset`'s trio semantics on the chrome side. Wire-driven
rows suppress all intents (same guard as the scrub, interaction.rs:762).

**D5 — Discrete gestures only.** The contract covers `Gesture`'s existing vocabulary
(Click / DoubleClick / RightClick). Drags/scrubs stay host-stateful — the retained side
has its drag machinery, the canvas has `DragMode`, and both already emit the same
commands. *Rejected: widening `Gesture` with drag variants to force scrub into the
contract* — intent dispatch is for node→action gestures by design (intent.rs:23–24);
drag state machines are genuinely host-specific and their command emission is already
parity-true.

**D6 — Conversion order: slider first, then scheduled sweep.** (Amended 2026-07-13 —
Peter rejected the original "convert when a parity bug touches them" sequencing:
*"remove the potential for future bugs... not wait until it breaks."* The opportunistic
model was this design's call, not his.) P1 converts the slider and closes BUG-105;
P2 the steppers/send-fader. The remaining widgets convert in scheduled phases P4–P6 —
dual-surface widgets first (P4, where the divergence disease already lives), then the
canvas text-input primitive (P5, unblocks BUG-102), then contract-derivation for
chrome-only widgets (P6, future-proofing). No widget waits for its first bug.

**D7 — Zone geometry is parameterized by metrics, not shared constants.** The node face
legitimately uses different label/value widths than cards (graph_canvas/mod.rs:157–179).
`zones()` takes the metrics; each host passes its own. What unifies is the *shape logic*
(which zone is where, given widths), not the numbers.

**D8 — One drag-lifecycle owner; per-gesture state folds INTO the payload variant.**
(Added 2026-07-13, P7 design pass — verified against code that day.) Every remaining
ad-hoc drag machine migrates onto `DragController<T>`, and the state each gesture carries
moves into that gesture's variant of the payload enum — never into parallel fields beside
the controller. The disease shared by every remaining machine is a discriminant plus
parallel per-gesture state that must agree by discipline: `InteractionOverlay`'s
`drag_mode` + six `Option<…State>` fields (interaction_overlay.rs:345, :381–390),
`ParamDragState`'s six slots (param_slider_shared.rs:665–706), the viewport's
`ViewportDragMode` + `marker_drag_id`/`marker_drag_start_beat` (viewport.rs:253,
viewport/interaction.rs:125–133), the canvas's `DragMode` + `drag_anchor`/`drag_pan_start`
(graph_canvas/mod.rs:435–436). Folding makes the desync unrepresentable.
*Accepted behavior delta, stated honestly:* today, arming gesture B while gesture A's slot
is still `Some` leaves both armed — a latent bug (one pointer, one gesture), never a
feature. The controller's documented "a fresh grab always wins" (drag.rs:99) replaces
that. Any call site that turns out to RELY on two simultaneously-armed slots is an
escalation, never an adaptation; the 2026-07-13 audit found none.
*Rejected: one `DragController` per slot/field* — preserves the multi-armed bug class and
the parallel-state disease P7 exists to kill; it is the migration-shaped shortcut, not the
migration.

**D9 — Canvas `DragMode`: all six variants migrate; session geometry replaces the
per-variant position fields.** (Answers open question 1 from the P7.0 handoff.) The
apparent misfit dissolves on inspection: `ParamScrub`/`VecScrub`'s `press_origin_x` IS
`session.start.x` (the scrub math at interaction.rs:506/:545 is `sx - press_origin_x`,
i.e. `current.x - start.x`); `Marquee`'s `origin_screen` IS `session.start`; `Pan`'s
`drag_anchor` IS `session.start` with `drag_pan_start` as grab-time payload. Migrating
*deletes* three position fields rather than adding an unused one. The scrubs reading only
the x component of `current` is not dead weight — the pointer genuinely has a y; the
variant just doesn't consume it, and nothing is stored that wasn't already.
*Rejected: scrub position kept outside the controller* — that keeps a second
position-tracking source of truth beside the lifecycle owner, which is exactly the
duplication this phase deletes.

**D10 — `ParamDragState`: single-active is enforced at type level, in this migration.**
(Answers open question 2; engineering call made at design time, not Peter's product
surface — reversible if he objects, but the reasoning: ) (a) `DragController<enum>`
cannot represent two active slots — building a shape that still could (six controllers)
would be deliberate extra work to preserve a bug class; (b) only-one-active is already
the informal contract, so no observed behavior changes — the invariant only forbids
states that are bugs today; (c) Peter's mandate is explicitly proactive
("even if they have not had any bugs raised"). The type keeps its name and grows
per-category accessors so the ~49 call sites convert one-for-one (P7.1 seam brief).

**D11 — `InteractionOverlay`: internal truth becomes `DragController<TimelineDrag>`;
the public `DragMode` enum survives as a derived discriminant.** External consumers
(auto-scroll polling, drag readout, snap, input routing — ui_state.rs, viewport,
input.rs, dock.rs et al. read `drag_mode()`/`is_dragging()`) keep their exact API:
`drag_mode()` returns the same Copy enum, now computed via `TimelineDrag::kind()`. The
three `AnimF32` fields (`lift_anim`, `ghost_alpha`, `settle_dx`,
interaction_overlay.rs:405–415) stay OUTSIDE the controller — they deliberately keep
easing after release against `drag_visual_clip_ids` (:416–424), so their lifetime is
longer than the drag's; their per-frame targets re-read their predicates through the
controller instead of the old field. The fold is staged over three landings
(automation → trim/region → move) with exactly ONE lifecycle owner at every commit —
a parallel old machine kept alive beside the controller is the forbidden move.
*Rejected: folding everything in one landing* — the Move fold alone deletes 11 loose
fields on the live timeline's show-critical gesture; risk isolation is worth three
sessions. *Rejected: migrating only the automation variants and leaving Move/Trim loose
fields forever* — that's the half-done state D8 forbids; the staging is a schedule, not
a scope.

**D12 — Sweep scope: the long tail is in, the input recognizer is out.** Peter,
2026-07-13: *"unify all of the graph and timeline widgets, UI, and interaction surfaces
even if they have not had any bugs raised previously — this is critical infra work."*
Accordingly P7 also covers the three further machines the design-pass audit found beyond
the P7.0 handoff's list: `TimelineViewportPanel`'s `ViewportDragMode` (+ its parallel
`marker_drag_id`/`marker_drag_start_beat` fields), `AudioSetupPanel`'s
`dragging_band`/`calibration_drag` pair (audio_setup_panel.rs:375, :383), and `dock.rs`'s
divider-edge drag (its hit→begin→drag→end triad at dock.rs:162–194 is a hand-rolled
`DragController<Edge>`). Named OUT, with reasons: **input.rs's drag recognizer**
(input.rs:387–419 — the platform layer that decides a drag EXISTS at all and feeds every
machine; it is upstream of `DragController`, not parallel to it);
**`scroll_container::drag_to_scroll`** (stateless position→fraction mapping, no
lifecycle); **every `SliderDragState`-backed panel** (clip_chrome, macros_panel,
layer_chrome, layer_header, master_chrome — already `DragController`-backed via
`SliderDragState`; don't touch).

## 3. The contract (committed shapes)

All in `manifold-ui/src/slider.rs` — the widget's one home, next to `build` and `draw`.

```rust
/// A slider's interactive zones, host-agnostic (no NodeIds).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SliderZone { Label, Track, ValueCell }

/// What a gesture on a zone MEANS, in widget terms. Hosts translate
/// (D3): chrome → PanelAction, canvas → GraphEditCommand.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SliderIntent {
    /// Write the widget's default back through the value path,
    /// undoable as one drag (D4).
    ResetToDefault,
    /// Open the mapping/binding surface for this param.
    OpenMapping,
    /// Begin text entry on the value.
    EditValue,
}

/// Zone rects for a slider occupying `rect` — THE one geometry source;
/// `build` and `draw` both delegate to it (P1 deletes their private copies).
/// `metrics` carries per-host label/value widths (D7).
pub struct SliderZones { pub label: Option<Rect>, pub track: Rect, pub value_cell: Rect }

impl BitmapSlider {
    pub fn zones(rect: Rect, metrics: &SliderMetrics) -> SliderZones { /* pure */ }

    /// The gesture contract. Pure, total, allocation-free.
    pub fn intent_for(zone: SliderZone, g: Gesture) -> Option<SliderIntent> {
        use Gesture::*;
        match (zone, g) {
            (SliderZone::Track, RightClick) => Some(SliderIntent::ResetToDefault),
            (SliderZone::Label, RightClick) => Some(SliderIntent::OpenMapping),
            (SliderZone::ValueCell, Click)  => Some(SliderIntent::EditValue),
            _ => None,
        }
    }
}
```

`SliderMetrics` is the small struct of widths the two hosts already hold as constants
(`slider::VALUE_BOX_W` / `DEFAULT_LABEL_WIDTH` vs `graph_canvas`'s
`PARAM_SLIDER_VALUE_BOX_W` / `PARAM_SLIDER_LABEL_W`).

Host translation, committed:

| Intent | Chrome (build-time, via `IntentRegistry`) | Canvas (input-time, via `pending_actions`) |
|---|---|---|
| `ResetToDefault` | The `Slider.reset` trio the builder already carries — registered on the track node | Absolute set with `default_value`; `SetOuterParam` on mirror rows (D4) |
| `OpenMapping` | The label's existing mapping action ⚠ VERIFY-AT-IMPL: which `PanelAction` each site registers today — `rg -n 'ids.label' crates/manifold-ui/src` and read the hits | The existing popover path (`on_right_button_down` → `open_mapping_popover`), now gated to the Label zone |
| `EditValue` | The value cell's existing click-to-type | **None** until P5 lands the canvas text-input primitive (BUG-102); explicit dead stop per D3 in P1–P4 |

Chrome derivation seam: `Slider` gains
`pub fn register_intents(&self, reg: &mut IntentRegistry<PanelAction>)` which walks the
contract and registers each translatable intent on the zone's node. The three hand sites
(`chrome/diff.rs:294`, `layer_header.rs:2152`, `audio_trigger_section.rs:609`) become
calls to it, then their literal `reg.on(ids.track, Gesture::RightClick, …)` lines are
deleted.

## 4. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| I1 — Slider gesture semantics exist once, in the contract; no host hand-registers a slider-zone gesture | Negative gate: `rg -n 'on\(ids\.track' crates/manifold-ui/src` returns zero hits outside `Slider::register_intents` + tests, after P1 |
| I2 — Chrome and canvas resolve the same intent for the same (zone, gesture) | Unit test in `slider.rs` pinning the full contract table, plus a canvas-side test asserting Track+RightClick on a node param row produces the D4 command (pattern: `macros_panel.rs:589`) |
| I3 — Zone geometry has one owner | `build` and `draw` call `zones()`; geometry-equivalence test: `zones().track` == the track node rect `build` produces for identical inputs |
| I4 — Wire-driven rows expose no intents on any surface | Canvas unit test (extends the `:762` guard); chrome N/A (wire-driven params don't build sliders) ⚠ VERIFY-AT-IMPL: confirm via `rg -n 'wire_driven' crates/manifold-ui/src/panels` |
| I5 — `DragController<T>` is the only drag-lifecycle owner in manifold-ui (post-P7.6) | Negative gates, landed with P7.6: `rg -n 'enum ViewportDragMode' crates/manifold-ui/src` → zero; `rg -n 'drag_mode: DragMode' crates/manifold-ui/src` → zero stored-field hits (the overlay stores `DragController<TimelineDrag>`; `DragMode` survives only as the derived return type of `drag_mode()`); no payload enum carries a `None`/idle variant — idle is the controller's `None` session; plus each P7.x phase's own deletion gate |
| I6 — One in-flight gesture per surface, by construction | The type itself: `DragController`'s `Option<DragSession<T>>` makes two simultaneously-armed gestures unrepresentable (D8/D10); pinned per migration by that phase's pinning tests |

## 5. Phasing

Forbidden across all phases (§5-named, per the observed catalog): patching
`on_right_button_down` with an inline reset that bypasses the contract (the symptom fix —
BUG-105 must land AS the contract's first consumer) · a canvas-local copy of zone
geometry (the accidental-duplication disease this design exists to kill) · minting fake
`NodeId`s to reuse `IntentRegistry` on the canvas (the registry is retained-tree-keyed;
the canvas consults the contract directly) · new shared state · widening `Gesture` with
drag variants (D5).

**P1 — the contract + slider right-click, both surfaces (closes BUG-105).**
Entry: re-verify the §1 anchors for slider.rs, diff.rs, interaction.rs (`rg -n
'Gesture::RightClick' crates/manifold-ui/src` — 3 registration sites today; if the count
differs, stop and list). Read-back: this doc §2–§4; `intent.rs` module doc;
interaction.rs:744–812. Deliverables: `SliderZone`, `SliderIntent`, `SliderMetrics`,
`zones()`, `intent_for()`, `Slider::register_intents`; the three hand sites converted
and their literals deleted; canvas `on_right_button_down` consuming zones+contract
(Track → D4 reset emission; Label → existing popover path; wire-driven suppressed);
I1–I4 checks landed by name. Existing reset tests stay green (`macros_panel.rs:589`,
`master_chrome.rs:543`, `param_card.rs:5769`, `layer_header.rs:3107`,
`layer_chrome.rs:369`). Gate: `cargo test -p manifold-ui --lib` + `cargo clippy -p
manifold-ui -- -D warnings`; I1 negative gate. Demo: `ui-snap gltfeditor --script` flow —
right-click a node slider track, assert the param snaps to default. Target **L3** if the
script driver supports a raw-position right-click on the canvas (⚠ VERIFY-AT-IMPL: read
`scripts/ui-flows/` action vocabulary + the driver in `ui_snapshot/script.rs`); else
**L2**: before/after PNG pair plus the emitted `SetGraphNodeParam` in the run log.
Performer gesture: right-click a node slider track to zero a param mid-set-build — the
demo exercises exactly this.

**P2 — stepper + send-fader contracts (closes BUG-070's remainder).**
Entry: re-read BUG-070's backlog entry; inventory the Audio Setup gain `[−]value[＋]`
stepper and the overlay-drag send-fader (`rg -n 'stepper|send.fader' -i
crates/manifold-ui/src/panels`). Deliverables: each widget gets the same shape — zones +
`intent_for` (ResetToDefault on the appropriate zone) + host registration through it.
Gate: `-p manifold-ui --lib`; reset works on both widgets (unit tests naming BUG-070);
BUG-070 entry closed in the same landing.

**P3 — full derivation for the slider's remaining gestures.**
Entry: resolve the §3 VERIFY-AT-IMPL markers (label-mapping and value-cell actions per
site). Deliverables: Click/DoubleClick rows of the contract derived at every retained
slider site; negative gate widens to all slider-zone gestures (`rg -n
'on\(ids\.(track|label|value_text)' crates/manifold-ui/src` → zero outside
`register_intents`). Gate: `-p manifold-ui --lib`; the widened I1 gate.

**P4 — dual-surface widget sweep (scheduled, not bug-triggered — D6 as amended).**
The canvas holds private twins of chrome widgets today: the enum dropdown
(`graph_canvas/interaction.rs:578`, modal over the canvas, vs `dropdown.rs`) and the
Color/Vec editor (`interaction.rs:604`). Entry: inventory every widget kind the node
face renders (`rg -n 'enum_dropdown|color|vec_editor|toggle' crates/manifold-ui/src/graph_canvas`)
and pair each with its chrome twin; if a kind has no twin, record why. Deliverables:
each dual-surface widget gets the P1 shape — zones + `intent_for` in the widget's home
module, both hosts consuming it, its I1-style negative gate. Gate: `-p manifold-ui
--lib` + the widened negative gates; contract-table unit test per widget (I2 pattern).

**P5 — canvas text-input primitive (unblocks BUG-102).**
Build the caret/selection/IME text-entry primitive for the immediate surface once,
host it in `MappingPopover` (label + section + future string fields), and flip the
slider contract's `EditValue` canvas translation from None to live. This is the largest
phase and may warrant its own short design pass at entry (input-method handling has
real edge cases); it is scheduled here so it stops being an indefinite deferral.
Gate: `-p manifold-ui --lib`; demo: type into a popover label field via `ui-snap`.

**P6 — contract derivation for chrome-only widgets.**
Browser popup, pickers, toasts, remaining panel widgets: single-host today, so no
parity bug class exists — this phase moves their gesture registrations onto
widget-declared contracts anyway, so a future canvas appearance (or any new host) is
free and I1's "no hand-registered gestures" gate can go repo-wide. Mechanical;
schedule after P4/P5 or interleave with release-push bricks.

**P7 — drag lifecycle onto `DragController<T>` (added 2026-07-13, Peter; expanded
2026-07-13, Fable design pass — D8–D12 govern everything below).**
D5 stands — drags stay OUT of the intent contract (no `Gesture` drag variants); P7
unifies the lifecycle plumbing. **Corrected inventory (verified 2026-07-13):** drag.rs's
module doc named five machines, but `UIState`'s timeline-drag copy was already folded
into `InteractionOverlay` before P7 began (ui_state.rs comment near :640; drag.rs module
doc records this), and the design-pass audit found three MORE machines the original list
missed (viewport scrubs, audio-setup panel, dock dividers — D12). The real remaining
set, in migration order, is the six sub-phases below. **P7.0 — LANDED 2026-07-13:**
`AudioTriggerSection::dragging_shape` → `DragController<(usize, AudioShapeParam)>`,
main `6917c0ea` (migration commit `b8537171`), pinned by 4 pre-migration tests in
`audio_trigger_section::tests` — the worked precedent every sub-phase copies.

Shared rules for every sub-phase (in addition to the §5-wide forbidden list):
**pin before you switch** — pinning tests are written against the CURRENT machine,
run green, and only then does the fold happen (P7.0's pattern); **compiler-driven** —
delete the old fields/enum first and let the build errors be the exhaustive call-site
checklist (DESIGN_DOC_STANDARD §6); **never arm the controller with fake geometry** —
every `start(payload, pos)` passes the real pointer position already in scope at the
begin site; a begin site with no position in scope is an escalation, never
`Vec2::ZERO`; **command emission stays byte-identical** — these are lifecycle swaps;
any change to what commands are emitted, or in what order, is a red flag to stop on;
**one lifecycle owner at every commit** — no phase leaves both the old machine and the
controller alive.

**P7.1 — `ParamDragState` → `DragController<ParamDragTarget>` (one session).**
Entry: re-run `rg -n 'dragging_param\b|dragging_trim\b|dragging_target_param\b|dragging_decay_param\b|dragging_audio_shape\b|dragging_step_amount\b'
crates/manifold-ui/src/panels/param_card.rs crates/manifold-ui/src/panels/param_slider_shared.rs`
— 49 hits 2026-07-13; a different count → stop and list before touching anything.
Read-back: D8/D10, drag.rs whole, `ParamDragState` (param_slider_shared.rs:665–707).
Committed shape (param_slider_shared.rs, replacing the six slots):

```rust
pub(crate) enum ParamDragTarget {
    Param { index: usize },                                   // was dragging_param: i32 (−1 idle)
    Trim { kind: TrimKind, index: usize, is_min: bool },      // was dragging_trim
    EnvTarget { index: usize },                               // was dragging_target_param
    EnvDecay { index: usize },                                // was dragging_decay_param
    AudioShape { index: usize, param: AudioShapeParam },      // was dragging_audio_shape
    StepAmount { index: usize },                              // was dragging_step_amount
}
pub(crate) struct ParamDragState { drag: DragController<ParamDragTarget> }
```

`ParamDragState` keeps its name and grows accessors so the ~49 sites convert 1:1:
`begin(target, pos)`, `end() -> Option<ParamDragTarget>`, `is_dragging()`, and one
projection per category (`param_index()`, `trim()`, `env_target_index()`,
`env_decay_index()`, `audio_shape()`, `step_amount()`, each `-> Option<…>`). Worked
examples per call-site category — *begin:* `self.drag.dragging_trim = Some((kind, pi,
is_min))` (param_card.rs:3742) → `self.drag.begin(ParamDragTarget::Trim { kind, index:
pi, is_min }, pos)`; *read:* `if let Some((kind, pi, is_min)) = self.drag.dragging_trim`
(:4022) → `… = self.drag.trim()`; *end:* the sequential per-slot `.take()` /
sentinel-reset chains in the end-of-drag handler (:4108–4135) collapse into ONE
`match self.drag.end()`. Deliverables: the enum + accessors; the six fields deleted;
one pinning test per category (six), written pre-switch against the current struct
(pattern: `audio_trigger_section::tests`, `b8537171`), re-run green post-switch.
Gate: `cargo test -p manifold-ui --lib` + `cargo clippy -p manifold-ui -- -D warnings`;
negative: the entry `rg` above → zero hits. Demo: none — L1 (a lifecycle-only swap with
zero pixel or command-shape change; the six pinning tests are the behavior record).
Performer gesture: drag a modulator trim handle on a param card — the Trim pinning test
exercises its begin→track→commit path.

**P7.2 — canvas `DragMode` → `DragController<CanvasDrag>` (one session).**
Entry: re-verify anchors — enum graph_canvas/interaction.rs:10; loose fields
`drag_anchor`/`drag_pan_start` graph_canvas/mod.rs:435–436; move dispatch
interaction.rs:464; release dispatch :1182; `rg -n 'DragMode'
crates/manifold-ui/src/graph_canvas` for the full site list. Read-back: D8/D9, D4/D6
parity notes in the variant doc comments (interaction.rs:28–61).
Committed shape (graph_canvas/interaction.rs; a `DragController<CanvasDrag>` field on
`GraphCanvas` replaces `drag_mode` + `drag_anchor` + `drag_pan_start`):

```rust
pub(crate) enum CanvasDrag {
    Pan { pan_at_grab: (f32, f32) },                          // was field drag_pan_start
    WireFrom { from_node: u32, from_port: String },
    NodeMove { node_id: u32, anchor_offset: (f32, f32) },
    ParamScrub { node_id: u32, param_name: String, range: (f32, f32),
                 start_value: f32, is_int: bool, outer_param_id: Option<String> },
    VecScrub  { node_id: u32, param_name: String,
                kind: crate::graph_view::ParamSnapshotKind, channel: usize,
                base: [f32; 4], range: (f32, f32) },
    Marquee,                                                  // origin = session.start
}
```

Field mapping (seam brief): `DragMode::None` → controller idle (no variant);
`press_origin_x` → `session.start.x` (scrub delta at :506/:545 becomes
`session.current.x - session.start.x`; feed `track()` from `on_pointer_move`);
`Marquee.origin_screen` → `session.start`; Pan's `drag_anchor` → `session.start`.
`self.cursor` STAYS — hover, the ghost wire, and the live marquee rect read it outside
the drag lifecycle (:485–488). `debug_label` moves onto `CanvasDrag` plus an idle case
at the readout call site. The rename (`DragMode` → `CanvasDrag`) is deliberate: it
proves the compiler-driven sweep touched every site. Deliverables: the enum; the three
deleted fields; pinning tests pre-switch for the value-math paths (`ParamScrub` px→value
mapping incl. `is_int` rounding + clamp; `VecScrub` channel-overwrite emitting the full
vector; marquee rect selection; pan math) — canvas-side test precedent per I2.
Gate: `-p manifold-ui --lib` + clippy; negative: `rg -n
'press_origin_x|drag_pan_start|drag_anchor' crates/manifold-ui/src/graph_canvas` → zero;
`rg -n 'enum DragMode' crates/manifold-ui/src/graph_canvas` → zero. Demo: **L2** —
`ui-snap gltfeditor` before/after PNG pair plus a scrub-emitted `SetGraphNodeParam` in
the run log (P1's demo precedent; the script driver has no canvas wiring, per the P1
landing note). Performer gesture: scrub a node param on the canvas mid-set-build; the
value must move exactly as before (same px-per-range feel).

**P7.3 — overlay stage 1: introduce the controller, fold the automation variants (one
session).** Entry: re-verify anchors — enum interaction_overlay.rs:143; the six
`Option<…State>` fields :381–390; `AnimF32`s :405–415; `poll_drag` :652;
`on_begin_drag` :1460; `on_drag` :1561; `on_end_drag` :1607; `cancel_drag` :1805;
re-derive `rg -c 'drag_mode' crates/manifold-ui/src/interaction_overlay.rs` (57 on
2026-07-13). Read-back: D8/D11 and the AnimF32 field docs (:392–431) — the visual layer
is deliberately NOT migrating.
Deliverables, part 1 — drag.rs API additions (committed, with unit tests in
`drag::tests`):

```rust
impl<T> DragController<T> {
    /// Mutable payload access — the automation handlers update
    /// last_beat/last_value-style fields each frame.
    pub fn payload_mut(&mut self) -> Option<&mut T>;
    /// Whole session out, NO commit signal — cancel-with-rollback reads the
    /// payload to clear previews before dropping it (overlay cancel_drag).
    pub fn take_session(&mut self) -> Option<DragSession<T>>;
}
```

Deliverables, part 2 — interaction_overlay.rs:

```rust
enum TimelineDrag {                       // private to the module
    Move,                                 // unit here; folds in P7.5
    TrimLeft, TrimRight,                  // unit here; fold in P7.4
    RegionSelect,                         // unit here; folds in P7.4
    AutomationPoint(AutomationDragState),
    AutomationSegmentBend(AutomationSegmentBendState),
    AutomationSegmentDrag(AutomationSegmentDragState),
    AutomationMarquee,                    // press corner = session.start;
                                          // AutomationMarqueeState is deleted
    AutomationGroupMove(AutomationGroupDragState),
    AutomationDraw(AutomationDrawState),
}
impl TimelineDrag { fn kind(&self) -> DragMode { /* 1:1 */ } }
```

The stored field `drag_mode: DragMode` is REPLACED by `drag:
DragController<TimelineDrag>`; the public API is unchanged: `drag_mode() -> DragMode`
now derives via `kind()` (idle → `DragMode::None`), `is_dragging()` =
`drag.is_active()`. The six `Option` fields are deleted; every automation handler reads
its state via `payload()`/`payload_mut()` — the existing as-ref-then-call-host-then-
as-mut discipline (:1151/:1179 pattern) maps 1:1. `tick()`'s predicates
(`drag_mode == Move`, :493–505) become `matches!(self.drag.payload(),
Some(TimelineDrag::Move))` — behavior unchanged (`duplicate_on_release` stays a loose
field until P7.5). `cancel_drag` uses `take_session()`. Pinning tests: per automation
gesture, begin→preview→commit against the in-file `TestHost`/`GestureTestHost`
(:2466/:2862), written pre-switch where the P4-unit suites don't already cover the
path. Gate: `-p manifold-ui --lib` + clippy; negative: `rg -n
'automation_drag:|automation_segment_bend:|automation_segment_drag:|automation_marquee:|automation_group_drag:|automation_draw:'
crates/manifold-ui/src/interaction_overlay.rs` → zero field declarations. Demo: **L3**
— re-run `scripts/ui-flows/drag-automation-point.json` green. Performer gesture: drag
an automation breakpoint and watch the param preview live — the flow drives exactly
this.

**P7.4 — overlay stage 2: fold trim + region (one session).**
Entry: P7.3 landed (`TimelineDrag` exists); re-verify `begin_trim` :614,
`capture_trim_selection` :624, trim handlers :2002/:2056, `update_region_drag` :2311.
Committed shapes:

```rust
struct TrimDrag {
    clip_id: ClipId,                       // was field trim_clip_id
    original_start_beat: Beats,            // was trim_original_start_beat
    original_duration_beats: Beats,        // was trim_original_duration_beats
    original_in_point: Seconds,            // was trim_original_in_point
    originals: Vec<TrimOriginal>,          // was trim_originals
}
struct RegionDrag { start_beat: Beats, start_layer: usize }
// variants become TrimLeft(TrimDrag), TrimRight(TrimDrag), RegionSelect(RegionDrag)
```

`begin_trim`/`capture_trim_selection` become `TrimDrag` constructors; the six loose
fields above plus `region_drag_start_beat`/`region_drag_start_layer` are deleted.
Pinning tests pre-switch: trim-left and trim-right fan-over-selection incl. the batched
undo entry and locked-clip skip; `drag_readout_clip_id` during a trim; region-select
extents. Gate: `-p manifold-ui --lib` + clippy; negative: `rg -n
'trim_clip_id|trim_original_|trim_originals|region_drag_start_'
crates/manifold-ui/src/interaction_overlay.rs` → zero. Demo: **L3 if** the flow driver
exposes a clip-edge surface target (verify by reading `scripts/ui-flows/drag-clip.json`
+ `ui_snapshot/script.rs` target vocabulary — a trim flow is authored in this phase if
so); **else L2**: before/after PNG of a scripted-position trim plus the commit command
sequence in the run log. Performer gesture: grab a clip's right edge and pull it out a
bar — the selection fans, the undo is one entry.

**P7.5 — overlay stage 3: fold Move (one session — the live timeline's show-critical
gesture; highest stakes in all of P7).**
Entry: P7.4 landed. Additional entry proof: `rg -n 'DragMode::Move'
crates/manifold-ui/src/interaction_overlay.rs` and confirm every arm that ARMS `Move`
also sets an anchor clip — the `poll_drag` guard `Move if
drag_anchor_clip_id.is_some()` (:660) must be proven vacuous before the fold makes the
anchor non-optional; if any path arms Move anchor-less, STOP and escalate (never
synthesize a placeholder `ClipId`). Committed shape:

```rust
struct MoveDrag {
    anchor_clip_id: ClipId,                 // was drag_anchor_clip_id: Option<ClipId>
    start_layer_index: usize,               // was drag_start_layer_index
    snapshots: Vec<DragSnapshot>,           // was drag_snapshots
    snapshot_clip_ids: HashSet<ClipId>,     // was drag_snapshot_clip_ids
    selection_min_start_beat: Beats,        // was drag_selection_min_start_beat
    selection_min_layer: usize,             // was drag_selection_min_layer
    selection_max_layer: usize,             // was drag_selection_max_layer
    layer_blocked: bool,                    // was drag_layer_blocked
    duplicate_on_release: bool,             // was the loose bool
    start_beat: Beats,                      // was drag_start_beat (anchor start at grab)
    offset_beats: Beats,                    // was drag_offset_beats
}
// variant becomes Move(MoveDrag); begin_move() becomes its constructor
```

Eleven loose fields deleted. AnimF32 wiring (D11): `tick()`'s move predicate matches
`Move(_)`; the ghost predicate reads `duplicate_on_release` through `payload()`;
`drag_visual_clip_ids`, `settle_dx`, `landing_flash*`, `error_shake`,
`was_layer_blocked` STAY loose — they ease/fire past release, after the payload is
gone, by design (:416–424). `finalize_move_snap` and `cancel_drag` take what they need
via `take_session()` before the state drops. Pinning tests pre-switch: move
begin→track→commit (multi-clip); opt-duplicate leaves copies; blocked-layer rising edge
fires `error_shake` exactly once; snap-settle seeding on release. Gate: `-p manifold-ui
--lib` + clippy; negative: `rg -n
'drag_anchor_clip_id|drag_start_layer_index|drag_snapshots|drag_snapshot_clip_ids|drag_selection_|drag_layer_blocked|duplicate_on_release: bool|drag_start_beat|drag_offset_beats'
crates/manifold-ui/src/interaction_overlay.rs` → hits only inside `MoveDrag` and tests.
Demo: **L3** — re-run `scripts/ui-flows/drag-clip.json` green (it asserts the moved
rect on the real input path). Performer gesture: grab a clip mid-set and move it two
bars — exactly what the flow drives; this is the gesture "a timing bug becomes the
show" is about, which is why it folds last, alone, fully pinned.

**P7.6 — long-tail sweep: viewport scrubs, audio-setup, dock dividers + the closing
inventory (one session — D12).**
Entry inventory (re-derive, don't trust): `rg -n 'ViewportDragMode|marker_drag'
crates/manifold-ui/src/panels/viewport.rs crates/manifold-ui/src/panels/viewport/interaction.rs`;
`rg -n 'dragging_band|calibration_drag' crates/manifold-ui/src/panels/audio_setup_panel.rs`;
`rg -n 'drag' crates/manifold-ui/src/dock.rs`. Deliverables:
- **Viewport:** `DragController<ViewportDrag>` replacing `ViewportDragMode` (viewport.rs:253)
  + `marker_drag_id` + `marker_drag_start_beat`, with `enum ViewportDrag { RulerScrub,
  OverviewScrub, MarkerDrag { marker_id: ⚠, start_beat: Beats }, ScrollbarHDrag { ⚠ } }`
  — ⚠ VERIFY-AT-IMPL: the `marker_id` field type and whatever grab-state the scrollbar
  drag tracks are read from the field declarations in viewport.rs at execution time
  (this pass verified the machine's existence and shape, not every field type).
- **Audio Setup:** ONE `DragController<AudioSetupDrag>` with `enum AudioSetupDrag {
  Band(BandDivider), Calibration(CalibrationDrag) }` replacing the `dragging_band` +
  `calibration_drag` pair (audio_setup_panel.rs:375/:383; the either-is-some guard at
  :1918 becomes `is_active()`).
- **Dock:** the divider-edge `Option` inside dock.rs's hit→begin→drag→end triad
  (:162–194) becomes `DragController<Edge>`; its existing unit tests (:343–357) are the
  pins.
- **Closing inventory (the phase that ENDS the hunt):** `rg -n 'dragging'
  crates/manifold-ui/src/panels/*.rs crates/manifold-ui/src/*.rs` — every remaining hit
  must be `SliderDragState`-backed (done), `DragController`-backed, or named in D12's
  out-list; anything else → stop and list in the landing report before touching it.
- **Value-math inventory (P7's original entry step, still owed):** the rg-and-read pass
  over drag sensitivity / fine-modifier scaling / snap math across hosts. Expected
  outcome per the P7.0 handoff: genuinely host-specific (px→normalized-param on cards
  and canvas vs px→beats on the timeline) → leave it, record why in the landing report.
  That expectation is a hypothesis to verify by reading, not a finding to transcribe.
- I5's repo-wide negative gates (see §4) land here, by name.
Gate: `-p manifold-ui --lib` + clippy; the I5 gates. Demo: **L2** — before/after PNG of
the timeline ruler scrub position via `ui-snap`, plus the dock/audio-setup pinning
tests. Performer gesture: scrub the timeline ruler to relocate during a build-up.

Phasing-completeness walk: contract both surfaces → P1; stepper/fader reset → P2; full
slider derivation → P3; dual-surface twins (dropdown, color/vec, node-face toggles) →
P4; canvas text entry → P5 (was Deferred, rescheduled 2026-07-13); chrome-only widgets
→ P6 (was Deferred); drag lifecycle consolidation → P7 (was Deferred — D5's
"emission is parity-true" claim was correct but incomplete: the lifecycle machines
share one shape, `DragController<T>` already exists with migrated consumers). Within
P7 (walk re-done 2026-07-13): audio-trigger shape → P7.0 (landed); param-card slots →
P7.1; canvas → P7.2; overlay automation → P7.3; overlay trim/region → P7.4; overlay
move → P7.5; viewport/audio-setup/dock + closing inventory + value-math read → P7.6;
input recognizer / scroll container / SliderDragState panels → D12 out-list (Deferred
with reasons). No body-committed affordance is unphased.

## 6. Decided — do not reopen

1. The retained/immediate split stays; unification happens in the widget layer, never by
   merging the surfaces (D1).
2. Widgets own zones + gesture contract as pure functions in widget language; hosts
   translate, and may translate to an explicit nothing (D2, D3).
3. Canvas reset = the scrub-commit command shape with `default_value`; mirror rows use
   `SetOuterParam`; undo == a drag (D4).
4. Contract covers discrete gestures only; drags stay host-stateful (D5).
5. Slider first, then bricks — no dedicated conversion window (D6, Peter's sequencing).
6. Zone geometry parameterized by per-host metrics; the node face's 72/84 widths are
   correct and stay (D7).
7. Per-gesture drag state folds INTO the `DragController` payload variant — never
   parallel `Option` fields beside the controller; "fresh grab wins" replaces the
   multi-armed latent-bug states (D8).
8. All six canvas `DragMode` variants migrate; `press_origin_x`/`origin_screen`/
   `drag_anchor` are the session's start position, not separate fields; scrubs reading
   only x is fine (D9).
9. `ParamDragState` enforces single-active at type level in the same migration (D10).
10. Overlay: public `DragMode` survives as the derived discriminant; the three
    `AnimF32`s stay outside the controller; three staged folds, one lifecycle owner at
    every commit (D11).
11. The sweep covers viewport scrubs, audio-setup band/calibration, and dock dividers;
    input.rs's drag recognizer, `scroll_container`, and the `SliderDragState`-backed
    panels are out, with reasons (D12).

## 7. Deferred

(2026-07-13: canvas text entry, the remaining widget kinds, and drag lifecycle
consolidation moved OUT of this section into scheduled phases P5 / P4+P6 / P7 —
Peter's call: unify by design, don't wait for bugs.)

- **Drag *semantics* in the intent contract** — still out, per D5: no `Gesture` drag
  variants; hosts own capture and coordinate transforms. P7 unifies the lifecycle
  plumbing (`DragController<T>`), not the gesture contract.
- **Graph-editor sidebar** — already on `IntentRegistry<GraphEditCommand>`; no work. Noted
  so nobody "unifies" it twice.
- **input.rs's drag recognizer** (threshold/arming, input.rs:387–419) — the platform
  layer that decides a drag EXISTS and feeds every machine; upstream of
  `DragController`, not parallel to it (D12). Revive only if a second recognizer ever
  appears.
- **`scroll_container::drag_to_scroll`** — stateless position→fraction mapping; no
  lifecycle to unify (D12).
- **`AudioSetupPanel` `IntentRegistry` derivation** — the P2 landing's scope note: the
  panel routes gestures through its own `UIEvent` match, not the registry; converting
  it is a panel-wide *dispatch* migration, a different unification axis than P7's drag
  lifecycle. Revive as its own phase if a second non-registry panel appears or the
  panel grows more contract widgets.
