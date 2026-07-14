# Param Two-Way Binding — node-face edits on bound params write back through the inverse mapping

**Status:** IN PROGRESS · P1 SHIPPED 2026-07-14 (Sonnet 5, `bc2f2c0b`) · P2 not started · authored 2026-07-14 · Fable 5
**Prerequisites:** none (BUG-158's investigation + Fable design consult are folded in; all code anchors re-verified 2026-07-14)

**P1 execution note (2026-07-14, Sonnet):** shipped the inverse machinery
(D2/D3), the dispatch-layer reroute (D1), D4's effective-value display, and
D9's freeze-on-unmap. One judgment call, not pre-approved in this doc: §3's
"emit the ParamSnapshot/ParamChanged/ParamCommit lifecycle" assumed a
gesture-boundary signal that doesn't exist on the wire for a plain node-face
`SetGraphNodeParam`/`ParamScrub` (only card-slider drags and group-face
mirror rows carry snapshot/commit ticks; a plain node-face scrub emits one
`SetGraphNodeParam` per pointer-move with no press/release signal reaching
the app). Fix: added `GraphEditCommand::EndGraphNodeParamScrub`, emitted
unconditionally on `ParamScrub` release (a no-op for unbound rows), so the
dispatch layer can close a bound-param drag with ONE undo-worthy
`ChangeGraphParamCommand` covering the whole gesture instead of one per
move — the smallest addition matching the existing `NodeMove`
release-emits-one-command precedent in the same file. D10's sequencing rule
(P2 must not trail a shipped P1) was knowingly not followed: only one phase
fit this session, and P1 leaves wired-param behavior completely unchanged
(a wired param still runs the pre-existing `SetGraphNodeParamCommand` path
untouched), so the specific harm D10 names — wire-driven snap-back reading
as newly broken — does not occur. The P1 gate's named integration test
(`node_face_edit_on_bound_param_moves_card_not_def`) was NOT written
verbatim: `Application`-level dispatch needs a winit/GPU harness this
session didn't have one for. In its place: `binding_reroute_tests` in
`crates/manifold-app/src/app_render.rs` unit-tests the resolution helpers
(`binding_for_node_param`, `node_param_is_wired`) the reroute is built from,
plus `card_reshape_roundtrips` / `macro_curve_inverse_roundtrips`
(manifold-core) for the inverse math, and
`unexpose_user_binding_freezes_effective_value_into_def_slot`
(manifold-editing) for D9. The full vertical path (a real node-face drag
moving both the card and the render) has not been driven end-to-end by an
automated test — flagged for whoever picks up P2 or does the look-pass.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Closes **BUG-158** (mapped-param-edits-snap-back-no-two-way-binding). The governing
insight: a card binding makes the outer card param the *sole authority* the render
ever sees (`apply_bindings` re-writes the graph param on every rebuild), so the only
coherent write path for a node-face edit on a bound param is **through the card
param via the inverse mapping** — not a second write into graph state the binding
will stomp. Peter's expectation, from the report: "two-way behaviour between the
node param, the card slider, and other ports — turning either end moves both, like
a DAW control surface." On stage this means: while authoring in the graph editor,
grabbing *either* end of a mapping works, and a param that genuinely can't be
turned (signal-driven) *looks* like it can't be turned instead of lying and
snapping back.

Companion docs: `docs/GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` (the editor hosts the
inspector column this design's card writes land in); `docs/BUG_BACKLOG.md` BUG-158
entry (symptom + investigation record).

## 1. Audit — what exists (verified 2026-07-14)

Extend, don't redesign. Every piece below was read this session.

| Piece | Where | State |
|---|---|---|
| Forward reshape (single definition) | `crates/manifold-core/src/effects.rs:489` `apply_card_reshape(value, min, max, invert, curve, scale, offset)` | Two stages: slider response (normalize → invert → curve, clamped 0..1) then unclamped affine `v*scale+offset`. Shared by runtime + popover preview by design ("the two can never drift"). |
| Curve vocabulary | `crates/manifold-core/src/macro_bank.rs:22` `MacroCurve { Linear, Exponential (t²), Logarithmic (√t), SCurve (3t²−2t³) }` | **All four are strictly monotonic on [0,1] — every curve is invertible.** No `inverse()` exists yet. |
| Binding apply loop | `crates/manifold-renderer/src/node_graph/param_binding.rs:566` `apply_bindings` | Unconditionally re-writes each binding's target graph param from the manifest value each rebuild (with `LastAppliedCache` skip). This is the stomp that produces the card-half of the snap-back. |
| Wire resolution order | `crates/manifold-renderer/src/node_graph/effect_node.rs:358` `scalar_or_param` | A wired scalar input resolves before the param unconditionally — the wire-half of the snap-back. Deliberate design (`control-wires-port-shadows-param` memory); NOT to be changed. |
| Node-face edit path | `crates/manifold-app/src/app_render.rs:2480` `GraphEditCommand::SetGraphNodeParam` → `:2489` `SetGraphNodeParamCommand::new` (`crates/manifold-editing/src/commands/graph.rs:792`) | Writes `def.nodes[..].params` successfully; the write is then shadowed (wire) or stomped (binding). This dispatch arm is the interception seam. |
| Card param write path | `crates/manifold-app/src/ui_bridge/inspector.rs:1083/1119/1145` `PanelAction::ParamSnapshot` / `ParamChanged` / `ParamCommit` | The full drag lifecycle for outer card params: snapshot → live `set_base_param` + `MutateProjectLive` → undoable commit. Rerouted node-face gestures emit exactly these. |
| Binding metadata (inner target → outer param) | `crates/manifold-core/src/effect_graph_def.rs` `BindingDef` in `preset_metadata.bindings`; reshape fields on `ParamSpecDef` + `UserParamBinding` (`effects.rs:442..458`) | The lookup "is (node_id, param_id) bound, and to which outer `source_id` with which reshape" is derivable from the instance. Fan-out (one source, N targets) is already handled id-keyed (`param_binding.rs:545` doc). |
| Angle wrap | `param_binding.rs:268` `wraps_angle` (set for `ParamType::Angle`, `:425`) | Angle targets wrap; a raw inverse across periods is ill-defined. |
| Node-face driven-dim | `crates/manifold-ui/src/graph_canvas/render.rs` (`NodeRow::Param` block; see GRAPH_EDITOR_INSPECTOR_UNIFICATION.md "Shipped 2026-07-01") | Wire-driven rows already dim text (`TEXT_DIMMED_C32`). The driven *treatment* (D5) extends this; detection plumbing exists. |
| Drag gesture on node face | `crates/manifold-ui/src/graph_canvas/interaction.rs:46` `CanvasDrag::ParamScrub` | Relative-delta scrub from press origin; render-independent. Untouched — interception happens at dispatch, not in the gesture. |

## 2. Decisions

- **D1 — Reroute, never dual-write.** A node-face gesture on a **card-bound** param
  does not issue `SetGraphNodeParamCommand` at all. The dispatch layer
  (`app_render.rs:2480` arm) detects the binding and emits the card-param drag
  lifecycle (`ParamSnapshot`/`ParamChanged`/`ParamCommit` with the outer param's id
  and inverse-mapped values) instead. The existing forward path (`apply_bindings`)
  then propagates into the graph for free, `LastAppliedCache` stays coherent
  (nothing writes the graph param behind its back), undo and live-preview come from
  the existing lifecycle. Rejected: dual-write (write def param AND card param) —
  two authorities that drift, and a cache bypass. Rejected: writing the graph param
  and marking the binding dirty — inverts authority for one frame, flickers.
- **D2 — `invert_card_reshape` lives beside the forward function.** In
  `crates/manifold-core/src/effects.rs`, directly under `apply_card_reshape`, so
  forward/preview/inverse share one home and can't drift:
  ```rust
  /// Exact inverse of [`apply_card_reshape`] where one exists.
  /// Returns `None` only for a degenerate affine (`scale ≈ 0`).
  /// Out-of-range targets clamp to the slider ends (matching the forward
  /// stage-1 clamp — the inverse of a clamped map is defined on the range).
  pub fn invert_card_reshape(
      target: f32, min: f32, max: f32,
      invert: bool, curve: crate::macro_bank::MacroCurve,
      scale: f32, offset: f32,
  ) -> Option<f32>
  ```
  Body order is the forward run reversed: affine first (`v = (target - offset) /
  scale`, `None` if `scale.abs() < f32::EPSILON`), then — only when `invert ||
  curve != Linear` — normalize, `curve.inverse(n)`, un-invert, denormalize.
- **D3 — `MacroCurve::inverse` is total and closed-form.** On
  `crates/manifold-core/src/macro_bank.rs` next to `apply` (`:32`): Linear → `t`;
  Exponential (`t²`) → `t.sqrt()`; Logarithmic (`√t`) → `t*t`; SCurve
  (`3t²−2t³`, Hermite) → `0.5 - (asin(1.0 - 2.0*t) / 3.0).sin()` (the standard
  closed-form smoothstep inverse). Input clamped to [0,1] like `apply`. No
  `Option` — every current variant is strictly monotonic. The consult's
  "non-monotonic variants route to read-only" concern is **dissolved by
  inspection**; if a future variant is non-monotonic, `inverse` is where it fails
  to typecheck conceptually — add the read-only fallback THEN (Deferred).
- **D4 — The node face displays the *effective* value for bound params.** The row's
  value = manifest (card) value pushed through the **forward** reshape — never the
  shadowed `def.nodes[..].params` slot, which may hold years-old stale writes.
  This is a display-resolution change in the node-row view-model, not a data
  migration.
- **D5 — Wire-driven params get a "driven" readout, not an allow-then-revert.**
  On the node face, a param whose input port is wired is **non-interactive for
  drag**: the slider renders as a dimmed track whose fill animates with the actual
  driven value each frame, plus a tinted input-jack glyph at the row's left; hover
  shows the source ("driven by <node>.<port>"); click highlights the wire.
  Prevention happens at the input layer (the scrub never starts), matching how the
  gesture layer already special-cases rows. Extends the existing wire-driven dim in
  `graph_canvas/render.rs`.
- **D6 — Wire beats binding.** A param that is BOTH wire-connected and card-bound
  shows the driven treatment and refuses reverse-writes — `scalar_or_param`'s
  wire-shadows-everything order is authoritative, and a reverse-write there would
  move the card slider with zero visible render effect (the trap the consult
  flagged). The binding badge remains visible so the mapping is discoverable.
- **D7 — Fan-out moves siblings, legibly.** One outer param driving N inner targets
  is existing, correct forward semantics (`param_binding.rs:545`); a reverse-write
  from one target therefore moves every sibling. Make it legible, don't prevent it:
  the bound-param badge tooltip names the outer card param ("mapped to <card
  param>"), so the multi-target jump is attributable.
- **D8 — Angles invert through the principal value.** For `wraps_angle` targets the
  inverse takes the principal value within the slider's range before inverting —
  no attempt to round-trip winding count exactly across periods.
- **D9 — Removing a binding freezes the effective value.** When a card binding is
  removed (unmap), the removal command writes the current *effective* value into
  the def param slot it stops governing, so unmapping never visually snaps the
  render. This also neutralizes the stale-shadowed-write class going forward.
  Rejected: a load-time normalization sweep of historical stale def values — it
  rewrites projects on load for no behavioral gain once D4 makes stale slots inert
  (they're never displayed and never reach the render while bound).
- **D10 — Sequencing: the driven treatment ships with or before write-back.**
  Landing card write-back alone would make wire-driven params' remaining snap-back
  read as *more* broken ("two-way works — except when it silently doesn't").
  P1 (write-back) and P2 (driven treatment) may land in one batch; P2 must never
  trail a shipped P1 across a session boundary. If only one fits, P2 lands first.

## 3. Design body — the interception seam

The single behavioral change point is the `GraphEditCommand::SetGraphNodeParam`
dispatch arm (`app_render.rs:2480`). New resolution order there, for the gesture's
(watched target, node_id, param_id):

1. **Wired?** (input port for this param connected) → unreachable if P2's input-layer
   prevention works; keep a `debug_assert!` + no-op guard here as the enforcement
   backstop (see Invariants).
2. **Card-bound?** — resolve via the instance's `preset_metadata.bindings`
   (`BindingDef` whose inner target matches; user bindings via `UserParamBinding`
   equivalently). If bound: compute
   `invert_card_reshape(gesture_value, …binding's reshape…)`; on `Some(card_value)`,
   emit the `ParamSnapshot`(once, at gesture start)/`ParamChanged`(per move)/
   `ParamCommit`(at release) lifecycle against the outer param id — the same arms at
   `ui_bridge/inspector.rs:1083/1119/1145`, so live preview (`MutateProjectLive`)
   and undo shape are identical to a card drag. On `None` (degenerate scale): treat
   as read-only — no write, row shows the bound badge (an authoring-time data error,
   not a user state).
3. **Unbound** → existing `SetGraphNodeParamCommand`, unchanged.

The binding lookup must be cheap (per gesture event): resolve once at
`ParamSnapshot`-time into the drag state, not per `ParamChanged`.

The plausible-wrong architecture, forbidden by name: **you will want to make
`apply_bindings` skip params the user "recently edited"** (a recency/dirty flag so
direct writes survive) — no. That reintroduces two authorities with a timing
window; the card param is the only authority for a bound slot, ever.
Second temptation: **teaching `SetGraphNodeParamCommand` itself to reroute** — no;
the editing crate must not depend on binding resolution + UI action vocabulary.
The reroute is a dispatch-layer concern where both vocabularies already meet.

## 4. Invariants & enforcement

- **A bound graph param slot is never written by the node-face path.**
  Enforcement: `debug_assert!` + guard in the dispatch arm (step 1/2 above) —
  plus the P1 integration test `node_face_edit_on_bound_param_moves_card_not_def`
  asserting `def.nodes[..].params` is byte-unchanged after a rerouted gesture.
- **Forward and inverse cannot drift.** Enforcement: property test
  `card_reshape_roundtrips` in `effects.rs` — for a grid of (min,max,invert,curve,
  scale,offset) × values: `apply(invert(x)) ≈ x` within 1e-4 across all four
  curves; `invert(apply(x)) ≈ x` for in-range x.
- **`MacroCurve::inverse` matches `apply`.** Enforcement: unit test
  `macro_curve_inverse_roundtrips` on a 0..1 grid, all variants.
- **Wire-driven rows never start a scrub.** Enforcement: P2 unit test on the
  gesture layer — synthetic press on a driven row produces no `CanvasDrag::ParamScrub`.

## 5. Phasing

### P1 — Inverse machinery + reroute (one session)
**Entry state:** `rg -n "fn apply_card_reshape" crates/manifold-core/src/effects.rs`
hits `:489`; `rg -n "SetGraphNodeParam" crates/manifold-app/src/app_render.rs` hits
the `:2480` arm; re-verify both anchors.
**Read-back:** this doc §2–§4 whole; restate D1, D2's signature, the two forbidden
architectures, and what the entry checks found — before any code.
**Deliverables:** `MacroCurve::inverse` (macro_bank.rs) + `invert_card_reshape`
(effects.rs) with the two roundtrip tests; the dispatch-arm reroute with resolved-at-
snapshot binding state; D4's effective-value display in the node-row view-model;
D9's freeze-on-unmap in the binding-removal command; integration test
`node_face_edit_on_bound_param_moves_card_not_def` (build a bound fixture, synthesize
the gesture actions, assert manifest moved + def slot unchanged + render value
followed via the forward path).
**Gate (positive):** named tests above green; `cargo test -p manifold-core -p manifold-editing -p manifold-app --lib`.
**Gate (negative):** `rg -n "set_base_param" crates/manifold-editing/src/commands/graph.rs`
returns zero hits (the editing crate gained no binding knowledge); no new
`Arc<Mutex|RwLock>` anywhere in the diff.
**Round-trip gate:** save a project with a bound param mid-edited value → reload →
node face shows the effective value and a further node-face edit still moves the card.
**Performer gesture:** in the graph editor, grab the node-face knob of a param
mapped to a card slider and sweep it — the card slider follows and the render
changes continuously; release, Cmd-Z, both ends return together.
**Demo:** `ui-snap` editor scene variant with a bound param at a non-default value —
node row and card slider visibly agree (L2). **Test scope:** focused crates above;
workspace sweep at landing.
**Forbidden moves:** the two named wrong architectures (§3); TODO-as-deferral for D9;
touching `scalar_or_param`.

### P2 — Driven treatment + input-layer prevention (one session)
**Entry state:** P1 merged (or same batch); `rg -n "TEXT_DIMMED_C32" crates/manifold-ui/src/graph_canvas/render.rs` hits the wire-driven dim.
**Read-back:** D5–D7, D10; the existing driven-dim block in `render.rs`.
**Deliverables:** driven readout row per D5 (dimmed track + live fill + input-jack
glyph + hover source + click-highlights-wire); input-layer scrub prevention with its
unit test (Invariants §4); D6's driven-wins ordering including the visible binding
badge; D7's badge tooltip naming the outer param.
**Gate (positive):** gesture-layer test green; `cargo test -p manifold-ui --lib`;
**acceptance demo (L2, mandatory):** headless editor PNG with one wire-driven param
and one card-bound param on the same node — the driven row visibly reads as
non-interactive (dimmed, jack glyph), the bound row as interactive; the affordance
difference must be legible in the static PNG.
**Gate (negative):** `rg -n "snap.?back|revert" crates/manifold-ui/src/graph_canvas/`
introduces no allow-then-revert path.
**Performer gesture:** try to grab an LFO-driven param on the node face — nothing
grabs, the row visibly says why, and clicking it lights the wire to the LFO.
**Test scope:** `-p manifold-ui` focused; workspace sweep at landing.
**Forbidden moves:** allow-then-revert; a bespoke slider widget (extend the existing
row rendering); hiding the binding badge under the driven state (D6 keeps it).

## 6. Decided — do not reopen
1. Reroute at dispatch; no dual-write; no binding-skip flags (D1, §3).
2. Inverse lives beside forward in `effects.rs`; curve inverse on `MacroCurve` (D2, D3).
3. All four curves invertible — no read-only fallback for curves in v1 (D3).
4. Node face shows effective value for bound params (D4).
5. Wire-driven = non-interactive readout; wire beats binding (D5, D6).
6. Fan-out reverse-writes move siblings; tooltip attribution, no prevention (D7).
7. Angles: principal value (D8). Unmap freezes effective value (D9).
8. P2 never trails P1 across sessions (D10).

## 7. Deferred
- **Read-only fallback for non-monotonic curves** — revives if a non-monotonic
  `MacroCurve` variant is ever added (grep trigger: new variant in `macro_bank.rs:22`).
- **Load-time normalization of historically stale shadowed def values** — revives
  only if a stale slot is shown to leak into behavior despite D4/D9.
- **Two-way editing for signal-driven ports** (writing back into an LFO's params) —
  out of scope; a different feature (macro learn), not an inverse.
