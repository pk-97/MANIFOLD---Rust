# UI Harness Unification — one headless test path that runs the app's real render, not a lookalike

**Status:** PROPOSED design, not built · 2026-07-09 · Opus (1M) · awaiting Fable review, then Peter approval
**Prerequisites:** none unbuilt. Builds on UI_AUTOMATION P1–P2 (shipped: the `--script` driver + `AutomationAction`) and UI_CLIP_AND_Z P1 (shipped: per-panel `begin_region` wrap). Relates to open BUG-060 and BUG-015.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

**The governing insight: the app renders its main-window UI through a stateful GPU atlas cache (`UICacheManager`), and *not one test touches that code.* Both existing headless harnesses reimplement a lookalike of it — one renders the whole tree fresh every frame on the GPU (`render_ui_to_png`), the other walks node bounds in pure CPU with no pixels (`footer_leak_probe`). A stale-pixel bug lives only in the cache's incremental update, so both lookalikes are structurally blind to it. That blindness is why BUG-060 has been "fixed" and reopened ~4 times: every fix was proven green in a path the performer never runs.** This design makes the app's real render path callable, drives it from a single headless harness that also feeds real input, and proves the harness faithful by making it fail on a known-broken commit before it's trusted.

Peter's directive, verbatim: *"I would want it to extend and generalise across all UI surfaces and windows."* And: *"I want to check that your proposal isn't hyper focused into just this one bug and is a general headless UI and Interaction harness."* Both are load-bearing and shape §6 and the phasing.

The stage translation: a bug like BUG-060 — stale UI chrome smeared across the footer during a set — can today be found only by Peter, live, on the rig. This harness moves that class to a test that fails in seconds before a gig. It is dev tooling whose entire purpose is stage reliability.

Companion docs: [HEADLESS_UI_HARNESS.md](HEADLESS_UI_HARNESS.md) (the existing `cargo xtask ui-snap` tool this extends), [UI_AUTOMATION_DESIGN.md](UI_AUTOMATION_DESIGN.md) (the `--script` input driver, reused wholesale), [UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md](UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md) (the region-clip P1 whose PNG gate verified the wrong path — the precedent failure this design closes).

---

## 1. Audit — what exists (verified 2026-07-09)

| Piece | Where | State |
|---|---|---|
| Live main-window render | `app_render.rs` `tick_and_render` (524) → `present_all_windows` (3882) | The real path. Extend, don't replace. |
| Atlas cache | `manifold-renderer/src/ui_cache_manager.rs` `UICacheManager` (45) | Persistent atlas texture, per-panel `panel_valid`, incremental `LoadOp::Load` sub-regions. **Referenced by zero tests.** |
| Incremental repaint + its documented hazard | `ui_cache_manager.rs` `render_dirty_panels` (175), `incremental_path_safe` (296) | Comments at 206–213, 296–325 name the exact BUG-060 mechanism: chrome/scrollbar "lives in no sub-region, so the Load path never repaints it and a stale pixel there would survive." |
| Invalidation decisions | `app_render.rs`: `invalidate_inspector` scroll path (963); rebuild/structural + `invalidate_all` (2821–2845); `invalidate_scroll_panels` (2828, 2850) | Driven by `UIRoot` flags (`needs_rebuild`, `needs_structural_sync`, `scroll_dirty`, drag guards). Cohesive block, extractable. |
| Offscreen composite | `app_render.rs` `present_all_windows`: `panel_cache_info` (3890) → `render_dirty_panels` (3904) → clear-to-black (4011) + full-atlas blit (4023–4039) + video-band blit (4042–4064) | Every dirty frame re-copies the whole atlas onto a cleared offscreen; the footer is not in the video band, so its pixels come **only** from the atlas. Confirms the atlas is the sole persistent surface that can hold a stale footer. |
| Fast path | `present_all_windows` (3951–3998) | When `!offscreen_dirty`, re-blits the cached offscreen to the drawable. Winit-side; out of harness scope. |
| Drawable acquire + present | `present_all_windows` (3956–3997, tail) | `next_drawable` / `present_drawable`. Genuinely winit-bound; stays in the App shell. |
| Emulating harness #1 | `ui_snapshot/render.rs` `render_ui_to_png` (26) | Real `GpuDevice`, real encoders, but renders the whole tree fresh as painter's-order Clear/Load passes into one target. **No `UICacheManager`, no persistent atlas.** A full repaint per frame cannot exhibit a stale pixel. |
| Emulating harness #2 | `ui_snapshot/mod.rs` `footer_leak_probe::cache_path_inspector_does_not_paint_below_footer_top` (444) | Pure CPU. Walks `traverse_flat_range`, reconstructs the clip stack in Rust, checks node **bounds**. Zero pixels. Emulates the traversal; does not run the cache. |
| Input driver (real) | `ui_snapshot/script.rs` `run` (74), `Runner::step` (214) → `ui.process_events()` (420) | Feeds `AutomationAction`s through the SAME per-frame `process_events` the live app calls (`app_render.rs:869`). This half is faithful and reusable. |
| Input driver's parallel rebuild (drift source) | `script.rs` `Runner` (150) fields `needs_rebuild`/`needs_structural_sync`/`scroll_dirty`/`invalidate_layers` (159–162), own `rebuild` (542), renders via `render_ui_to_png` (651) | The Runner reimplements the App's update-half decision logic and then renders the fake path. This is exactly the drift the design eliminates. |
| Fixture system | `ui_snapshot/fixtures.rs` `build` (28), `SceneData` (19) | Builds real `Project`s through the real core→UI translation (`sync_project_data`/`push_state`). `bug060` scene (36) exists. **`project:<path>` (29) loads a real `.manifold` through the live load path** — realistic fixtures already possible. |
| Graph-editor render (separate path) | `app_render.rs` `present_graph_editor_window` (3206) → `ui_offscreen` + `render_tree` (3311) | **No `UICacheManager`, no atlas, no `panel_valid`.** Recomposites from scratch every render. The stale-pixel class cannot exist here — decisive for §6. |
| Readback helper | `ui_snapshot/render.rs` `readback` (841) | GPU texture → `Vec<u8>`; requires width a multiple of 64. Reusable for atlas-band readback. |
| Scroll gesture (real) | `UIRoot::try_inspector_scroll` (used at `mod.rs:455`) | Drives the live scroll path headlessly. |

Classification: the render path, the cache, the input driver, the fixtures, and the readback all **exist** — this is mostly wiring. **Genuinely new:** two extracted shared functions (§4), a differential atlas-band assertion (§5), a heavy fixture (§7), and the discipline that proves faithfulness (D2). The design shrinks to those four.

Negative claim, checked: `rg "UICacheManager|render_sub_region"` over `crates/**/*.rs` returns only the renderer definition and the live-app call sites — **no test file constructs it.** Verified 2026-07-09.

---

## 2. Decisions

**D1 — The harness renders through the real `UICacheManager`, never a lookalike.** It constructs the actual cache, calls the actual `render_dirty_panels`, and reads back the actual atlas. Rationale: the four BUG-060 reopens all trace to verifying a path the live app doesn't run. Rejected: *a full-repaint render (`render_ui_to_png`) or a CPU-bounds walk (`footer_leak_probe`) as the reliability-critical assertion* — both are structurally incapable of showing a stale pixel, which is the exact history being corrected. They survive only in the roles they're honestly good at (D4).

**D2 — Faithfulness is proven by a red bracket, and this is a merge gate.** A new harness assertion is trusted only after it has been demonstrated to **fail on a commit where the bug is live.** Rationale: every prior fix passed a green test that could not see the bug; a green-only test is unfalsifiable and indistinguishable from the lookalikes. Peter's read, well-founded across four reopens, is that only a full clear fixes the artifact — a caching signature. Rejected: *green-only acceptance* — it is precisely what let three fixes land against a bug none of them touched.

**D3 — The shared seam is two functions the app and the harness both call; drift becomes impossible by construction, not by discipline.** `apply_ui_frame_invalidations` (owns the invalidate/rebuild decision block) and `composite_main_ui_frame` (owns dirty-panel atlas render + offscreen composite). Rationale: the bug's trigger is the *difference* between "scroll invalidates only the inspector" and "tab-swap invalidates everything" — that decision lives in the App tick, between input and pixels. A harness that drives input and then a render function still has to decide when to invalidate; if it transcribes that decision it drifts, which is exactly what the `script.rs` Runner already does. Sharing the decision code removes the failure mode. Rejected: *the harness replicates the invalidation sequence itself and calls only a render function* — this is the minimal driver (P0) and it is correct as a beachhead, but as the permanent shape it keeps the Runner's parallel logic alive and forces every future test to re-transcribe invalidation. Drift returns; the lie comes back.

**D4 — Assertions are layered by bug class; pixel goldens are kept off the reliability-critical path.** (a) Stale-pixel / dirty-clear → **differential self-comparison**: read a suspect atlas band before and after an invalidation sequence *from the same device in the same run*, assert byte-identical. No baseline file, immune to driver/OS/Metal-compiler drift. (b) Geometry / containment → the existing CPU bounds test (`footer_leak_probe`) — kept; it correctly proves non-leak. (c) "Chrome looks right" → hashed or human-read golden, explicitly **not** gating reliability. Rationale: Peter's bar is "accurate and reliable"; goldens are a flakiness treadmill for regression but are the right tool for the "does it look right" acceptance question. Rejected: *pixel goldens as the primary regression mechanism* — fails the reliability bar the whole effort exists to meet.

**D5 — Unification is at the scaffolding layer, not the render path.** One harness *framework* — input driver, readback, assertion helpers, fixture system — that every window plugs into with its own render entry and its own invariants. Peter asked for it to "extend and generalise across all UI surfaces and windows"; the technical shape that honors that is shared scaffolding, because the windows do not share a render path: the main window is a stateful atlas cache, the graph editor is cacheless immediate-mode (`present_graph_editor_window` → `render_tree`, no `UICacheManager`; audit §1). The stale-pixel class cannot exist in the editor, so asserting a cache invariant there is meaningless, and forcing one render abstraction would bury the real per-window behavior. **Dissent recorded (per DESIGN_AUTHORING §9):** Peter's instinct is "unify everything"; my position is "unify the scaffolding, keep the render entries distinct" — the reason is the cacheless editor path, verified at `app_render.rs:3311`. Both positions are here so the choice is his at review. This delivers full cross-surface *coverage* without a false abstraction — and is a truer test, because each window is asserted against what it actually does.

**D6 — Phasing is beachhead-then-refactor: catch the bug with zero live-code change first, extract the hot-path seam only after the harness is proven faithful.** Rationale: the seam extraction touches `present_all_windows`, which runs every frame of the live show — maximum blast radius. Doing it first bets the rig's render path on a harness that hasn't yet caught anything. P0 (the ~40–60-line direct-API driver) proves the harness sees the bug with no risk; P1 does the refactor against a harness that already works. Rejected: *extract-first* — front-loads the highest-risk change before any evidence the approach pays.

**D7 — Fixtures are realistic, matching the repro conditions, not toy scenes.** A heavy generator fixture (multiple effects on a generator layer + dense modulation draws, per Peter's observed repro) plus the existing `project:<path>` real-project load. Rationale: Peter reports BUG-060 is content-sensitive — worst with many effects and heavy modulation while scrolling. A light scene may not reproduce it; a harness that passes only because its fixture is too small is fixture-overfitting (DESIGN_AUTHORING §5). Rejected: *toy fixtures only* — risks a green harness on a bug that reproduces only under load.

---

## 3. The real render path today (what the seam cuts)

One frame of the live main window, with anchors, is the map every later section refers to:

1. **`tick_and_render`** (524) drains content-thread state, advances animation, processes input, and decides what is dirty. The decisions that matter here:
   - scroll-in-place → `cm.invalidate_inspector()` (963);
   - `needs_rebuild || needs_structural_sync` (guarded by `inspector_dragging` / `layer_dragging`) → `ui_root.build()` + `cm.invalidate_all()` (2821–2845);
   - else `scroll_dirty.any()` → `rebuild_scroll_panels` + `cm.invalidate_scroll_panels()` (2847–2851).
2. At the end of the tick it calls **`present_all_windows(front)`** (3134/3142).
3. **`present_all_windows`** (3882): fast path re-blits the cached offscreen if `!offscreen_dirty` (3951); otherwise `panel_cache_info()` (3890) → **`render_dirty_panels`** (3904) paints only dirty panels into the persistent atlas via `LoadOp::Load`, then the offscreen is cleared (4011), the whole atlas is blitted onto it (4023–4039), the compositor video is blitted into the video band (4042–4064), and finally a drawable is acquired and presented (tail).

**The stale-pixel hazard is inside `render_dirty_panels`** (`ui_cache_manager.rs:175`): a panel that is `panel_valid` and has no dirty nodes is skipped entirely (202), so on an inspector-only scroll the footer panel is never repainted — correct *iff* its atlas pixels are intact. The incremental Load path (213–248) repaints only dirty sub-regions and preserves everything else; `incremental_path_safe` (296) guards two conditions — sub-region extents unchanged, and no dirt outside sub-regions — because a drifted extent or out-of-sub-region chrome would leave stale pixels. **BUG-060 is a suspected hole in that guard or in what counts as a panel's sub-regions.** This design does not fix that bug; it makes it observable and deterministic. The fix is separate work the harness gates.

---

## 4. The shared seam (committed signatures)

Two functions, extracted verbatim from the blocks in §3, called by both the live app and the harness. They live in a new module `crates/manifold-app/src/ui_frame.rs` (App-internal; no new crate boundary, no new dependency). The App owns them; the UI thread calls them; no thread residency change.

```rust
// crates/manifold-app/src/ui_frame.rs

/// The per-frame UI dirty signals the invalidation decision reads. Filled by the
/// caller from its own state (the live App from `self.needs_rebuild` etc.; the
/// harness from the gesture it just applied). Exact membership is re-derived at
/// P1 against app_render.rs:963 + 2819–2852 — see the P1 seam brief.
pub(crate) struct UiFrameSignals {
    pub needs_rebuild: bool,
    pub needs_structural_sync: bool,
    pub scroll_dirty: ScrollDirty,
    pub scrolled_in_place: bool,   // the 963 path
}

/// Owns the invalidate/rebuild decision block. Reads `ui_root`'s drag guards and
/// applies `build()` / `rebuild_scroll_panels()` and the matching
/// `UICacheManager::invalidate_*` exactly as tick_and_render does today. THE
/// single place these decisions live. Precedent: app_render.rs:2819–2852 + 963,
/// moved, not rewritten.
pub(crate) fn apply_ui_frame_invalidations(
    ui_root: &mut UIRoot,
    cache: &mut UICacheManager,
    signals: UiFrameSignals,
);

/// Composites the main-window UI for one frame into `offscreen`:
/// `render_dirty_panels` (atlas, LoadOp::Load) + clear-to-black + full-atlas
/// blit + optional video-band blit. Does NOT acquire or present a drawable —
/// that stays in `present_all_windows`. `video` is the compositor output for the
/// video band; `None` in the harness. Precedent: present_all_windows 3890–4064
/// minus the fast path (3951–3998) and the drawable tail.
pub(crate) fn composite_main_ui_frame(
    device: &GpuDevice,
    ui_renderer: &mut UIRenderer,
    cache: &mut UICacheManager,
    ui_root: &UIRoot,
    offscreen: &GpuTexture,
    video: Option<&GpuTexture>,
);
```

`⚠ VERIFY-AT-IMPL (P1): the exact field set of UiFrameSignals` — re-read `app_render.rs:950–965` and `2815–2852` and confirm every input the decision reads is either a `UiFrameSignals` field or reachable from `ui_root`. A signal the block reads that is neither is an escalation, not a guessed addition.

What stays in `present_all_windows`, unchanged: the fast path, `next_drawable`, the offscreen→drawable blit, `present_drawable`. What stays in `tick_and_render`, unchanged: content drain, playback, perform mode, input, editor routing — everything except the invalidation block, which becomes a single call to `apply_ui_frame_invalidations`.

---

## 5. The assertion model

Three layers, per D4. The reliability-critical one is the differential:

**Differential atlas-band (stale-pixel / dirty-clear).** The recipe, which is also the BUG-060 repro:

```
build heavy fixture; ensure_atlas; invalidate_all
composite_main_ui_frame(...)                       // frame 1: full, self-clearing render
BAND_0 = readback(atlas, footer_band_rows)
apply the scroll gesture (try_inspector_scroll to the bottom)
apply_ui_frame_invalidations(...)                  // the live decision: inspector-only invalidation
composite_main_ui_frame(...)                       // frame 2: incremental Load path
BAND_1 = readback(atlas, footer_band_rows)
assert BAND_1 == BAND_0                             // footer must not change on an inspector-only scroll
```

The footer is a different panel from the inspector and is not invalidated by a scroll, so its pixels must be identical across the two frames. If BUG-060 fires, frame 2's footer band carries stale inspector chrome and the bands differ — RED. Both readbacks come from the same device in the same run, so there is no golden file and no cross-environment drift. The footer band is `[footer_top, footer_top + footer_height)` in atlas rows; readback width stays a multiple of 64 (readback helper constraint, render.rs:841).

**Geometry / containment.** Keep `footer_leak_probe` as-is. It proves no node geometrically escapes into the footer — true and worth pinning — and it is honestly labeled as a bounds check, not a pixel check.

**"Looks right" golden.** For chrome-appearance regressions, a hashed band or a human-read PNG, produced by the same composite path. Explicitly not a reliability gate; it answers "did we change how it looks," not "did a pixel go stale."

---

## 6. Cross-window model (D5)

The unification is a framework each window plugs into:

| Shared scaffolding (built once) | Per-window (kept distinct) |
|---|---|
| Input driver (`script.rs` `process_events` path, reused) | Render entry: main = `composite_main_ui_frame`; editor = `present_graph_editor_window`'s `ui_offscreen`+`render_tree`; monitor = its blit path |
| Readback + band comparison (§5) | Invariants asserted: main = atlas-band differential + containment; editor = cacheless "recomposite is complete" checks; monitor = output-fit checks |
| Fixture system (`fixtures.rs`, incl. `project:<path>`) | The scene each window needs |
| Assertion helpers, result/exit-code plumbing (`script.rs` precedent) | — |

The main window is the only one with the stale-pixel class, so it is the only one that gets the differential assertion. The editor and monitor windows reuse the driver and readback to answer their own questions. This is the whole of Peter's "generalise across all windows," minus the false abstraction of one render path.

---

## 7. Fixtures

**`bug060heavy`** (new, `fixtures.rs`): a generator layer carrying multiple effects and dense modulation draws — modeled on Peter's stated worst case. Built through the real translation path like every other fixture. This is the held-out-style input for the differential (D7): the harness must reproduce BUG-060 on a fixture shaped like the real repro, not a minimal one.

**Real-project load** already exists: `project:<abs-path>` loads a `.manifold` through the live loader (`fixtures.rs:68`). The canonical `Liveschool Live Show V6 LEDS.manifold` (53 layers / 2928 clips) is loadable as a soak/realism fixture. No new code needed; named here so it's in scope.

`⚠ VERIFY-AT-IMPL (P0): before building bug060heavy, ask Peter which generator and roughly how many effects he sees it worst on, and model the fixture on that answer.` If the answer is unavailable at build time, default to a Plasma generator + 3 effects + a Color Compass stack with audio modulation on several params (the reopen notes' "Plasma + stacked Color Compass" case), and record that this default may under-reproduce.

---

## Phasing

### P0 — Atlas-band differential driver + BUG-060 red bracket (zero live-code change)

The vertical slice: real fixture → real cache → real pixels → assertion. No extraction yet; the driver calls the public `UICacheManager` API directly in the app's order.

- **Entry state:** `rg -n "fn render_dirty_panels|fn atlas_texture|fn invalidate_inspector|fn invalidate_all|fn ensure_atlas" crates/manifold-renderer/src/ui_cache_manager.rs` matches the audit lines (175/163/126/101/131); `rg "fn panel_cache_info" crates/manifold-app/src/ui_root.rs` exists; `footer_leak_probe` compiles under `--features ui-snapshot`.
- **Read-back (first step):** read `ui_cache_manager.rs:175–281`, `app_render.rs:3882–4064`, `ui_snapshot/mod.rs:420–549` (footer_leak_probe), `ui_snapshot/render.rs:26–145` + `:841` (readback). Restate: the invalidation order the live tick uses, the readback width constraint, and D1/D2/D7.
- **Deliverables:** `bug060heavy` fixture (`fixtures.rs`); a new test module `cache_path_footer_differential` under feature `ui-snapshot` (structure like `footer_leak_probe`, rendering through a real `UICacheManager` + `UIRenderer` + atlas like `present_all_windows`); the differential recipe of §5; a saved PNG of both footer bands + a printed changed-row count as the artifact.
- **Gate (positive):** run the driver against **current `main`** and produce a **non-empty** footer-band diff (the RED bracket, D2). Report the changed-pixel count and save the two band PNGs. Then, on the branch that carries the eventual BUG-060 fix, the same driver goes GREEN. **Escalation branch (a legitimate P0 outcome, not a failure):** if the diff is empty on broken `main`, the bug is not a deterministic function of the replayed sequence — stop and document what the driver replays vs. what the live frame does (dual-device IOSurface, CVDisplayLink cadence, drawable pooling, or a content-state signal the fixture doesn't carry). That result reframes BUG-060 as timing/environment and is worth more than a false green.
- **Gate (negative):** `rg "render_ui_to_png|traverse_flat_range" ` in the new test file returns zero hits — proving the driver renders through the cache, not a lookalike (D1).
- **Acceptance demo:** the printed changed-row count + the two band PNGs, read by a reviewer. **L2** (a headless test whose artifact is looked at). No flow driver reaches this internal path, so L3 does not apply.
- **Forbidden moves:** rendering via `render_ui_to_png` or any full-repaint (D1) · asserting on node bounds instead of pixels · a toy fixture that can't reproduce under load (D7) · declaring the test done while green-only (D2 — it must be shown red first) · fixing BUG-060 in this phase (out of scope; P0 delivers the *observatory*, not the fix).
- **Test scope:** `manifold-app --features ui-snapshot` focused; this touches the GPU render path, so run it deliberately, not as part of the default sweep.

### P1 — Extract the shared seam (behavior-preserving hot-path refactor)

- **Entry state:** P0 green-on-fix / red-on-broken demonstrated (the harness is proven faithful before the risky refactor, D6). Re-derive the call-site inventory below and confirm counts.
- **Read-back:** read §3 and §4 of this doc, `app_render.rs:950–965` and `2815–2852` and `3882–4064`. Restate the seam signatures, the may/must-escalate line, and that this is the live show's per-frame path.
- **Seam brief (per DESIGN_DOC_STANDARD §6):**
  - *Old → new:* the invalidation block at 2819–2852 + the scroll-in-place `invalidate_inspector` at 963 become the body of `apply_ui_frame_invalidations`; the composite at 3890–4064 (minus fast path 3951–3998 and the drawable tail) becomes `composite_main_ui_frame`. `tick_and_render` and `present_all_windows` each replace the moved block with one call.
  - *Call-site inventory (re-derive, don't trust):* `rg -n "invalidate_inspector|invalidate_all|invalidate_scroll_panels|render_dirty_panels" crates/manifold-app/src/app_render.rs` — expected sites 963, 2828, 2844, 2850, 3904 (5). If the count differs, stop and list new sites before touching anything.
  - *Technique:* extract by move; the app calls the new functions and must produce byte-identical frames. Not compiler-driven-delete (nothing is renamed away), so the deletion gate is "the inline block no longer exists": `rg "cm.invalidate_all\(\)" app_render.rs` returns only the call inside `apply_ui_frame_invalidations`.
  - *Misfit escalation:* if any signal the block reads is neither a `UiFrameSignals` field nor reachable from `ui_root` (the §4 VERIFY-AT-IMPL), stop and ask — do not widen the struct silently.
- **Gate (positive):** the app boots and renders; a captured main-window offscreen readback is byte-identical before vs. after the extraction on the `inspector` and `bug060heavy` scenes (drive via the P0 driver, which now calls the extracted `composite_main_ui_frame`). `MANIFOLD_RENDER_TRACE=1` shows no frame regression (no added per-frame allocation or cost). **Gate (negative):** the inline invalidation block and inline composite are gone (rg proofs above).
- **Acceptance demo:** the byte-identical offscreen diff (zero changed pixels) on both scenes, plus a `MANIFOLD_RENDER_TRACE` line showing steady frame time. **L2**, and the landing runs the real app (L4) since this is the show's render path — a human confirms the UI still draws.
- **Forbidden moves:** widening the extraction into content-drain / playback / perform-mode (scope fence — the seam is exactly the two blocks) · changing behavior "while I'm in here" · adding a flag to toggle old vs new path · leaving `present_all_windows`'s inline composite alive in parallel · crossing a crate boundary (all of this is App-internal).
- **Test scope:** `manifold-app` + `manifold-renderer --features gpu-proofs` focused; workspace sweep at end of phase.

### P2 — Repoint the input driver and headless harness at the seam (kill the drift)

- **Entry state:** P1 landed; `apply_ui_frame_invalidations` + `composite_main_ui_frame` exist and the app calls them.
- **Read-back:** read `script.rs:150–260` (Runner + step), `:403` and `:542` (its rebuild), `:651` (its render call). Restate D3 and the forbidden-moves list.
- **Deliverables:** the P0 differential driver and `script.rs`'s `Runner` both drive frames through `apply_ui_frame_invalidations` + `composite_main_ui_frame`. The Runner's parallel `rebuild()` invalidation logic is **deleted**, not wrapped. `render_ui_to_png`'s fake whole-tree *panel* pass is replaced by the seam; its genuinely-separate immediate-mode passes (clips, thumbs, automation lanes, overlays — the live app draws these as immediate passes too, `app_render` 4b/5) are kept only after confirming they match the live app's passes. `⚠ VERIFY-AT-IMPL (P2): diff render_ui_to_png's clip/thumb/lane/overlay passes against the live immediate-mode passes; a pass that doesn't match is an escalation, not a silent keep.`
- **Gate (positive):** the two shipped flows `scripts/ui-flows/select-and-inspect.json` and `scripts/ui-flows/drag-clip.json` still exit 0 via `cargo xtask ui-snap <scene> --script <flow.json>`; the P0 differential still brackets BUG-060. **Gate (negative):** `rg "needs_rebuild|invalidate_layers" crates/manifold-app/src/ui_snapshot/script.rs` returns zero hits (the Runner's parallel decision state is gone); the Runner no longer calls a private `rebuild` (rg proof). One update+composite path, three callers: live app, headless differential, script runner.
- **Acceptance demo:** both ui-flows green through the shared seam. **L3** (scripted flows drive the real input path — the target level since UI_AUTOMATION landed).
- **Forbidden moves:** keeping the Runner's `rebuild` "just for scripts" (adapter/parallel-path — forbidden by name) · silently keeping a `render_ui_to_png` panel pass that diverges from the live passes · widening scope to refactor the flow format.
- **Test scope:** `manifold-app --features ui-snapshot` + the ui-flows; workspace sweep at end.

### P3 — Generalize the scaffolding to the editor window (and stub the monitor path)

- **Entry state:** P2 landed; the shared driver + readback + fixtures are the single harness framework.
- **Read-back:** read `present_graph_editor_window` (3206–3400), `fixtures::generator_editor_fixture`, and D5. Restate that the editor is cacheless and gets its OWN invariants, not the atlas differential.
- **Deliverables:** an editor-window harness entry that drives input through the shared driver and reads back the editor `ui_offscreen`, asserting an editor-appropriate invariant (e.g. a node the fixture places renders at its expected screen rect; a recomposite after a canvas gesture is complete). Reuses the scaffolding; adds no cache assertion.
- **Gate (positive):** an editor flow produces a readback and passes its structural assertion. **Gate (negative):** the editor path constructs no `UICacheManager` (rg proof — it must not borrow the main window's cache model).
- **Acceptance demo:** the editor readback PNG + its assertion result. **L2** (or L3 if the flow driver reaches the editor input path — verify at impl).
- **Forbidden moves:** forcing the editor onto the atlas cache (D5) · asserting a stale-pixel invariant on a cacheless path · pulling monitor/output windows into this phase (Deferred).
- **Test scope:** `manifold-app --features ui-snapshot` focused.

---

## Decided — do not reopen

1. The harness renders through the real `UICacheManager`; no full-repaint or CPU-bounds path is ever the reliability-critical assertion. (D1)
2. A harness assertion is trusted only after it is shown RED on a known-broken commit. Green-only is not a gate result. (D2)
3. The shared seam is two functions — `apply_ui_frame_invalidations`, `composite_main_ui_frame` — in `manifold-app/src/ui_frame.rs`; the app and every harness call them. (D3)
4. Assertions are layered: differential atlas-band for stale-pixel (primary), CPU bounds for containment, hash/golden for looks-right (never a reliability gate). (D4)
5. Unification is the scaffolding (input, readback, assertions, fixtures); each window keeps its own render entry and invariants. The editor is cacheless and never borrows the atlas model. (D5)
6. Order is P0 beachhead (zero live-code change) before P1 hot-path extraction. (D6)
7. Fixtures reproduce the real repro conditions; toy-only fixtures are insufficient. (D7)
8. Drawable acquire/present and the offscreen fast path stay winit-side in `present_all_windows`; the seam ends at the offscreen texture.

## Deferred

- **Monitor / output-window assertions** — revive when a monitor-window render bug is reported or when projection-mapping output needs headless proof. The P3 scaffolding is built to extend to it; only the per-window invariant is missing.
- **The BUG-060 fix itself** — this design delivers the observatory and the gate, not the fix. Revive immediately after P0 brackets it (the fix is a separate change that turns the P0 differential green). If P0's escalation branch fires (bug not reproduced headless), the fix work reopens as a timing/environment hunt instead.
- **CI wiring of the differential** — running the harness on every push. Revive once P0–P2 are green and stable; until then it runs deliberately (GPU path, minutes-long, per CLAUDE.md test-scope discipline).
- **Perform-mode surfaces (track HUD, session grid)** — additional render entries under the same scaffolding. Revive when a perform-surface render bug needs headless proof.
