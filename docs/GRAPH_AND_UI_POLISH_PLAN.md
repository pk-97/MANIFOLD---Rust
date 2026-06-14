# Graph Editor + UI Polish — Plan / Handoff

Working doc capturing everything from the long planning session, so a fresh chat can
pick it up. Three threads: (A) the preset-grouping campaign that shipped, (B) making
grouped graphs actually readable, (C) polishing the existing UI — plus the design
guardrails learned the hard way and the working method.

**Branch:** `preset-grouping` (off `graph-editor-ux`). Grouping work is committed + pushed; PR not opened yet.

---

## The method (read this first — it's the lesson of the session)

- **Polish in the real code, not in mockups.** We burned a lot of the session on SVG mockups. SVG is the wrong medium: it has gradients / rounded / arbitrary fonts the bitmap renderer treats differently, and it ignores that the UI is *already a mature, factored system*. Every mock was a double-guess. Stop mocking.
- **The loop:** pick the one thing that grates → make the exact change in code → Peter builds + runs + screenshots → iterate. Fast, true, no translation gap.
- **This is tuning, not redesign.** Peter likes the current UI and it works. Almost everything "proposed" already exists in code (see the architecture map). The job is small, surgical edits to central constants.

## Design guardrails (do not relearn these)

- Peter is **fond of the current design.** Polish only. Do **not** propose a new visual language.
- **Blocky / direct / raw is right.** Not web/SaaS/AI gloss. No gradients, no glow, no soft rounded "premium" look, no decorative colour.
- **No terminal/mono font for values** — reads too techy. Use the normal UI sans.
- **Colour is functional and already defined** — don't reassign it: blue = active/on, **teal = the modulated (driven) fill**, **amber/gold = envelope / ENV+DRV badges / lit `E`**, pink = MOD badge. These exist in `color.rs`.
- **The left "effect card" IS the live performance surface.** Legibility wins over everything. Any change to it lands on the timeline device chain too (same shared component). Design it once.
- Keep density — a performer needs a lot on screen. The fix is rhythm/alignment, not whitespace.

---

## A. Preset grouping — DONE (refine passes remain)

Shipped on `preset-grouping`: BlackHole (`89cb711e`), GROUP tier 10 (`af5cc04a`), medium-grouped 5 (`905b3547`), flat-titled 21 (`f0fbfb91`). **40 / 45 presets** grouped or titled+described; all gate green (3-set nodeId equivalence + check-presets 45/45 + one-frame Metal execute). See [[project_graph_grouping_campaign]] memory for the full machinery.

- **Throwaway tooling still on disk** (uncommitted, deliberately): `crates/manifold-core/examples/apply_grouping.rs` (spec→grouped JSON via `group_edit::group_selection`, verbatim bodies; `--batch` mode) and `crates/manifold-core/tests/grouping_equivalence.rs` (the gate). Baselines in `/tmp/grouping-baselines/` (re-snapshot from git if gone). Kept for the refine passes; delete when fully closed.
- **Refine-later:** ComputeStrangeAttractor's WGSL kernel is untitled; Duocylinder under-grouped (1 box / 19 nodes); DepthOfField maybe over-boxed (8); Infrared 1-box borderline; per-graph description-voice polish; the §4 slider-rescale folds (hand-verify each — they delete nodes + repoint bindings, kept out of the agent phase on purpose).
- **Deferred (decomposition-pending bundles, per CLAUDE.md):** DigitalPlants, NestedCubes, Tesseract. **Skipped (fixtures):** NodeGraphTest, TrivialPassthrough.
- **Open decision:** open the PR / merge, or keep iterating on the branch.

---

## B. Graph readability — make the grouped graphs usable

Grouping made the structure right but FluidSim3D still opens as a hairball. The causes are not the grouping — they are pins, layout, and wires. Leverage order: editor-side fixes are write-once and benefit every graph (existing + future + user-built); graph-side fixes ride one driver re-run.

1. **Coalesce fan-out pins** *(graph-side, one re-run of the grouping driver).* Today one signal (e.g. `active_count`) fanned to N inner nodes becomes N identical input pins. Coalesce to one pin per external source feeding many inner sinks. Equivalence-preserving (flatten resolves the same). Biggest single declutter — Move Particles drops ~22 pins → ~12.
2. **Semantic pin names** *(per-graph).* Group ports auto-name to the inner port (`out_2`, `active_count_5`). Rename to the signal (`blurRadiusH`). The grouping agents mostly skipped `portRenames`; a focused naming pass (or re-run) fixes it.
3. **Left-to-right auto-layout** *(editor, systemic).* The wire tangle is mostly positioning. A layered (dagre/Sugiyama) layout that orders boxes and pins to minimise crossings fixes most graphs for free. First step: check what auto-layout the graph canvas does today.
4. **Wire hover-highlight + dim the rest** *(editor, systemic).* The cheap transformative one — hover a node/pin, its wires light, others dim. Turns "can't trace anything" into "follow this signal." Render-time alpha, no model change.
5. **Wire routing that bends around node bodies** *(editor).* Current beziers cut through nodes; route around them (or orthogonal) for a calmer read.
6. **Named reroutes / buses for global signals** *(per-graph + small model add).* For `time` / resolution / `active_count` that fan everywhere: publish once, reference by name (UE named-reroute / TD in-out style). Kills the worst webs at the source. Mechanism is general; choosing which signals to bus is per-graph.

## C. Node + group anatomy

- **Preview-as-node:** image-output nodes show a small live preview *inside* the node, placed above the pins so it never covers them (fixes the "preview blocks ports" gripe). Non-image nodes show the value / sparkline (smart preview already distinguishes encodings).
- **Group nodes get their own output preview** — the texture their `group_output` producer makes (same dump/atlas plumbing, pointed at the group's output node).
- **Zoom LOD:** far = preview tiles + colour, mid = + pins, close = + values. Crossfade. Keeps it legible at every zoom.
- Larger, more legible node bodies now that graphs are cleaner.
- (Rejected: ambient "preview glow" on the canvas — gimmick, fights legibility. Only worth it later as a zoom-out heat layer, off by default.)

---

## D. UI polish — the active thread

**This is the real renderer, already well-factored. Polish = tune central constants.**

### Architecture map (where polish lives)

- `crates/manifold-ui/src/color.rs` — **the theme.** Every colour, font size, corner radius as a named constant (`ACCENT_BLUE`, `ENVELOPE_ACTIVE`, `DRIVER_ACTIVE`, `TRIM_FILL`, `TEXT_DIMMED`, `FONT_BODY`, `FONT_CAPTION`, `BUTTON_RADIUS`, …). Recolour / resize type here.
- `crates/manifold-ui/src/panels/param_slider_shared.rs` — **layout constants + builders.** `ROW_HEIGHT=20`, `ROW_SPACING=4`, `PADDING=6`, `GAP=4`, `DE_BUTTON_SIZE=20`, `DE_BUTTON_GAP=2`, the driver/envelope drawer sizes, `BEAT_DIV_COUNT=11`. Also `build_param_row`, the driver/envelope config drawers, button styles (`de_btn_style`, `config_btn_style`, `toggle_btn_style`).
- `crates/manifold-ui/src/slider.rs` — **`BitmapSlider`** (track / fill / handle geometry) + `SliderColors` (default / envelope / gen_param sets).
- `crates/manifold-ui/src/panels/param_card.rs` — the card shell (header, rows).
- `crates/manifold-ui/src/bitmap_painter.rs` — timeline **clip** painting only (`fill_rect`, `draw_border`, `draw_clip`, `get_clip_color`). Note: per-layer clip colour already exists here (lightens/darkens the layer colour for state).

The retained tree is `UITree` (`add_panel` / `add_button` / `add_label`, `BitmapSlider::build`). `UIStyle` carries `bg_color` / `hover_bg_color` / `pressed_bg_color` / `text_color` / `font_size` / `corner_radius` / `text_align`. So hover/press states, rounded corners, and SDF icons (PUA `U+E000..E004` waveform glyphs) all exist already.

### First targets (from Peter's own feedback: "text overlapping" + "buttons squished/cramped")

These are spacing / text-fit, i.e. constant tweaks — ideal first in-code changes:

- **Text overlap.** Likely culprits: param label cells too narrow for friendly names (the `label_width` arg to `build_param_row` — inspector default vs the wider graph-lane value), and the value text right-aligned into the `E`/`→` buttons. Audit where label/value/buttons can collide and give them non-overlapping cells.
- **The squished driver-config row.** 11 beat-div buttons (`BEAT_DIV_COUNT`) divided across the panel width → each `btn_w` is tiny and "1/32" cramps/clips. Options: smaller font already used (8), but consider two rows, fewer-but-grouped, or wider drawer.
- **Cramped rows.** `ROW_HEIGHT=20` / `ROW_SPACING=4` / `DE_BUTTON_GAP=2` are tight. A couple px more height/spacing buys legibility on the live surface. Tune and look.
- **Value legibility.** Confirm value text size/contrast (`FONT_BODY`, `TEXT_*` constants) reads at a glance without going mono.

Everything else (the functional colour, the modulation visuals, per-layer clip colour, hover/press) already works — leave it.

---

## E. Whole-app cohesion (timeline mode) — later, lower priority

The sliders/cards are already the same shared components, so the timeline mostly inherits the card polish. Bigger items for later:

- **Per-layer colour identity** — already supported by `get_clip_color`; verify layers actually have distinct colours assigned (the screenshot showed uniform salmon).
- **Clips show their output thumbnail** — uses the preview engine; future.
- **Transport bar grouping** — cluster transport / tempo / file / render with dividers, weight the primary action.

## F. Parked ideas (revisit after the above)

- **Node recommender / autocomplete** — type-filtered + corpus co-occurrence from the preset graphs (deterministic), ranked suggestions in the picker, never restrictive.
- **Rosetta Stone** — cross-tool node aliases (TD / Blender / Resolume / Ableton / Resolve), search-only hidden aliases.

---

## Recommended order

1. **Effect-card polish** (D) — text overlap + cramping. The live instrument, highest daily payoff, small in-code changes.
2. **Graph readability** (B) — coalescing + hover-highlight + auto-layout. Makes the grouped graphs usable.
3. **Node anatomy / previews** (C).
4. **Grouping refine passes** (A) + decide on the PR.
5. **Whole-app cohesion** (E).
6. **Parked** (F).
