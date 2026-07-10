# UI Harness Unification — one headless test path that runs the app's real render, not a lookalike

**Status:** APPROVED design, REFRAMED 2026-07-10 (Opus + Peter) — **read the "Reframe 2026-07-10" block immediately below before anything else; it supersedes D2, demotes D4a/§5, and restates P0.** P0 BUILT-UNLANDED on `feat/ui-harness-p0` @ `b51e6bbc` (worktree `ui-harness-p0`) · 2026-07-09 · Opus (1M) · Fable review 2026-07-09: every audit anchor re-verified against code; amended (D8–D9 added, §4 signals now `&mut`, §5 recipe strengthened to full-sequence replay, P0/P2 deliverables extended, BUG-071 pulled into P0) · Peter approved for Sonnet execution 2026-07-09 · 2026-07-10 re-cut (Fable): BUG-060 was root-caused and CLOSED independently of this wave.

---

## Reframe 2026-07-10 (Opus + Peter) — general faithful-render harness; the red bracket is retired

**Read this before §1. It changes the goal, not the machinery.** Peter's redirect: *"Don't worry about BUG-060 now — it's been patched. This upgrade is no longer BUG-060 focused."* And, on why the harness never needed a failing test to prove itself: *"Why do we need to watch it fail? We just need to watch it render the full app properly."*

**The goal, restated.** The harness renders the whole live app through its *real* rendering code — the same functions the rig runs — and produces pixels and filmstrips a human or an agent can look at and trust. That is the deliverable: a faithful-render-and-inspect tool, not a regression gate.

**What that retires:**
- **D2 (red bracket = merge gate) is retired.** "Watch it fail on a known-broken commit" earns its keep only for an *automated pass/fail assertion nobody looks at* — there you must prove the "fail" is reachable or green is meaningless. This harness's output is looked at, and an image cannot lie the way a green checkmark can. No broken commit, no bracket, no BUG-060 specimen. (The reasoning that produced D2 was sound for the assertion it guarded; we removed the assertion, so the guard goes with it.)
- **D4(a) / §5 differential atlas-band is demoted from reliability-critical to a shelf tool.** The byte-for-byte band comparison is no longer P0's centerpiece or any phase's gate. It stays documented and may be revived *only* if a specific stale-pixel regression is ever reported. Nothing in the wave asserts it.

**What survives, and is now more central:**
- **D1** — render through the real `UICacheManager`, never a lookalike. This is the whole point now: fidelity comes from running the app's real drawing code.
- **D3** — the two shared seam functions (`apply_ui_frame_invalidations`, `composite_main_ui_frame`). P1 extracts them. This is what makes "the harness draws exactly what the app draws" true *by construction* rather than by hope — it is the faithfulness proof, replacing the red bracket.
- **D5** — unify the scaffolding across all windows; each window keeps its own render entry. Peter's "generalise across all surfaces and windows" is now the spine, not a nice-to-have.
- **D8** — scale factor 1.0 at the fixture's logical size.
- **D9** — agent-legible captures (filmstrip contact sheets, CPU-side pointer stamps, truthful dump). These are now the *headline* output, since "look at it" is the entire verification model. BUG-071's dump fix still rides P0.

**How faithfulness is established now (replaces D2):** structurally, because P1 makes the app and the harness call the identical render functions; plus a one-time human eyeball comparing a harness PNG against the real app on screen. The only automated check is a smoke test — it rendered something sane, not all black, didn't crash.

**Fixture:** no BUG-060-worst-case fixture is needed. A realistically heavy scene for general coverage suffices; the existing `project:<abs-path>` real-project load (the Liveschool fixture) provides it. The D7 "which generator, how many effects" question is closed — not asked.

Phasing is unchanged in shape (P0 beachhead → P1 seam extraction → P2 repoint the runner + captures → P3 editor window); only P0's deliverable and gate change, restated in the P0 phase block below. Where the sections further down still speak of the red bracket, the differential-as-gate, or BUG-060 as the target, **this block wins.**
**Prerequisites:** none unbuilt. Builds on UI_AUTOMATION P1–P2 (shipped: the `--script` driver + `AutomationAction`) and UI_CLIP_AND_Z P1 (shipped: per-panel `begin_region` wrap). Relates to BUG-060 (CLOSED 2026-07-10 @ `cc4eeb37`) and open BUG-015; fixes BUG-071 in P0 (D9c).
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
| Atlas cache | `manifold-renderer/src/ui_cache_manager.rs` `UICacheManager` (45) | Persistent atlas texture, per-panel `panel_valid`, incremental `LoadOp::Load` sub-regions. **No test constructs one or renders a pixel through it** — the `#[cfg(test)]` block in the same file (372–487) unit-tests the guard predicates (`incremental_path_safe`, `extents_unchanged`, `sub_region_sig`) CPU-side, zero pixels. |
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

Negative claim, checked: `rg "UICacheManager|render_sub_region"` over `crates/**/*.rs` returns the renderer definition, the live-app call sites, comments in `ui_snapshot/mod.rs`, and the CPU-side predicate tests inside `ui_cache_manager.rs` itself — **no test constructs a `UICacheManager` or renders through it.** Verified 2026-07-09; re-verified at Fable review the same day.

---

## 2. Decisions

**D1 — The harness renders through the real `UICacheManager`, never a lookalike.** It constructs the actual cache, calls the actual `render_dirty_panels`, and reads back the actual atlas. Rationale: the four BUG-060 reopens all trace to verifying a path the live app doesn't run. Rejected: *a full-repaint render (`render_ui_to_png`) or a CPU-bounds walk (`footer_leak_probe`) as the reliability-critical assertion* — both are structurally incapable of showing a stale pixel, which is the exact history being corrected. They survive only in the roles they're honestly good at (D4).

**D2 — Faithfulness is proven by a red bracket, and this is a merge gate.** A new harness assertion is trusted only after it has been demonstrated to **fail on a commit where the bug is live.** Rationale: every prior fix passed a green test that could not see the bug; a green-only test is unfalsifiable and indistinguishable from the lookalikes. Peter's read, well-founded across four reopens, is that only a full clear fixes the artifact — a caching signature. Rejected: *green-only acceptance* — it is precisely what let three fixes land against a bug none of them touched.

**D3 — The shared seam is two functions the app and the harness both call; drift becomes impossible by construction, not by discipline.** `apply_ui_frame_invalidations` (owns the invalidate/rebuild decision block) and `composite_main_ui_frame` (owns dirty-panel atlas render + offscreen composite). Rationale: the bug's trigger is the *difference* between "scroll invalidates only the inspector" and "tab-swap invalidates everything" — that decision lives in the App tick, between input and pixels. A harness that drives input and then a render function still has to decide when to invalidate; if it transcribes that decision it drifts, which is exactly what the `script.rs` Runner already does. Sharing the decision code removes the failure mode. Rejected: *the harness replicates the invalidation sequence itself and calls only a render function* — this is the minimal driver (P0) and it is correct as a beachhead, but as the permanent shape it keeps the Runner's parallel logic alive and forces every future test to re-transcribe invalidation. Drift returns; the lie comes back.

**D4 — Assertions are layered by bug class; pixel goldens are kept off the reliability-critical path.** (a) Stale-pixel / dirty-clear → **differential self-comparison**: read a suspect atlas band before and after an invalidation sequence *from the same device in the same run*, assert byte-identical. No baseline file, immune to driver/OS/Metal-compiler drift. (b) Geometry / containment → the existing CPU bounds test (`footer_leak_probe`) — kept; it correctly proves non-leak. (c) "Chrome looks right" → hashed or human-read golden, explicitly **not** gating reliability. Rationale: Peter's bar is "accurate and reliable"; goldens are a flakiness treadmill for regression but are the right tool for the "does it look right" acceptance question. Rejected: *pixel goldens as the primary regression mechanism* — fails the reliability bar the whole effort exists to meet.

**D5 — Unification is at the scaffolding layer, not the render path.** One harness *framework* — input driver, readback, assertion helpers, fixture system — that every window plugs into with its own render entry and its own invariants. Peter asked for it to "extend and generalise across all UI surfaces and windows"; the technical shape that honors that is shared scaffolding, because the windows do not share a render path: the main window is a stateful atlas cache, the graph editor is cacheless immediate-mode (`present_graph_editor_window` → `render_tree`, no `UICacheManager`; audit §1). The stale-pixel class cannot exist in the editor, so asserting a cache invariant there is meaningless, and forcing one render abstraction would bury the real per-window behavior. **Dissent recorded (per DESIGN_AUTHORING §9):** Peter's instinct is "unify everything"; my position is "unify the scaffolding, keep the render entries distinct" — the reason is the cacheless editor path, verified at `app_render.rs:3311`. Both positions are here so the choice is his at review. This delivers full cross-surface *coverage* without a false abstraction — and is a truer test, because each window is asserted against what it actually does.

**D6 — Phasing is beachhead-then-refactor: catch the bug with zero live-code change first, extract the hot-path seam only after the harness is proven faithful.** Rationale: the seam extraction touches `present_all_windows`, which runs every frame of the live show — maximum blast radius. Doing it first bets the rig's render path on a harness that hasn't yet caught anything. P0 (the ~40–60-line direct-API driver) proves the harness sees the bug with no risk; P1 does the refactor against a harness that already works. Rejected: *extract-first* — front-loads the highest-risk change before any evidence the approach pays.

**D7 — Fixtures are realistic, matching the repro conditions, not toy scenes.** A heavy generator fixture (multiple effects on a generator layer + dense modulation draws, per Peter's observed repro) plus the existing `project:<path>` real-project load. Rationale: Peter reports BUG-060 is content-sensitive — worst with many effects and heavy modulation while scrolling. A light scene may not reproduce it; a harness that passes only because its fixture is too small is fixture-overfitting (DESIGN_AUTHORING §5). Rejected: *toy fixtures only* — risks a green harness on a bug that reproduces only under load.

**D8 — Fidelity contract (added 2026-07-09, Fable review): the harness renders the real layout at scale factor 1.0 and the fixture's logical size; the three gaps to the MacBook screen are named, not implied.** The live app sizes the atlas in *logical* pixels and passes Retina scale separately — `cm.set_scale_factor(scale)` then `cm.ensure_atlas(&gpu.device, logical_w, logical_h)` (app_render.rs:3901–3902) — so the harness gets the identical layout, wrapping, and scroll geometry by calling the same API with `scale_factor = 1.0` at the fixtures' existing logical size (1536-wide; already a multiple of 64 per the readback constraint, render.rs:841). Layout is pixel-exact by construction; the raster is ~4× cheaper than a 2x pass. The three honest gaps to what Peter sees on the rig: (1) *scale* — a 2x-only raster artifact is invisible at 1x; the knob exists (`set_scale_factor(2.0)`) as a deliberate Deferred variant, never the default. (2) *video band* — `composite_main_ui_frame` takes `video: None`; UI chrome is 1:1, compositor content is absent. (3) *time* — the driver's clock advances only on `Step` at a fixed 60 fps DT (script.rs:55–56, 225), deterministic where the live app follows the display clock; that determinism is a feature for reproducibility and the reason a genuinely timing-dependent artifact lands in P0's escalation branch rather than being missed silently. Rejected: *shrinking the window for speed* — layout is a function of logical size, so a smaller window tests a different layout, not a cheaper render of Peter's.

**D9 — Captures are agent-legible (added 2026-07-09, Fable review): motion as filmstrips, input as pointer stamps, structure as a truthful dump.** The harness's consumers are agents that cannot watch the app; captures must carry what a human gets by watching. Three commitments: (a) **Filmstrip** — a capture mode that saves the composited frame after every stepped frame between two script actions and assembles a contact sheet (one PNG, N tiles), so a drawer tween or a landing flash is readable in a single file read; the fixed DT means the same script produces the same frames every run. (b) **Pointer stamp** — saved captures carry a crosshair at the gesture point(s) the Runner synthesized (the centers and interpolated path points fed to `pointer_event`, script.rs:343–398), drawn CPU-side on the readback bytes AFTER that frame's assertions have run — never into the atlas or offscreen, which would poison the differential. This is the difference between "the flow failed" and "the click landed 40px left of the button." (c) **The dump tells the truth** — BUG-071 (`ui_snapshot/dump.rs` serializes the mint-time `parent_id` instead of the live `tree.parent_index` a reparent actually mutates) is fixed in P0 by serializing `tree.parent_index[i]` at dump.rs:38/:92. That is the read-only fix shape; the backlog's alternative (mutating `nodes[i].parent_id` inside `reparent_root_nodes`) touches live UI code and is rejected for P0's zero-live-code rule. Rationale: agents target elements through the dump + selector surface and judge results through pixels; a dump that lies about hierarchy already cost a real debugging session (it's how BUG-071 was found).

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
///
/// `signals` is `&mut` because the block WRITES BACK (amended 2026-07-09, Fable
/// review): it clears `needs_rebuild` when it rebuilds, and deliberately KEEPS
/// it set when an active inspector/layer drag defers the rebuild to the next
/// frame (app_render.rs:2821–2834). By-value signals cannot express the defer.
/// The live caller copies the residual flags back into its own state after the
/// call; the harness carries them to its next frame.
pub(crate) fn apply_ui_frame_invalidations(
    ui_root: &mut UIRoot,
    cache: &mut UICacheManager,
    signals: &mut UiFrameSignals,
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
build heavy fixture; ensure_atlas (scale 1.0, D8); invalidate_all
composite frame                                     // full, self-clearing render
REF = readback(atlas, footer_band_rows)             // baseline after a full clear
for each frame of the repro sequence:               // scroll to bottom · expand a drawer
                                                    //   (tween forces needs_rebuild every frame,
                                                    //   app_render.rs:2942–2944) · scroll ·
                                                    //   swap tab Layer↔Master · scroll again
    apply the live invalidation decision            // P0: transcribed; P2+: apply_ui_frame_invalidations
    composite frame
    if the footer panel was invalidated this frame (the invalidate_all path):
        REF = readback(atlas, footer_band_rows)     // re-baseline: footer legitimately repainted
    else:
        assert readback(atlas, footer_band_rows) == REF   // a skipped panel's pixels must not change
```

The invariant asserted is exactly the cache's own contract: **a panel `render_dirty_panels` skips keeps its pixels byte-identical.** Re-baselining after every full clear means the assertion never depends on cross-repaint raster determinism — only on "skipped means untouched." The sequence is the full observed repro (scroll + drawer tween + tab swap, interleaved — the reopen notes' trigger set), not a single scroll (amended 2026-07-09, Fable review: the original single-scroll recipe encoded exactly one suspect and could have fired the timing/environment escalation off an under-powered replay). If BUG-060 fires, some incremental frame's footer band differs from the last baseline — RED. All readbacks come from the same device in the same run, so there is no golden file and no cross-environment drift. The footer band is `[footer_top, footer_top + footer_height)` in atlas rows; readback width stays a multiple of 64 (readback helper constraint, render.rs:841).

Gestures go through the real input paths — `try_inspector_scroll` for scrolls, `pointer_event`/`process_events` for the drawer-header and tab clicks, per the Runner's vocabulary (script.rs:328–420) — so the invalidation signals arise from real `UIRoot` state, never hand-set flags. Default for the tab swap: resolve the tab as an `AutomationTarget` and click it; if the selector cannot resolve it, drive the inspector's own tab-switch API instead and note the substitution in the phase report.

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

### Phasing amendment 2026-07-10 (Fable) — BUG-060 closed out-of-band; P0 restaged

What happened between approval and execution:

- **P0 was built** (2026-07-10, `feat/ui-harness-p0` @ `b51e6bbc`, worktree `ui-harness-p0`) but **not landed**. Its differential ran clean on then-broken `main` — the escalation branch fired, in a form the design didn't enumerate: the bug was deterministic and replayable, but **the asserted bands were the wrong rows**. The differential asserted the footer band `[footer_top, footer_top+h)`; rig screenshots relocated the artifact to the scroll viewport's clip edges INSIDE the inspector (bottom sliver above `footer_top` on both tabs; top sliver under the tab strip on Master). See the BUG-060 backlog entry for the full trail.
- **BUG-060 was then root-caused on the rig, not in the harness**: a flush-time scissor theft (`push/pop_transform`/`push/pop_depth` cut the pending rect run and batched tree rects under the immediate clip). Fixed @ `39836352`, rig-verified, landed on main @ `cc4eeb37` (2026-07-10), then **class-killed** the same day: clip+depth are now bound per command at enqueue and ALL flush-time scissor inference was deleted (invariant recorded in `docs/DEVELOPMENT_REFERENCE.md`, "UI Renderer Invariant"). BUG-060 is CLOSED.
- **The design's thesis is strengthened, not weakened**: the old harness said "0 diff" while the rig showed dirt — a green harness lied again. But the flush machinery P0 renders through has since been refactored by the class-kill, so **every §Audit/§3/§4 anchor into `app_render.rs` and the UI renderer must be re-derived against post-`cc4eeb37` main before any phase runs.**

P0's restaged execution (supersedes the Gate-positive staging below; everything else in P0 stands):

1. Rebase/merge `feat/ui-harness-p0` onto current `main`; re-verify entry-state anchors.
2. **Extend the differential's asserted bands to cover the two clip-edge slivers** (where BUG-060 actually lived) in addition to the footer band. The "stop extending the harness" ruling in the backlog applied to the *hunt*, which is over; band coverage is now informed by a known artifact location.
3. **RED half of the bracket:** run the driver in a worktree checked out at the pre-fix commit (`cc4eeb37^` on the first-parent line, or `39836352^`) — the extended bands must show a non-empty diff of stale UI chrome, confirmed by reading the band PNGs. **GREEN half:** same driver on current `main` shows zero diff. This satisfies D2/D6 with a real known-broken commit; the regression test `transform_boundary_keeps_tree_scissor_on_pending_batch` (landed with the fix) is the unit-level sibling, not a substitute — D6 wants the *harness* proven faithful.
4. Land P0 (BUG-071's backlog entry closes with it), then P1–P3 proceed unchanged.

### P0 — Faithful full-app headless render + inspectable captures (zero live-code change)

> **Superseded framing — the "Reframe 2026-07-10" block at the top governs this phase.** P0 is no longer a differential or a red bracket; it is a faithful render of the whole app to a PNG plus a drawer-tween filmstrip that a reviewer looks at. The **Entry state** and **Read-back** steps below still stand (they establish the real render path); the **Deliverables** and **Gate** are restated here for the reframe. The "Phasing amendment 2026-07-10" block above (red-bracket restaging, clip-edge bands) is retired with D2.

The vertical slice: real fixture → real cache → real pixels → a saved full-app PNG + a drawer-tween filmstrip a reviewer looks at. No extraction yet; the driver calls the public `UICacheManager` API directly in the app's order.

- **Entry state:** `rg -n "fn render_dirty_panels|fn atlas_texture|fn invalidate_inspector|fn invalidate_all|fn ensure_atlas" crates/manifold-renderer/src/ui_cache_manager.rs` matches the audit lines (175/163/126/101/131); `rg "fn panel_cache_info" crates/manifold-app/src/ui_root.rs` exists; `footer_leak_probe` compiles under `--features ui-snapshot`.
- **Read-back (first step):** read `ui_cache_manager.rs:175–281`, `app_render.rs:3882–4064`, `ui_snapshot/mod.rs:420–549` (footer_leak_probe), `ui_snapshot/render.rs:26–145` + `:841` (readback). Restate: the invalidation order the live tick uses, the readback width constraint, and D1/D2/D7/D8 + D9c (the dump fix).
- **Deliverables:** a realistically heavy fixture for general coverage (a heavy generator layer, or the `project:<abs-path>` Liveschool load — no BUG-060-specific tuning); a new test module `cache_path_full_render` under feature `ui-snapshot` (structure like `footer_leak_probe`, rendering the **whole main window** through a real `UICacheManager` + `UIRenderer` + atlas like `present_all_windows`, at scale factor 1.0 per D8 — `cm.set_scale_factor(1.0)` + `ensure_atlas`, mirroring app_render.rs:3901–3902) that drives the §5 gesture sequence (scroll + drawer tween + tab swap) and **saves a full-app PNG plus a contact-sheet filmstrip of the drawer tween** (D9a) as its artifacts. Plus the **BUG-071 dump fix** (D9c): `dump.rs` serializes `tree.parent_index[i]` at :38/:92 instead of the mint-time `parent_id`, and its BUG_BACKLOG entry is closed — every later phase debugs through dumps, so the dump stops lying first.
- **Gate (positive):** run the driver against **current `main`** and produce the full-app PNG + drawer-tween filmstrip. **The gate is the orchestrator reading those artifacts and confirming a faithful render of the whole app — full chrome, correct layout, the drawer visibly opening across the filmstrip tiles, not all black.** The only automated assertion is a smoke check (the readback is non-empty and not a uniform clear colour — it drew *something* sane, no crash). No differential, no baseline file, no red/green bracket.
- **Gate (negative):** `rg "render_ui_to_png|traverse_flat_range" ` in the new test file returns zero hits — proving the driver renders through the cache, not a lookalike (D1). This is the one thing that makes the render *faithful* rather than a re-emulation.
- **Acceptance demo:** the full-app PNG + the drawer-tween contact sheet, read by a reviewer and compared once against the real app on screen. **L2** (a headless render whose artifact is looked at). No flow driver reaches this internal path yet, so L3 arrives at P2.
- **Forbidden moves:** rendering via `render_ui_to_png` or any full-repaint (D1 — that reintroduces the lookalike this whole design exists to kill) · a toy fixture too small to exercise real chrome under load (D7) · reviving the differential/red-bracket as a gate (retired by the Reframe) · shrinking the window for speed (D8).
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
- **Read-back:** read `script.rs:150–260` (Runner + step), `:403` and `:542` (its rebuild), `:651` (its render call). Restate D3, D9 (filmstrip + stamp constraints), and the forbidden-moves list.
- **Deliverables:** the P0 differential driver and `script.rs`'s `Runner` both drive frames through `apply_ui_frame_invalidations` + `composite_main_ui_frame`. The Runner's parallel `rebuild()` invalidation logic is **deleted**, not wrapped. `render_ui_to_png`'s fake whole-tree *panel* pass is replaced by the seam; its genuinely-separate immediate-mode passes (clips, thumbs, automation lanes, overlays — the live app draws these as immediate passes too, `app_render` 4b/5) are kept only after confirming they match the live app's passes. `⚠ VERIFY-AT-IMPL (P2): diff render_ui_to_png's clip/thumb/lane/overlay passes against the live immediate-mode passes; a pass that doesn't match is an escalation, not a silent keep.` Plus the two agent-legibility captures (D9): **filmstrip mode** — save the composited frame after every stepped frame between two script actions (the `Step` clock, script.rs:225; fixed 60 fps DT, :55–56) and assemble a contact sheet (one PNG, N tiles); **pointer stamp** — a crosshair at the Runner's synthesized gesture points (script.rs:343–398), drawn CPU-side on the readback bytes after that frame's assertions have run.
- **Gate (positive):** the two shipped flows `scripts/ui-flows/select-and-inspect.json` and `scripts/ui-flows/drag-clip.json` still exit 0 via `cargo xtask ui-snap <scene> --script <flow.json>`; the P0 differential still brackets BUG-060, **and still brackets it with filmstrip capture enabled on the same run** (capture must not perturb the render). **Gate (negative):** `rg "needs_rebuild|invalidate_layers" crates/manifold-app/src/ui_snapshot/script.rs` returns zero hits (the Runner's parallel decision state is gone); the Runner no longer calls a private `rebuild` (rg proof). One update+composite path, three callers: live app, headless differential, script runner.
- **Acceptance demo:** both ui-flows green through the shared seam, plus a contact-sheet PNG of the inspector drawer tween (8–12 tiles) read by the reviewer. **L3** (scripted flows drive the real input path — the target level since UI_AUTOMATION landed).
- **Forbidden moves:** keeping the Runner's `rebuild` "just for scripts" (adapter/parallel-path — forbidden by name) · silently keeping a `render_ui_to_png` panel pass that diverges from the live passes · drawing stamps or any annotation into a texture an assertion reads — overlays are CPU-side on the readback copy only (D9b) · widening scope to refactor the flow format.
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
2. ~~A harness assertion is trusted only after it is shown RED on a known-broken commit. Green-only is not a gate result. (D2)~~ **RETIRED by the Reframe 2026-07-10** — the harness's output is looked at, not asserted, so there is no green checkmark to falsify. Faithfulness is structural (D3's shared seam) + a one-time eyeball.
3. The shared seam is two functions — `apply_ui_frame_invalidations`, `composite_main_ui_frame` — in `manifold-app/src/ui_frame.rs`; the app and every harness call them. (D3)
4. **DEMOTED by the Reframe 2026-07-10** — the differential atlas-band is no longer the primary assertion; it is a shelf tool, revived only on a reported stale-pixel regression. The primary output is a faithful full-app render looked at by a human/agent. CPU bounds (`footer_leak_probe`) is kept as an honest containment check. (was D4)
5. Unification is the scaffolding (input, readback, assertions, fixtures); each window keeps its own render entry and invariants. The editor is cacheless and never borrows the atlas model. (D5)
6. Order is P0 beachhead (zero live-code change) before P1 hot-path extraction. (D6)
7. Fixtures reproduce the real repro conditions; toy-only fixtures are insufficient. (D7)
8. Drawable acquire/present and the offscreen fast path stay winit-side in `present_all_windows`; the seam ends at the offscreen texture.
9. The harness renders at scale factor 1.0 at the fixtures' logical size, exactly as the live app sizes the atlas (logical dims + scale passed separately). Shrinking the window for speed is forbidden — layout is a function of logical size. Retina (2x) runs are a Deferred variant. (D8)
10. Captures are agent-legible: filmstrip for motion, pointer stamps CPU-side after assertions (never into an asserted texture), and a truthful dump — BUG-071 fixed in P0 in `dump.rs`, not by mutating live UI state. (D9)
11. ~~The differential asserts "a skipped panel's pixels never change," re-baselined after every full clear; the P0 escalation branch fires only after the full §5 repro sequence has been replayed. (§5, amended 2026-07-09)~~ **RETIRED with D2/D4a by the Reframe 2026-07-10** — no differential gates any phase; the §5 gesture sequence survives only as the motion the filmstrip captures.
12. `apply_ui_frame_invalidations` takes `&mut UiFrameSignals` — the block writes residual flags back (the drag-defer case keeps `needs_rebuild` set). (§4, amended 2026-07-09)

## Deferred

- **Monitor / output-window assertions** — revive when a monitor-window render bug is reported or when projection-mapping output needs headless proof. The P3 scaffolding is built to extend to it; only the per-window invariant is missing.
- **The BUG-060 fix itself** — this design delivers the observatory and the gate, not the fix. Revive immediately after P0 brackets it (the fix is a separate change that turns the P0 differential green). If P0's escalation branch fires (bug not reproduced headless), the fix work reopens as a timing/environment hunt instead.
- **CI wiring of the differential** — running the harness on every push. Revive once P0–P2 are green and stable; until then it runs deliberately (GPU path, minutes-long, per CLAUDE.md test-scope discipline).
- **Perform-mode surfaces (track HUD, session grid)** — additional render entries under the same scaffolding. Revive when a perform-surface render bug needs headless proof.
- **Playback-stepped capture (agents watch a timeline play)** — step `PlaybackEngine::tick` (`crates/manifold-playback/src/engine.rs:713`) with a fixed dt alongside the UI frame loop, so playback-driven UI motion (playhead, clip starts/ends) becomes filmstrip-capturable. The engine is an ordinary tickable struct today; LIVE_RECORDING_PROOFS (proposed, unbuilt) plans the same headless stepping for its own ends — coordinate, don't duplicate. Revive when a bug or feature needs headless proof of playback-driven UI motion, or when LIVE_RECORDING_PROOFS starts building.
- **Retina (2x) capture runs** — the same driver with `set_scale_factor(2.0)`. Revive on the first suspected scale-dependent artifact (visible on the MacBook, absent at 1x). Until then every run is 1x per D8.
- **Full-app auto-drive (agents driving the real running app)** — the seam built here is already the plug-in point: an in-app driver would sit exactly where the harness sits, above `apply_ui_frame_invalidations`, with only drawable acquire/present beyond it. Revive under UI_AUTOMATION's later phases; nothing in this design forks the path it will need.
