# Chrome API — declarative widget/layout for panels

Status: **SHIPPED — Phases 2a AND 2b complete 2026-06-22** (see the "Phase 2b COMPLETE" summary at the end of `docs/UI_ARCHITECTURE_OVERHAUL.md` §13: every panel's chrome is declarative on the Chrome API — footer, header, transport, master_chrome, layer_chrome, macros_panel, clip_chrome, param_card, layer_header, audio_setup_panel, inspector). Header corrected 2026-07-16 — the 2026-07-05 baseline review wrote "2b in progress" without checking the overhaul doc, where 2b had already been complete for two weeks. Sub-design-doc for the UI Architecture Overhaul Phase 2.

## Why

Every chrome panel today is written twice. `build()` lays out nodes with manual
cursor arithmetic (`x += w + GAP`) and stores ~dozens of node-id fields;
`update()`/`sync_*()` walks those same fields back and pushes new text/colors.
The two must agree node-for-node — when they drift, a slider updates a stale id
or a label sizes to the wrong cell. `param_card` is the extreme case: ~40 stored
id fields, a dozen per-param id vectors, and a `sync_values` that mirrors
`build_effect_sliders` by hand.

The Chrome API removes the second write. A panel **describes** its UI once, as a
value tree; the runtime decides whether that description is a fresh build or an
in-place update and emits the minimal tree mutations either way. Layout is
solved, not hand-cursored. Clicks attach to the description, not to stored ids.

## Shape of the API

Three pure layers over the existing `UITree` (no new shared state, no engine
dependency):

1. **`view`** — the description. `View` is an immutable value: a node `kind`,
   a `UIStyle`, optional `text`, a per-axis `Sizing`, a container `Layout`, a
   `Vec<View>` of children, and an optional intent (click / double-click /
   right-click `PanelAction` + `claims_area`). Built fluently:

   ```rust
   View::column(GAP)
       .pad(Pad::all(8.0))
       .children([
           View::row(4.0).children([
               View::label(&name).fill_w().clip(),
               View::button("ON").fixed(40.0, 18.0).on_click(PanelAction::EffectToggle(i)),
           ]),
           View::slider().fill_w().h(Sizing::Fixed(22.0)).inert(),
       ])
   ```

2. **`layout`** — a pure mini-flexbox. `solve(&View, rect, &dyn TextMeasure) ->
   Vec<LaidNode>` resolves every view to a `Rect` in DFS pre-order. No `UITree`
   dependency, so it is unit-tested headlessly with a deterministic measurer.

3. **`diff`** — the reconciler. A `ChromeHost` owns the retained laid tree, the
   assigned `NodeId`s, and a structural signature. `build()` does a fresh
   structural pass (`add_node` per laid node); `update()` compares signatures
   and, if the structure is unchanged, does in-place `set_bounds` / `set_text` /
   `set_style` / `set_visible` on the retained ids (no `add_node`, no
   `structure_version` bump — drags and intents survive). If the structure
   changed, it returns `Reconcile::NeedsRebuild` and touches nothing (a mid-tree
   restructure would corrupt later panels; the app re-runs `build()` for the
   affected range, exactly as it does today for collapse/drawer toggles).

### Sizing model

Per axis, independently:

- `Fixed(px)` — exactly this size.
- `Hug` — shrink-wrap: a leaf hugs its measured text; a container hugs its
  laid-out children plus padding and gaps.
- `Fill` — grow to the space the parent offers, split equally among sibling
  `Fill`s on the main axis, stretch to the container on the cross axis.

`main_align` (Start / Center / End) distributes leftover main-axis space when no
child fills it. `cross_align` places each child on the cross axis at its own
size. This is the whole layout vocabulary — enough for every chrome panel, small
enough to fit in one screen of solver.

### Intent at build

A view carries its gesture intents directly (`on_click`, `on_right_click`,
`on_double_click`, `claims_area`). After layout assigns node ids, the host
populates the `IntentRegistry` from the laid tree — so `register_intents`
becomes a one-line delegation, and a panel can no longer attach an intent to the
wrong id. Drag / scroll / hover stay in the stateful `handle_event` path
(unchanged) — intent-at-build is for discrete node→action gestures only, the
same boundary the Phase-1 intent system drew.

### Loud-fail validation

`validate(&View)` walks the tree and flags any interactive node (Button /
Slider / Toggle, or `interactive`-flagged) that has **no** intent and is **not**
explicitly `.inert()`. The host runs it on every build: `debug_assert!` in debug
(a dead control fails the test that builds it), `eprintln!` in release. `.inert()`
is the explicit opt-out for an interactive node whose gesture is handled
elsewhere (e.g. a slider whose drag lives in `handle_event`). The point: an
unwired control is a build-time error, not a silent dead zone discovered on
stage.

## How a panel uses it

A migrated panel writes **one** method:

```rust
fn view(&self) -> View { /* describe the whole panel from self state */ }
```

and thin trait glue:

```rust
fn build(&mut self, tree, layout)  { self.host.build(tree, self.view(), self.rect(layout)); }
fn update(&mut self, tree)         { self.host.update(tree, self.view(), self.rect); }
fn register_intents(&self, reg)    { self.host.register_intents(reg); }
```

`build()` and `update()` describe the same tree from the same state — there is
no second, hand-mirrored write to drift. The stored id fields and the
`sync_*` mirror code are deleted.

## Integration with the existing tree

The API mutates the existing `UITree` through its existing public surface
(`add_node`, `set_bounds`, `set_text`, `set_style`, `set_visible`,
`truncate_from`). It introduces no new node storage and no new shared state. The
contiguous append-only buffer, the panel-boundary `truncate_from` rebuild model,
and the `structure_version`-gated intent rebuild are all unchanged — the host is
a panel-local realisation of the same model the app already runs globally. That
is what makes per-panel migration in Phase 2b incremental and reversible: a
migrated panel and an un-migrated one coexist in the same tree.

## Phase 2a deliverables and the param_card proof

- **2a.1** this doc.
- **2a.2** `layout` engine, headless unit tests.
- **2a.3** `diff` reconciler — build vs in-place update vs needs-rebuild.
- **2a.4** `view` builders + intent-at-build + `validate`.
- **2a.5** prove on `param_card`. The proof is a **golden structural test**: a
  param-card-shaped tree (header + slider rows + a drawer) built on the API,
  asserting (a) value-only updates stay in-place with stable ids, (b) a
  structural change reports `NeedsRebuild`, (c) intents populate from the
  description, (d) validation fires on an unwired control.

**Live `param_card` cutover is deliberately the first task of Phase 2b, not
2a.5.** `param_card` is the most interaction-dense panel in the app (the full
drag surface — trims, envelope target/decay, audio-shape, reorder). Cutting the
live instrument over to a brand-new API as its first consumer, with no runtime
visual verification available headlessly, is the wrong order of operations.
Phase 2a proves the API is correct on param-card-shaped UI; Phase 2b does the
live cutover with eyes on the running app, now that the foundation is proven and
tested. This keeps the instrument safe while still proving the API against the
hardest real case.
