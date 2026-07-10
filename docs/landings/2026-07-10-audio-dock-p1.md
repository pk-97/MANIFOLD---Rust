# Audio Setup Dock & Trigger Unification — P1 landing

**Phase:** P1 — the dock (layout column + overlay-path deletion + scroll) · **Level reached: L3** (headless full-app PNG read by orchestrator + `scripts/ui-flows/` flow driving the real input path, exit 0)
**Orchestrator:** Opus · **Worker:** Sonnet
**Base:** `ab215ab8` · **Worker content:** `36a96791` · merged `origin/main` `5c0f86f2` in before landing (clean, disjoint file sets) · landing merge on `main`

## What shipped

The Audio Setup panel is now a fold-out `ScreenLayout` column pinned to the inspector's left edge — it expands leftward when opened, pushing preview + timeline to shrink, inspector stays anchored right. The modeless overlay path is **deleted**, not paralleled.

- **`ScreenLayout` (`crates/manifold-ui/src/layout.rs`)** — new `audio_setup_width: f32` input (0.0 = closed) + `audio_setup_width_anim`; `content_area()` subtracts both inspector and dock width; new `audio_setup()` rect (`x = screen_width − inspector_width − audio_setup_width`, full height, zero-guard mirroring `inspector()`); `reset_audio_setup_width()` + `is_split_reset_animating()`/`tick_splits()` extended for the double-click snap-back. `DEFAULT/MIN/MAX_AUDIO_SETUP_WIDTH` consts in `color.rs`. 5 new unit tests (open shrinks both preview + timeline; zero-width = today's rects byte-identical; column sits exactly between content and inspector; reset tween settles).
- **Overlay deletion (`ui_root.rs` + `audio_setup_panel.rs`)** — `OverlayId::AudioSetup` enum arm / Z-order / `overlay_mut` / `overlay_is_open` deleted; the dock is built from the root pass into `layout.audio_setup()` when width > 0. `compute_overlay_rect`, `Modality`, `impl Overlay`, `PANEL_W_FRAC`/`PANEL_H_FRAC` removed from the panel; `build()` → `build_docked(rect)`. Escape + toggle wired through a single new `UIRoot::toggle_audio_dock()` called by both `app_render` and the UI bridge (root fix for a misfit — see below). Dock resize-handle press/drag/double-click in `window_input.rs`, hover feedback in `app.rs`.
- **Scroll / BUG-047** — panel body wrapped in a `ScrollContainer` (`guide_scroll_and_clipping`: GPU scissor, no hand-rolled clip math); `scope_h` is now a fixed fraction of the panel rect so control rows overflow into the scroll region instead of clamping the spectrogram at its floor. Closes BUG-047.

## Misfit found + resolved (not adapted around)

The doc's "header Audio button" is a **menu item** (`M::Audio` → `PanelAction::OpenAudioSetup`), not a directly-clickable header button, and `OpenAudioSetup` was a no-op in `ui_bridge::dispatch` (the toggle lived only in `app_render`, unreachable from the script harness). Rather than shim, the worker escalated and the root fix landed: one `UIRoot::toggle_audio_dock()` called by both paths — the toggle now has a single home. This is the correct seam fix, not a workaround.

## Gate (orchestrator-verified, post-merge on the branch)

- `cargo test -p manifold-ui --lib`: **654 passed, 0 failed** (orchestrator re-ran after merging origin/main).
- `cargo clippy -p manifold-ui -p manifold-app --features manifold-app/ui-snapshot -- -D warnings`: **clean** (orchestrator re-ran; only pre-existing manifold-media C-code deprecations, unrelated).
- **Negative gates** (orchestrator re-ran independently): `compute_overlay_rect|Modality|PANEL_W_FRAC` in `audio_setup_panel.rs` → **0**; `OverlayId::AudioSetup` in `crates/` → **0**.
- Test scope was focused per the phase brief; the workspace sweep is the wave's final phase (P4), not this one.

## Demo (L3) — artifacts read by the orchestrator

- **Full-app PNG, dock open** — `target/ui-snapshots/bug047/bug047.after.png`: preview (shrunk) + timeline (shrunk, ruler bars 1–3) + Audio Setup dock (20 sends + spectrogram, close ×) + inspector (Bloom/Amount) **all visible at once**. Orchestrator viewed it: the column sits between content and inspector exactly as designed; inspector is not occluded.
- **Toggle, closed state** — `run-audio-dock-toggle/07.png`: after Escape the dock fully collapses, preview + timeline reclaim full width (ruler now bars 1–8), inspector anchored right. One layout rule, no residual dock artifact.
- **L3 flow** — `scripts/ui-flows/audio-dock-toggle.json`, **exit 0, 9/9 steps**: opens the dock (⌘⇧A, the keyboard route through the identical `OpenAudioSetup` toggle), asserts an inspector param row is still clickable while open, closes via Escape.

## Click-script for Peter (≤2 min)

1. Open the app, open a project with audio sends. **Expect:** normal layout, no dock.
2. Trigger Audio Setup (menu → Audio, or ⌘⇧A). **Expect:** a column folds out from the inspector's left edge; preview + timeline shrink to make room; inspector stays put and stays usable.
3. Drag a bound param's slider in the inspector while the dock is open. **Expect:** it responds — the dock never covers the inspector.
4. Drag the dock's left edge to resize; double-click that edge. **Expect:** width changes on drag; double-click eases back to the default width.
5. Select a source with many input/consumer rows; scroll the dock body. **Expect:** sections scroll, nothing clips past the bottom (BUG-047). **Known issue (BUG-101):** the spectrogram waterfall does not yet follow the scroll offset — cosmetic, logged.
6. Press Escape. **Expect:** dock collapses, preview + timeline reclaim the space.

## Shortcuts / honest gaps

- **BUG-101 logged (not fixed):** the spectrogram GPU blit rect (`scope_rect`) is computed at build time without the scroll offset, so a scrolled dock body draws the waterfall at its pre-scroll position. Correct at `scroll_offset == 0` (the default, and everything the gate exercises). LOW.
- **Dock open-state + width are not persisted** (default closed each session); inspector width *is* persisted. Deliberate — a transient calibration surface defaulting closed is reasonable — but an asymmetry. Flagged for Peter; revisit if width-persistence is wanted. Not a bug entry.
- **Narrow-width feel** — at inspector 500 + dock 460 the content column gets genuinely small (Peter's decided tradeoff; mitigations are the resize handle + snap-back + Escape). Not headless-judgable — **Peter's feel-pass**.
- **Live spectrogram position while scrolled, and live band-divider calibration drag** — L4, need Peter in-app; routing proven by the `ui_root` immediate-drag test.

## Orchestrator note — harness gap (FINDING)

`git worktree add` does **not** carry the gitignored `*.manifold` fixtures, so the P1 worker's fixture check correctly STOPPED on an empty `tests/fixtures/`. The orchestrator copied the 6 canonical fixtures from the main checkout into the worktree, then resumed. This is the known `agent-execution-playbook` hazard; logged here so the pattern (orchestrator provides fixtures, or the worker brief bakes in a copy step) carries to the P2–P4 worktrees.
