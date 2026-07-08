# Footer Overpaint / Disappearance — Investigation Handoff (for Fable)

**UPDATE 2026-07-08 (Fable): fork §5 resolved — it's (B).** A patched trace
(commits `28d74981`, `6332df8f`: footer-pass draws included, clip early-outs
logged) plus one live repro proved all 8 footer nodes draw with correct bounds,
`scissor=None`, on every clear frame. The footer is innocent; something
overwrites its atlas pixels afterward. §4.1's "scissor proof" only ever covered
tree-node draws — immediate-mode draws are untraced and remain the prime
suspect, consistent with the "near-black" bad pixels matching inspector
card/drawer backgrounds (~18), which §7's "blue overpaint ruled out" reasoning
wrongly generalized. **Resolution path chosen (Peter):** don't hunt the single
leaking element — execute `docs/UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` P1 (binding
per-region GPU scissors), which kills the class by construction. A per-pass
`[FOOTER-BAND]` probe is built on this branch if pass-level attribution is ever
still wanted. This branch's binary is the verification rig for P1.

**Status: UNSOLVED.** Two shipped "fixes" did not fix it. This document is the full
history, evidence, reasoning, and the current impasse. It is written to let a fresh
reasoner (Fable) pick up without re-deriving. Author: Opus, 2026-07-08 session.

Instrumentation lives on branch `fix/bug-060-footer-leak-trace` (worktree
`.claude/worktrees/footer-leak`). **None of the debug code is on `main`.** The two
prior *fixes* (BUG-060, BUG-015) ARE on main.

---

## 1. The symptom (Peter, verbatim + clarified)

- With an audio-modulation drawer open on an effect card, **scrolling the inspector
  causes the footer bar's controls (Q: quantize, FPS) to disappear / be obscured.**
- "If I close the audio mod drawer it shows the footer; if I scroll up it then hides
  the footer again."
- "It's during scrolling **100%**."
- "The footer renders correctly in **some** scenarios" (drawer closed; certain scroll
  positions).
- Early in the session Peter described "blue artifacts" and a stray blue rounded rect.
  **That was a red herring** — the real, consistent phenomenon is the footer's right
  half going **dark/missing**, not a blue overpaint. (The earlier "blue video patch"
  was separately resolved as the preview window, not a bug — see BUG-015 verdict §3.)

The footer is the global bottom bar: left = a selection status label ("Layers: N |
Clips: M"), right = `Q:` + quantize button + `FPS:` + fps field. The **right** group
sits horizontally under the inspector (the inspector is the right-edge sidebar).

---

## 2. The two prior fixes (both on `main`, neither fixed this)

### BUG-060 — "Inspector content paints over the footer bar" — shipped `27557d18`

**Claimed root cause:** (1) the inspector had no pixel clip of its own — `build_in_rect`
created no `CLIPS_CHILDREN` container, so any node past the inspector's bottom edge
would paint into the footer's atlas region; the inspector renders *after* the footer in
the atlas panel loop, so its spill would win and persist. (2) `build_toggle_trigger_row`'s
drawer lacked the `drawer_reveal` mid-tween clip that `build_param_row` has (~119.5px of
unclipped paint below the card frame mid-animation).

**Fix shipped:** (1) `build_in_rect` now mints a `CLIPS_CHILDREN` node bounded to
`(rect.x, columns_y, rect.width, columns_h)` and sweeps both scroll columns under it via
`reparent_root_nodes` ([inspector.rs:1966](../crates/manifold-ui/src/panels/inspector.rs#L1966),
sweep at :2131). (2) `build_toggle_trigger_row` gained the `drawer_reveal` param + mid-tween
`ClipRegion`.

**Honest caveat in the original entry:** fix #2 is proven by a regression test. Fix #1
(the clip container) was shipped as **defense-in-depth** and the author wrote that they
"could not independently reproduce a settled-state footer overpaint this fix alone
closes" — because `ScrollContainer::reparent_content` already swept card content under
its column clip. **This session proved fix #1 addresses a non-problem** (see §4.1).

### BUG-015 — "incremental cache drops out-of-sub-region dirt" — shipped `4319eb8d` (this session)

A **different** bug, fixed correctly, but **explicitly not the footer bug.** The UI atlas
cache's incremental path repainted only dirty card sub-regions and dropped dirt in the
panel range belonging to no sub-region (tab strip, chrome), which the blanket
`clear_dirty` then erased — producing stale *chrome* (ghost hover, stale scrollbar). Fix:
incremental path falls back to full render on out-of-sub-region dirt
(`has_dirty_outside_ranges` + `incremental_path_safe`), blanket clear narrowed to the
overlay region. It is documented as a stale-chrome-state fix, not a footer fix. Included
here only because it's adjacent (same `UICacheManager`) and shipped this session.

---

## 3. How the UI compositing works (the model everything below assumes)

- The main-window UI is composited into **one full-screen atlas texture**
  (`UICacheManager`, [ui_cache_manager.rs](../crates/manifold-renderer/src/ui_cache_manager.rs)).
- Each **panel** (Transport, Header, Footer, Inspector, SplitHandles, LayerHeaders,
  Viewport) renders into the atlas at its screen position with `LoadOp::Load` (preserve).
  Panel order is Transport, Header, **Footer(2), Inspector(3)**, … — footer renders
  *before* the inspector.
- On a full rebuild (`invalidate_all`, e.g. opening a drawer) the atlas is **cleared to
  (0,0,0,0)** and all panels re-render. On an in-place inspector scroll,
  `invalidate_inspector` re-renders **only** the inspector; the footer's atlas region is
  untouched ([app_render.rs:963](../crates/manifold-app/src/app_render.rs#L963)).
- The atlas is blitted to the offscreen (pass 2), then compositor video into
  `video_area` (pass 3), timeline (pass 4), overlays drawn straight into the offscreen
  (pass 5), then offscreen → drawable.
- `render_dirty_panels` renders each panel via `render_tree_range` (full) or
  `render_sub_region` (incremental flat traversal of one dirty card). Both apply clip
  regions; nested clips **intersect** ([ui_renderer.rs:1067](../crates/manifold-renderer/src/ui_renderer.rs#L1067)).

---

## 4. What we PROVED this session (with evidence)

Instrumentation (env `MANIFOLD_TRACE_FOOTER_LEAK=1`):
- `draw_node` trace: fires whenever any UI node paints into the footer band, tagged by
  render pass (`inspector`, `footer`, `overlay`, `layer-headers`, …), printing node id,
  bounds, and the **effective scissor**.
- Sequential atlas PNG dumps (atlas texture read back to `/tmp/atlas_dumps/atlas_NNNN.png`).
- Per-frame `FOOTER-DBG` log: footer + inspector render decision, rect, node range,
  `did_clear`; plus (latest, not-yet-run build) each footer node's bounds/visibility/text.

### 4.1 The inspector's clip WORKS. No draw reaches below the footer line.

Across **two** full traced sessions with Peter actively reproducing the bug, **every**
draw that reaches the footer band has a scissor clamped **exactly at the footer top**.
Only two distinct scissors ever appear on fires:
- `Rect { x:1000, y:94, w:492, h:775 }` → y_max = 94+775 = **869** (inspector column)
- `Rect { x:0, y:715.1, w:230, h:153.9 }` → y_max = 715.1+153.9 = **869** (layer-headers)

`footer_top = 869`. **Zero** draws had a scissor permitting paint below 869. The nodes
themselves extend to y=887 (bottom cards straddling the line) but are scissored at 869.

**Conclusion: the inspector cannot and does not paint into the footer region.** The
entire BUG-060 premise ("inspector content escapes its clip into the footer") is refuted.
The clip was always clamping. Fix #1 of BUG-060 fixed nothing real.

### 4.2 The footer's RIGHT half goes dark; the LEFT half stays correct.

Atlas dumps, footer band [869,905] logical / [1738,1810] physical, measured per frame:
- **Good frames:** footer-right (Q/FPS, x≈1300–1486) has bright pixels, max RGB **230**
  (the text). Footer-left (status label) max **230**, with a live/updating clip count.
- **Bad frames:** footer-right max **13–16** — *darker than the footer's own background*
  `PANEL_BG_DARK = (22,22,24)`. So it is not the footer bar with text missing; the bar's
  **background is gone too**. Near the atlas clear color. Footer-**left** stays max 230.

Row scan under the inspector in a bad frame: y 850–865 (inspector's last card) ≈ dim (bg
~18), y 868–904 (footer band) ≈ near-black (~9/px). Visual zoom shows a faint card border
above the line and near-black below it — **no footer bar under the inspector at all.**

### 4.3 The footer renders only on clear frames; its rect is always full-width.

`FOOTER-DBG` over a session: footer **"render" 20×** (all `did_clear=true`), **"SKIP"
269×**. Footer rect **always** `(0,869,1496,36)` — full width, never narrow. Node range
**always** `[48,56)` = 8 nodes. This is the intended static-panel behavior: the footer
only re-renders on a full atlas clear (`invalidate_all`), and is skipped otherwise
(`panel_valid=true, dirty=false`).

### 4.4 The good/bad state toggles with scroll, and is frozen between clears.

Dump sequence (one every 12 frames): good 1–9, **bad 10–16**, good 17–19, **bad 20–24**.
Because the footer only renders on clear frames and nothing else writes its atlas region,
**the footer-right pixels are frozen between clears.** So each bad stretch is the result
of one *clear-frame footer render that produced a dark right half*, persisting until the
next clear. The toggling therefore means **different clear-frame rebuilds produce
different footer outcomes**, and which one you get correlates with the inspector's
scroll/drawer state at that rebuild (matches Peter: drawer-open + scrolled = bad).

### 4.5 A GPU resize fires at the first bad frame — but is not the toggling cause.

`GPU resize sent: 3456x2234 @ 1.00x render scale` + `resized IOSurface to 1484x960`
appears exactly at the first bad frame (dump 0010). But: it's a **content render-scale**
change (preview surfaces), the **UI atlas stayed 2992×1810 the whole session**, and it's
a one-time event so it cannot explain good→bad→good→bad. Layout and atlas agree the
logical screen is 1496×905 (footer at 869); **no atlas/window size mismatch was found.**
Worth a second look by Fable only to rule out that the resize triggers an atlas
recreate/clear-without-footer-repaint path despite the constant size.

---

## 5. The paradox (why this is stuck)

The footer is **one panel, drawn atomically** by a single `render_tree_range(48,56)` — 8
nodes: full-width dark background row + left status label + `Q:` label + quantize button
+ `FPS:` label + fps field ([footer.rs `view()`](../crates/manifold-ui/src/panels/footer.rs#L108)).
It renders full-width on clear frames (§4.3). And **no draw_node call ever paints below
the footer line** (§4.1).

Yet on some clear-frame rebuilds the footer's right half — **background included** —
comes out near-black while the left half is correct (§4.2). For a single atomic render
that emits all 8 footer nodes, "left painted, right dark" should be impossible unless:

- **(A)** the footer's Q/FPS (and the right portion of its background) are **misplaced or
  hidden** on those specific rebuilds (wrong bounds, `VISIBLE` cleared, zero width, or the
  ChromeHost wrapping the row in a `CLIPS_CHILDREN` clipped to a too-narrow rect), **or**
- **(B)** the footer-right is **overwritten after** the footer draws, by something that is
  **not** a `draw_node` call (since all draw_node draws are scissored at 869) — e.g. the
  atlas clear, a second/partial clear, a GPU copy, or a render-pass load/viewport quirk in
  `prepare_and_draw`.

We have not distinguished (A) from (B). Everything measured is consistent with both.

---

## 6. Next step already staged (not yet run)

The latest committed build adds a `FOOTER-DBG   node N: bounds=… visible=… text=…` line
per footer node per frame. Running it and reading a **bad clear-frame** answers §5
directly: if Q/FPS bounds are correct and `visible=true`, it's **(B)** an overwrite (and
the hunt moves to non-draw_node atlas writes — instrument `prepare_and_draw` / the atlas
encoder). If bounds are wrong / off-atlas / `visible=false`, it's **(A)** the footer's own
layout/reconcile under the drawer-open+scrolled rebuild.

A complementary decisive test not yet built: **dump the atlas immediately after the footer
panel renders** (mid-`render_dirty_panels`, on clear frames), then again at end of frame.
If footer-right is bright right after the footer draws and dark at end → (B), and the dump
after the *next* panel isolates which panel. If dark right after the footer draws → (A).

Run:
```
MANIFOLD_TRACE_FOOTER_LEAK=1 "/…/.claude/worktrees/footer-leak/target/debug/manifold" 2>/tmp/footer-leak.log
```
Analysis helpers used this session (crop footer band, measure brightness per frame) are
straightforward `PIL` one-liners over `/tmp/atlas_dumps/atlas_*.png`.

---

## 7. Ruled out (do not re-chase)

- Inspector content escaping its clip into the footer — **refuted** (§4.1, scissor proof).
- BUG-060 fix #1 (inspector clip container) as the cure — it fixed a non-problem.
- The trigger-row drawer's missing reveal clip (BUG-060 fix #2) — real but unrelated; that
  clip only exists mid-tween, and the footer breaks in settled states.
- Stale out-of-sub-region dirt (BUG-015) — different mechanism, fixed, not this.
- Blue overpaint / video bleed — red herring; the phenomenon is the footer-right going
  **dark/missing**, and the compositor writes video only into `video_area`, never the footer.
- A too-narrow footer rect or a skipped footer render — refuted (§4.3: rect full-width,
  renders on every clear frame).

---

## 8. Open questions for Fable

1. **(A) vs (B):** run the node-bounds build; read a bad clear frame. This is the fork.
2. If **(B)**: what writes the atlas footer region besides scissored draw_node calls?
   Suspects: the clear path, `prepare_and_draw`'s render-pass setup (`LoadOp::Load` +
   viewport/offset via `prepare_with_offset`), or an encoder ordering issue on clear frames.
3. If **(A):** why does the drawer-open + scrolled rebuild misplace/hide the footer's
   right group specifically, when the footer rect is full-width and the golden layout test
   passes? Does the ChromeHost row add a clip? Does `self.rect` (captured at footer build)
   diverge from `layout.footer()` when a rebuild races the GPU resize?
4. Why is the state **frozen per clear** yet **toggles** across the session — i.e. what in
   the inspector's scroll/drawer state deterministically flips the footer's clear-frame
   render between correct and broken? Reproducing that exact rebuild in a headless harness
   (driving `render_dirty_panels` full→scroll→full and reading the atlas) would make this
   deterministic — current headless PNG (`render_ui_to_png`) bypasses the cache entirely,
   which is why this class has been so slippery.

---

## 9. Cleanup owed once fixed

All instrumentation is isolated on `fix/bug-060-footer-leak-trace`. When the root cause is
found and fixed, either strip the trace (fields/methods tagged `BUG-060 footer-leak trace`
in `ui_renderer.rs` and `ui_cache_manager.rs`, the app.rs armed-log call, the app_render
pass labels) or delete the branch+worktree and land only the real fix. Also revisit
whether BUG-060 fix #1 (the inspector clip container) should stay — it's harmless
defense-in-depth but it fixed a non-problem, and its existence misled this investigation.
