# Node-Intent Dispatch — friendly, position-robust UI event API

Status: **right-click migration complete — single dispatch path, old handlers
deleted** (2026-06-18, branch `intent-dispatch`). Every node-addressed
right-click in the app resolves through one declarative `intent` layer; the
per-panel `event.node_id == self.some_id` right-click handlers are gone. The
button-grid clicks are migrated too; the rest of the click handlers remain (a
later, optional consistency pass).

## Outcome (what shipped on this branch)

**Right-click — one path, everywhere, old handlers deleted:**
- Foundation: `intent.rs` (`IntentRegistry` + fold-up `resolve`), `UIRoot`
  wiring, `process_right_click` emit-on-miss (cause #1).
- Param cards: track = instant reset; the full-row catcher gives label / value /
  gaps the param menu (no narrow-target lottery).
- Inspector chrome: `master_chrome` / `layer_chrome` / `macros_panel` register
  track/label intents (clip_chrome has none).
- Layer headers: every node of a row registers `LayerHeaderRightClicked(i)`, so
  a right-click anywhere on the row opens that layer's menu — reproducing the old
  whole-row positional behaviour through the registry, and dropping the manual
  X-bounds guard (the hit test scopes it to the panel for free).
- **Graph editor card:** the editor window's hand-rolled loop now resolves the
  card's right-click through a dedicated `editor_card_intents` registry against
  `ed.ui_root.tree` — the second `ui_root` is no longer a holdout.
- **Deleted:** `param_card::handle_right_click[_effect/_generator]`, the four
  chrome `handle_right_click`s, the inspector `route_right_click` + its
  `RightClick` arm, and `layer_header::handle_right_click`. Intent is the *sole*
  right-click path — no twins, so a registration gap surfaces immediately instead
  of being masked by a fallback.
- Perf: repopulation gated on `UITree::structure_version` — off the hot path.

**The one positional exception (justified):** `viewport` timeline markers. They
are *painted directly*, not `UITree` nodes, so there is nothing to attach an
intent to; they resolve right-click by scanning marker-flag rects. This isn't the
narrow-node anti-pattern (it's whole-flag positional), and it can't join the
registry without first making markers tree nodes — a separate change.

**No right-click to migrate:** `browser_popup`, `ableton_picker`,
`audio_setup_panel`, `waveform_lane`, `stem_lane` (click/drag/scrub only).

**Clicks:** `transport` / `header` / `footer` button grids register Click intents
(verified by registry-resolve tests); their `handle_click` twins remain for now.
Migrating the remaining panels' clicks is an optional later pass — clicks have no
dead-zone bug, so it's consistency, not correctness.

## The problem this removes

Today every panel does two things by hand:

1. In `build()` it creates tree nodes and stashes their raw `u32` ids in panel
   fields (`self.header_bg_id`, `slider_ids[i].track`, …).
2. In `handle_event()` it matches the incoming `event.node_id` against those
   stored ids and returns a `PanelAction`.

This is "direct" in the bad sense — the dispatch logic is scattered, repeated,
and **exact-id-keyed**. Two bug classes fall out of it:

- **Dead zones.** A gesture only does something if it lands on the one node the
  panel happened to enumerate. Padding, a slider's `fill`/thumb/value cell, the
  gap between widgets, a card's empty body — all silently swallow the gesture.
  This is why right-click "sometimes works": the live pixels are narrow strips
  with dead gaps between them (see the right-click investigation that motivated
  this doc).
- **Silent-drop on miss.** `process_right_click` only emits an event if
  `hit_test(pos) >= 0`. A right-click over any non-`INTERACTIVE` pixel produces
  no event at all — there is no miss path.

Left-click hides the pain because the timeline has a backing node and a miss
still clears focus; right-click has neither.

## Core idea

Attach **intent** to a node at build time — *what this region means for
dispatch* — instead of matching ids at handle time. A single central dispatcher
resolves every incoming gesture:

```
pointer gesture (pos, button)
   → hit_test(pos)                      // topmost INTERACTIVE node, or -1
   → fold up the parent chain           // nearest ancestor carrying this gesture's intent
   → emit the registered PanelAction
```

The **fold-up** is the dead-zone killer: a right-click on a slider's `fill`
(no intent) walks up to its row, then the card body, then the card, and fires
the card's right-click intent. Padding inside a region resolves to that region.

For true empty space (outside any intent-bearing node) the dispatcher still
emits a position-carrying `Unhandled` so position-based consumers (timeline
overlay, canvas) get their shot — this is cause #1's fix, folded into the same
path instead of a one-off sentinel.

## API surface

New module `manifold-ui/src/intent.rs`:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Gesture { Click, DoubleClick, RightClick }

/// Per-node intent: which PanelAction each gesture maps to.
/// Stored sparsely (most nodes have none).
#[derive(Default, Clone)]
pub struct NodeIntent {
    pub click: Option<PanelAction>,
    pub double_click: Option<PanelAction>,
    pub right_click: Option<PanelAction>,
    /// When true, this node *claims its whole area* for fold-up resolution:
    /// a gesture on any non-intent descendant resolves here. Container
    /// backgrounds (card body, panel bg) set this so their padding is live.
    pub claims_area: bool,
}

/// Dense, node-id-indexed (id == index, parallels the SoA tree).
pub struct IntentRegistry { slots: Vec<Option<Box<NodeIntent>>> }

impl IntentRegistry {
    pub fn clear(&mut self);                                   // call at build start
    pub fn on(&mut self, node: u32, g: Gesture, a: PanelAction);
    pub fn claim_area(&mut self, node: u32);                   // mark container bg
    /// Fold up from `hit` to the nearest ancestor with an intent for `g`.
    pub fn resolve(&self, tree: &UITree, hit: i32, g: Gesture) -> Option<PanelAction>;
}
```

Tree gains one accessor (topology is already stored):

```rust
impl UITree { pub fn parent_of(&self, id: u32) -> i32 { self.parent_index[id as usize] } }
```

### Builder ergonomics

Panels register intent right where they create the node — the id never needs to
be stored on the panel:

```rust
let track = tree.add_button(row, x, y, w, h, style, "");
intents.on(track, Gesture::RightClick,
           PanelAction::ParamRightClick(target, pid, default));

let body = tree.add_panel(card, cx, cy, cw, ch, style);
intents.claim_area(body);
intents.on(body, Gesture::RightClick, PanelAction::CardRightClicked(target));
```

A thin `IntentBuilder<'a>` wrapper bundling `(&mut tree, &mut intents)` is the
sugar layer so a panel can write `b.button(...).on_right_click(action)` and get
the node id + intent in one call. (Built after the core lands.)

## Layering

`intent.rs` lives in `manifold-ui` alongside `panels` and `tree`, so it can name
`PanelAction` without a new dependency. The tree stays pure UI (only gains
`parent_of`). `UIRoot` owns the `IntentRegistry`, clears it at build start, and
runs the central resolve pass in `process_events` *before* the per-panel
`handle_event` loop.

## Migration plan (one panel at a time, behavior-preserving)

The registry runs **alongside** the existing `handle_event` path. A panel is
migrated when its `node_id ==` matches move into build-time `intents.on(...)`
and its `handle_event` static-dispatch arms are deleted. Stateful/positional
handlers (slider drag math, scrub, card reorder drag) **stay** in
`handle_event` — intent dispatch is for discrete node→action gestures only.

Order (broken-surface-first):

1. **Core** — `intent.rs`, `UITree::parent_of`, `UIRoot` wiring, central resolve
   pass, `Unhandled` fallback for misses. Unit tests for fold-up + miss.
2. **param_card** (the densest dead-zone surface) — right-click + click intents;
   prove sliders' fill/thumb/value fold to the card.
3. **inspector** chrome (master/layer/clip), **macros_panel**, **master_chrome**.
4. **header / footer / transport** (simple button grids — mechanical).
5. **layer_header**, **browser_popup**, **ableton_picker**, **audio_setup_panel**.
6. **waveform_lane / stem_lane** (mixed positional + node — partial migration).
7. Delete dead `*_id` fields and the `node_id ==` arms left behind.

`graph_editor` canvas is **out of scope** — it has its own hit model
(`on_right_button_down` resolves node+param by geometry already).

## Verification per panel

- Unit test: a right-click at a known dead-zone pixel resolves to the expected
  action (fold-up), and an in-strip pixel still resolves to the specific action.
- Manual: every previously-enumerated affordance still fires; previously-dead
  padding now fires the container action.
- No new per-frame allocation — registry is built during `build()` only
  (interaction frames are `set_*` only, per the tree invariant).

## What "done" looks like

Zero `event.node_id == self.*_id` comparisons remain in panel `handle_event`
for discrete gestures. Right-click (and click) behave identically across every
surface because they all flow through one resolver with the same fold-up rule.
