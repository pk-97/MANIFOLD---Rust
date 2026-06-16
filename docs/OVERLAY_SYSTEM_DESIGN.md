# Overlay System Design

Status: **proposed** (2026-06-16). No code yet. This doc is the contract to agree before building.

## Why

Today every top-level floating thing — dropdown, browser popup, Ableton picker, Audio Setup, perf HUD — is a separately-typed field on `UIRoot`, and **four different concerns each re-enumerate that set by hand**: tree build, viewport-build (a duplicate of tree build), the render pass, and input routing. They drift. The live proof: the Audio Setup panel was wired into build + input but never added to the render pass's popup chain, so it only draws as an accident of the perf HUD's render-to-end-of-tree sweep ([app_render.rs:3660-3688](../crates/manifold-app/src/app_render.rs#L3660-L3688)). Adding a modal means touching five places correctly; miss one and it half-works.

The goal is infra where panels / modals / popups / settings pages / overlays are **easy to implement, easy to reason about, and safe to use** — where adding one is a registration, not a five-site landmine.

## What exists today (the seams that are missing)

- **No stack.** Open overlays are a hard-coded priority list in two places: the [Escape chain](../crates/manifold-app/src/input_handler.rs#L78-L100) (Level 0 browser, Level 1 dropdown, …) and the `process_events` if/else cascade ([ui_root.rs:732-905](../crates/manifold-app/src/ui_root.rs#L732-L905)).
- **No modality model.** "Modal captures input + dims background" vs "modeless floats" is hand-coded: each overlay decides on its own whether to add a backdrop and whether to `continue` (swallow) per event.
- **No anchoring.** Each overlay hardcodes its own rect + edge-clamp math (centered, at-click, etc.).
- **No registry / lifecycle driver.** Concrete fields, no `dyn` view, no single iteration point. The render pass uses `first_node()..usize::MAX` arithmetic and assumes one overlay open at a time.
- **Heterogeneous input APIs.** `dropdown.handle_event(ev, tree) -> Option<DropdownAction>`; `browser_popup` / `ableton_picker` use `handle_escape()` + `handle_click(node)` + `contains_node(node)` + `handle_scroll()`; `audio_setup` uses `handle_click(node) -> Option<PanelAction>` + `owns_node(node)`. Each returns a different action type, converted to `PanelAction` inline at the call site.
- **Fixed enums elsewhere** (`Layer` = Base/Overlay/Tooltip; `PanelSlot` = 7 cached panels). Overlays bypass the atlas entirely and draw fresh each frame. That part is fine and stays.

## Design

Keep the typed fields (preserves each overlay's specific `open(args)` and typed context — structural fidelity). Add a thin uniform layer over them.

### 1. The `Overlay` trait — uniform lifecycle + input

```rust
/// A floating, dismissable surface drawn above the main UI on Layer::Overlay.
/// Standalone, NOT `: Panel` — the modal panels don't implement Panel, and the
/// driver captures node ranges itself (brackets build_at with tree.count()), so
/// no node_range() on the trait.
pub trait Overlay {
    fn is_open(&self) -> bool;
    fn modality(&self) -> Modality;
    fn anchor(&self) -> Anchor;
    fn desired_size(&self) -> Vec2;                 // anchor + this → rect via one helper
    fn build_at(&mut self, tree: &mut UITree, rect: Rect);
    fn on_event(&mut self, ev: &UIEvent, tree: &mut UITree) -> OverlayResponse;
    fn close(&mut self);
}

pub enum Modality {
    Modal { dim_background: bool },   // captures ALL input; optional backdrop
    Modeless,                         // only consumes its own nodes; click-outside dismisses
}

pub enum Anchor { Centered, At(Vec2), ToNode(i32), Fixed(Rect) }

pub enum OverlayResponse {
    Ignored,                          // not mine; fall through
    Consumed(Vec<PanelAction>),       // mine; emit these (may be empty)
    Dismiss(Vec<PanelAction>),        // mine, and close me after emitting
}
```

The per-overlay typed action enums stay; each overlay's `handle_event` impl does its own match and lowers to `PanelAction`/`OverlayResponse`. The migration is mechanical: move the existing inline match bodies from `process_events` into each overlay's `handle_event`.

### 2. `OverlayId` registry — the exhaustive-match safety net

```rust
pub enum OverlayId { Dropdown, BrowserPopup, AbletonPicker, AudioSetup, PerfHud }

impl UIRoot {
    fn overlay_mut(&mut self, id: OverlayId) -> &mut dyn Overlay {
        match id {
            OverlayId::Dropdown      => &mut self.dropdown,
            OverlayId::BrowserPopup  => &mut self.browser_popup,
            OverlayId::AbletonPicker => &mut self.ableton_picker,
            OverlayId::AudioSetup    => &mut self.audio_setup_panel,
            OverlayId::PerfHud       => &mut self.perf_hud,
        }
    }
}
```

This match is what makes "built but never drawn" **unrepresentable**: the driver iterates `OverlayId`s through one dispatcher, so build + draw + input all derive from the same enum. Adding an overlay = add a variant; the compiler then forces the match arm. That is the whole fix to the original bug class.

### 3. `OverlayStack` — order = z-order

```rust
pub struct OverlayStack { open: Vec<OverlayId> }   // bottom → top; top = highest z + first input
```

`open(id)` pushes (or raises to top if already present); `close(id)` removes. Pure UI-thread state on `UIRoot` — no `Arc<Mutex>`, clear of the no-new-shared-state rule. The typed `open(args)` call still happens first (`self.browser_popup.open(req)`), then `stack.open(OverlayId::BrowserPopup)`. (Or fold the push into a single `UIRoot::open_overlay` helper per overlay.)

### 4. The driver — one place for build, draw, input

- **Build:** iterate `stack.open` bottom→top; for each, compute rect (`anchor` + `desired_size` + clamp helper), `build()` into the tree, draw a backdrop node first if `Modal { dim_background: true }`. Replaces the modal blocks in both `build_scroll_panels` and `build_viewport_panels` (kills that duplication).
- **Draw (Pass 5):** iterate bottom→top, `render_tree_range(node_range)` on `Layer::Overlay`. Replaces the hand-rolled HUD-cutoff chain and the popup if/else entirely. No more `first_node()..MAX` arithmetic.
- **Input:** iterate top→bottom. First overlay returning `Consumed`/`Dismiss` stops the walk. A `Modal` overlay blocks fall-through even on `Ignored` (clicks outside its rect hit the backdrop → dismiss). A `Modeless` overlay lets unrelated clicks fall through but a click outside it dismisses it. Replaces the `process_events` cascade.
- **Escape:** top-of-stack gets first crack; if it dismisses, pop. Replaces Levels 0–1 of the [escape chain](../crates/manifold-app/src/input_handler.rs#L78-L100); Levels 2–3 (inspector focus, clear selection) remain below the stack.

### 5. perf HUD folds in

The HUD becomes a `Modeless` overlay that never consumes input (always `Ignored`). It's drawn by the driver like everything else, so the accidental render-sweep coupling that hid/showed Audio Setup **disappears by construction** — that's the bug fix falling out of the architecture rather than being patched.

## What the Audio Setup bug becomes

Nothing special. `audio_setup_panel` is `OverlayId::AudioSetup`; opening it pushes the stack; the driver builds, backdrops, draws, and routes its input. There is no separate "remember to add it to the render pass" step, because there is no per-overlay render code anymore.

## Refinements from validating against real panels (2026-06-16)

Found by implementing the trait against the actual code, not just asserting it:

- **`Overlay` is standalone**, not a `Panel` supertrait. The modal panels (audio_setup, browser_popup, ableton_picker, dropdown) don't implement `Panel` — they have bespoke build/click APIs. Forcing `: Panel` would mean a second, fake implementation.
- **The driver captures node ranges**, not the overlays. It brackets `build_at` with `tree.count()` to record each overlay's `[start, end)`. Audio Setup, for one, tracks no `first_node`/`node_count` today — pushing that onto every overlay would be busywork.
- **Overlays must own their resolution context.** Dropdown selection lowers through `dropdown_context` and Ableton selection through `ableton_picker_context`, both stored on `UIRoot` today. For `on_event` to be self-contained (return the final `Vec<PanelAction>`), those contexts move *into* their panels, set at `open()`. This is the largest follow-up refactor; Audio Setup needed none of it (its `handle_click` already returns `PanelAction` directly), which is why it's the proof-of-concept.
- **Modality classification:** Modal + backdrop = browser_popup, ableton_picker, audio_setup; Modeless = dropdown, perf_hud. (Audio Setup gains a real full-screen backdrop it lacked before — today its "background" node only covers the panel itself, so clicks outside leak through.)

### Status

- **Landed (foundation):** `Overlay` trait + `Modality`/`Anchor`/`OverlayResponse` + `compute_overlay_rect` in [crates/manifold-ui/src/panels/overlay.rs](../crates/manifold-ui/src/panels/overlay.rs); `Overlay` implemented for `AudioSetupPanel` as the proof (additive — the old `build(tree, w, h)` stays until the driver lands). Compiles under `-D warnings`; panel tests green.
- **Next:** impl `Overlay` for the other four (with the dropdown/picker context moves), then `OverlayId` + `OverlayStack` + the driver, then the atomic cutover.

## Migration plan (one cutover, no half-state)

Per the no-interim-stopgaps rule, all five overlays move onto the system in a single change — not some-on/some-off.

1. Land `Overlay` trait, `Modality`/`Anchor`/`OverlayResponse`, the `compute_rect` clamp helper.
2. Impl `Overlay` for all five panels (mechanical move of existing `handle_*` bodies).
3. Add `OverlayStack` + `overlay_mut` dispatcher; route open/close through it.
4. Replace the build blocks, Pass 5, and the input cascade with the driver. Delete the duplicated build block and the `first_node()..MAX` arithmetic.
5. Drop the `needs_rebuild`-on-toggle Audio Setup workaround ([app_render.rs:948-955](../crates/manifold-app/src/app_render.rs#L948-L955), [1020-1031](../crates/manifold-app/src/app_render.rs#L1020-L1031)).

### Verification (this is live-show code)
Manual run-skill pass per overlay: open / draw / dismiss-by-Escape / dismiss-by-click-outside / input-capture-correct / backdrop-correct, with the perf HUD both on and off (the original failure was HUD-state-dependent). Confirm Escape order unchanged for Levels 2–3. Confirm only one path now enumerates overlays.

## Deferred (second pass)

**Declarative top-level content** — the "easy to *implement*" half. Generalize the [drawer DSL](../crates/manifold-ui/src/panels/drawer.rs) (today scoped to slider sub-panels) upward so a settings page is "declare rows," not a bespoke `build_nodes()`. Separable, most open-ended; do it after the stack is proven by this migration. This first pass delivers "reason about" + "safe" fully and "easy to implement" partially (registration is trivial; content is still bespoke until the DSL lands).

## Decisions (signed off 2026-06-16)

1. **Modeless click-outside: dismiss only.** A click outside a modeless overlay dismisses it and is consumed — it does NOT fall through to the panel beneath.
2. **perf HUD folds into the stack** as a non-capturing modeless overlay, pinned to the *bottom* of the overlay z-order (a true modal always draws on top of it). This removes the render-sweep coupling that caused the Audio Setup bug. Implementation note: overlays carry a z-tier so a persistent low overlay (HUD) can't end up above a later-opened modal regardless of toggle order.
3. **Anchors recompute each rebuild** from the live anchor (cheap; no cached rect).
