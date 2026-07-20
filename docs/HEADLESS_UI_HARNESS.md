# Headless UI Harness — an agent-facing tool for MANIFOLD's UI/UX

**Status:** BUILT 2026-06-28 (Phases 1–3), branch `feat/timeline-ui-redesign`. A feature-gated
subcommand of `manifold-app` (feature `ui-snapshot`), run via the `cargo xtask` alias.
Extended 2026-06-30 with the `inspector` and `graph` scenes, and 2026-07-01 with the `editor`
scene (see **Scenes** below).

**Testing doctrine (2026-07-20):** Pixels are for looking, not asserting. Nearly every UI bug of
2026-07 was a state/wiring bug with a visual symptom — PNG assertions are slow, GPU-bound, and
green while click paths are dead. The gate tests state on the REAL dispatch path (real
EditingService, real state sync): hit-test geometry as pure math, click→command dispatch,
display-value resolution (BUG-260 conviction test and the undo baseline `1bdb69a9` are the only
permitted patterns — replicate, never invent harness). Headless PNG render stays as an on-demand
look oracle for humans/Fable, out of the automated gate. See
`docs/SYSTEM_UPGRADE_2026_07_PLAN.md` §Testing doctrine.

**Usage:**
```
cargo xtask ui-snap timeline --dump                      # whole timeline + tree dump
cargo xtask ui-snap timeline --interact "select:plasma"  # drive a real click; base + .after
cargo xtask ui-snap states                               # state matrix (6 states in one image)
cargo xtask ui-snap inspector                            # inspector: layer + effect chain + mod drawer
cargo xtask ui-snap graph                                # node-graph editor canvas (default: Mirror)
cargo xtask ui-snap graph --preset Tesseract             # any effect/generator's graph
cargo xtask ui-snap editor --preset FluidSimulation      # FULL editor window: preview | canvas | card
cargo xtask ui-snap timeline --vs-mockup                 # app | mockup side-by-side
cargo xtask ui-snap timeline --thumbs                    # inject a test atlas into the clips
cargo xtask ui-snap all                                  # render every scene in one sweep
cargo xtask ui-snap timeline --interact "collapse:kick"  # toggle is_collapsed on a layer; base + .after
cargo xtask ui-snap timeline --interact "delete:flowers" # remove a layer (+ children); base + .after
cargo xtask ui-snap scrollshrink --scroll 5000 --interact "collapse:stack-2"  # scroll + shrink content
cargo xtask ui-snap hairlineclips --dump                 # far zoom (1px/beat), 200 sub-pixel trigger clips
cargo xtask ui-snap diff a.tree.json b.tree.json          # node-level diff of two tree dumps; exit 1 if any differ
cargo xtask ui-snap states --probe "100,50;200,80"        # sample pixel colors on the just-written base PNG
cargo xtask ui-snap probe states.png --probe "100,50"     # standalone: sample an existing PNG
cargo xtask ui-snap timeline --crop "0,0,200,140"         # crop the just-written base PNG -> timeline.crop.png
cargo xtask ui-snap crop timeline.png --crop "0,0,200,140" # standalone: crop an existing PNG
```
`--probe`/`--crop` alongside a scene render apply to that scene's BASE PNG only (never a
`--interact` `.after` render). On a run that can't honor them — `all`, `graph`, `editor`,
`transform`, or a `--script` run — they are an error (exit 2, pointing at the standalone form),
never silently ignored; use standalone `probe`/`crop` on a specific file for those.
`--probe`/`--crop` coordinates are PNG pixel space, which today
is 1:1 with the tree dump's `rect` values (`SCALE = 1.0` in `ui_snapshot/mod.rs` — the harness
renders at the fixture's logical size, not Retina/2x; §6 below is stale on this point). If `SCALE`
is ever raised for a Retina capture, `rect` values and PNG pixels would diverge and this note (and
`probe`/`crop`, which do no rescaling) would need to change with it.

Output goes to `target/ui-snapshots/<scene>/`. Verified end-to-end: real `UIRoot`/`state_sync`
path, the tree dump with real node values, a real-input-host `select:` that flips the selection-
ring node in the dump and the PNG, the 6-state matrix, the mockup composite, and atlas injection
through the real `ClipThumbGpu`. **Next step:** the §F aspect-locked multi-window tiling layers
onto the same `ThumbQuad`/atlas inputs (`clip_filmstrip::aspect_windows`); the `--thumbs` cut
currently injects one full-body window per clip. Golden-image diffing remains deferred by design.

**Reading results:** don't write an ad-hoc script for this — the harness answers all four common
questions directly. Read the PNG (agents read images natively) for "does it look right." Read the
`.tree.json` dump for an exact-value question ("what's this node's rect/bg/font_size"). Run
`ui-snap diff` on two dumps to answer "what changed" across an edit — node-level, field-level,
exits non-zero on any difference so it can gate a check. Run `ui-snap probe`/`ui-snap crop` for a
pixel question the tree dump can't answer (composited/blended color, a specific sub-region to look
at closely) — standalone on any PNG, or as `--probe`/`--crop` flags on the render that just wrote
one.

## Scenes

| Scene | Renders | Notes |
|---|---|---|
| `timeline` | Whole timeline: ruler, header column, lanes, clips, playhead. Inspector dropped. | The original whole-UI scene. `--interact`, `--dump`, `--thumbs`, `--vs-mockup`. |
| `states` | One layer per state (normal/selected/muted/solo/collapsed/expanded) in one image. | State matrix. |
| `scrollshrink` | 14 uniform video layers — deliberately past the `timeline` scene's exact-7-lane budget, so a vertical scrollbar exists. | P0.0 evidence fixture (`docs/TIMELINE_LAYOUT_P0_SPEC.md`) for RC1 (dual scroll state): pair with `--scroll <px>` + `--interact collapse:<id>` to capture a scrolled, content-shrinking edit. |
| `hairlineclips` | One lane of 200 short trigger clips (0.5 beats each, 4-beat spacing), rendered at the minimum zoom level (1px/beat — this scene name alone triggers the `zoom_ppb` override in `ui_snapshot::mod::render_ui_scene`; every other scene keeps the fixed 24px/beat). | P0.3 evidence fixture (`docs/TIMELINE_LAYOUT_P0_SPEC.md`) for the sub-pixel-clip cull bug: each clip's on-screen width (0.5px) rounds below 1px, so it proves `visible_clip_rects` clamps to a 1px hairline instead of culling. |
| `inspector` | A selected video layer (GLOW) with a real Mirror→Bloom chain; a sine LFO armed on Mirror so the source-tinted (teal) modulation drawer renders. | The inspector — param cards, sliders, enum rows, mod drawer — was invisible before this scene (the others zero the inspector width). The fixture is built through the real `sync_inspector_data` path. |
| `graph` | The node-graph **editor canvas only** for one preset: nodes, typed ports, wires, and **real per-node output thumbnails** (effects), on the dot-grid backdrop. `--preset <TypeId>` picks any effect or generator (default `Mirror`). | The canvas is synthesized from the catalog (`loaded_preset_view_by_id` → `snapshot_for_view` → `ui_translate::graph_snapshot_to_ui`); the thumbnails come from a **headless one-frame graph render** (the parity-harness machinery: `EffectGraphDef::into_graph` → `Executor::execute_frame_with_gpu` with `set_dump_all`), then each node's output texture is blitted over its placeholder. **Effects** render correctly (a UV-gradient fixture feeds the `Source`, so spatial effects are legible). **Generators are structure-only** — a single raw-executor frame can't produce correct generator output (particle warmup / per-frame state / HDR tonemap live in the content pipeline), so their thumbnails are skipped rather than shown wrong. Driving generators through `GeneratorRenderer` is the follow-up. The card lane / sidebar are intentionally omitted here — see `editor` for the full window. |
| `editor` | The **FULL graph-editor window**: left preview sidebar (chrome only — backing panel, "Node Output" / "Master Out" titles, empty-state hint), center canvas (same as `graph`), right card lane (the real `ParamCardPanel` + inner-node param list, same widgets the live editor drives). `--preset <TypeId>` — **generator presets only** (e.g. `FluidSimulation`); an effect needs a chain to live in, which the fixture doesn't build yet. | Builds a one-layer fixture `Project` carrying the preset (`fixtures::generator_editor_fixture`) so the editor card resolves the real param projection (`state_sync::param_surface` — `ParamCardConfig` deleted, WIDGET_TREE P1b), not synthesized. The preview-monitor **images** are content-thread-bound (`SetGraphPreviewNode`/`SetNodeAtlasVisible`) and can't render headless — left as the live editor's own "Select a node" hint, not faked. Layout: preview docks left, card docks right (same side as the main timeline's inspector) — `EDITOR_CARD_LANE_WIDTH` / `SIDEBAR_WIDTH` in `manifold_ui::panels::graph_editor`. |
| `all` | Renders `timeline`, `states`, `inspector`, `graph` (default preset), and `editor` (default preset) in one process. | A full-app sweep for eyeballing everything after a change. |

**Preset ids for `--preset`:** any shipping effect or generator id — e.g. `Mirror`, `Bloom`,
`Tesseract`. The catalog lives in `docs/NODE_CATALOG.md` (§5/§6.1) and
`assets/effect-presets/` + `assets/generator-presets/`. An unknown id exits 2 with a message.

**Provenance:** the `feat/timeline-ui-redesign` review found the redesign rode three
throwaway harnesses (`timeline_header_preview.rs`, `clip_preview.rs`, `headless_ui_spike.rs`)
that each render one piece in isolation, with hand-built structs, assert only `drew == true`,
and write to a dead session scratchpad path. "Verified by render" meant "a human looked, maybe."
This tool replaces them.

## What this is

A single tool, built **for the agent**, to see, measure, and iterate on MANIFOLD's custom
bitmap UI **without a window**. It renders the real UI to a PNG, dumps the UI tree as
machine-readable layout, drives real input events and re-renders, and puts the app next to the
HTML mockup. One command: `cargo xtask ui-snap`.

The point is to stop guessing from pixels. With a render *and* a tree dump, the agent reasons on
values ("header `rgb(194,85,127)`, chip `rgb(71,71,74)`, radius 2, border 0") instead of vibes
("saturation looks high"), catches off-by-2px and wrong-alpha bugs vision misses, and verifies
stateful behaviour (selection, hover, expand) that a static render can't show.

## Non-goals (read this before adding scope)

- **No golden-image regression gate yet.** A pixel-diff CI gate would fight a moving design and
  become noise. It comes *later*, once the visual design is locked. Deferred on purpose.
- **Not a CI gate / not a user feature.** This is a dev instrument for the agent. It can live in
  tests/xtask, never on a hot path.
- **Not a new UI framework.** It drives the *existing* panels, input host, and renderer. If a
  capability is missing in `UIStyle` (borders on skins, box-shadow, letter-spacing), that's a
  separate backlog item — this tool *reveals* the gap, it doesn't paper over it.

## Capabilities, in priority order

### 1. Scene tree dump — the most important feature
Alongside every PNG, emit a machine-readable dump of the rendered `UITree`: for each node its
id/role, rect (x,y,w,h), `bg_color`, `text` + `text_color`, `font_size`/`weight`, `corner_radius`,
`border_color`/`border_width`, and z/paint order. Print a compact form to stdout so the agent sees
it inline without opening a file; write the full form as JSON next to the PNG.

This is what turns vision-guessing into exact inspection. It is also the cheapest reliable diff:
re-run after a change, compare two dumps, see exactly which node moved/recoloured/resized.

### 2. Interaction driver
Feed real input events through the actual input host (`timeline_input_host.rs` /
`timeline_editing_host.rs`) — click, hover, key, scroll, drag, select-layer, expand-group,
open-dropdown — then render + dump the result. The UI is stateful and event-driven; a static
render misses half of it. The selection indicator and group nesting work **cannot** be verified
without this (you have to actually *select* something to see the selection treatment).

`--interact "select:layer2"`, `--interact "hover:mute@layer0"`, `--interact "expand:BG STACK"`,
chained.

**Implemented today** (`ui_snapshot/interact.rs`): `select:<layer-id>` (drives a
real click through the input host — see its own doc comment for the exact
path), `collapse:<layer-id>` (toggles `is_collapsed` directly on the `Project`
data, not via a synthesized chevron click — the bug classes this harness
exists to catch live in the render/sync path's reaction to that state, not in
input dispatch), `delete:<layer-id>` (removes the layer + any children),
`open:settings` / `open:audio_setup`, the automation-lane verbs
(`automation_add/move/bend/segment_drag/group_move/group_delete`), and the
clip-selection verbs (`click_clip`/`shift_click_clip`/`cmd_click_clip`/`cmd_d`/
`drag_clip_toward_zero`/`drag_readout`). Every verb returns a structural
hit/miss outcome — a miss fails the run (exit 1) with a tree dump as evidence,
never a fabricated "after" render (2026-07-07; was a string-match that never
fired). Plus a `--scroll <px>` flag (not an interact verb — seeds
`Viewport::set_scroll` then re-syncs, because the header column bakes its Y
offsets at build time; applied before the BASE render as of 2026-07-07).
`hover:`/`expand:` above are aspirational, not yet built.

**Since 2026-07-07 the `--script` driver dispatches every resolved
`PanelAction` through the real `ui_bridge::dispatch`** (driver-owned scratch
state; `UserPrefs::in_memory()` keeps determinism) — transport, inspector, and
popup wiring are all headless-drivable now, not just `LayerClicked`. Scenes:
`project:<path>` loads a real `.manifold` through the app's exact load path
(real-scale renders — the 52-layer Liveschool fixture is the canonical
subject), and `empty` renders the zero-layer File→New state.

### 3. State matrix
Render one component across all its states — normal / selected / muted / collapsed / expanded /
hovered — as a single grid image (+ a dump per cell). See every state at once instead of N runs.

### 4. Whole-UI from a real fixture
A scene that builds the integrated timeline (ruler + header column + lanes + clips + playhead)
together at a real `ScreenLayout`, from a tiny real `.manifold` Project pushed through the actual
`ui_translate` / `state_sync` path — so the image **is** the app, not hand-built `LayerInfo`.
Keep per-panel scenes too for focused work.

### 5. Mockup side-by-side
Render the app scene next to the HTML mockup (via headless Brave/Chromium), scaled to match, in
one image. For the agent's eye and for app-vs-target comparison. No pass/fail.

### 6. One command, stable paths, deterministic
`cargo xtask ui-snap <scene> [--state x] [--interact "..."] [--dump] [--vs-mockup]`.
Always writes to a predictable location (`target/ui-snapshots/<scene>/`). Fixed 2× DPI, bundled
Inter font, animation seeded to t=0 — reproducible run to run.

## Architecture sketch

- **Render core** — promote `headless_ui_spike::render_to_png` into a real reusable module
  (`snapshot(scene, layout) -> Png` over a windowless `GpuDevice` + `UIRenderer`). Stable output
  dir, not a session scratchpad.
- **Scene registry** — named scenes in code (Storybook-style), listable and renderable
  individually. Each scene = (fixture or hand-built data) + which panels to build.
- **Fixture loader** — load/construct a small `Project`, run it through the real translation path.
  Reuse the canonical-fixture pattern.
- **Dump serializer** — walk the built `UITree`, serialize node style + bounds to JSON + a terse
  stdout summary.
- **Input driver** — feed events through the real input host, then re-build + re-render + re-dump.
- **Atlas injection** — a seam to populate the clip thumbnail atlas with fixed test images
  without the content thread, so clip previews and the §F aspect-locked "Resolve window" render
  headless.
- **Mockup renderer** — shell out to headless Brave to PNG; compose side-by-side.

## Build order

1. **Phase 1 (core loop):** render core + tree dump (#1) + one whole-timeline scene (#4) + the
   `cargo xtask ui-snap` command with stable paths (#6). This alone makes UI work see-able and
   measurable.
2. **Phase 2 (state + interaction):** interaction driver (#2) + state matrix (#3). Unlocks the
   selection / grouping / hover work.
3. **Phase 3 (content):** thumbnail atlas injection + mockup side-by-side (#5). Unlocks the clip
   thumbnail / Resolve-window work.
4. **Later, deferred:** golden-image diffing, once the design is locked.

## Open unknowns to derisk

- **Whole-UI fidelity:** driving real panels headless may hit `WorkspaceState` / window coupling.
  Fallback: build the real panels directly from a fixture at a real `ScreenLayout` (lighter, likely
  enough); wiring full `ui_root` headless is the stretch goal.
- **Atlas injection** needs a small `content_pipeline` seam to accept an injected atlas.
- **Input host headless:** confirm the input hosts run without a live window/event loop.

## First use cases (why it's needed now)

The motivating timeline-redesign gaps all need render + dump + interaction:
- **Selection indicator / Ableton-style selection outline** — needs `--interact "select:"` then dump.
- **Group nesting inset + spine** — needs the whole-UI scene with a parent + children.
- **Clip name-strip band + Resolve thumbnail window** — needs atlas injection.
- **Control hairlines / badge chips / value pass** — needs the dump to verify exact px/colour.

## Prior art (adapt, don't adopt — our UI is custom)

- **egui_kittest** — egui's headless harness: runs the UI, simulates events, snapshots, queries by
  semantics. Closest match to this design.
- **AccessKit** — semantic/accessibility tree; the model behind "query the UI by meaning, not
  pixels" (that's the dump, #1).
- **cargo-insta** — text/structural snapshot testing; a good fit for dump diffs later (not image
  goldens).
- **Storybook** — the component-catalog / named-story idea (the scene registry).

## Location

**Built as a feature-gated subcommand inside `manifold-app`** (`src/ui_snapshot/`, feature
`ui-snapshot`), not a separate crate. Derisk found `manifold-app` is **bin-only** — `ui_root` and
`ui_bridge` are private modules in `main.rs`, with no `lib` target — so the real translation path
(`UIRoot` + `state_sync`) is unreachable from a sibling crate or integration test without
restructuring the app crate. The subcommand reaches it directly with zero restructure, gated off
the shipping binary by the optional feature. Invoked via the `cargo xtask` alias
(`.cargo/config.toml`): `cargo xtask ui-snap <scene> [--dump]`.
