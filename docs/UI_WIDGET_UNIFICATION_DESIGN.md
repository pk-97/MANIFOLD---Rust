# UI Widget Unification — one widget vocabulary, two hosts

**Status: IN PROGRESS · 2026-07-10 · Fable · AMENDED 2026-07-13 (Peter): opportunistic conversion replaced by scheduled sweep — P4–P7 added, D6 superseded · 2026-07-13 (Sonnet): P1 + P2 LANDED (main). P3 (full slider derivation) and P4 (dual-surface dropdown/color-vec widgets) not attempted — each is comparable in scope to P1 itself and deserves its own session. P5 (canvas text-input) BLOCKED pending a short design pass (caret/selection/IME model, per this doc's own §"P5" acknowledgment). P6 not started. · 2026-07-13 (Fable, P7 design pass): P7 expanded to P7.0–P7.6 with D8–D12, per Peter's mandate to "unify all of the graph and timeline widgets, UI, and interaction surfaces even if they have not had any bugs raised previously". P7.0 (AudioTriggerSection) LANDED on main `6917c0ea`; P7.1–P7.6 not started, fully briefed below. · 2026-07-13 (Fable, P3–P6 design pass): Peter's skepticism of the "deferred on budget" claims audited against the code. Verdicts — **P3 was NOT execution-ready**: three genuine design gaps found and resolved (D13–D15), including a wrong committed contract row (chrome value-cell type-in is DoubleClick, not Click); P3 is now fully briefed. **P4's premise was stale**: the "chrome twins" of the canvas editors were deleted by GRAPH_EDITOR_REDESIGN Phase 6 — re-scoped to canvas-side hygiene (D17). **P6 dissolved in audit**: every named target is already unified; retired (D18), remainder moved to Deferred. **P5 unblocked**: Peter's four answers written in as D16; phased P5a–P5d, zero open decisions (one non-blocking one-liner flagged for Peter inside D16). All-app interaction survey (Peter's "all UI, all pages, all interaction" mandate): §1b — one new duplication class found (text editing ×3 → P5 kills it) plus the double-click-constant twin (→ P4). · 2026-07-13 (Sonnet, P7 execution): **P7.1 LANDED (main `2b29b92a`)** — `ParamDragState`'s six sentinel slots onto `DragController<ParamDragTarget>`, all 49 call sites converted, 6 pinning tests (begin→track→end through the real handlers). **P7.2 LANDED (same merge)** — canvas `DragMode` renamed `CanvasDrag` per D9 and folded onto `DragController<CanvasDrag>`, `drag_anchor`/`drag_pan_start`/`press_origin_x` deleted, all 6 variants migrated, existing value-math tests in `graph_canvas/tests.rs` re-verified green post-migration as the phase's pinning coverage. Both phases' negative gates pass; manifold-ui --lib 688/688, workspace nextest 3178/3178, clippy -D warnings + cargo deny clean. **P7.3–P7.6 NOT ATTEMPTED this session** — stopped deliberately before the three-stage `InteractionOverlay` fold (each stage individually scoped as "one session" in this doc, culminating in P7.5's live-show timeline Move drag) rather than rush the highest-risk remaining work. Fully briefed below, unchanged.** · 2026-07-13 (Sonnet, P3+P4 execution): **P3 LANDED** — the D13 contract-row flip (`ValueCell, Click` → `ValueCell, DoubleClick`), `BitmapSlider::register_label_mapping` (D14's build-time OpenMapping twin) converting `param_card.rs`'s label registration and `macros_panel.rs`'s hand site, `value_cell_typein`'s D14 input-time contract consult (`debug_assert_eq!`), the D15 per-host translation table recorded as a doc-comment on `intent_for`. Widened I1 gate (`rg -n '\.on\(\w+\.label' crates/manifold-ui/src'` and the pre-existing `on(ids.track` gate) both zero outside `slider.rs`. **P4 LANDED** — `DOUBLE_CLICK_RADIUS_PX` added to `color.rs` beside `DOUBLE_CLICK_TIME_SEC` (I8), the canvas's private `DOUBLE_CLICK_SECONDS`/`DOUBLE_CLICK_RADIUS_PX` consts replaced with aliases into `color.rs`; 5 new pinning tests for `option_at`/`channel_at`/`cell_at` boundary coordinates + a cross-editor modal-priority case; a no-twin classification comment landed on `EnumDropdown`/`VecEditor`/`TableEditor` per D17. Gates: `manifold-ui --lib` 694/694 (+6 over the P7.2 baseline), clippy -D warnings clean, I1/I8 negative gates clean.** · 2026-07-13 (Sonnet, P5a+P5b execution): **P5a LANDED** — `crates/manifold-ui/src/text_edit.rs`: `TextEditModel` (byte-offset caret+anchor) + `byte_offset_for_x`/`x_for_byte_offset` exactly per D16's committed shape; 34 unit tests (multi-byte UTF-8 boundaries incl. é/CJK/emoji, word motion, selection-replace-on-type, arrow-collapses-selection, midpoint-rounding hit-testing). **P5b LANDED** — `manifold-app/src/text_input.rs`'s `TextInputState` embeds the model (old `text`/`cursor`/`select_all` fields deleted — negative gate clean); `begin()` seeds all-selected; the three key-policy blocks in `window_input.rs` gained Shift(select)/Option(word)/Cmd(home-end) arrow modifiers, Cmd+Z (in-session revert to seed), Cmd+C/X/V (new `macos_pasteboard.rs` general-string functions); new `text_input_pointer_down/move/up` on `Application` (wired into `input_mouse_input`/`input_cursor_moved`) place the caret/select-word/drag-select on press-inside, and commit-then-fall-through on press-outside (D16 blur-commit) — mouse-driven double-click-selects-word uses its own minimal timer/radius state on `TextInputState`, keyed off `color.rs`'s single-sourced I8 constants (per D12, the RECOGNIZER stays per-surface; only the constants are shared). `render_text_input_overlay` draws the ranged selection (was whole-field-only). **Dead-key/IME follow-up investigated per the coordinator's mid-task ask, NOT implemented**: read winit 0.30.13's macOS backend (`view.rs`) — `interpretKeyEvents` (the AppKit call that performs ANY key composition, dead-key accents included) only runs when `ime_allowed` is set via `window.set_ime_allowed(true)`; this app never calls it, so today Option+E delivers the raw dead-key glyph and a separate 'e', never a composed 'é' — confirmed broken, not "maybe already works." Fixing it requires enabling `ime_allowed` and handling the previously-unhandled `WindowEvent::Ime(Preedit/Commit)` — genuinely new input-layer plumbing, and AppKit has no way to request "dead keys only": enabling `ime_allowed` turns on the SAME NSTextInputClient machinery CJK composition uses, which Peter separately declined. Left as an explicit open question for Peter, not built. Gates: `manifold-ui --lib` 728/728, `manifold-app` integration tests 14/14 (no `--lib` target — bin crate), clippy -D warnings clean on both, full `cargo build --workspace` clean.**
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

### 1b. Second audit — P3/P4/P5/P6 design pass + all-app interaction survey (verified 2026-07-13)

Run because Peter was "skeptical of the agents exiting their work because they ran out
of session budget" — every deferred phase re-audited against the code, not the prior
agent's self-report. Findings that changed the design:

| Finding | Where (verified) | Consequence |
|---|---|---|
| Chrome value-cell type-in opens on **DoubleClick**, not Click | `inspector.rs:2375–2380` (`UIEvent::DoubleClick` → `route_value_typein`); `slider.rs`'s "click to type" comments (`:13`, `:47`) are aspirational, not behavior | P1's committed contract row `(ValueCell, Click)` was wrong — corrected by D13, code flip lands in P3 |
| `EditValue`'s chrome action is built at INPUT time from live state (anchor bounds, current value, clamp range) | `param_card.rs:877–901` (`value_cell_typein` → `PanelAction::BeginParamTextInput`); registry entries are build-time constants and would go stale (values change on `set_*`-only frames, no re-registration) | Registry derivation is impossible for EditValue → D14's two-mechanism rule |
| Label gestures are genuinely heterogeneous per host | Label+Click = OSC-copy on macros (`macros_panel.rs:458`, `copied_flash`) and param cards (`osc_address`, param_card.rs:140-ish field doc); Label+Click = drawer expand on audio-trigger rows (`audio_trigger_section.rs` module doc); no label mapping action at all on gain / master / layer-chrome sliders | Contract can't own Label+Click → D13's ownership boundary + D15's dead-stop table |
| The canvas editors' "chrome twins" no longer exist | `graph_editor.rs:1–21` module doc: post GRAPH_EDITOR_REDESIGN Phase 6 "every param control now lives on the node face in the canvas"; the panel is a read-only preview inspector. Chrome param cards carry NO color/vec/table rows (`ParamInfo`, param_card.rs — kinds are slider/toggle/trigger/string; enum params render as *labeled sliders* via `value_labels`, not dropdowns) | P4's dual-surface premise is dead → D17 re-scope |
| Text editing is implemented three times | `manifold-app/src/text_input.rs` (`TextInputState`: cursor + whole-field select_all only, no ranged selection, no mouse, no clipboard); `mapping_popover.rs:591–635` (`edit_buffer`: append/pop only, no caret); the window-specific key-policy blocks (`window_input.rs:1174–1186`, `:1338–1357`, `:1794–1857`) | The BUG-102 "primitive" must be an extraction + upgrade, not a green-field widget → D16 |
| No pointer path commits or cancels a text session | commit sites are Enter only (`window_input.rs:1341`, `:1812`); cancel = Esc / overlay-close (`cancel_if_owned_by`) / perform-mode enter. Clicking elsewhere leaves the session active | D16 adds blur-commit (a deliberate, stated behavior change) |
| Double-click thresholds are declared twice, equal by discipline | `color.rs:838` `DOUBLE_CLICK_TIME_SEC = 0.3` vs `graph_canvas/mod.rs:401–404` `DOUBLE_CLICK_SECONDS = 0.3` / `DOUBLE_CLICK_RADIUS_PX = 4.0` (= `DRAG_THRESHOLD_PX`, color.rs:837) | Single-source the constants → P4, I8 |

**All-app survey** (the mandate: duplicated logic doing the same interaction job in more
than one place — not "everything that touches the UI"):

| Pattern | Evidence | Verdict |
|---|---|---|
| Text editing | three implementations, row above | **The real find** → P5a–P5d (D16) |
| Double-click recognition | `input.rs:645–658` vs `graph_canvas/interaction.rs:1111` (`is_double_click`) | Constants single-sourced in P4; the recognizers themselves stay per-surface — the canvas keys on node identity (`last_click_node`), which has no retained analog, and the retained one is welded into `UIInputSystem`'s event stream |
| Drag lifecycle | eight machines | P7.0–P7.6 — in flight in a concurrent session; untouched by this pass |
| Overlay/popup lifecycle | `Overlay` trait + one app driver (`panels/overlay.rs:1–14`), `popup_shell.rs` (one scrim/container), `picker_core.rs` (shared browser/Ableton core) | Already unified (OVERLAY_SYSTEM_DESIGN) — no work |
| Scroll normalization | `window_input.rs::normalize_scroll_delta` — one rule, all consumers | Already unified — no work |
| Keyboard routing | `window_input.rs:1–28` — one owner for both windows since its Phase 7 | Already unified — no work |
| Hover tracking | `UIInputSystem::hovered_widget` (retained) vs canvas `self.hovered` | Per-surface by design (D1) — not duplication |
| Context menus | `dropdown.rs::open_context`, items carry typed `PanelAction`s | One implementation, data-driven — no work |
| Panel click dispatch | ten panels hand-match node ids in `handle_click` beside the `IntentRegistry` (inventory in D18) | Non-uniformity, NOT duplication — each match is the sole implementation of its panel's behavior. Rejected as a phase (D18); moved to Deferred with its real design cost named |
| Tooltips | ONE implementation, canvas-only (`draw_hover_tooltip`, graph_canvas/render.rs:319); retained chrome has none. AUDIO_SETUP_DOCK's P4 deferral formally assigned the "shared tooltip primitive" gap to THIS design (its §Deferred, "the primitive gap itself still belongs to UI_WIDGET_UNIFICATION") | Not duplication today; the obligation is recorded in Deferred with its trigger (first chrome tooltip consumer) so it can't be dropped |

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
(2026-07-13 note: the audit later re-scoped P4 — no divergence disease remained, D17 —
and retired P6, D18; the sequencing principle stands.)

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

**D13 — Contract ownership boundary; EditValue is DoubleClick.** (Added 2026-07-13,
P3–P6 design pass.) The contract owns exactly three (zone, gesture) pairs — the ones
whose meaning must be identical on every surface for muscle memory:
`(Track, RightClick) → ResetToDefault`, `(Label, RightClick) → OpenMapping`,
`(ValueCell, DoubleClick) → EditValue`. The last is a **correction** to P1's committed
table: chrome's shipped type-in gesture is DoubleClick (inspector.rs:2375 →
`route_value_typein`), and P1 transcribed slider.rs's aspirational "click to type"
comment instead of the behavior. Nothing observable regresses from the flip — the
canvas EditValue translation is still a dead stop until P5d, and chrome's dispatch
never consulted the contract's ValueCell row. Every OTHER (zone, gesture) pair is
**host-attached**: hosts may freely bind pairs the contract maps to `None`
(Label+Click OSC-copy on cards/macros; Label+Click drawer-expand on audio-trigger
rows), but may never hand-bind a contract-owned pair — that's the widened I1 gate.
*Rejected: putting Label+Click in the contract (e.g. `CopyAddress`)* — the pair means
different things on different hosts today (copy vs. expand); a contract row that
half the hosts must override is worse than no row.
*Rejected: changing chrome to single-click type-in to match P1's table* — an
unrequested live-UX change smuggled in as a refactor; single-click on chrome value
cells is also drag-adjacent (the cells sit in slider rows) and would mis-fire.

**D14 — Two derivation mechanisms, split by payload class.** Constant-payload intents
(OpenMapping — the action is fixed at build: `ParamLabelRightClick(target, pid)`,
`MacroLabelRightClick(i)`) derive at BUILD time through `register_intents`, P1-style.
Live-payload intents (EditValue — the action carries anchor bounds, current value,
clamp range that change between builds) derive at INPUT time: the host's existing
input-time resolver consults `intent_for()` and constructs the action fresh, exactly
the pattern the canvas already uses (`on_right_button_down`, P1) and chrome already
uses (`route_value_typein` → `value_cell_typein`, inspector.rs:2375/param_card.rs:877).
*Rejected: extending `IntentRegistry` to store closures so everything derives at build
time* — allocation + captured state inside the registry, and the closure would still
read stale values captured at build; the staleness class is the reason input-time
construction exists.

**D15 — Hosts without a target surface translate to nothing; adding surfaces is
deferred product work.** Per D3, a host lacking a mapping popover (gain, master/layer
chrome, audio-trigger) or a type-in (every slider host except param_card/gen-params)
translates the intent to an explicit, greppable nothing. The per-host translation
table is committed in the P3 brief. Wiring NEW type-in / mapping surfaces onto those
hosts (so every value cell in the app can be typed into) is a product call, not a
derivation task — Deferred, with D14's seam making each a one-command addition when
Peter wants it.

**D16 — One text-editing model: `manifold-ui/src/text_edit.rs`; sessions buffer and
commit once.** (Peter's four P5 answers, 2026-07-13, quoted where they decide.)

- **Selection model** (Peter: *"Normal click and drag, shift click selection, standard
  OS and everyday interaction — this should be a unified text system"*): click places
  the caret, click-drag selects a range, shift-click extends, double-click selects the
  word, Cmd+A selects all; typing replaces the selection. Cmd+C/X/V clipboard.
- **Architecture** (Peter: *"General"*): a new widget-layer module
  `crates/manifold-ui/src/text_edit.rs` — sibling of `slider.rs`/`stepper.rs`/`drag.rs`,
  no deps beyond the crate (satisfies ui-depends-only-on-foundation). It owns the
  **editing model only**; each host keeps its renderer and its session policy:
  `manifold-app/src/text_input.rs` keeps `TextInputField` routing, ctx payloads,
  owners, and anchors, but its hand-rolled editing mechanics (`text`/`cursor`/
  `select_all` + `insert_char`/`backspace`/`delete`/`move_*`) are REPLACED by an
  embedded model; `MappingPopover` embeds a second instance for its fields and deletes
  `edit_buffer`. The three implementations become one (I7). The window-specific
  key-policy blocks stay window-specific — window_input.rs:19–24 already documents why
  merging the *policy* would be a behavior change; what unifies is the mechanics
  underneath, which is exactly the split that comment endorses.
- **Committed model shape** (load-bearing; interiors free):

  ```rust
  // crates/manifold-ui/src/text_edit.rs
  pub struct TextEditModel {
      text: String,
      caret: usize,   // byte offset, always on a char boundary
      anchor: usize,  // selection anchor; anchor == caret ⇒ no selection
  }
  // API (all selection-aware): new(&str), text(), selection() -> Range<usize>,
  // insert_char, insert_str, backspace, delete,
  // move_left/move_right(select: bool, word: bool), move_home/move_end(select: bool),
  // select_all, select_word_at(byte), caret_to(byte, extend: bool), drag_to(byte),
  // selected_text() -> &str, take_text() -> String
  //
  // Pointer x ↔ byte offset stays OUTSIDE the model — hosts resolve it via the
  // shared pure helper, parameterized by their own measurer (Painter text_width /
  // UIRenderer measure):
  pub fn byte_offset_for_x(text: &str, rel_x: f32, measure: &mut dyn FnMut(&str) -> f32) -> usize;
  ```
- **IME** (Peter: *"No"*): **interpretation, stated so the executor isn't guessing** —
  the primitive consumes committed characters only (winit `Key::Character`, exactly
  today's path); marked-text / candidate-window composition is UNSUPPORTED: no
  composition events are handled, unknown/control input is ignored (never a crash
  path). Consequence, stated honestly: composition-based scripts (Japanese/Chinese/
  Korean) and macOS dead-key accents (option-e → é arrives via composition) cannot be
  typed into MANIFOLD text fields. ⚠ One-liner for Peter, non-blocking: confirm losing
  dead-key accents is acceptable; if not, that's a scoped follow-up (NSTextInputClient
  adoption feeding this same model — see Deferred), not a change to this design.
- **Undo/commit** (Peter: *"Whatever is industry and user standard"* — researched and
  decided, not left vague): **buffer locally; ONE `EditingService` command on commit;
  commit on Enter AND on blur (a click outside the field commits first, then the click
  proceeds); Esc cancels with no command; in-session Cmd+Z reverts the buffer to its
  seed text (single-level) and never touches the app undo stack.** Rationale: (a)
  comparable creative tools (Ableton clip/track rename, Blender field edit, Figma)
  treat one completed text edit as one undo step — Ctrl+Z after a rename that undoes
  one letter is user-hostile; (b) this codebase's own precedent is already exactly
  this — every existing `TextInputField` commit dispatches one command
  (`handle_text_input_commit`), and the popover's numeric commits are one
  snapshot→changed→commit triad (one undo entry); (c) per-keystroke commands would
  flood the 200-cap undo stack. *Rejected: per-keystroke commands through
  EditingService* — for the reasons above; *rejected: cancel-on-blur* — silently
  discarding typed text on a stray click loses work; Esc is the explicit discard.
  Blur-commit is a small deliberate behavior change (today no pointer path ends a
  session — §1b); overlay teardown keeps its cancel semantics (`cancel_if_owned_by` —
  closing a popup still discards, unchanged).

**D17 — P4 re-scoped: the dual-surface premise is dead; P4 is canvas-side hygiene.**
The §1b audit found the canvas modal editors (`EnumDropdown`, `VecEditor`,
`TableEditor`, graph_canvas/mod.rs:858/:931/:1035) are **single-host widgets** — their
chrome "twins" were deleted when GRAPH_EDITOR_REDESIGN Phase 6 moved all param
authoring onto the node face, and chrome cards never render color/vec/table rows (card
enum params are labeled sliders, not dropdowns). The only genuine overlap with
`dropdown.rs` is the semantic "click an option picks, click outside dismisses" — with
zero shared geometry or infrastructure (retained overlay + scroll + anim vs. a 60-line
pure-math struct). *Rejected: forcing a shared option-list contract between
`dropdown.rs` and `EnumDropdown`* — it would be an adapter around a misfit, the exact
§6 forbidden move; the canvas editors already HAVE the P1 shape (pure anchor-derived
zone geometry + hit queries: `option_at`/`channel_at`/`cell_at`), so the "give them
contracts" work is already done by construction. What remains is pinning + the
double-click-constant single-sourcing — the rewritten P4 brief.

**D18 — P6 retired; panel hand-dispatch is not the disease.** Audit per original P6
target: browser popup / pickers → already one implementation each on the unified
overlay system (`Overlay` trait + `popup_shell` + `picker_core`); dropdown → gesture
semantics already data-driven (items carry typed `PanelAction`s); toasts → trivial
single site; steppers/sliders → P1/P2/P3. **No chrome-only widget remains whose
gesture semantics exist in more than one place**, so contract derivation there has no
duplication to delete — the phase would be ceremony. The real remaining non-uniformity
is that ten panels hand-match node ids in `handle_click` beside the registry
(ableton_picker.rs:595, audio_setup_panel.rs:1689, audio_trigger_section.rs:467,
browser_popup.rs:818, clip_chrome.rs:751, layer_chrome.rs:236, layer_header.rs:2095,
macros_panel.rs:443, master_chrome.rs:341, param_card.rs:3235) — but each match is the
SOLE implementation of its panel's behavior (no twin anywhere), and several click paths
mutate panel-local state (`copied_flash.trigger`, macros_panel.rs:458; drawer toggles),
which the registry's constant-action model cannot express. Wholesale registry migration
is therefore a real design problem (registry actions with local effects) with no bug
class driving it. *Rejected as a phase; moved to Deferred with a trigger.* If Peter
wants mechanism uniformity for its own sake, that's his call to make with the cost in
front of him — it was NOT what "deferred on budget" was hiding.

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
            // D13 correction (2026-07-13): DoubleClick, not Click — chrome's
            // shipped type-in gesture. The as-built P1 code still says Click;
            // P3 flips it (no observable change: canvas is a dead stop, chrome
            // never consulted this row).
            (SliderZone::ValueCell, DoubleClick) => Some(SliderIntent::EditValue),
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
| `OpenMapping` | Resolved 2026-07-13 (the former ⚠ marker): `param_card.rs` registers `ParamLabelRightClick(target, pid)` on the label AND the drawer catcher (`:4218–4224`); `macros_panel.rs` registers `MacroLabelRightClick(i)` (`:475`). No other slider host has a label mapping action — gain / master / layer-chrome / audio-trigger translate to nothing (D15). Build-time derivation per D14 | The existing popover path (`on_right_button_down` → `open_mapping_popover`), now gated to the Label zone |
| `EditValue` | The value cell's existing type-in, opened on **DoubleClick** (D13) and constructed at INPUT time (`inspector.rs:2375` → `route_value_typein` → `value_cell_typein` param_card.rs:877 → `BeginParamTextInput`) — input-time contract consult per D14, never a registry entry | **None** until P5d wires it; explicit dead stop per D3 in P1–P4 |

Chrome derivation seam: `Slider` gains
`pub fn register_intents(&self, reg: &mut IntentRegistry<PanelAction>)` which walks the
contract and registers each translatable intent on the zone's node. The three hand sites
(`chrome/diff.rs:294`, `layer_header.rs:2152`, `audio_trigger_section.rs:609`) become
calls to it, then their literal `reg.on(ids.track, Gesture::RightClick, …)` lines are
deleted.

## 4. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| I1 — Slider gesture semantics exist once, in the contract; no host hand-registers a **contract-owned** slider-zone pair (D13 — host-attached pairs on `None` rows stay legal) | Negative gate: `rg -n 'on\(ids\.track' crates/manifold-ui/src` returns zero hits outside `Slider::register_intents` + tests, after P1. Widened by P3 to labels: `rg -n '\.on\(\w+\.label' crates/manifold-ui/src` → zero hits outside `slider.rs`'s `register_*` fns + tests (value cells never registry-register — they derive at input time per D14, so no `on(…value_text…)` may ever appear) |
| I2 — Chrome and canvas resolve the same intent for the same (zone, gesture) | Unit test in `slider.rs` pinning the full contract table, plus a canvas-side test asserting Track+RightClick on a node param row produces the D4 command (pattern: `macros_panel.rs:589`) |
| I3 — Zone geometry has one owner | `build` and `draw` call `zones()`; geometry-equivalence test: `zones().track` == the track node rect `build` produces for identical inputs |
| I4 — Wire-driven rows expose no intents on any surface | Canvas unit test (extends the `:762` guard); chrome N/A (wire-driven params don't build sliders) ⚠ VERIFY-AT-IMPL: confirm via `rg -n 'wire_driven' crates/manifold-ui/src/panels` |
| I5 — `DragController<T>` is the only drag-lifecycle owner in manifold-ui (post-P7.6) | Negative gates, landed with P7.6: `rg -n 'enum ViewportDragMode' crates/manifold-ui/src` → zero; `rg -n 'drag_mode: DragMode' crates/manifold-ui/src` → zero stored-field hits (the overlay stores `DragController<TimelineDrag>`; `DragMode` survives only as the derived return type of `drag_mode()`); no payload enum carries a `None`/idle variant — idle is the controller's `None` session; plus each P7.x phase's own deletion gate |
| I6 — One in-flight gesture per surface, by construction | The type itself: `DragController`'s `Option<DragSession<T>>` makes two simultaneously-armed gestures unrepresentable (D8/D10); pinned per migration by that phase's pinning tests |
| I7 — One text-editing model (post-P5c): every text buffer/caret/selection mutation lives in `text_edit.rs` | Negative gates, landed with P5c: `rg -n 'edit_buffer' crates/manifold-ui/src` → zero; `rg -n 'fn insert_char|fn backspace' crates/manifold-ui/src crates/manifold-app/src` → hits only in `text_edit.rs`; plus the P5a unit suite pinning the model |
| I8 — Gesture-timing constants have one home (`color.rs`) | Negative gate, landed with P4: `rg -n 'const DOUBLE_CLICK' crates/manifold-ui/src` → hits only in `color.rs` |

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

**P1 LANDED 2026-07-13 (Sonnet, main `02418e4d`).** Entry re-verify found 4 hand
registration sites, not 3 — `param_card.rs`'s `register_intents` had three independent
`on(ids.track/sl.track/cfg.decay_slider.track, RightClick, reset)` calls (main rows,
envelope decay, audio-shape drawer rows) the §1 audit table never named. Converted all 4;
I1's own negative gate requires it regardless of the count discrepancy. Demo: the
`--script` JSON runner (`scripts/ui-flows/` + `ui_snapshot/script.rs`) has no graph-canvas
wiring at all (confirmed by reading both — `AutomationTarget`/`Gesture::RightClick` exist
but nothing routes them to `GraphCanvas`), so **L3 isn't reachable**; landed as **L2**:
the `gltfeditor` scene's base PNG (`target/ui-snapshots/gltfeditor/gltfeditor.png`) plus
the pre-existing `right_click_track_zone_resets_numeric_param_to_default` test, which
asserts the exact emitted `SetGraphNodeParam` on the real `on_right_button_down` path.

**P2 — stepper + send-fader contracts (closes BUG-070's remainder).**
Entry: re-read BUG-070's backlog entry; inventory the Audio Setup gain `[−]value[＋]`
stepper and the overlay-drag send-fader (`rg -n 'stepper|send.fader' -i
crates/manifold-ui/src/panels`). Deliverables: each widget gets the same shape — zones +
`intent_for` (ResetToDefault on the appropriate zone) + host registration through it.
Gate: `-p manifold-ui --lib`; reset works on both widgets (unit tests naming BUG-070);
BUG-070 entry closed in the same landing.

**P2 LANDED 2026-07-13 (Sonnet, main `e68f033f`).** Entry re-read found BUG-070 already
FIXED before this session (`docs/BUG_BACKLOG.md`, no reopen needed) — the stepper and the
overlay-drag send-fader turned out to be the SAME underlying gain value with two input
methods, already sharing one reset gesture; no second widget to contract separately.
Added a minimal `StepperZone`/`StepperIntent` contract (`crates/manifold-ui/src/
stepper.rs`) and converted `audio_setup_panel.rs`'s hand `UIEvent::RightClick` id match to
consult it. Scope note NOT closed here: unlike the slider hosts, `AudioSetupPanel` routes
none of its gestures through `IntentRegistry` — full P1-style `register_intents`
derivation would be a panel-wide dispatch migration, left as a follow-up.

**P3 — full derivation for the slider's remaining gestures (re-briefed 2026-07-13; D13–D15 govern).**
History: **NOT ATTEMPTED 2026-07-13 (Sonnet)** — "heterogeneous, non-uniform
label-mapping/value-cell-edit logic… comparable in scope to P1." The design-pass audit
confirmed the deferral was hiding real design gaps, not just budget (§1b): the
committed ValueCell row was wrong, EditValue can't ride the registry, and label
gestures diverge by host. D13–D15 resolve all three; the phase below has zero open
decisions.

Entry: re-verify the §1b anchors this phase depends on — `rg -n 'ParamLabelRightClick|MacroLabelRightClick'
crates/manifold-ui/src` (expect: enum defs in panels/mod.rs:792/:803, registration at
param_card.rs:4218–4224 and macros_panel.rs:473–475, plus consumers/tests; a different
registration-site set → stop and list); `rg -n 'route_value_typein|value_cell_typein'
crates/manifold-ui/src` (expect inspector.rs + param_card.rs + tests). Read-back: D13,
D14, D15, §3's corrected table.

Deliverables:
1. **Contract-row flip** in `slider.rs::intent_for`: `(ValueCell, Click)` →
   `(ValueCell, DoubleClick)` per D13, plus fixing the two stale "click to type"
   comments (slider.rs:13, :47) and P1's contract-table pinning test to match.
2. **`BitmapSlider::register_label_mapping(ids: &SliderNodeIds, mapping: &PanelAction,
   reg: &mut IntentRegistry)`** — the D14 build-time twin of `register_track_reset`:
   walks `intent_for(Label, RightClick)`, registers `mapping` on `ids.label` when the
   contract says `OpenMapping`, no-ops when `ids.label` is `None`. `Slider::register_intents`
   grows an optional mapping action the same way it carries `reset`.
   Convert the two hand sites: param_card.rs:4218–4224 (label + drawer catcher — the
   catcher is a second node carrying the SAME action; pass both through the new fn, or
   keep the catcher line hand-registered with a comment naming it host-attached
   chrome — executor's choice, both end states pass the gate as long as the *label*
   registration goes through the contract) and macros_panel.rs:473–475. Delete the
   literal `on(…, RightClick, …)` lines they replace.
3. **Input-time contract consult for EditValue** (D14): `value_cell_typein`
   (param_card.rs:877) gates on
   `BitmapSlider::intent_for(SliderZone::ValueCell, Gesture::DoubleClick) == Some(SliderIntent::EditValue)`
   (a const-foldable check whose value is that the contract is the single written
   table) and its doc-comment names the contract row; same for
   `driver_period_typein` if the executor judges the Free field a value cell —
   default: leave it host-attached (it's a drawer field, not a slider zone).
4. **Per-host translation table, landed as a doc-comment on `intent_for`** (the D15
   record): param_card/gen-params = all three intents live; macros = Reset + OpenMapping,
   EditValue → nothing; gain/master-chrome/layer-chrome/audio-trigger/chrome-spec
   sliders = Reset only, OpenMapping + EditValue → nothing. Host-attached pairs
   (Label+Click OSC copy, audio-trigger Label+Click drawer expand) stay hand-dispatched
   and are named in the comment as deliberate non-contract gestures.
5. I1's widened negative gate (§4) landed by name; I2's contract-table test updated to
   pin all three rows including the DoubleClick correction.

Forbidden moves (this phase's specific temptations): registering `BeginParamTextInput`
in the registry "just once to be uniform" (D14 staleness class) · changing any host's
observable gesture besides the contract-row flip (which changes nothing observable —
if a diff would change what a click DOES anywhere, stop) · adding mapping/type-in
surfaces to hosts that lack them (D15 — Deferred, product call).
Gate: `cargo test -p manifold-ui --lib` + `cargo clippy -p manifold-ui -- -D warnings`;
the widened I1 gate; existing reset tests stay green (P1's list). Demo: none — **L1**
(derivation-only; the six pinned behaviors are the record — no pixel or command-shape
change is permitted, which is itself the check). Performer gesture: right-click a macro
label mid-set → the mappings dropdown opens exactly as before the derivation.

**P4 — canvas-editor pinning + gesture-constant single-sourcing (re-scoped 2026-07-13 — D17).**
The original P4 premise ("the canvas holds private twins of chrome widgets") did not
survive the §1b audit: no chrome twins remain (GRAPH_EDITOR_REDESIGN Phase 6 deleted
the authoring sidebar; cards never render color/vec/table rows). The canvas editors
already have the P1 contract shape by construction — pure anchor-derived zone geometry
with hit queries (`EnumDropdown::option_at` mod.rs:912, `VecEditor::channel_at` :1015,
`TableEditor::cell_at` :1095). What remains is small and canvas-side:
Entry: re-derive existing test coverage — `rg -n 'option_at|channel_at|cell_at|is_double_click'
crates/manifold-ui/src/graph_canvas/tests.rs` — and list which hit-geometry paths and
which modal-priority branches (enum → vec → table → row dispatch order in
`on_left_button_down`, interaction.rs:594/:621/:668) are unpinned. Read-back: D17, D13.
Deliverables: (1) unit tests pinning every unpinned hit query (option/channel/cell at
boundary coordinates) and the modal priority + dismiss-vs-swallow semantics per editor
(press on option/channel/cell vs. inside-padding vs. outside), pattern:
graph_canvas/tests.rs:259; (2) `DOUBLE_CLICK_SECONDS` / `DOUBLE_CLICK_RADIUS_PX`
(graph_canvas/mod.rs:401–404) deleted and re-pointed at `color.rs` — add
`pub const DOUBLE_CLICK_RADIUS_PX: f32 = 4.0` beside `DOUBLE_CLICK_TIME_SEC`
(color.rs:838) and alias both surfaces to them (I8); the recognizers themselves stay
per-surface (§1b survey — the canvas keys on node identity). (3) The no-twin
classification recorded: a one-line comment on each editor struct naming it
single-host by design (so a future "unify with dropdown.rs" impulse hits D17's
rejection). Forbidden: a shared option-list abstraction between `dropdown.rs` and
`EnumDropdown` (D17's named adapter trap) · touching `DragMode`/drag fields (P7.2 owns
that file's drag machinery — in flight in another session; coordinate at landing if
both touch interaction.rs). Gate: `-p manifold-ui --lib` + clippy; I8's negative gate.
Demo: none — **L1** (pin-only phase).

**P5 — unified text editing (P5a–P5d; designed 2026-07-13 from Peter's four answers — D16 governs all four).**
Was BLOCKED on a design pass; now fully decided. Not green-field: the §1b audit found
three existing text-editing implementations — P5 extracts ONE model and re-hosts all
of them on it. Order matters: P5a is pure library, P5b is the app session, P5c closes
BUG-102, P5d closes the contract's last dead stop. P3 must land before P5d (it flips
the ValueCell row to DoubleClick).

**P5a — the model (one session).**
Deliverables: `crates/manifold-ui/src/text_edit.rs` — `TextEditModel` +
`byte_offset_for_x` exactly as committed in D16; unit tests covering every op at
multi-byte UTF-8 boundaries (`é`, CJK, emoji), word-motion boundaries, selection
replace-on-type, `byte_offset_for_x` with a fake monospace measurer (midpoint
rounding: a click past a glyph's midpoint lands after it). No caller changes.
Forbidden: OS/pasteboard/winit deps in manifold-ui (the model takes/returns plain
strings) · storing selection as a second position type (anchor+caret byte offsets
only). Gate: `-p manifold-ui --lib` + clippy. Demo: none — **L1**.

**P5b — the app session re-hosted + mouse + clipboard (one session).**
Entry: P5a landed; re-derive the field/caller inventory — `rg -n '\.text_input\.'
crates/manifold-app/src | wc -l` and `rg -n 'text_input\.(text|cursor|select_all)\b'
crates/manifold-app/src` — list before touching. Deliverables:
- `TextInputState`'s `text`/`cursor`/`select_all` replaced by `model: TextEditModel`
  (compiler-driven: delete the fields first); `begin()` seeds the model all-selected
  (today's first-keystroke-replaces behavior, now as a real selection); the key
  handlers' `insert_char`/`backspace`/`move_*` calls forward to the model, gaining
  `shift`/`word` modifiers (Shift+arrows select; Option+arrows word-move; Cmd+arrows
  home/end — standard macOS bindings).
- **Mouse**: a pointer branch in `window_input.rs`'s press/move/release dispatchers,
  gated on `text_input.active`: press inside the anchor rect → `caret_to(byte_offset_for_x(…), shift)`
  (double-click → `select_word_at`); drag → `drag_to`; press OUTSIDE → **commit, then
  let the press continue** (D16 blur-commit). Single-line fields only need x;
  multiline (GraphWgsl) maps line-by-y first — same helper per line.
- **Clipboard**: `macos_pasteboard.rs` (the sole NSPasteboard module) gains
  `general_pasteboard_string() -> Option<String>` and `set_general_pasteboard_string(&str)`;
  Cmd+C/X/V on an active session route through the model (`selected_text`/delete/
  insert_str; paste strips `\n` in single-line fields). Cmd+Z in-session reverts to
  seed text per D16.
- Overlay renderer (`app_render.rs::render_text_input_overlay`, :4801) draws the
  ranged selection highlight + caret from `model.selection()` (today: select_all-only
  highlight).
Forbidden: merging the window-specific key-policy blocks (window_input.rs:19–24 — they
stay separate BY DESIGN; only the mechanics beneath them unify) · a parallel legacy
editing path kept alive (the old field trio must be gone — negative gate) · touching
`handle_text_input_commit`'s per-field command routing (commit payloads are out of
scope). Gate: `-p manifold-ui --lib` + `cargo test -p manifold-app --lib` + clippy on
both; negative: `rg -n 'select_all: bool|cursor: usize' crates/manifold-app/src/text_input.rs`
→ zero. Demo: target **L3** if the `ui-snap --script` driver has key/text-event
actions (⚠ VERIFY-AT-IMPL: read `ui_snapshot/script.rs`'s action vocabulary); else
**L2** — a PNG of an active session showing a ranged (not whole-field) selection +
caret, plus a ≤2-min click-script for Peter (rename a marker: click into the name,
double-click a word, type over it, click away → committed). Performer gesture: click
into a timeline-marker name, select one word, retype it — no Enter needed, click away
and it sticks.

**P5c — MappingPopover fields on the model (one session — closes BUG-102).**
Entry: P5b landed (blur-commit semantics exist); re-verify popover anchors —
`enter_edit`/`on_text_char`/`commit_edit` (mapping_popover.rs:591/:604/:641) and the
BUG-102 entry's write-path note (`BindingMappingEdit::section` already shipped and
tested at the command layer). Deliverables: `edit_buffer` + `on_text_char`/
`on_backspace` replaced by an embedded `TextEditModel` (numeric fields keep their
char-filter in front of `insert_char`; label takes any printable); caret + ranged
selection drawn in the popover's field row via `Painter` (the model's selection Range
× `draw.rs::text_width` through `byte_offset_for_x`'s inverse — a small pure
`x_for_byte_offset` helper is permitted in text_edit.rs if needed); pointer press/drag
inside an editing field routes to the model (the popover already receives
on_press/on_move/on_release); **an `EditField::Section` row** wired to the shipped
`BindingMappingEdit.section` write path (outer-Option = touched, inner = value/clear —
the BUG-102 entry's shape); the module doc's stale "label is read-only" paragraph
(mapping_popover.rs:24–29) rewritten. Commit semantics per D16 (Enter/blur commit —
blur includes clicking another popover control; Esc cancels). Gate: `-p manifold-ui
--lib` + clippy; negative: `rg -n 'edit_buffer' crates/manifold-ui/src` → zero (I7
lands here by name). BUG-102 → FIXED in `docs/BUG_BACKLOG.md` in the same landing.
Demo: **L2** — `ui-snap` PNG pair of the popover label mid-edit (caret + partial
selection visible) and post-commit; affordance check per DESIGN_DOC_STANDARD §5 (the
label/section rows must read as editable — hover/edit chrome, not bare text).
Performer gesture: rename a mapped knob's label and give it a section, live, without
leaving the canvas.

**P5d — canvas EditValue goes live (one session — the contract's last dead stop).**
Entry: P3 landed (ValueCell row = DoubleClick) + P5b landed (session mechanics);
re-verify the canvas numeric-row dispatch (interaction.rs:846–872: press anywhere on a
numeric row starts `ParamScrub`) and `is_double_click` (:1111). Deliverables: in the
numeric-row branch, a double-click (via `is_double_click`) landing in the row's
value-box zone (`BitmapSlider::zones` with the canvas metrics — the P1 geometry, NOT a
private copy) consults `intent_for(ValueCell, DoubleClick)` and emits a new
`GraphEditCommand::EditGraphNodeNumericParam { node_id, param_name, current, min, max,
whole_numbers, outer_param_id, anchor }` instead of arming a scrub; the app maps it to
a text session exactly like `InspectorParam` (new `TextInputField::GraphNumericParam(u32)`
+ ctx carrying id/range/`outer_param_id`; commit parses, clamps, dispatches
`SetGraphNodeParam` — or `SetOuterParam` for a group-face mirror row, D4 parity).
Wire-driven rows stay suppressed (the `:762` guard, I4). ⚠ VERIFY-AT-IMPL: confirm a
press+release with no movement emits no scrub command (read the release dispatch,
interaction.rs:1182) — if a zero-move scrub commits anything, gate the first click's
emission before layering double-click on top, and say so in the report. Forbidden: a
canvas-local zone-geometry copy (the §5-wide rule) · click-to-cycle or any non-list
enum shortcut (Peter's 2026-07-01 call, mod.rs:854) · reusing `InspectorParam`'s field
with a smuggled node id (mint the real variant). Gate: `-p manifold-ui --lib` +
`cargo test -p manifold-app --lib` + clippy; canvas-side test asserting double-click
on a value box emits the command and single-press elsewhere still scrubs. Demo: **L2**
— `ui-snap gltfeditor` PNG with the type-in open over a node value box, plus the
committed `SetGraphNodeParam` in the run log (P1's demo precedent). Performer gesture:
double-click a node's value box mid-set-build, type `0.5`, Enter — the exact value
lands without a scrub.

**P6 — RETIRED 2026-07-13 (D18).** The audit found no chrome-only widget whose gesture
semantics exist in more than one place — every original target is already unified
(overlay system, popup_shell, picker_core, typed dropdown items) or covered by
P1/P2/P3. "Mechanical" was wrong on both counts: there is nothing mechanical left, and
what remains (panel dispatch migration) is neither mechanical nor duplication — see
D18 and Deferred. I1's repo-wide gates land with P3; no separate phase.

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

Phasing-completeness walk (re-done 2026-07-13 for the redesigned phases): contract
both surfaces → P1; stepper/fader reset → P2; label-mapping derivation + the
ValueCell-row correction + D15 dead-stop record → P3; canvas-editor pins +
gesture-constant single-sourcing → P4; the text model → P5a; app session + mouse +
clipboard + blur-commit → P5b; popover label/section (BUG-102) → P5c; canvas numeric
type-in (last dead stop) → P5d; chrome-only derivation → RETIRED (D18 — nothing to
derive); type-in/mapping surfaces on hosts that lack them, IME composition, and panel
dispatch migration → Deferred, each with its trigger; drag lifecycle consolidation →
P7 (was Deferred — D5's
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
12. The contract owns three pairs — Track+RightClick, Label+RightClick,
    ValueCell+**DoubleClick** (corrected from P1's Click); all other pairs are
    host-attached and legal (D13).
13. Constant-payload intents derive at build time via the registry; live-payload
    intents (EditValue) consult the contract at input time; the registry never stores
    closures (D14).
14. Hosts without a mapping/type-in surface translate to an explicit nothing; ADDING
    those surfaces is deferred product work, never smuggled into a derivation phase
    (D15).
15. One text-editing model, `manifold-ui/src/text_edit.rs`; standard click/drag/
    shift-click selection; NO IME composition (committed characters only); buffer
    locally, ONE command on Enter/blur, Esc cancels, in-session Cmd+Z reverts to seed
    (D16 — Peter's four answers, 2026-07-13).
16. P4 is canvas-side pinning + constant single-sourcing; the dual-surface premise is
    dead — no chrome twins remain, and no shared dropdown abstraction gets built
    (D17).
17. P6 is retired; panel hand-`handle_click` dispatch is single-implementation
    non-uniformity, not duplication — wholesale registry migration is deferred with
    its real design cost named (D18).

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
- **Panel dispatch migration (all hand-`handle_click` panels onto the registry)** —
  the D18 inventory: ten panels, each match the sole implementation of its behavior,
  several mutating panel-local state in the click path (a shape the registry's
  constant-action model can't express — that's the design problem to solve first).
  No duplication, no bug class. Trigger: Peter prioritizes mechanism uniformity with
  the cost in front of him, or a real cross-panel discrete-gesture parity bug appears.
- **Type-in and mapping surfaces on the hosts that lack them** (macro value cells,
  layer gain, master/layer opacity+brightness) — D15: the contract makes each a
  one-command addition through the D14 seam, but each is NEW behavior, a product
  call. Trigger: Peter asks for "type-in everywhere" (or right-click-mapping
  everywhere).
- **IME composition + dead-key accents** — Peter declined IME for v1 (D16). The
  primitive ignores composition events; dead-key accents (option-e → é) are lost with
  them. Trigger: Peter wants non-Latin (or accented) text in fields → NSTextInputClient
  adoption in the app layer, feeding the SAME `TextEditModel` — the model's API was
  shaped so composition arrives as `insert_str` + selection ops, no redesign.
- **Per-surface double-click recognizers** — constants unify (P4/I8); the recognizers
  themselves stay per-surface (canvas keys on node identity; the retained one is
  welded into `UIInputSystem`). Revive only if a third recognizer appears.
- **Shared tooltip primitive** — assigned to this design by
  AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION's P4 escalation ("the primitive gap itself
  still belongs to UI_WIDGET_UNIFICATION"). Today tooltips exist ONCE, canvas-only
  (`draw_hover_tooltip`, graph_canvas/render.rs:319) — no duplication, so no phase
  yet. Trigger: the first retained-chrome tooltip consumer. Shape when revived: a
  widget-layer primitive (hover-dwell timing + placement as pure functions, per-host
  drawing — the D2 pattern), with the canvas's bespoke drawer converted as the second
  host; never a second bespoke implementation.
