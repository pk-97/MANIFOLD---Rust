# UI Widget Unification — one widget vocabulary, two hosts

**Status: APPROVED design, not built · 2026-07-10 · Fable · AMENDED 2026-07-13 (Peter): opportunistic conversion replaced by scheduled sweep — P4–P7 added, D6 superseded**
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

**P7 — drag lifecycle onto `DragController<T>` (added 2026-07-13, Peter).**
D5 stands — drags stay OUT of the intent contract (no `Gesture` drag variants) — but
the lifecycle plumbing is quintuplicated: `drag.rs`'s own module doc names five drag
state machines sharing one shape (`SliderDragState` — already migrated as the proof
consumer; per-panel `dragging` bools; `UIState` timeline drag;
`InteractionOverlay::DragMode`; canvas `DragMode`). Deliverables: migrate the four
remaining machines onto `DragController<T>`, one landing each, behavior pinned by a
test per migration before the switch. Entry step: also inventory drag *value* math
(sensitivity, fine-modifier scaling, snap) across hosts — if duplicated, fold it into
the widget's home module in the same pass; if genuinely host-specific (camera-zoom
delta scaling is), leave it and record why. Gate: `-p manifold-ui --lib` per landing.

Phasing-completeness walk: contract both surfaces → P1; stepper/fader reset → P2; full
slider derivation → P3; dual-surface twins (dropdown, color/vec, node-face toggles) →
P4; canvas text entry → P5 (was Deferred, rescheduled 2026-07-13); chrome-only widgets
→ P6 (was Deferred); drag lifecycle consolidation → P7 (was Deferred — D5's
"emission is parity-true" claim was correct but incomplete: five lifecycle machines
share one shape, `DragController<T>` already exists with one migrated consumer). No
body-committed affordance is unphased.

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

## 7. Deferred

(2026-07-13: canvas text entry, the remaining widget kinds, and drag lifecycle
consolidation moved OUT of this section into scheduled phases P5 / P4+P6 / P7 —
Peter's call: unify by design, don't wait for bugs.)

- **Drag *semantics* in the intent contract** — still out, per D5: no `Gesture` drag
  variants; hosts own capture and coordinate transforms. P7 unifies the lifecycle
  plumbing (`DragController<T>`), not the gesture contract.
- **Graph-editor sidebar** — already on `IntentRegistry<GraphEditCommand>`; no work. Noted
  so nobody "unifies" it twice.
