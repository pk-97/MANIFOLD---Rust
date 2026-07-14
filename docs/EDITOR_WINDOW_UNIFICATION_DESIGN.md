# Editor Window Unification — rendering is a property of the tree, never of the window

**Status:** APPROVED design · P1 LANDED 2026-07-14 (BUG-151 FIXED) · P2–P3 not built · Fable 5 (with Peter in the room) · Sonnet-executable
**Prerequisites:** the popup professional pass and the BUG-150 fix (both LANDED on main by `e310c592`, verified 2026-07-14). BUG-151 was fixed by P1 of this design (see docs/BUG_BACKLOG.md) — the standalone BUG-151 hunt prompt (prompt 3 of the `popup-professional-pass-prompt` memory) is SUPERSEDED; do not run it.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter's mandate, verbatim (2026-07-14): make "these types of bugs and bug classes outright impossible by design and architecture", and — on scope — "This is everything? I don't want to have to do another unification session on the editor page." The bug class in question: BUG-151, where the graph editor's node browser renders its cells but not its popup container, because the editor window's compositor never got an overlay pass. The class is *a cross-cutting UI mechanism that exists in the main window's frame composition but must be re-plumbed by hand in every other window; forgetting one is a silent, partial failure.*

**The governing insight: every cross-cutting UI mechanism must be a function of the `UIRoot` tree, applied identically by every window that hosts one; windows differ only in their INPUTS (which tree, which immediate-mode content layers, which offscreen), never in traversal or pass logic.** Today the main window composes overlays through a dedicated region-aware pass ([ui_frame.rs:667–704](../crates/manifold-app/src/ui_frame.rs)) while the editor draws its whole tree in one flat root-scan ([editor_frame.rs:264](../crates/manifold-app/src/editor_frame.rs)) with no overlay handling at all. Both windows own a full `UIRoot`, but **only the main window ever calls `UIRoot::build()`** ([ui_root.rs:994/1008 → build_overlays:1085](../crates/manifold-app/src/ui_root.rs)) — the editor's `Workspace::ui_root` is built via plain `UIRoot::new()` and its tree is assembled by hand each frame, so it never records overlays at all. That gap — not a traversal quirk in the flat scan — is BUG-151's actual root cause (found at P1 impl 2026-07-14, corrected here from this section's original false claim that the editor's `build()` populated `overlay_draw` "exactly like the main window's"; see the P1 fix-shape spec's `build_overlays_for_screen` wrapper, D2). This design extracts ONE shared tree-pass function both windows call, and adds the machine check that makes a future fork fail at commit time.

Peter's acceptance bar, stated plainly: **after this ships, a new overlay or tree-borne mechanism added to the main window either works in every UIRoot-hosting window automatically, or the meta-test goes red at commit time — there is no third outcome.**

The stage translation: the graph editor is the authoring surface for every effect and generator in the show. A popup that renders broken there isn't cosmetic — it makes the node browser unreadable mid-session, and the same class would silently break any future overlay (toasts, pickers, tooltips) in any future window (monitor, perform surfaces) the day it gains a tree.

This design deliberately does NOT re-litigate UI_HARNESS_UNIFICATION D5 ("unify the scaffolding, not the render paths — the graph editor is cacheless"). D5 protects the *base-content* render: the main window's atlas cache and the editor's cacheless immediate-mode canvas stay distinct, per-window. What unifies here is the *tree-overlay* pass — a layer D5 never spoke to, sitting above base content in both windows.

Companion docs: [UI_HARNESS_UNIFICATION_DESIGN.md](UI_HARNESS_UNIFICATION_DESIGN.md) (the seam-extraction precedent — P1/P3 of that wave produced `ui_frame.rs`/`editor_frame.rs`, which this design edits; its D3-hardening "no parallel pass assembly, input presence never caller identity" is the invariant this design extends across windows) · [UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md](UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md) (the region machinery `build_overlays` uses) · [GRAPH_EDITOR_REDESIGN.md](GRAPH_EDITOR_REDESIGN.md) (owns editor UX; untouched here).

---

## 1. Audit — the seam census (verified 2026-07-14 at `e310c592`)

**This table is the deliverable Peter asked for: every mechanism in the main window's frame composition and event loop, with how the editor gets it today and how it is unified.** Rows were derived from reading `ui_frame.rs` (whole), `editor_frame.rs` (whole), `UIRoot::build_overlays`/`process_events`, `workspace.rs`, and `window_input.rs`'s routing layer — not from memory. Extend, don't redesign.

| # | Mechanism | Main window today | Editor window today | Unified by | Regression guard |
|---|---|---|---|---|---|
| 1 | Base tree render | Atlas cache: `composite_main_ui_frame` ([ui_frame.rs:250](../crates/manifold-app/src/ui_frame.rs)) | Flat root-scan `render_tree_range(0, usize::MAX)` ([editor_frame.rs:264](../crates/manifold-app/src/editor_frame.rs)) | **Stays per-window (D5).** Editor's call NARROWS to `[0, overlay_region_start)` (D2) | I1 meta-test forbids raw calls outside the compositor modules |
| 2 | Overlay pass: `overlay_draw` ranges, per-overlay `Depth::OVERLAY.above(i)`, scrim-skip shadow hook, region-aware `render_sub_region` | [ui_frame.rs:667–704](../crates/manifold-app/src/ui_frame.rs) | **MISSING — this is BUG-151** | Shared `render_tree_overlay_passes` (D1) | I1 meta-test + BUG-097 GREEN test + P1 editor PNG |
| 3 | Card-drag ghost at `Depth::TOOLTIP` | [ui_frame.rs:709–711](../crates/manifold-app/src/ui_frame.rs) | Missing (harmless today: editor inspector card drag never sets it — unverified) | Shared pass; input presence — `card_drag_first_node()` returns `None` when absent | Covered by I1 |
| 4 | Text-input overlay | Renders whenever `text_input.active` ([ui_frame.rs:713](../crates/manifold-app/src/ui_frame.rs)) — **including graph fields it does not own; latent double-render** | Renders only `is_graph_field()` ([editor_frame.rs:294](../crates/manifold-app/src/editor_frame.rs)) | Shared pass takes `Option<&TextInputState>`; each caller passes `Some` only for fields it owns (D4) | I2 negative rg (no caller-identity flags in the seam) |
| 5 | Mapping popover | n/a | Immediate-mode draw at `Depth::POPOVER` ([editor_frame.rs:286–291](../crates/manifold-app/src/editor_frame.rs)) | **Exempt (D5-class):** immediate-mode per-window content, like the canvas. Depth constants order it above OVERLAY globally (`Depth`: BASE 0 / CONTENT 100 / OVERLAY 200 / POPOVER 300 / TOOLTIP 400, [ui_renderer.rs:178–190](../crates/manifold-renderer/src/ui_renderer.rs)) | Deferred row (revive trigger listed) |
| 6 | Toasts, perf HUD, dropdown, settings popup, pickers, browser popup | Tree overlays recorded by `build_overlays` → drawn by row 2 | **Never recorded** — `build_overlays` (and thus `overlay_draw`) was never called for the editor's `UIRoot` at all (corrected 2026-07-14; originally misstated here as "recorded identically, never drawn") — nor drawn | Row 2 + the `build_overlays_for_screen` wrapper (P1 fix-shape spec) that gives the editor a call path into `build_overlays` without going through main-window-only `UIRoot::build()` | Covered by I1; P2 demo opens the perf HUD in the editor scene |
| 7 | Browser-popup thumbnail registration | Inline in `render_main_ui_passes` ([ui_frame.rs:646–657](../crates/manifold-app/src/ui_frame.rs)) | Missing (editor node browser has no PNG thumbnails today — Node mode; unverified whether preset mode reaches it) | Moves into the shared pass (it reads `ui_root.browser_popup`, needs `device`) | Covered by I1 |
| 8 | Timeline content passes (grid, clip bodies, waveforms, thumbs, lanes, playhead, scrollbar, VQT) | `render_main_ui_passes` passes 4a–5 | n/a | **Exempt (D5-class):** main-window *content*, exactly like the editor's canvas | — |
| 9 | Canvas immediate draws, dock dividers, mini-timeline | n/a | [editor_frame.rs:260–282](../crates/manifold-app/src/editor_frame.rs) | **Exempt (D5-class):** editor content | — |
| 10 | Overlay-region dirty-clear | Tail of `render_main_ui_passes` ([ui_frame.rs:889–891](../crates/manifold-app/src/ui_frame.rs)) | Missing (flat render + editor's own `offscreen_dirty` flow masks it) | Moves into the shared pass (it is overlay bookkeeping) | Covered by I1 |
| 11 | Input translation (cursor/mouse/wheel per window) | `primary_*` fns | `editor_*` fns ([window_input.rs:226–292](../crates/manifold-app/src/window_input.rs) routing) | **Exempt with rule:** window_input *translates* (coordinates, canvas gestures), `UIRoot::process_events` *decides* — and process_events is already the same code for both windows | Enforcement: none — honest cost; see Deferred |
| 12 | Overlay input lifecycle (click-outside dismiss, Escape, keyboard nav) | Inside `UIRoot::process_events` — shared type, shared code | Same code, editor's own `UIRoot` instance | Already unified by construction | Existing `manifold-ui` tests |
| 13 | Redraw scheduling | Dirty-driven + animation keepalives on the main tick | `ed.offscreen_dirty = true` scattered at input sites ([window_input.rs](../crates/manifold-app/src/window_input.rs), 8+ sites) | One aggregate `UIRoot::overlay_redraw_needed()` both windows OR into their dirty flag (D6) | P2 deliverable names the fn + its test |
| 14 | Popup shadows | `SHADOWS_ENABLED` gate inside row 2's loop ([ui_frame.rs:673](../crates/manifold-app/src/ui_frame.rs)) | n/a (never had the pass) | Rides row 2 — one gate, all windows | Covered by I1 |
| 15 | Scissor/clip regions, depth-sorted batching | `UIRenderer` internals (per-command clip+depth capture, BUG-060 fix) | Same renderer | Already unified | Existing renderer tests |

**Classification:** rows 2/3/4/7/10 are *one extraction away from existing* — the code exists once, in `ui_frame.rs`, and moves verbatim. Row 13 is *genuinely new but small* (one aggregate predicate). Rows 1/5/8/9/11 are *decided exemptions*, not gaps. Nothing else is genuinely new. The design is mostly moving one block of code into a module both windows call — the audit shrank it, as it should.

**Traversal semantics (load-bearing, verified):** `build_overlays` records each overlay's `(start, end)` with the region root at `start - 1`, deliberately OUTSIDE the range ([ui_root.rs:1085–1135](../crates/manifold-app/src/ui_root.rs)); `render_tree_range` is a ROOT scan that renders NOTHING for such a range, `render_sub_region` is the ancestor-aware flat scan that draws it — the permanent RED→GREEN proof is `overlay_fidelity_proof::bug097_…` ([ui_snapshot/mod.rs:1703–1834](../crates/manifold-app/src/ui_snapshot/mod.rs)). The editor's `0..usize::MAX` root scan DOES include region roots, so what exactly its flat pass draws for an open popup (Peter's screenshot shows cells without container fill, inspector bleeding through) is **confirmed by observation in P1, not assumed** — the fix does not depend on that last inch, because the editor stops rendering the overlay region through the root scan entirely.

## 2. Decisions

**D1 — One shared tree-overlay pass, in a new module `crates/manifold-app/src/tree_passes.rs`, called by both windows.** It owns, verbatim-moved from `ui_frame.rs`: the `overlay_draw` loop (per-overlay depth stacking, scrim-skip shadow hook, `render_sub_region`), the TOOLTIP tier (card-drag ghost + text-input overlay), the browser-popup thumbnail registration, and the overlay-region dirty-clear. Precedent: the `ui_frame.rs`/`editor_frame.rs` seam extractions themselves (move, don't rewrite; module doc records deviations). Rationale: the mechanism exists once today and BUG-151 is what "twice, by hand" looks like. **Rejected: adding a copy of the overlay loop to `composite_editor_frame`** — that is the point-fix; it fixes BUG-151 and re-creates the class the same day (the next mechanism forks again). **Rejected: routing the editor's whole frame through `render_main_ui_passes`** — passes 4a–5 are main-window timeline *content*; forcing the editor through them violates UI_HARNESS_UNIFICATION D5 and buries per-window behavior, the exact false abstraction that design refused.

**D2 — The editor's base render narrows to `[0, overlay_region_start)`; overlay nodes render ONLY through the shared pass.** This is the BUG-151 fix: overlay nodes stop being swept up by the flat root scan at CONTENT depth and start rendering region-aware at OVERLAY depth, identically to the main window. **Precondition (found at P1 impl, fix-shape spec 2026-07-14):** this narrowing is only meaningful once `overlay_region_start` is real for the editor — it requires `UIRoot::build_overlays_for_screen(w, h)`, a `pub(crate)` wrapper that sets `screen_width`/`screen_height` (the editor's `UIRoot` never receives `resize()`) and then calls the existing `build_overlays()`, called by the editor's per-frame render in place of `UIRoot::build()` (which the editor must never call — it lays out the whole main-window panel set). Without this precondition, `overlay_region_start` stays at its `UIRoot::new()` default and D2's narrowing would blank the editor's entire tree — the gap the design's original §1 audit missed. Consequences, stated honestly: any editor pixel that today *accidentally* depended on overlay nodes rendering in the flat pass will change appearance; the P1 PNG review is the catch, and a divergence there is an escalation, not a tweak-until-it-looks-right.

**D3 — UI_HARNESS_UNIFICATION D5 stands; the exemption table is closed.** Per-window content stays per-window: main's atlas cache + timeline passes; the editor's canvas, dock dividers, mini-timeline, mapping popover. These are inputs *under* the tree passes, not forks *of* them. A future window (monitor, perform surface) that gains a `UIRoot` adopts the shared pass for its tree and keeps its own content layers. **Rejected: unifying the base-content render too** — re-litigating a decision Peter already approved 2026-07-10, and the windows genuinely do not share that path.

**D4 — Text-input overlay unifies on input presence: the shared pass takes `Option<&TextInputState>`, and each caller passes `Some` only for fields it owns.** Formalize the ownership predicate on `TextInputState` (the existing `field.is_graph_field()` becomes the editor's ownership test; the main window passes `Some` only when the active field is NOT a graph field — fixing the latent double-render at [ui_frame.rs:713](../crates/manifold-app/src/ui_frame.rs), which today draws graph-field text sessions into the main window too). This follows the HARNESS_FIDELITY caller test: a pass may skip on absent *input*, never on caller *identity* — `tree_passes.rs` must never see a `WorkspaceKind`. **Rejected: a `window: WorkspaceKind` parameter on the shared pass** — that is the caller-identity fork reborn inside the seam, forbidden by the same invariant that governs the harness.

**D5(doc) — The mapping popover stays immediate-mode, exempt from `overlay_draw`.** It draws via `popover.render(ui_renderer)` at `Depth::POPOVER`, not from tree nodes, so it is content, not a tree overlay; depth constants (verified: OVERLAY 200 < POPOVER 300 < TOOLTIP 400) order it above popups globally regardless of enqueue order. Revive trigger (Deferred): a second immediate-mode floating panel appears, or the popover needs click-outside dismissal from the overlay lifecycle.

**D6 — Redraw keepalive becomes one aggregate: `UIRoot::overlay_redraw_needed(&self) -> bool`, OR-ed into each window's `offscreen_dirty`.** Membership is re-derived at P2 (the popup enter animations were deleted by the popup pass, so the survivor set is small — toast timers and any remaining overlay tween). Rationale: today each animation source hand-wires its own keepalive into the main tick and the editor's scattered `offscreen_dirty = true` sites; a missed wire is a frozen animation in exactly one window — the input-side sibling of BUG-151.

**D7 — The structural guard is a workspace meta-test: outside the compositor allowlist, `render_tree_range`/`render_sub_region` call sites in `manifold-app` are a test failure.** Allowlist: `tree_passes.rs` (the shared pass), `ui_frame.rs`/`editor_frame.rs` base-content calls (rows 1/8/9), and the BUG-097 proof module (`ui_snapshot/mod.rs`, which needs raw calls to prove the traversal semantics). Inventory at design time (re-derive at P3): `ui_frame.rs:702,710` (move into `tree_passes.rs`), `editor_frame.rs:264` (rewritten by D2), `ui_snapshot/mod.rs:1822,1824` (allowlisted proof), `ui_cache_manager.rs:239,271` + `ui_renderer.rs:2337` (other crate — internal cache renders and a renderer unit test; out of the guard's scope by crate). Precedent: the docs-index freshness meta-test (a repo-discipline test in the default sweep). Rationale: prose rules don't stop a hurried future window author; a red test does. This is the "no third outcome" mechanism in Peter's acceptance bar.

## 3. The committed seam

```rust
// crates/manifold-app/src/tree_passes.rs  (new module; App-internal, pub(crate),
// no new crate boundary, no new dependency, no thread-residency change)

/// Caller-resolved inputs. Every Option is input PRESENCE (HARNESS_FIDELITY
/// caller test) — never caller identity. `text_input` is `Some` only when the
/// active field belongs to this window (D4).
pub(crate) struct TreeOverlayInputs<'a> {
    pub text_input: Option<&'a crate::text_input::TextInputState>,
    pub frame_timer: &'a crate::frame_timer::FrameTimer,
}

/// The single owner of the tree-overlay pass sequence for EVERY UIRoot-hosting
/// window: overlay_draw loop (per-overlay Depth::OVERLAY.above(i), scrim-skip
/// shadow hook, render_sub_region) → TOOLTIP tier (card-drag ghost, text-input
/// overlay) → browser-popup thumbnail registration → overlay-region dirty-clear.
/// Enqueue-only into `ui_renderer` (the caller owns begin_frame, prepare/render
/// flush, and the encoder) EXCEPT thumbnail registration, which needs `device`.
/// Precedent: ui_frame.rs:646-716 + :889-891, moved verbatim.
pub(crate) fn render_tree_overlay_passes(
    device: &GpuDevice,
    ui_renderer: &mut UIRenderer,
    ui_root: &mut UIRoot,
    logical_w: u32,
    logical_h: u32,
    inputs: TreeOverlayInputs<'_>,
);
```

Caller order, committed. **Main** (`render_main_ui_passes`): passes 4a–5's content draws unchanged → `render_tree_overlay_passes` (replacing the inline block) → VQT waterfall → flush/commit as today. **Editor** (`composite_editor_frame`): canvas → base tree `[0, ui_root.overlay_region_start)` → dock → mini-timeline → `render_tree_overlay_passes` → mapping popover (POPOVER depth) → single prepare/render flush as today. Depth sorting, not enqueue order, governs stacking across these (verified constants) — so the popover drawn after the overlay pass still layers correctly above OVERLAY and below TOOLTIP.

`⚠ VERIFY-AT-IMPL (P1):` the main window's flush currently happens at [ui_frame.rs:719–721](../crates/manifold-app/src/ui_frame.rs) *before* the VQT waterfall, and the dirty-clear at the very end; when moving the block, keep the flush at the caller and the dirty-clear inside the shared pass, and confirm by the byte-identical gate that main-window pixels are unchanged. If the dirty-clear's position relative to the VQT pass turns out to be order-sensitive, escalate — do not reorder silently.

## 4. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| I1 — Tree overlays render only through `render_tree_overlay_passes`; no window forks traversal | New meta-test `tree_render_call_sites_are_allowlisted` (D7), in the default `nextest` sweep — P3 deliverable |
| I2 — The shared pass branches on input presence, never caller identity | Negative gate in the same meta-test: `rg "WorkspaceKind|is_graph_editor|is_primary"` over `tree_passes.rs` = zero hits |
| I3 — Overlay ranges are root-excluding and need the flat scan | Existing `bug097_render_sub_region_draws_root_excluding_overlay_that_render_tree_range_blanks` (stays GREEN, untouched) |
| I4 — Main-window pixels are unchanged by the extraction | P1 gate: byte-identical offscreen readback before/after, per the UI_HARNESS_UNIFICATION P1 precedent |
| I5 — The editor renders overlays correctly | P1 acceptance demo: headless editor PNG with the node browser open, container opaque, reviewed by the orchestrator |

## 5. Phasing

### P1 — Extract the shared pass; both windows consume it; BUG-151 fixed

- **Entry state:** `git log --oneline -1` in the worktree matches `origin/main`; BUG-151's backlog Status is still OPEN (if it reads FIXED, someone ran the superseded standalone prompt — STOP, read what landed, and re-scope this phase to replace that point-fix with the shared pass before anything else). Re-verify anchors: `rg -n "render_tree_range\(&ui_root.tree, 0, usize::MAX\)" crates/manifold-app/src/editor_frame.rs` (one hit, ~:264) and `rg -n "overlay_draw.iter\(\)" crates/manifold-app/src/ui_frame.rs` (one hit, ~:667).
- **Read-back:** this doc §1–§4 whole; `ui_frame.rs:640–892`; `editor_frame.rs` whole; `ui_snapshot/mod.rs:1703–1834` (the traversal-semantics proof). Restate: D1/D2/D4, the forbidden moves, and what the entry-state checks found.
- **Deliverables:** `crates/manifold-app/src/tree_passes.rs` with the §3 signature (module doc records any forced deviations, per the `ui_frame.rs` precedent); `render_main_ui_passes` calls it (inline block deleted); `composite_editor_frame` rewritten to the §3 caller order; the D4 ownership predicate on `TextInputState` with both callers using it; BUG-151's backlog entry marked FIXED with the commit; a docs-index regen if any doc was added.
- **Gate (positive):** `bug097_…` test green; main-window byte-identical readback before/after the extraction (drive via `cache_path_full_render`, the harness renders through the same seams); the editor headless scene (`ui-snap editor` / `editor_window_harness`) renders with the node browser open and the PNG shows an opaque container + scrim over the graph — **the orchestrator reads the PNG; this is also the observation that closes the audit's "last inch"** (record in the phase report what the flat pass was actually doing wrong). **Gate (negative):** `rg "overlay_draw.iter" crates/manifold-app/src/ui_frame.rs` = zero hits (moved, not copied); `rg "render_tree_range\(&ui_root.tree, 0, usize::MAX\)" crates/manifold-app/src/editor_frame.rs` = zero hits; `rg "WorkspaceKind|is_graph_editor" crates/manifold-app/src/tree_passes.rs` = zero hits.
- **Acceptance demo:** the editor PNG with the open node browser (before/after pair), **L2** (L3 if the flow driver can open the editor's node picker — check `scripts/ui-flows/` and `HEADLESS_UI_HARNESS.md` at impl; if it can, write the flow).
- **Performer-gesture line:** open the node browser in the graph editor mid-session and read it — the browser is legible over any graph.
- **Forbidden moves:** copying the overlay loop into `editor_frame.rs` instead of extracting (the point-fix D1 rejects) · a `WorkspaceKind`/`is_editor` parameter anywhere in `tree_passes.rs` (D4's named temptation — you will want it for the text-input gate; the answer is `Option` at the caller) · touching the canvas render, dock, or mini-timeline (D3 exemptions) · "fixing" any main-window pixel difference by tweaking the moved code (byte-identical or escalate) · reordering the flush/dirty-clear without the §3 VERIFY-AT-IMPL escalation.
- **Test scope:** `cargo nextest run -p manifold-app --lib` focused + the `ui-snapshot`-feature harness runs named above; scoped clippy `-p manifold-app`. Full workspace sweep at landing in the main checkout.

### P2 — Policy rows: redraw keepalive aggregate + census long-tail proof

- **Entry state:** P1 landed (its negative gates hold on `origin/main`).
- **Read-back:** this doc rows 4/6/13; the phase report from P1.
- **Deliverables:** `UIRoot::overlay_redraw_needed()` (D6) with membership derived by `rg "is_animating|tick\(" crates/manifold-ui/src/panels/` at impl, both windows OR-ing it into their dirty flag, plus a unit test per member (animating overlay ⇒ predicate true); the editor's scattered overlay-related `offscreen_dirty` sites reduced to the aggregate where they were overlay-driven (input-driven sites stay); a P2 demo PNG: the perf HUD toggled open in the **editor** scene, rendering through the shared pass (proves row 6's "whatever overlay_draw holds" claim on a second overlay type).
- **Gate (positive):** the named unit tests; the perf-HUD-in-editor PNG read by the orchestrator. **Gate (negative):** `rg "offscreen_dirty = true" crates/manifold-app/src/window_input.rs` count is ≤ the P1 count (no new scattered sites).
- **Acceptance demo:** the perf-HUD-in-editor PNG, **L2**.
- **Forbidden moves:** widening into input-routing unification (Deferred, explicitly) · per-window keepalive lists (the aggregate is the point).
- **Test scope:** `-p manifold-app -p manifold-ui --lib` focused; workspace sweep at landing.

### P3 — The structural guard + supersession sweep

- **Entry state:** P1–P2 landed.
- **Read-back:** D7; the docs-index freshness test as the meta-test precedent (find it via `rg -l "docs_index" crates/` at impl).
- **Deliverables:** the `tree_render_call_sites_are_allowlisted` meta-test (D7's allowlist, I2's negative gate folded in), in the default sweep; re-derived call-site inventory in the phase report (if it differs from D7's baked list, list the new sites and classify before touching anything); the supersession sweep — this doc's Status → SHIPPED, BUG-151 Status confirmed FIXED, the `popup-professional-pass-prompt` memory's prompt 3 tombstoned, `rg "BUG-151|editor.*overlay" docs/` for stale claims; landing report per DESIGN_DOC_STANDARD §8.10.
- **Gate (positive):** the meta-test passes on the landed tree AND fails when a raw `render_tree_range` call is temporarily added to `editor_frame.rs` (prove the guard fires — a guard never seen red is a hope, not a gate; revert the probe). **Gate (negative):** the allowlist in the test names files, not line numbers (line drift must not rot it).
- **Acceptance demo:** the meta-test's red-then-green probe output, **L1** (this phase's surface is the test itself).
- **Forbidden moves:** an allowlist so broad the test can't fail (e.g. allowlisting all of `manifold-app`) · skipping the red-probe proof.
- **Test scope:** default `nextest` sweep (the meta-test must live there); workspace sweep at landing.

**Phasing-completeness check (run at authoring, 2026-07-14):** every census row lands in a phase or an exemption — rows 2/3/7/10/14 in P1, rows 4 (D4) in P1, 13 (D6) + 6-proof in P2, the guard in P3; rows 1/5/8/9/11/12/15 are decided exemptions or already-unified, recorded above with triggers where applicable. No affordance in the body is absent from the phase list.

## 6. Decided — do not reopen

1. Rendering mechanics are tree properties; windows supply inputs only. The shared pass is `tree_passes.rs::render_tree_overlay_passes`. (D1)
2. The editor's base render is `[0, overlay_region_start)`; overlays render only via the shared pass. (D2)
3. UI_HARNESS_UNIFICATION D5 stands: base content (atlas cache / canvas / timeline passes / dock / mini-timeline) is per-window forever. (D3)
4. No caller-identity parameter ever enters `tree_passes.rs`; variation is input presence (`Option`) resolved by the caller. (D4)
5. The mapping popover is immediate-mode content, exempt, ordered by `Depth::POPOVER`. (D5)
6. Redraw keepalive is one aggregate predicate on `UIRoot`, not per-window wiring. (D6)
7. The guard is a default-sweep meta-test with a files-not-lines allowlist, proven able to fail. (D7)
8. The standalone BUG-151 prompt (prompt 3, `popup-professional-pass-prompt` memory) is superseded by P1.

## 7. Deferred

- **Input-translation unification** (`window_input.rs`'s `primary_*`/`editor_*` fns; census row 11) — the translation layer is genuinely per-window (canvas gestures, cursor sources), and `UIRoot::process_events` already unifies the decision layer. Enforcement of "translate, never decide" is prose today — an honest cost. Revive: the first input-class cross-window bug (an input behavior that works in main and silently doesn't in the editor), which would make this the second BUG-151 and justify its own design.
- **Monitor / output window adoption** — the Output window has no `UIRoot` (blit-only, `workspace.rs` module doc); the day any new window gains a tree, it adopts `render_tree_overlay_passes` and the meta-test already covers it (the allowlist doesn't grow). Revive: MULTI_DISPLAY / perform-surface work that adds a UIRoot-hosting window.
- **Folding the mapping popover into tree overlays** — revive per D5's trigger (a second immediate-mode floating panel, or popover needs the overlay input lifecycle).
- **Editor atlas caching** — deliberately out of scope forever unless editor render cost is measured as a frame-budget problem on the rig; D3/D5 territory.
