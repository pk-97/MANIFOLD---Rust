# Headless UI Harness — an agent-facing tool for MANIFOLD's UI/UX

**Status:** BUILT 2026-06-28 (Phases 1–3), branch `feat/timeline-ui-redesign`. A feature-gated
subcommand of `manifold-app` (feature `ui-snapshot`), run via the `cargo xtask` alias.
Extended 2026-06-30 with the `inspector` and `graph` scenes (see **Scenes** below).

**Usage:**
```
cargo xtask ui-snap timeline --dump                      # whole timeline + tree dump
cargo xtask ui-snap timeline --interact "select:plasma"  # drive a real click; base + .after
cargo xtask ui-snap states                               # state matrix (6 states in one image)
cargo xtask ui-snap inspector                            # inspector: layer + effect chain + mod drawer
cargo xtask ui-snap graph                                # node-graph editor canvas (default: Mirror)
cargo xtask ui-snap graph --preset Tesseract             # any effect/generator's graph
cargo xtask ui-snap timeline --vs-mockup                 # app | mockup side-by-side
cargo xtask ui-snap timeline --thumbs                    # inject a test atlas into the clips
cargo xtask ui-snap all                                  # render every scene in one sweep
```
Output goes to `target/ui-snapshots/<scene>/`. Verified end-to-end: real `UIRoot`/`state_sync`
path, the tree dump with real node values, a real-input-host `select:` that flips the selection-
ring node in the dump and the PNG, the 6-state matrix, the mockup composite, and atlas injection
through the real `ClipThumbGpu`. **Next step:** the §F aspect-locked multi-window tiling layers
onto the same `ThumbQuad`/atlas inputs (`clip_filmstrip::aspect_windows`); the `--thumbs` cut
currently injects one full-body window per clip. Golden-image diffing remains deferred by design.

## Scenes

| Scene | Renders | Notes |
|---|---|---|
| `timeline` | Whole timeline: ruler, header column, lanes, clips, playhead. Inspector dropped. | The original whole-UI scene. `--interact`, `--dump`, `--thumbs`, `--vs-mockup`. |
| `states` | One layer per state (normal/selected/muted/solo/collapsed/expanded) in one image. | State matrix. |
| `inspector` | A selected video layer (GLOW) with a real Mirror→Bloom chain; a sine LFO armed on Mirror so the source-tinted (teal) modulation drawer renders. | The inspector — param cards, sliders, enum rows, mod drawer — was invisible before this scene (the others zero the inspector width). The fixture is built through the real `sync_inspector_data` path. |
| `graph` | The node-graph **editor canvas** for one preset: nodes, typed ports, wires, on the dot-grid backdrop. `--preset <TypeId>` picks any effect or generator (default `Mirror`). | The snapshot is **synthesized from the catalog** (`loaded_preset_view_by_id` → `snapshot_for_view` → `ui_translate::graph_snapshot_to_ui`), so no content thread or running chain is needed. Node preview thumbnails are black headless (no content thread produces node outputs) — the graph *structure* is what this surfaces. The editor's left card lane is the same `ParamCard` the `inspector` scene covers; the right preview monitors are content-thread-bound and out of scope. |
| `all` | Renders `timeline`, `states`, `inspector`, and `graph` (default preset) in one process. | A full-app sweep for eyeballing everything after a change. |

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
