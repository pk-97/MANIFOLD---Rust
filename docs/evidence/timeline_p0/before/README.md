# Timeline Layout P0.0 — before evidence

Generated 2026-07-04 against `docs/archive/TIMELINE_LAYOUT_P0_SPEC.md`'s P0.0 phase, via
the headless `ui-snap` harness (`docs/HEADLESS_UI_HARNESS.md`). Each command
below is exact — re-run to regenerate, or to produce the P0.1+ "after" set for
comparison. All commands assume cwd = repo root (or worktree root).

| PNG | Command | What it shows |
|---|---|---|
| `01-baseline.png` | `cargo xtask ui-snap timeline --dump` | The 7-layer redesign-mockup fixture (text/video/generator/group+2 children/audio), unmodified. Headers and lanes agree — the reference "good" layout. |
| `02-collapsed-group.png` | `cargo xtask ui-snap timeline --interact "collapse:bg-stack"` (`.after.png`) | BG STACK collapses, hiding CLOUDS/NOISE FIELD. **Headers and lanes stay aligned** — see "Honest findings" below for why this scene does NOT reproduce RC2. |
| `03-post-delete.png` | `cargo xtask ui-snap timeline --interact "delete:flowers"` (`.after.png`) | FLOWERS removed; the layers below reflow. **Headers and lanes stay aligned** — same caveat as above. |
| `04-audio-expanded.png` | `cargo xtask ui-snap timeline --scroll 300 --interact "select:kick"` (`.after.png`) | KICK (audio, not collapsed) scrolled into view. Card fits within its row here because KICK is the *last* layer — nothing below it to overflow into. See caveat below. |
| `05-audio-collapsed.png` | `cargo xtask ui-snap timeline --scroll 300 --interact "collapse:kick"` (`.after.png`) | KICK collapsed. **RC3 reproduces cleanly**: the header row shrinks to `TrackHeight::Collapsed` but the Gain slider + "No send" dropdown still draw at their expanded-row positions, spilling below the collapsed card into blank canvas. |
| `06-shrunk-content-while-scrolled.before.png` | `cargo xtask ui-snap scrollshrink --dump` (base, unscrolled) | Reference: the dedicated 14-layer overflow fixture (`fixtures::scroll_shrink_scene`), top of list, unscrolled. |
| `06-shrunk-content-while-scrolled.png` | `cargo xtask ui-snap scrollshrink --scroll 5000 --interact "collapse:stack-2"` (`.after.png`) | Scrolled to the bottom (clamped to 1843px), then LAYER 2 collapses (content shrinks). **RC1 reproduces**: an orphan lane clip-strip renders with no corresponding header row above it in frame, and each header row's lane content sits at a visibly different vertical offset than its header — the header column and lane column disagree about which rows are in view. |

## Harness additions made to capture these (all in `crates/manifold-app/src/ui_snapshot/`)

P0.0 is repro-only; these are the minimum scaffolding needed to drive scenes
the existing harness didn't yet support. No production code changed.

- `interact.rs`: two new verbs, `collapse:<layer-id>` (toggles `is_collapsed`
  directly on the `Project` layer — the same field `fixtures::states_scene`
  already sets directly, not a synthesized chevron click) and
  `delete:<layer-id>` (removes the layer and any children from
  `project.timeline.layers`, mirroring what `EditingService`'s delete command
  achieves at the data level).
- `mod.rs`: a `--scroll <px>` flag. Seeds `Viewport::scroll_y_px` AND the
  header panel's `ScrollContainer` offset to the same value, right after the
  base render (once the mapper's real `total_content_height()` is known) and
  before any `--interact` — mirrors `ui_root.rs:512-517`'s settings-restore
  path exactly. Not a general "scroll" interact verb: it seeds state that
  predates the interaction under test, not an action being tested.
- `fixtures.rs`: a new `scrollshrink` scene (`scroll_shrink_scene`) — 14
  uniform video layers, deliberately more than the `timeline` scene's 7 (which
  was sized to exactly fit `LOGICAL_H`), so a vertical scrollbar exists and
  `--scroll` is meaningful.

## Honest findings (per the spec's "if a state cannot be reproduced headless, say so")

**RC1 (dual scroll state) reproduces headless — scene 06 above.** Read
`Viewport::set_scroll` (`crates/manifold-ui/src/panels/viewport.rs:757`) and
`ScrollContainer::set_content_height`/`clamp_scroll`
(`crates/manifold-ui/src/scroll_container.rs:95-98`): the header's
`ScrollContainer` self-clamps its offset against its own measured content
height on *every* build (since `set_content_height` runs every build and
calls `clamp_scroll()` unconditionally), while `Viewport::scroll_y_px` is
*never* auto-clamped by any structural-sync path — only by an explicit
`set_scroll()` call, which `sync_project_data`/`rebuild_mapper_layout` never
make. So after scrolling, a content-shrinking edit self-corrects the header's
offset but leaves the viewport's stale — exactly RC1's symptom, and this
mechanism holds regardless of whether the resync is full or partial.

**RC2 (copied Y offsets, stale until the next `needs_structural_sync`) did
NOT reproduce headless — scenes 02 and 03 above render correctly aligned,
not detached.** `needs_structural_sync` is a dirty-flag on `Application`
(`crates/manifold-app/src/app.rs:532`, gated in `app_render.rs:2438`) that
lives entirely outside the `ui_snapshot` harness's path — `sync_build`
(`ui_snapshot/mod.rs`) calls `sync_project_data` unconditionally on every
render, so this harness never skips a resync and can't demonstrate the "missed
resync leaves the header's `LayerInfo` copies stale" failure mode. Reproducing
RC2 headless would need the harness to drive `Application`'s own dirty-flag
gate (or a comparable partial-rebuild path), which is out of this phase's
scope — flagging for whoever designs P0.1's regression test.

**RC3 (audio height rule) reproduces headless — scene 05 above, cleanly.**
No interact-path subtlety involved; it's a pure static-layout defect.

**Caveat on scene 04/05:** KICK is the *last* layer in the `timeline` fixture,
so nothing renders below it — these scenes show the audio card's own internal
row-cramming but can't show content "bleeding into the next card" (the other
half of RC3's symptom, per the spec's D4). A P0.1/P0.2 fixture with a layer
*after* the audio layer would be needed to capture that half.
