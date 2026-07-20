<!-- index: Timeline layout P0 — kill the header/lane detach bug class: single Y source + single scroll offset read at draw time; audio card fits the height rule. Runs BEFORE UI_CRAFT_AND_MOTION_PLAN in the UI lane. -->

# Timeline Layout P0 — headers and lanes must be un-detachable

**Status: SHIPPED 2026-07-04 (`lane/timeline-p0` → main; phase commits `37cf28d4`
P0.0 · `82ce2f35` P0.1 · `c66c7e63` P0.2 · `b9ab3a7b` P0.3 · `d145ca5a` P0.5 ·
`1168f067` P0.4; evidence at `docs/evidence/timeline_p0/`). All six phases merged;
ancestors of `8b306de0`. Unblocked: UI_CRAFT_AND_MOTION_PLAN and
TIMELINE_INTERACTION_P1 (which now sits on the single Y source this delivered).
Original approval (Peter, 2026-07-04): "layer heads and layers must fundamentally
not split apart."**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 + §8 first. Anchors
below are a 2026-07-04 snapshot — re-verify before each phase.**

## Symptom (Peter, in-app)

Layer headers and their track lanes visibly detach when layers are expanded,
collapsed, or deleted. Audio layer headers overflow their rows. Third recurrence
of a known bug class — see `feedback_single_source_y_layout` (2026-03-18) and
`feedback_track_header_invariant` (waveform-lane 56px float-apart).

## Root causes (code-verified 2026-07-04; repro pending P0.0)

**RC1 — duplicated scroll state.** Viewport owns `scroll_y_px`, clamped against
`mapper.total_content_height()` in `Viewport::set_scroll`
(`crates/manifold-ui/src/panels/viewport.rs:757-767`). The header panel owns an
independent `ScrollContainer` offset (`panels/layer_header.rs:751-752, 799-801`),
updated ONLY from scroll input (`crates/manifold-app/src/window_input.rs:474`)
and settings restore (`ui_root.rs:517`). Any programmatic viewport scroll change
— including the re-clamp when a collapse/delete shrinks content height, and the
auto-scroll at `viewport.rs:686` — moves lanes without moving headers.
Persistent until the next user scroll event.

**RC2 — headers draw from copied Y values.** `sync_project_data`
(`crates/manifold-app/src/ui_bridge/state_sync.rs:791-812`) rebuilds the mapper,
then copies `get_layer_y_offset/height` into each `LayerInfo`
(`panels/layer_header.rs:269-271`). Headers render from those copies; lanes read
the mapper live (`panels/viewport/coordinate.rs:86`). The copies are only correct
if `needs_structural_sync` (`app_render.rs:2438`) fires after every mutation that
moves layout, forever. Every missed or late path = drift for that window.

**RC3 — audio card violates the height rule.** The mapper's height rule is
deliberately state-only — same row height for video/generator/audio/text,
`TrackHeight::Collapsed` when collapsed (`coordinate_mapper.rs:241-262`,
doctrine in `docs/TIMELINE_API_DESIGN.md`). But audio cards "never collapse
their detail controls" (`panels/layer_header.rs:575`, `compute_audio_row`), so
the card's content ignores the row height the mapper assigned. Overflow = the
"audio headers don't fit" symptom.

Cleanup, same class: dead `navigate_cursor` computes heights inline
(`if l.is_collapsed { 0.0 } else { 140.0 }`, `crates/manifold-app/src/app.rs:895`)
— delete it or route through the mapper; no inline height math may survive this pass.

## The fix — remove the state, not the misalignment

Do NOT fix by syncing the copies more often (per-frame `set_scroll_y` pushes,
extra `needs_structural_sync` triggers). Copies that must be kept in sync are
the mechanism of this bug class; the fix is that no second copy exists.

- **D1 — one Y source, read at draw time.** `LayerInfo` loses `y_offset` and
  `height` (compiler enforces no consumer remains). Header panel build/render
  takes `&CoordinateMapper` (or an equivalent borrowed view) and queries
  `get_layer_y_offset/height` per layer at draw time — the exact values lanes
  use, same frame, same rebuild.
- **D2 — one scroll offset.** The header panel's `ScrollContainer` stops owning
  a vertical offset. One owner (the viewport, which already clamps), both
  columns read it at draw time. `window_input.rs:474` push and the settings
  restore path collapse into that single owner. Scroll wheel over the header
  column routes to the same owner it already targets today.
- **D3 — clamp at rebuild.** `rebuild_mapper_layout` re-clamps the shared
  scroll offset against the new `total_content_height` immediately, so a
  collapse/delete that shrinks content moves both columns in the same frame.
- **D4 — audio cards obey the height rule.** Confirmed in-app (Peter,
  2026-07-04): the gain slider + dB label overflow into the next card and the
  Send dropdown lands invisibly below it. `compute_audio_row`
  (`panels/layer_header.rs:576-607`) stacks name / M|S|A / Gain / Send — four
  rows in a `TrackHeight::Normal` card that fits three — with no height check
  and no per-card clip. Fix: collapsed audio shows collapsed chrome exactly
  like every other type (drop the never-collapse exception at `:575`), and the
  expanded card goes three rows — the Gain slider joins the M|S|A button row
  (the slot video cards give BLEND), Send keeps the third row. The state-only
  height doctrine stands; if that layout genuinely can't fit, taller-audio-row
  is a must-escalate question, not an adaptation. Additionally: card content
  must be clipped to the card rect (per `subregion-scissor-invariant`) so a
  future overflow is a visible truncation inside its own card, never a bleed
  into the neighbor.

## Phases

- **P0.0 — Repro + evidence.** Headless `ui-snap` timeline scenes: baseline,
  collapsed-group, post-delete, audio-expanded, audio-collapsed, and a
  shrunk-content-while-scrolled case (scroll to bottom, collapse a tall layer).
  Commit the PNGs as the before set. This proves which channels fire and is the
  comparison basis for the after set. If a state cannot be reproduced headless,
  say so in the report — do not claim it.
- **P0.1 — D1 + D2 + D3.** One commit-able unit. Gates: workspace builds with
  `LayerInfo` fields gone (no `#[allow(dead_code)]` shims); focused tests
  `-p manifold-ui --lib` incl. new tests — scroll re-clamp on rebuild, and
  header/lane Y agreement (assert both columns resolve the same Y for every
  layer in collapsed/hidden-child/group fixtures); after-PNGs of every P0.0
  scene with headers and lanes aligned.
- **P0.2 — D4 audio fit.** Gates: audio-expanded and audio-collapsed PNGs show
  the card fully inside its row; `collapsed_layer_has_no_expanded_controls`-
  style test extended to audio.
- **P0.3 — Sub-pixel clips never vanish.** `layer_clip_rects` culls any clip
  narrower than 1px (`crates/manifold-ui/src/panels/viewport.rs:567`,
  `if w < 1.0 { continue; }`) — at far zoom, short trigger clips (the MIDI-
  mockup workflow's bread and butter) disappear entirely. Change the cull to
  offscreen-only and clamp width to a 1px hairline (`w.max(1.0)`), matching
  the overview strip's existing rule (`viewport/render.rs:125`). Check group
  summary rows and any other per-clip screen-rect path for the same pattern.
  Gate: headless PNG at far zoom over a dense short-clip lane — every clip
  present as a hairline; plus the P0.0 scenes unchanged.
- **P0.5 — Generator label back on the card, both states.** (Peter,
  2026-07-04: "add back info… so the layer controls show what generator is
  used.") The data is already synced — `LayerInfo.generator_type`
  (`panels/layer_header.rs:253`, populated at `ui_bridge/state_sync.rs:839`) —
  but no control renders it. Add a label control: expanded cards show the
  generator name on its own line under the layer name (video layers keep their
  folder text there — same slot, source-type-appropriate text); collapsed
  cards show it as dimmed text right of the name, ellipsized, never widening
  the row. Display only — no picker behavior in this pass. Gates: unit test
  that the control exists iff `generator_type.is_some()`; collapsed +
  expanded PNGs showing the label in both.
- **P0.4 — Sweep.** `rg` for inline track-height/Y math outside
  `coordinate_mapper.rs` (`140.0`, `TrackHeight::`, cumulative `y +=` loops
  over layers) — delete or route through the mapper (incl. `app.rs:895` dead
  code). Gate: the search comes back clean and is pasted into the report.
  Clippy workspace clean.

Escalation rule: any anchor that doesn't match, any consumer of the deleted
fields that can't draw-time-query the mapper (threading/borrow constraints),
or the D4 taller-row question → STOP that item, report file:line, don't adapt.

## Forbidden shortcuts

Re-aligning by pushing sync more often; keeping the copied fields "for
performance" (a per-layer f32 read per frame is nothing); a second "mirror
mapper" owned by the header panel; fixing audio by special-casing its height
in the mapper without escalating.
