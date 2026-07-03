# UI Craft & Motion Plan — finish-quality pass over the existing design system

**Status:** APPROVED design, not built · 2026-07-03 · Fable 5
**Prerequisites:** none (independent of Wave 1 designs; touches manifold-ui + bundled preset JSONs only)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

The governing insight: **the design language is right and settled; what separates MANIFOLD
from Ableton-grade is finish, not style.** This doc converts a Fable art-director audit
(2026-07-03, headless `ui-snap` renders + 4× crops + token verification) into mechanical
fixes. Two re-themes were proposed and REJECTED by Peter in the same session — the
saturated layer headers stay ("VERY bright and direct to know what to look at straight
away") and blue keeps its dual selection+active role. Motion was GREENLIT from an
interactive playground: "I LOVE the motion it makes everything feel so fluid."

Companion docs: `UI_DESIGN_SYSTEM_AND_INSPECTOR_REDESIGN.md` (the visual-language
north star this doc finishes, does not reopen) · `GRAPH_EDITOR_REDESIGN.md` (owns
graph-editor structure; this doc only touches its wires/labels/positions) ·
`HEADLESS_UI_HARNESS.md` (the verification tool every phase uses).

---

## 1. Audit — what exists (verified 2026-07-03)

Extend, don't redesign. Re-verify anchors at phase entry; a moved anchor is an
escalation, not a guess.

| Piece | Where | State |
|---|---|---|
| Token system (BG ramp, spacing, radii, fonts) | `crates/manifold-ui/src/color.rs` | Complete; radius drift + duplicate families (below) |
| Chip grammar (one control skin) | `crates/manifold-ui/src/chrome/components.rs` | Shipped app-wide 2026-06-28; 2 stragglers |
| BitmapSlider (track/fill/thumb) | `crates/manifold-ui/src/slider.rs:29-35` | Correct concentric geometry (radius 6, fill inset 1 → 5, pill thumb); no bipolar mode |
| Slider track color | `color.rs:639` `SLIDER_TRACK_C32 (12,14,19)` | **Invisible** on card wells (`EFFECT_CARD_INNER_BG` = BG_0 (9,9,11)) |
| Mute clip tint | `crates/manifold-ui/src/bitmap_painter.rs:140-149` blends 50% toward `MUTED_COLOR` | `color.rs:381` = pure red (255,0,0) — Unity-era leftover; mute *brightens toward red* |
| Radius tokens | `color.rs:949-960` | 7 values 0–6px: HAIRLINE 1, SMALL 2, BUTTON 3, CHIP 4 (:783), CARD 5, POPUP 6 + slider TRACK 6 |
| BUTTON_RADIUS(3) users | buttons: `panels/macros_panel.rs:231`, `panels/audio_setup_panel.rs:936,1844` · list cells (deliberate): `panels/graph_editor.rs:297`, `panels/ableton_picker.rs:383,481`, `panels/browser_popup.rs:416,584` | Two buttons missed the chip sweep |
| Hardcoded radii outside tokens | one: `panels/transport.rs:70` (documented circular-dot exemption) | Token discipline otherwise clean |
| Duplicate scrollbar token families | `color.rs:209,239` vs `color.rs:749-751` | Two sets, different values |
| Comment-synced text tokens | `color.rs:627,630` (`TEXT_PRIMARY_C32`, `TEXT_DIMMED_C32` — "§A: synced w/") | Drift waiting; alias instead |
| Timeline scrollbar | `panels/viewport.rs` (draw side) | Square thumb; generic `scroll_container.rs:34` already supports `corner_radius` |
| Clip rendering | `manifold-renderer/src/clip_draw.rs` (GPU SDF body) | Rounded top (CLIP_RADIUS 4), square-cornered name strip bottom; ring at body radius |
| Layer seam composition | `color.rs:896` `CLIP_VERTICAL_PAD 6` + `:233` `TRACK_SEPARATOR_HEIGHT 2` | Visible inter-layer boundary = fuzzy multi-edge band; header column groove is crisp 2px |
| Graph auto-layout (Sugiyama) | `crates/manifold-ui/src/graph_canvas/layout.rs:251` (`auto_layout`), `:234` (`request_relayout`, **Cmd+L**, undoable, persists `editor_pos`) | Built, correct, feedback-aware. Bundled presets ship pre-layout hand positions, so it never benefits them |
| Wire drawing | `graph_canvas/render.rs:1155-1205` | Cubic bezier + `skip_bump` + feedback return arc; no under-node avoidance |
| Node boxes | `graph_canvas/render.rs` | Zero corner radius anywhere in the file (only sharp-cornered card surface left in the app) |
| Save flash (transient-feedback precedent) | `color.rs:363` `SAVE_FLASH_GREEN` + transport use | Precedent for toast/flash work |
| Headless verification | `crates/manifold-app/src/ui_snapshot/` (`ui-snap` scenes: timeline/states/inspector/graph/editor) | No popup/hover/drag scenes; no pixel-diff ratchet |
| Design-token ratchet | ⚠ VERIFY-AT-IMPL: `crates/manifold-ui/tests/design_tokens.rs` exists with baseline 200 — `rg -n "200" crates/manifold-ui/tests/design_tokens.rs` | Catches raw `Color32::new` drift; 69 graph-editor literals inside the baseline |
| Param LOD threshold | ⚠ VERIFY-AT-IMPL: `rg -n "PARAM_LOD_ZOOM" crates/manifold-ui/src` | Params hide below 0.5 zoom; port labels do NOT LOD (illegible at 41%) |
| Bipolar slider handling | none — `rg -n "bipolar|center" crates/manifold-ui/src/slider.rs` returns 0 hits | Fill always anchors left; Flow at −0.01 renders ~half-full |
| Animation/tween machinery | none in manifold-ui (`rg -n "ease|tween|lerp.*dt" crates/manifold-ui/src`) | UI redraws on state change only; no animation clock |

## 2. Decisions

- **D1 — Motion tokens.** `MOTION_FAST` 90 ms (hover/press) · `MOTION_MED` 160 ms
  (drawers, tab ink, card collapse) · `MOTION_SLOW` 240 ms (value flash, toast) ·
  one curve = cubic ease ≈ `cubic-bezier(.25,.1,.25,1)`. Constants live in `color.rs`
  beside the other design tokens. Peter approved these exact defaults in the
  playground. Rejected: per-widget bespoke durations — three tokens or the system rots.
- **D2 — Motion is chrome-only.** Never the content/render path, never video output,
  never the compositor. Respect OS reduced-motion. Rejected: animating clip/layer
  visual state — the timeline is a performance readout, not a web page.
- **D3 — Tween primitive, no clock thread.** One `AnimF32` value type ticked by the
  existing UI frame loop; a node with a live tween stays dirty until settled. No new
  threads, no timers, no `Arc<Mutex>` (the named tempting-wrong-turn for "animation
  state"). No in-repo precedent exists for easing — this is the one genuinely new
  piece; keep it under ~100 lines. Nearest relative: the save-flash transient in
  transport (state + deadline, redrawn until elapsed).
- **D4 — Mute dims, never reddens.** Muted clip = 50% blend toward `BG_1`, and drop
  the strip to the same blend. Delete `MUTED_COLOR`/`SOLO_COLOR` (color.rs:381-382)
  outright; the M-pill active red comes from `RED_*`. Rationale: red = record/alarm
  in this palette; "off" must read dimmer, and must survive red-identity layers.
- **D5 — Bipolar sliders fill from center.** When a param's min < 0 < max, fill
  anchors at the 0-position, extending toward the value. Detection from the param's
  range, not a new flag. Rejected: leaving it — a parked bipolar param reading "on"
  is a stage-legibility bug (`param_values` is the live instrument).
- **D6 — Radius scale collapses to 2/4/6.** SMALL 2 (badges, hairline fills) ·
  CHIP 4 (every control) · CARD 6 (cards, popups, slider tracks). `BUTTON_RADIUS`
  and `CARD_RADIUS(5)`/`POPUP_RADIUS(6)` merge accordingly; the list-cell sites keep
  a renamed `LIST_CELL_RADIUS = 3.0` token to pin that they are cells, not buttons.
  Rejected: leaving 7 steps — 1px-apart radii read as manufacturing error.
- **D7 — Slider track becomes visible.** `SLIDER_TRACK_C32` lifts to a value
  distinguishable on BG_0 wells (target ≈ BG_2 vicinity; exact value tuned by
  Peter's eye from a rendered sheet, gate checks Δ against both BG_0 and BG_1
  backgrounds ≥ 10 luma).
- **D8 — Layer headers stay loud; blue keeps both jobs.** Peter, verbatim: "VERY
  bright and direct to know what to look at straight away" · "blue sort of indicates
  selection right?". Rejected (do not re-propose): redistributing header saturation;
  one-meaning-per-hue reform. The ONLY accepted change: the white selection ring
  (`SELECTED_LAYER_RING`, color.rs:415) extends to the selected layer *header*, so
  selection survives blue-identity layers.
- **D9 — Bundled presets get re-baked positions** through the existing
  `auto_layout()` — a one-time batch (dev tool or test-mode dump) rewriting
  `editor_pos` in the 45 bundled preset JSONs. Rejected: hand-tidying positions;
  rejected: layout-on-load (positions are user data once edited — bake the shipped
  defaults, leave user files alone).
- **D10 — Wires dim under nodes.** Segments passing under a node rect draw at
  reduced alpha. Rejected (for now): full obstacle-avoiding routing — the Sugiyama
  waypoints already minimize crossings after D9; avoidance is polish, revisit only
  if D9+dimming still reads badly.
- **D11 — Undo/redo toast.** Transient bottom-center toast "Undid: <command name>" /
  "Redid: …", MOTION_SLOW in, ~1.4 s hold, fade. Command names already exist on the
  undo stack. Shape it like the save flash, as a UI-tree overlay node.
- **D12 — Discoverability.** Hold-`?` shortcut overlay (grid of the real keymap,
  generated from the binding table, not hand-listed) + every context-menu/tooltip
  shows its shortcut. Trigger event: Peter did not know Cmd+L existed while asking
  for exactly that feature.
- **D13 — Value formatting is per-unit, not per-param.** One table in code:
  floats 2 dp · integers bare · beats as `fmt_free_period` already does
  (`param_slider_shared.rs:726`) · percent 0 dp · degrees 0 dp with `°`. Applied
  wherever a param value renders (card, graph inline cell, macros).
- **D14 — OPEN (Peter's eye, escalate before implementing):** graph node corner
  radius — 4px to harmonize vs 0px as deliberate technical-canvas statement. Ship a
  4px variant behind one constant and render both `graph` scene PNGs for his pick.

## 3. Design body — the one new piece

```rust
// crates/manifold-ui/src/anim.rs (new, ~100 lines)
pub struct AnimF32 {
    current: f32,
    target: f32,
    from: f32,
    t: f32,        // 0..1 progress
    dur_ms: f32,   // one of MOTION_FAST/MED/SLOW
}
impl AnimF32 {
    pub fn set_target(&mut self, v: f32);        // restarts from current
    pub fn snap(&mut self, v: f32);              // no animation (reduced-motion / init)
    pub fn tick(&mut self, dt_ms: f32) -> bool;  // true = still animating (keep dirty)
    pub fn value(&self) -> f32;                  // eased current
}
```

Single cubic ease baked in (D1). Widgets own their `AnimF32` fields inside existing
per-row/per-panel UI state (the card-reuse mechanism from the design-system pass keeps
that state stable across rebuilds — anchor: reuse-by-effect-id described in
`UI_DESIGN_SYSTEM_AND_INSPECTOR_REDESIGN.md`). The panel tick calls `tick(dt)` and
keeps its node dirty while any returns true. Hot-path rule: zero allocations; fields,
not HashMaps.

Everything else in this plan modifies existing code; seams are listed per phase.

## §4. Phasing

Every phase: fresh session, read-back first (this doc §1-§3 + the files the phase
touches), batch edits, ONE verify cycle at the end. Test scope is manifold-ui-local
until P7's single workspace-adjacent sweep. `ui-snap` renders are the visual gate;
Peter eyeballs taste-tagged items.

### P1 — Motion foundation
- **Entry:** anchors in §1 re-verified; `cargo test -p manifold-ui --lib` green.
- **Deliverables:** `anim.rs` (D3) + `MOTION_*` tokens (D1) + reduced-motion check;
  applied to: kit chip hover/press (background + 1px press drop) and drawer
  open/close height.
- **Gate (positive):** `anim.rs` unit tests (progress, retarget mid-flight, snap);
  Peter feels hover/drawer in the running app. **(negative):**
  `rg -n "AnimF32" crates/manifold-renderer crates/manifold-app/src/content*` → 0 hits ·
  `rg -n "Arc<Mutex|thread::spawn" crates/manifold-ui/src/anim.rs` → 0 hits.
- **Forbidden:** timer threads; animating anything in manifold-renderer; easing
  library dependency; touching content-thread code at all.

### P2 — Motion patterns
- **Entry:** P1 merged.
- **Deliverables:** tab-ink slide, card collapse + caret rotate, value-change flash
  (brightness pulse on slider fill when value changes from binding/MIDI — glanceable
  "something moved this"), undo/redo toast (D11).
- **Gate:** toast renders in a new `ui-snap` still (structure only); Peter eyeballs
  motion; `cargo test -p manifold-ui --lib` green.
- **Forbidden:** animating layout sizes other than listed; toast queueing systems
  (latest wins, one slot).

### P3 — Color & token craft
- **Deliverables:** D4 mute-dim (delete MUTED_COLOR/SOLO_COLOR) · D7 visible slider
  track · D8 white ring on selected layer header · duplicate scrollbar family
  collapsed to one · text `_C32` aliases made real aliases · transport "Off" LED
  idle value lifted to legible · LFO pill badge → CHIP_RADIUS rect (one badge
  geometry per header).
- **Gate (positive):** `ui-snap all`; `states` scene shows muted lane visibly dimmer
  than normal (Peter confirms); slider-track Δ-luma check per D7. **(negative):**
  `rg -n "MUTED_COLOR|SOLO_COLOR" crates/` → 0 hits ·
  `rg -n "SCROLLBAR_BG|SCROLLBAR_HANDLE\b" crates/` → 0 hits (old family gone).
- **Seam brief:** token deletions are compiler-driven — delete first, fix reds.
  Re-derive user counts before editing: `rg -c "MUTED_COLOR" crates/` (doc snapshot:
  bitmap_painter + M-pill sites; if count differs, list before touching).
- **Forbidden:** adjusting any identity color, any header saturation (D8);
  "improving" adjacent colors not listed.

### P4 — Geometry craft
- **Deliverables:** D6 radius collapse (+ `LIST_CELL_RADIUS`) · clip name-strip
  bottom corners rounded to body silhouette (clip_draw.rs strip pass) · timeline
  scrollbar thumb pill (route through `scroll_container` radius or mirror it) ·
  selection ring concentric (ring radius = body radius + ring offset) · D5 bipolar
  center-fill · layer-seam fix: lane bg under `CLIP_VERTICAL_PAD` made visibly
  distinct from the void so the inter-layer boundary reads as one line (try pad 6→4
  + explicit lane fill; render `timeline` scene 4× crop at the seam for Peter).
- **Gate (positive):** `ui-snap` timeline/states/inspector + 4× seam crop; bipolar
  fixture (min<0) renders centered fill. **(negative):**
  `rg -n "BUTTON_RADIUS|CARD_RADIUS|POPUP_RADIUS" crates/manifold-ui/src` → only
  `color.rs` alias definitions remain, or 0 if fully migrated.
- **Seam brief:** radius migration is compiler-driven: delete old consts, fix reds
  per D6 mapping (BUTTON→CHIP for the 2 button sites; BUTTON→LIST_CELL for the 5
  cell sites; CARD 5→6; POPUP→6). Worked example: `macros_panel.rs:231`
  `.radius(color::BUTTON_RADIUS)` → `.radius(color::CHIP_RADIUS)`.
- **Forbidden:** touching graph-editor value-cell styling (held for Peter — standing
  decision); changing CLIP_RADIUS value itself; blanket search-replace of numeric
  radii (only tokened sites move).

### P5 — Graph editor legibility
- **Deliverables:** D9 preset position re-bake (write the batch tool, run it, commit
  regenerated JSONs) · D10 wire under-node dimming · port-label LOD (hide below the
  same threshold params use; ⚠ VERIFY-AT-IMPL the constant per §1) · param-row echo
  fix (bound name == param name → render once; else ellipsize target only) · D13
  value-format table · investigate-and-fix the graph-vs-editor scene node-border
  divergence (`ui-snap graph` borderless vs `editor` cyan-bordered — one is
  unintended; find the style fork, unify, report which).
- **Gate (positive):** `ui-snap graph --preset <3 presets>` + `editor` before/after
  PNGs; `cargo test -p manifold-renderer --lib bundled_presets` (preset JSONs
  changed — required per repo rule). **(negative):** re-bake touches ONLY
  `editor_pos` fields: `git diff --stat` on preset JSONs shows no wire/param changes
  (`rg -n '"wires"|"params"' <(git diff)` → 0 hits).
- **Forbidden:** hand-editing positions; changing graph topology; "improving" preset
  params while in the files; obstacle-avoiding wire routing (D10 rejected it).
- **Escalate:** D14 (node radius) — render both variants, stop for Peter's pick.

### P6 — Feel & chrome
- **Deliverables:** D12 shortcut overlay + shortcuts in tooltips/menus · split/resize
  affordances (hover grip + cursor on the 3 main splits) · window chrome (unsaved
  dot, proxy icon, "<project> — MANIFOLD" title) · pointer cursor audit→fix (resize/
  grab/scrub zones set system cursors) · text-input polish audit→fix (caret blink =
  MOTION timing, selection highlight, double-click word, Cmd+A).
- **Gate:** shortcut overlay generated from the real binding table
  (negative: `rg -n "Cmd\+" <overlay source>` shows no hand-maintained string list);
  Peter drives the app for feel items.
- **Forbidden:** rebinding any existing shortcut; new keymap systems.

### P7 — Harness & audits (final phase, single sweep)
- **Deliverables:** `ui-snap` scenes `popup` (dropdown open + modal + toast),
  `hover` (forced hover states), `drag` (clip drag ghost + insert line) · pixel-diff
  ratchet (golden PNGs + max-diff threshold, CI-runnable) · findings files (not
  fixes): stage-readability audit (font-size floor per surface), 4K/1080p scale
  audit, glyph-coverage audit vs UI strings, elevation/shadow consistency pass →
  one shadow scale proposal for Peter.
- **Gate:** new scenes render in CI; ratchet catches a deliberately-broken token in
  a dry run (prove the trap springs). Single `cargo test -p manifold-ui -p
  manifold-renderer --lib` sweep + `cargo clippy --workspace -- -D warnings`.
- **Forbidden:** fixing audit findings in-phase (they're the next plan's input);
  golden images from a dirty tree.

## §5. Decided — do not reopen
1. Headers stay full-saturation (Peter, verbatim in D8).
2. Blue keeps selection+active roles; only the white header ring ships.
3. Motion = 90/160/240 ms, one cubic ease, chrome-only.
4. No animation clock thread; `AnimF32` ticked by the UI frame.
5. Radius system = 2/4/6 (+3 for list cells under a cell-named token).
6. Mute dims toward BG_1; MUTED_COLOR/SOLO_COLOR deleted.
7. Preset layout fix = re-bake via existing `auto_layout`, not layout-on-load.
8. No obstacle-avoiding wire router in this plan (dim-under-node only).
9. Graph value-cell table styling stays untouched (Peter's eye, standing).
10. "Array" is the user word; grids stay button grids (older standing decisions
    touching these surfaces — see decision log).

## §6. Deferred
- **Perform-mode skin** — rides PERFORM_SURFACE P1; revive when its chrome exists.
- **Commerce surface polish** (license entry, watermark, update prompt) — rides
  COMMERCIALIZATION_DESIGN phases; must use this kit when built.
- **Trackpad gestures** (pinch zoom, two-finger graph pan) — revive after P6 ships;
  own small design (event-loop work, not styling).
- **Full wire routing** — revive only if D9+D10 still read badly to Peter.
- **Hierarchy re-judgement** — only if Peter re-raises after real clip thumbnails
  ship in the timeline (clip bodies currently empty in fixtures).
- **Click-to-photon measurement** — profiler exists; revive with the perf campaign.
- **Graph-editor untokenized literals (69)** — already tracked by the ratchet and
  the graph-editor redesign; not this doc's scope.
