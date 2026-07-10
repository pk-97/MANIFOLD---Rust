# Harness Fidelity Invariant

**Status: APPROVED — 2026-07-10.** Opus (1M) authored, from the UI_HARNESS_UNIFICATION P0–P3 wave; raised by Peter after two same-class defects surfaced in one session. Fable reviewed and returned **ADOPT the invariant and build §4 step 2**, with three amendments — now folded in below (§3's caller test, §4 step 2's seam scope, §5's essential-duplication carve-out) — and the §6 open questions resolved. Folds into `UI_HARNESS_UNIFICATION_DESIGN.md` (harden D3) + `DESIGN_AUTHORING.md` (the lesson in §4). **BUG-097 is closed by construction as part of §4 step 2 — never as a point fix** (see §4).

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

**The boundary is a caller test, not a data/code taxonomy (amendment 1, Fable).** The earlier "data seam vs code seam" phrasing invited hair-splitting; the sharp rule is: **the shared seam may branch on INPUT PRESENCE, never on CALLER IDENTITY.**

- *Allowed — branch on input presence.* A pass that has no input to draw skips itself: no thumbnail atlas → skip the thumbnail blit; `video: None` → skip the video-band blit; no spectrogram columns → skip the VQT waterfall. The condition is a property of the frame's data, and **the live app takes the identical branch when its own input is absent** (an idle app with no open modal skips the overlay pass too). Fake inputs are fine — `thumbs::make_test_atlas` standing in for the content-thread atlas is a labeled test *input*, and the render code that consumes it is still shared.
- *Forbidden — branch on caller identity.* No `is_harness` flag, no `if headless`, no parameter that only a harness caller ever sets to a distinguishing value. A harness-only branch is the parallel fork reborn *inside* the seam — the exact drift the seam exists to remove, now hidden one level deeper where the next lookalike will grow.

Test to apply at every `if` in the seam: *would the live app, on a frame whose data happened to match, take this same branch?* If yes, it's input-presence — allowed. If the only caller that reaches it is the harness, it's caller-identity — forbidden, extract the difference back out to the caller as a real input.

## 4. Fix path (incremental, pattern already proven)

P1/P3 are the template. Remaining work to satisfy the invariant:

1. ~~Overlay pass point-fix (BUG-097).~~ **Not built as a point fix.** BUG-097 (harness overlay pass uses `render_tree_range` where the live app uses `render_sub_region`, so every open overlay renders as nothing in a harness PNG) is closed *by construction* in step 2 — the harness overlay pass is deleted, not realigned. A standalone `render_sub_region` patch would leave the parallel assembly alive and the class open.

2. **Fold the immediate-pass assembly into the shared seam.** Extend `composite_main_ui_frame` (or a sibling `render_main_ui_passes` the extended composite delegates to) to own the **full main-window offscreen pass sequence**, so `app_render.rs`, `render_ui_to_png`, and the script Runner all call the one function and no harness code re-sequences passes or re-chooses a per-pass render call. `draw_immediate_passes` and the harness overlay pass are **deleted, not kept-and-verified**.

   **Seam scope — everything drawn into the offscreen (amendment 2, Fable).** The seam owns the *whole* pass list that lands in the main offscreen, not just the four the sketch named. Verified against current `app_render.rs::present_all_windows`, that is: dirty-panel atlas composite + clear + atlas blit + video-band blit (the P1 `composite_main_ui_frame` body) → grid bitmaps (4a) → clip bodies (4b) → per-clip waveforms (4b′) → clip thumbnails (4b″) → lane/overview/collapsed-group bitmaps (4c) → timeline region/cursor/markers + landing-flash → clip names → automation lanes → playhead → horizontal scrollbar → browser-popup thumbnails → top-level overlays via **`render_sub_region` at `Depth::OVERLAY`** (the BUG-097 pass) → card-drag ghost + text-input at `Depth::TOOLTIP` → the **Audio-Setup VQT waterfall** (`app_render.rs` ~4518–4662). The harness may **skip** a pass only because its input is `None` (per §3's caller test — no content-thread thumbnail atlas, `video: None`, no spectrogram columns), **never because the code wasn't shared.** Silent omission is the same defect class as divergence, quieter — a pass the harness never learns to draw is a lookalike with a blank spot instead of a wrong one.

   *Citation note:* the review brief pointed at "VQT overlay, monitor images, workspace previews (app_render.rs 4520–4667)"; on the code that range is the VQT waterfall only. The **preview monitors and workspace/graph previews are the editor window's passes** (`present_graph_editor_window`), already shared via `editor_frame.rs` (P3) — they are not in the main-window offscreen sequence. This step's scope is the main window; VQT is the one main-offscreen pass the original sketch omitted, and it is in scope.

3. Editor (P3) already satisfies the invariant via `editor_frame.rs`.

Scope note (per `dont-cascade-redesign`): this is bounded — one seam extraction extending the P1 precedent to the passes P1 left in `present_all_windows`, not a rewrite. It removes the audit step permanently. The blast radius is real (this is the live show's per-frame path, D6), so the gate is a byte-identical no-overlay frame **plus** a RED→GREEN overlay proof (see the acceptance gates in the execution brief).

## 5. DESIGN_AUTHORING lesson (proposed §4 addition)

**"Reimplement-and-verify" is a drift carve-out.** When a design's entire value is fidelity-*by-construction* (the harness shows the real app; the exporter writes the real bytes; the preview is the real render), any step that says *"reimplement this part and verify it matches"* reintroduces the drift the design exists to kill — just at a smaller granularity, where it hides longer. A `VERIFY-AT-IMPL: does X match the live path?` gate is a **smell that a seam is missing**: the fix is to share the code so there is nothing to verify, not to verify harder. The catch that works is a seam; the catch that decays is an audit. Specimen: UI_HARNESS_UNIFICATION kept the immediate-pass assembly as a parallel harness copy behind a match-audit, and produced two lookalikes (editor, overlays) in a single execution session — the exact "0-diff lie" class the wave was built to eliminate.

**The carve-out has two cases; the smell is the same in both (amendment 3, Fable).** What both fixes replace is a *manual, discretionary "does it match?" audit* — that human/agent read is the decaying net, whichever case you're in:

- **Accidental duplication → extract a seam.** Two copies exist only because the code was written twice (the harness's parallel pass assembly). There is nothing the second copy *is* that the first isn't. Delete it and share the one function; the difference had no reason to exist.
- **Essential duplication → automated value-level equivalence tests.** Sometimes two implementations *are* the feature: the freeze/fusion compiler must produce the same pixels as the unfused graph; an exporter's fast path must match its reference path. You cannot collapse them to one — the whole point is that two paths agree. Here the seam is impossible, so the net is an **automated equivalence test** that asserts value-level identity every run (this repo's `gpu_proofs` value-parity suite is the model), never a person eyeballing "looks the same." The tempting wrong turn is the same audit; the right tool differs because the duplication is load-bearing.

Diagnosis first: ask *does this duplication carry information the other copy doesn't?* No → extract (case 1). Yes → automate the equivalence (case 2). Either way, the manual match-audit is the thing being retired.

## 6. Resolved (Fable, 2026-07-10)

- **Input vs. code boundary — resolved by the caller test (§3, amendment 1).** "No parallel render-pass assembly" needed the sharper line, and it's now the input-presence-vs-caller-identity test folded into §3. The thumbnail substitution is clean: a labeled test input, consumed by shared render code.
- **Does the single seam conflict with the winit-side split? No — verified against current code.** Every live immediate pass (clips ~4063, names ~4353, lanes ~4368, overlays ~4488–4496 in `app_render.rs`) draws into the offscreen *before* the drawable is acquired (~4719). The seam ends at the offscreen, unchanged from P1's boundary; the fast path, `next_drawable`, the offscreen→drawable blit, and `present_drawable` stay in `present_all_windows`. Only the immediate passes move in.
- **Does the lesson generalize? Yes — as the two-case carve-out (§5, amendment 3).** It covers any fidelity-by-construction design. Where a seam can be extracted (harness, preview) → share the code. Where the duplication is essential (freeze/fusion parity, export fast-vs-reference path) → automated value-level equivalence tests. The retired thing in both is the manual match-audit.
