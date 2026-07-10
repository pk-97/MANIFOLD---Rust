# Harness Fidelity Invariant — proposal

**Status: DRAFT — for Fable review.** 2026-07-10 · Opus (1M), from the UI_HARNESS_UNIFICATION P0–P3 wave · raised by Peter after two same-class defects surfaced in one session. If approved, folds into `UI_HARNESS_UNIFICATION_DESIGN.md` (harden D3) + `DESIGN_AUTHORING.md` (the lesson in §4).

## 1. The finding

UI_HARNESS_UNIFICATION shipped P0–P3 (the headless harness renders the real app through shared seams). During execution, **two independent "lookalike" defects surfaced from the same source** — a harness render path that *looks like* the live app but diverges:

- **The editor scene (P3).** The headless `editor` render built the sidebar + inspector as three separate scratch trees and issued three render calls; the live `present_graph_editor_window` builds one merged tree and issues one `render_tree_range`. Caught only because P3 went to build on it. Fixed by extracting `editor_frame.rs` (a shared seam the live app + harness both call).
- **The overlay pass (BUG-097).** The harness overlay pass called `render_tree_range` on each `overlay_draw` range; the live app calls `render_sub_region` on the same range, because the range deliberately excludes its region root (`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md`), so `render_tree_range` renders *nothing*. Structural — it hits every open overlay (dropdowns, popovers, modals, perf HUD, toasts), i.e. the entire open-overlay interaction surface. Nearly shipped mislabeled "LOW, tracked"; Peter pushed and it was re-graded.

Two divergences in one session, both in the same category, is not bad luck. It's a design seam that was left open.

## 2. Precise diagnosis (grounded, not inferred)

**D3 of the design states the principle:** drift becomes impossible *"by construction, not by discipline"* — the harness calls the app's real render code, so nothing can diverge. **P1 and P3 applied this to the composite/cached path** (`composite_main_ui_frame`, `composite_editor_frame`) — the app and harness call the identical function.

**But the principle was not applied to the immediate-mode pass *assembly*.** Verified against current code:

- The individual draws are mostly fine: `draw_immediate_passes` (`ui_snapshot/render.rs:142`) calls the **shared** primitives the live app also calls — `emit_clips`/`emit_clip_names`/`ClipBody` (`manifold_renderer::clip_draw`), `emit_automation_lanes` (`manifold_renderer::automation_lane_draw`). Those are not reimplemented; they share the real draw code, which is why clips and lanes "match."
- The drift lives **one level up, in the parallel *assembly*** — *which* passes run, in *what* order, with *which* per-pass call. `draw_immediate_passes` is a harness-side re-sequencing of the live app's `app_render` pass order, and the overlay pass within it chose `render_tree_range` where the live app's assembly chose `render_sub_region`. The pre-P3 editor path was a parallel assembly too (three trees vs one).
- This parallel assembly was kept honest **only by a `⚠ VERIFY-AT-IMPL` "does it match the live app" audit** (the P2 brief's instruction: "a pass that doesn't match is an escalation"). That is "by discipline" — a recurring, per-pass, human/agent audit — which is exactly what D3 rejects. The audit *did* catch both cases, but as an escalation late in a phase, and it nearly under-graded the overlay one. An audit is a net that decays with attention; a seam is a net that holds by construction.

So the flaw is **parallel render-pass assembly in the harness**, gated by a decaying audit, contradicting the design's own D3.

## 3. Proposed invariant

> **The harness contains no parallel render-pass assembly. The full main-window pass sequence — composite + all immediate-mode passes (clips, names, lanes, overlays) — is a single shared seam the live app also calls. The harness calls it; it never re-sequences passes or re-chooses per-pass render calls.**

Consequences:
- `VERIFY-AT-IMPL: does this pass match the live app?` becomes **unnecessary by construction** for render passes — there is no parallel copy to match. (It remains valid for genuinely different concerns, e.g. data substitutions below.)
- The seam is the *only* place pass order and per-pass call choice (`render_sub_region` vs `render_tree_range`, depth pushes, shadows) are decided — for both the live app and every harness caller.

**Explicitly allowed exception — labeled *data* substitution, never *render-code* substitution.** The harness may feed different *inputs* where the real input is unavailable headless (e.g. thumbnails: `thumbs::make_test_atlas` stands in for the content-thread atlas, already opt-in via `with_thumbs` and labeled). That is a data seam, not a render-path fork — the render code is still shared. The invariant forbids parallel render *code*, not clearly-labeled test *inputs*.

## 4. Fix path (incremental, pattern already proven)

P1/P3 are the template. Remaining work to satisfy the invariant:
1. **Overlay pass** — in flight (BUG-097): align to `render_sub_region` + `Depth::OVERLAY`. This is a *point* alignment; the *structural* fix is step 2.
2. **Fold the immediate-pass assembly into the shared seam.** Extend `composite_main_ui_frame` (or a sibling `render_main_ui_passes`) to own the full sequence — dirty-panel composite, then clips/names/lanes, then the overlay region at `Depth::OVERLAY` — so `app_render.rs`, `render_ui_to_png`, and the script Runner all call it and no harness code re-sequences passes. `draw_immediate_passes` and the harness overlay pass are deleted, not kept-and-verified.
3. Editor (P3) already satisfies the invariant via `editor_frame.rs`.

Scope note (per `dont-cascade-redesign`): this is bounded — one seam extraction extending the P1 precedent, not a rewrite. It removes the audit step permanently.

## 5. DESIGN_AUTHORING lesson (proposed §4 addition)

**"Reimplement-and-verify" is a drift carve-out.** When a design's entire value is fidelity-*by-construction* (the harness shows the real app; the exporter writes the real bytes; the preview is the real render), any step that says *"reimplement this part and verify it matches"* reintroduces the drift the design exists to kill — just at a smaller granularity, where it hides longer. A `VERIFY-AT-IMPL: does X match the live path?` gate is a **smell that a seam is missing**: the fix is to share the code so there is nothing to verify, not to verify harder. The catch that works is a seam; the catch that decays is an audit. Specimen: UI_HARNESS_UNIFICATION kept the immediate-pass assembly as a parallel harness copy behind a match-audit, and produced two lookalikes (editor, overlays) in a single execution session — the exact "0-diff lie" class the wave was built to eliminate.

## 6. Open questions for Fable

- Is the invariant worded to allow the thumbnail-style *data* substitution cleanly, or does "no parallel render-pass assembly" need a sharper boundary between *input seams* (allowed) and *code seams* (forbidden)?
- Does step-2's single-seam-owns-all-passes conflict with the winit-side split the design deliberately kept (fast path, drawable acquire/present stay in `present_all_windows`)? My read: no — the seam ends at the offscreen, unchanged from P1's boundary; only the immediate passes move into it. Worth a second set of eyes.
- Should the DESIGN_AUTHORING lesson generalize beyond render harnesses to any fidelity-by-construction design (export, preview, freeze/fusion parity)? The pattern seems general.
