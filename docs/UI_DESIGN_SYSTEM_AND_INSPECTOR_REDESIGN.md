# UI Design System & Inspector Redesign

**Status:** Layout changes shipped; design-system + inspector redesign scoped, not built.
**Owner:** Peter. **Captured:** 2026-06-24 (from a working session).
**Scope:** the visual language of the whole UI (tokens), and a full redesign of the
inspector / effect cards on top of it.

This document captures everything decided in that session: what already shipped, the
problems we identified, the design rules we adopted (from reference apps + complaint
research), the token system, the component set, the card redesign, what gets removed,
and the build order. It is the single source for this work — update it as we go.

Related: [UI_ARCHITECTURE_OVERHAUL.md](UI_ARCHITECTURE_OVERHAUL.md) (the structural/declarative
overhaul), [GRAPH_AND_UI_POLISH_PLAN.md](GRAPH_AND_UI_POLISH_PLAN.md). This doc is the
*visual design system* layer that those didn't cover.

---

## 1. Goal

Manifold is a live VJ instrument (Ableton-workflow meets Resolume-performance). The UI is
competent and dense but reads as **flat**: grey-on-grey-on-dark-grey, inconsistent padding,
weird groupings, hand-hacked dividers, and effect cards crammed with one-off button styles.

Target: as deep as the pros (Resolume, Resolve, Ableton, TouchDesigner, Blender) but
**legible, consistent, and calm** — and tuned for *live* use, which most desktop-app
checklists ignore.

---

## 2. Shipped this session ✅

All committed and pushed to branch `ui-layout-fullwidth-timeline`.

| Change | What | Commit |
|---|---|---|
| Inspector full-height | Right panel now runs transport-bottom → footer-top (was a short top-right box with wasted space). | `bc61a1ae` |
| Layer headers → left | Track headers moved to the left of the timeline body; tracks scroll to their right (DAW/NLE convention). | `bc61a1ae` |
| Default tab → Layer | Selecting a clip or layer now opens the Layer tab, not Clip. Clip tab still one click away. | `bc61a1ae` |
| Footer = global chrome | Status bar is now full-width, pinned to the very bottom — the bottom counterpart to the transport bar. | `a52b2997` |

Files touched: [layout.rs](../crates/manifold-ui/src/layout.rs),
[viewport.rs](../crates/manifold-ui/src/panels/viewport.rs),
[state_sync.rs](../crates/manifold-app/src/ui_bridge/state_sync.rs).
`ScreenLayout` remains the single source of truth — all panels follow its accessors.

---

## 3. The core problem (grounded in `color.rs`)

The flatness is not a taste issue; there is **no design system underneath**. Every panel
hand-picks its own grey, padding, and divider, so it drifts. Evidence from
[color.rs](../crates/manifold-ui/src/color.rs):

- **~15 background greys with no ramp.** Many are visually identical:
  - `INSPECTOR_BG` 26 ≈ `CONTROL_BG` 27 ≈ `DROPDOWN_BG` 27 ≈ `TRACK_BG_ALT` 27
  - `TRACK_BG` 36 ≈ `PANEL_BG` 37
  - So "layers" of the UI don't read as separate — that *is* the grey-on-grey.
- **Three different divider colours** — `SEPARATOR_COLOR` 15, `GROUP_SEPARATOR_COLOR` 10,
  `DIVIDER_COLOR` 56. Inconsistent "hacked-together lines."
- **No spacing or radius scale** — paddings are ad-hoc per panel; everything is hard rectangles.
- Text tiers are actually fine already: `TEXT_NORMAL` 224 / `TEXT_DIMMED` 158 /
  `TEXT_SUBTLE` 107 / `TEXT_FAINT` 80.
- One accent exists: `ACCENT_BLUE` (89,148,235). Keep it.

**Fix:** define tokens once; make everything consume them. Grouping comes from *fill level*,
not lines.

---

## 4. Design tokens — the foundation 🧱

Build these first. Without them, any new card drifts again within weeks.

> **Status: LOCKED + implemented (Phase 3).** Tokens live in
> [color.rs](../crates/manifold-ui/src/color.rs) under the `DESIGN TOKENS` banner.
> The existing semantic constants (`PANEL_BG`, `INSPECTOR_BG`, `DROPDOWN_BG`, …) now map
> *onto* the ramp rather than each hand-picking a grey, so editing one token shifts every
> surface that consumes it. This is a **global, visible palette shift** (panels darken from
> ~37 to bg-1 22; the colliding 26/27 greys spread across distinct steps) — everything is in
> one file and trivially tunable, but it needs an eyeball pass on the running app.

### 4.1 Grey ramp (the big one)
Replace the muddle with a small ramp where each step is clearly distinct (~9–10 values apart):

```
bg-0   app background     ~13   (keep DARK_BG)
bg-1   panel              ~22
bg-2   card / section     ~31
bg-3   control / input    ~42
hover  one notch up (+~8 on the relevant level)
```
Grouping = fill level, not boxes. A section is `bg-2` sitting on `bg-1`; a control is `bg-3`
sitting on `bg-2`. Collapse the ~15 existing greys down to this ramp (+ a couple of
purpose-specific ones like HUD/overlay).

### 4.2 Spacing scale
4px base: **4 / 8 / 12 / 16 (/24)**. One rhythm everywhere. Kill ad-hoc paddings.

### 4.3 Radius
Small and consistent: **~3px** controls, **~5px** cards. Softens the hard rectangles
without going consumer-app bubbly.

### 4.4 Dividers
**One** hairline colour, used *between groups only* — not as boxes around everything.

> **Deviation from "retire all three" (deliberate).** Grounded in usage, the three constants
> are actually **two roles**: `SEPARATOR_COLOR` (15) + `GROUP_SEPARATOR_COLOR` (10) are the
> dark *track grooves* in the timeline / layer panel, while `DIVIDER_COLOR` / `DIVIDER_C32`
> (both 56) are the light *chrome hairlines*. Forcing the timeline grooves to a light hairline
> would restyle the most-used surface blind. So Phase 3 collapses the **redundancy** into one
> token per role — `DIVIDER` (hairline, 56) and `GROOVE` (12) — instead of one global value.
> The old names persist as thin aliases. If we want grooves gone too, it's a one-line change.

### 4.5 Text tiers
Keep the existing ramp (primary/secondary/dim/faint). Ensure each clears a contrast floor
against its background level.

### 4.6 Accent + state colours
One accent (the blue) used **sparingly and boldly** for active/selected. State colours
(armed / on / warning) defined once. See §11 — never colour alone.

### 4.7 Honest caveat: contrast steps, not brightness
"High contrast" for a live tool means clearly *distinct levels*, not a *bright* UI. Keep the
palette dark — a bright UI is fatiguing on stage and the screen glows in a dark room.
Distinct steps + bold accents, still dark.

---

## 5. Component vocabulary 🧩

A small typed set, built on the tokens, applied everywhere. Built on the existing
Chrome/View declarative API.

> **Status: built (Phase 4).** Kit lives in
> [chrome/components.rs](../crates/manifold-ui/src/chrome/components.rs), on the Phase-3
> tokens. Each component has **two forms** because the runtime has two write paths: a
> `*_style(state) -> UIStyle` (for the in-place `set_style` update path) and a `*(..) -> View`
> constructor (for the declarative build path). Built: Toggle, Button (primary/secondary),
> IconButton, SegmentedControl (`segment` cell), Dropdown trigger, plus the ParamRow trailing
> atoms (`reset_button`, `mod_badge`). The **full ParamRow composite is deferred to Phase 5** —
> it has to thread the live slider materialisation + drag state that lives in `param_card`, so
> it gets assembled in that generic card and tuned against Edge Detect as the reference instance.
> These supersede the scattered
> `*_btn_style` helpers in `param_slider_shared`; the old helpers stay in use until Phase 6
> swaps them out. The kit is unused until Phase 5 wires it — intentional (build the kit, then
> apply it), and it's covered by 7 unit tests.

| Component | Used for |
|---|---|
| **Toggle** | `ON`, `Inv`, `Delta`, mute/solo — one style, shape *and* colour |
| **Dropdown** | option lists: `Source`, `Feature`, `Band`, `Mode` — flat, single-level; **type-ahead** (first char jumps + steps through matches) |
| **SegmentedControl** | nav tabs (Clip/Layer/Master) + any param flipped *live* |
| **IconButton** | hamburger menu, chevrons |
| **Button** (primary / secondary) | `Change`, dialog actions |
| **ParamRow** | label · slider · value · modulation badge · reset; **double-click value → type-in** (numeric params only) |

Dropdowns are the default for option pickers (your call — reduces clutter, scales). The
guardrail: keep them **single-level flat lists**; never bury a frequent action in a menu.

---

## 6. Inspector / card redesign 🎛️

Goal: every card identical and calm; clutter hidden until wanted; modulation legible.

### 6.1 Card header template (same for every effect *and* the generator)
```
☰   Title …………………………   ● On   ▾
↑menu  ↑name (fills)        ↑toggle ↑expand
```

### 6.2 Behaviour
- Cards collapse/expand per-card (persisted); new cards stay **expanded** (Ableton/Resolve
  convention) and a **Collapse-all / Expand-all** control declutters a big stack — see 5c.
- Each slider's modulation config lives in a drawer below the row.
- **Modulation — DECIDED (5e), revised from the original plan.** The **E / → / A arm buttons stay
  on the row** (one-click arm — fast for live; moving them into the drawer would make arming 3
  clicks). The original pain — three *config drawers* stacking when several are armed — is fixed
  by giving them **one shared drawer with E/→/A tabs that appears only when ≥2 configs are
  active**. One armed mod shows its config directly (unchanged); arming a mod focuses its tab.
  Track overlays (driver/audio trim bars, envelope target) stay on the slider for *every* armed
  mod regardless of the open tab.
- A **glance badge** on the collapsed row shows modulation state — *already done* via the
  header DRV/ENV/ABL/MOD chips (visible when collapsed too).
- **Reset** — *already done*: right-click a slider track resets it to default (no icon needed).
- **Drag-scrub number fields** (drag or type) so sliders can shrink and reclaim width.
- **Type-in any numeric param** — double-click the value cell → type → clamp to range →
  dispatch via the same path as a drag edit. Reuses the existing `TextInputState`
  ([text_input.rs](../crates/manifold-app/src/text_input.rs)); graph params / FPS / BPM already
  type, the inspector sliders are the only gap. Enums use dropdowns, toggles stay toggles — no
  text entry.
- **The handle + number already track the live post-modulation value** (`param_values` via
  `sync_values`), so "value rides" is *done*. The open gap is marking the *base setpoint* while
  modulated — see §11.

### 6.3 Mockups (rough — spacing tuned in the real renderer)

Inspector, top to bottom (cards collapsed by default):
```
┌ Layer ─────────────────── Master ┐
│ ▸ Macros                          │
│ ▾ Layer · Gen 10                  │
│     Opacity   ▓▓▓▓▓▓▓▓▓░  1.00    │
│ ▾ Plasma                   ● On ▾ │
│     Pattern   ▓▓░░░░░░░░  2       │
│     Speed     ▓▓▓▓▓░░░░░  4.00    │
│ ▸ Edge Detect              ● On   │
│ ▸ Infrared                 ● On   │
│ ▸ Bloom                    ○ Off  │   ← off = hollow + greyed
│                                  ▕│   ← slim auto-hide scrollbar
└───────────────────────────────────┘
```

Effect card collapsed → expanded:
```
┌───────────────────────────────────┐
│ ☰  Edge Detect            ● On  ▸ │
└───────────────────────────────────┘
┌───────────────────────────────────┐
│ ☰  Edge Detect            ● On  ▾ │
├───────────────────────────────────┤
│ Amount     ▓▓▓▓▓▓▓░░░  0.96  ○A ↺│
│ Threshold  ▓░░░░░░░░░  0.00  ●A ↺│   ← ●A = audio-armed
│ Mode       [ Sobel        ▾ ]    ↺│   ← dropdown, not button grid
└───────────────────────────────────┘
```

Param row → modulation drawer open:
```
│ Amount     ▓▓▓▓▓▓▓░░░  0.96  ●A ↺│   ← click the ●A badge
│  ┌ Modulation ──── [ Env  LFO ◀Audio ] ┐ │  ← tabs, one at a time
│  │ Source  [ Audio 1 ▾ ]  Feature [ Flux ▾ ]│
│  │ Band    [ Full    ▾ ]  Amount  ▓▓▓░ 1.00 │
│  │ Attack  ▓░ 5ms   Release ▓▓ 120ms        │
│  │ Invert ○      Delta ○                     │
│  └───────────────────────────────────────────┘ │
```

The clutter win (one row, before vs after):
```
now:    Amount  ▓▓▓▓▓░░  0.96  [E][→][A]
        Source [Audio1]  Feature [Amp|Cen|Noi|Flux|Tra]
        Band [Full|Low|Mid|High]  Inv  Δ  ...always shown
after:  Amount  ▓▓▓▓▓░░  0.96  ●A  ↺      ← guts in the drawer
```

### 6.4 Toggle/trigger rows (e.g. "Clip Trigger") — DONE (5b + 5d)
The original misalignment was two parts: labels (toggle left, slider right) and the button
pinned to the far edge. **5b** flipped slider labels to left-align, so all labels now share one
column start. **5d** right-aligns the toggle/trigger button to the **same control column as
slider values** (`x = cx + slider_w`) instead of the card's far edge — a toggle can't be
modulated, so the D/E/A lane to its right is correctly left empty, and the row now lines up
with the slider grid. **Still open (deferred):** "Clip Trigger" is a card *behaviour* setting
(does this generator react to clip launches), not a look param — moving it to a
card-settings/header area is a structural change, parked for now (the alignment was the felt
problem; relocation is a separate call).

### 6.5 Modulation drawer — follow-ups (decided 2026-06-24, eyeball of 5e)
5e shipped the tabbed drawer. Eyeballing it on Layer 2 → Transform surfaced four more, in build order:

1. **Rename the three modulators to full words. ✅ SHIPPED 2026-06-24.** The old "Envelope" mod is
   **no longer a full ADSR** — it's a trigger-fired decay (target + decay). Renamed everywhere
   (tabs, arm buttons, header chips):
   - **Trigger** (was Envelope / E / ENV) — arm button **T**, chip **TRG**.
   - **LFO** (was Driver / → / DRV) — arm button shows the **waveform icon** (the renderer's SDF
     glyph U+E000..E004 for the driver's current shape, default sine), chip **LFO**. A plain "∿"
     char isn't in the UI font and renders as tofu — must use the PUA icon glyph.
   - **Audio** (was A) — arm button **A**.
   Tabs spell the full word; arm buttons stay compact glyphs to match.
2. **Hide all mod settings while keeping mods armed. ✅ SHIPPED 2026-06-24** (Peter's ask: "hide all
   the settings while leaving modulation enabled"). A **global compact toggle** (⚙ in the inspector
   tab strip, left of Collapse-all) hides **every** card's modulation config drawers at once. Mods
   stay armed (arm buttons lit) and the slider track overlays still show the live ranges — only the
   config drawers collapse. Implemented as inspector `mods_compact` → `card.set_compact()` →
   `build_param_row(show_drawer=false)` (empty `active_tabs` ⇒ no tab strip / no drawer, height 0).
   *Deferred:* a **per-row ▾/▸** (the doc's original finer-grained idea, not requested) — the global
   toggle covers "hide all"; add per-row only if finer control is wanted.
3. **LFO drawer redesign — keep the grid, neaten it, add a free period. ✅ SHIPPED 2026-06-24.**
   The earlier "grids → dropdowns (blanket)" decision is **reversed.** Eyeballing the grid, Peter:
   *"this is actually good and useful"* — keep it as a button grid, just make it **standardised,
   ordered, neat, logical.** What shipped, three uniform-width button rows:
   - **Row 1 — Rate grid:** the 11 beat-division cells (1/32…32), now **uniform width** (were
     ragged/proportional). Lights the base division in sync mode; none in free mode.
   - **Row 2 — Rate detail:** `[Straight][Dotted][Triplet][Free]`. The feel trio replaces the
     cryptic **"." / "T"** toggles with a mutually-exclusive segment whose labels say what they do
     (addresses the "better UX around dotted/triplet" ask). **Free** opens a beats type-in.
   - **Row 3 — Shape + polarity:** the 5 waveform icons (kept — Peter: "the current icons are
     pretty good already") then **Invert** (renamed from **"Rev"** — verified correct: `reversed`
     is `1 - value`, an amplitude invert, matching audio mod's INV).
   - **Free period (PRO).** New serialized `ParameterDriver.free_period_beats: Option<f32>`
     (omitted when `None`; old projects round-trip). `None` = sync (grid/feel); `Some(p)` = free,
     the LFO cycles every **p beats** → polyrhythm against the bar. The type-in takes a single
     **beats** number (`3`, `1.5`, `0.375`); fractions/bars stay the grid's job (unambiguous).
     `evaluate_with_period()` is the shared core; grid/feel click clears free (back to sync).
   Audio Source/Feature/Band grids were **not** touched (they stay grids). The blanket dropdown
   conversion is dropped.
4. **Merge the chrome header to one row. ✅ SHIPPED 2026-06-24.** Layer chrome was three stacked
   rows (type header / name / opacity); now one: `[Layer 2 ▾]  [Opacity ▓▓▓▓▓░ 1.00]`. Master
   chrome moved opacity inline onto the title row (`[Master FX ▾] [Opacity]`); the LED row stays
   below. Clip chrome is a content panel (single name row + source / warp / trigger sections) with
   no header+opacity triple — nothing to merge, left as-is.

Also note — **card panels are now reused across rebuilds** (matched by effect id; generator by
layer), not re-allocated every sync. Fixed the mod-tab snap-back (UI-only tab state was on a
card thrown away each frame) and removed a per-frame allocation. Shipped.

---

## 7. Text & density rules

- **Left-align param labels** in a fixed column (kill the ragged right-aligned labels).
- **Type scale:** card title bolder / param label regular / value in tabular figures (digits line up).
- **Tighter row rhythm** (~24–26px) and consistent card padding.
- **Keep one column** — sliders want width for fine live control; just shorter rows.

---

## 8. Being removed ❌

- The always-visible **E / → / A** button trio on every param (→ into the drawer).
- The **repeated full audio-mod panels** shown per param (→ collapsed by default).
- **Ragged right-aligned labels** and **mismatched per-card headers**.
- **Knobs as a control idea** — explicitly rejected (see §10).

---

## 9. Stolen from reference apps 📐

From the screenshots reviewed (Resolume Arena, DaVinci Resolve, Blender, Ableton Live):

- **Resolve — consistent right-side affordance column.** Every section row identical:
  enable-dot left, name, value, reset ↺ / keyframe pinned right, all vertically aligned.
  Biggest legibility win. → our ParamRow template.
- **Resolve — "toggle row, guts hidden until on."** Off features are just a labelled toggle
  until enabled. → our collapse-by-default + drawers.
- **Resolume — one-line collapsed effects** with bypass/reset/delete + inline value bar.
  → our collapsed card header.
- **Blender / Resolve — drag-scrub number fields** (drag or type). → reclaims slider width.
- **Resolume — Dashboard layout** (a compact macro row) for the Macros section —
  *layout* idea only, **not** the knobs (see §10).

**Parked for later:** Blender's vertical icon tab-rail (right edge) — only worth it if
inspector scopes multiply past Clip/Layer/Group/Master.

---

## 10. Pitfalls to avoid (complaint research) ⚠️

Recurring complaints across the pro apps, and the rule each implies. Sources at the bottom.

- **Tiny / non-scaling text** (Ableton's #1 gripe — pixels vanish on hi-res). You run at
  3456×2234. → type scale in logical units, DPI-aware, minimum legible sizes; test at native res.
- **Low contrast, grey-on-grey** (general). → real contrast on text and on active-vs-idle states.
- **Overwhelming / cluttered / too many panels** (Resolve, TouchDesigner). → collapse-by-default,
  progressive disclosure.
- **Can't reclaim screen space** (Resolume — wants to minimise layers). → everything collapses.
- **Inconsistency / forced relearning** (Blender 2.8). → don't ship half-standardised; one
  template applied everywhere or it's worse than before.
- **Deep nested menus + hamburger-hidden features kill discoverability** (Blender, UX lit). →
  flat single-level dropdowns; don't hide core actions behind the ☰.
- **Knobs are mouse-hostile** (Slashdot/HN consensus: knobs are the least useful UI control;
  sliders/number-drag are more natural with a mouse). → **no knobs**; use compact sliders or
  drag-scrub fields. (This overrode an earlier idea to copy Resolume's knob dashboard.)

---

## 11. Live-performance-specific rules 🎚️

Not in most desktop checklists — these are ours because the tool is played live.

- **Modulation legibility** — the classic "why won't this move?" **Status: already largely
  handled** — the slider handle + number track the live post-modulation value (`param_values`,
  written each frame by drivers/Ableton/envelopes, fed via `sync_values`
  [state_sync.rs:749](../crates/manifold-app/src/ui_bridge/state_sync.rs#L749)). The green
  range / orange target handles show the mod's config. The "green band" is the audio-mod
  *output sub-range* (trim handles), **not** a live indicator. Remaining gap: the **base
  setpoint isn't cleanly marked** while the handle rides live (you lose sight of the value you
  set).
- **Shape + colour, never colour alone** — ~8% of men can't separate red/green, and on a dark
  stage with coloured wash, hue-only states wash out. An armed toggle changes fill/icon, not
  just hue.
- **Generous hit targets** (Fitts) — current inspector handle is 4px, split handle 6px.
  Visually thin is fine; decouple the *grab zone* from the *draw width*.
- **Affordances** — hover states + cursor changes on everything clickable (flat design hides
  what's interactive).
- **Visible reset** per row (not hidden double-click).
- **Right-click is a shortcut, never the only path** to an action.
- **Tooltips show the keyboard shortcut.**

---

## 12. Decisions & open questions

**Settled:**
- Layout: full-height inspector right, headers left, global footer. (Shipped.)
- Default inspector tab = Layer.
- Dropdowns are the default for option params; segmented control only for nav + live-switched params.
- Modulation lives in a per-slider collapsible drawer, with E/→/A as tabs inside it.
- No knobs.
- **No HTML / Claude Design mockups** — prototype directly in the native bitmap renderer.
  HTML would misrepresent the real font/metrics/AA, and we build in Rust anyway.
- Tokens come *before* components, components before applying.
- **Type-in for numeric params** — double-click → type → clamp → dispatch; reuses `TextInputState`.
  Enums/toggles excluded (dropdown / toggle, not text entry).
- **Dropdown type-ahead** — first char jumps `hovered_index` and steps on repeat; slots into the
  existing dropdown `KeyDown` ([dropdown.rs:597](../crates/manifold-ui/src/panels/dropdown.rs#L597)).
- **Per-card audio level meter — dropped** for now.
- **Correction:** the inspector slider already shows the live post-modulation value (handle +
  number). An earlier note here that it didn't was wrong (it read only the build path, missed
  the per-frame `sync_values` update).

**Open:**
- **Which params (if any) are switched *live*?** Those stay segmented (one-click); everything
  else becomes a dropdown. Best guess: Feature/Band/Source/Mode are all set-once → all dropdowns.
- **Clip Trigger** — keep as an aligned toggle row, or move to a card-settings area? (§6.4)
- **Base setpoint marking** while a param is modulated — the handle rides live, so the value you
  *set* isn't shown cleanly. Worth a subtle base tick / hover-reveal.
- **"Modulated-only" view** — optional per-layer filter to audit just audio-driven params (idea,
  not committed).
- ~~Exact token values (grey ramp steps, radii, spacing) — §4 is a proposal to lock.~~ **Locked
  in Phase 3** (13/22/31/42 ramp, 3px/5px radius, 4/8/12/16/24 spacing); tunable in one file.
- Whether to match transport/footer heights exactly or keep a deliberate ratio.

---

## 13. Build order

1. **Quick wins** — match transport/footer heights ✅ (done, `FOOTER_HEIGHT` locked to
   `TRANSPORT_BAR_HEIGHT`). NOTE: the inspector scrollbar **already exists** — a 4px draggable
   thumb on both columns ([inspector.rs:1654](../crates/manifold-ui/src/panels/inspector.rs#L1654)),
   not missing. Any visibility polish (width/contrast) folds into Phase 3 tokens, not a separate add.
2. **Type-in + dropdown type-ahead** — extend `TextInputState` to inspector numeric params
   (double-click → type → clamp → dispatch); add type-ahead to the dropdown `KeyDown`.
   Self-contained, infra already exists.
3. **Design tokens** ✅ — `color.rs` audited; grey ramp (`BG_0..BG_3` + hover/pressed), spacing
   (`SPACE_XL` 16 / `SPACE_XXL` 24), card radius (4→5), and the two divider tokens (`DIVIDER`
   hairline + `GROOVE`) locked. Semantic greys re-pointed onto the ramp. Static checks pass
   (clippy, 385 ui tests); the global palette shift needs an eyeball pass on the running app.
4. **Components** ✅ — typed kit in [chrome/components.rs](../crates/manifold-ui/src/chrome/components.rs)
   on the tokens: Toggle / Button / IconButton / SegmentedControl / Dropdown trigger + ParamRow
   atoms (reset, mod badge). Two forms each (`*_style` + `View` constructor). Full ParamRow
   composite deferred to Phase 5 (needs the live slider/drag wiring). 7 tests, clippy clean.
5. **Redesign the generic card** ✅ — landed once in `panels::param_card` (hits all cards). Shipped
   as five passes: **5a** card radius token + toggle on the kit; **5b** left-align labels; **5c**
   Collapse-all / Expand-all control (new cards stay expanded); **5d** toggle/trigger rows aligned
   to the slider grid (§6.4); **5e** tabbed modulation config drawer, one-click arm kept (§6.2).
   Grounding showed most of the original list already existed (reset = right-click, glance badge =
   header chips, type-in = Phase 2b, per-slider drawers), so Phase 5 was the genuine deltas. Static
   checks pass each pass (param_card tests incl. golden + 3 new tabbed tests; clippy). **Still needs
   the running-app eyeball** — the renderer is custom, can't screenshot here.
6. **Modulation-drawer follow-ups (§6.5) — ✅ Phase 6 COMPLETE 2026-06-24.**
   **6a** ✅ renamed modulators (Trigger / LFO / Audio; arm T / waveform-icon / A; chips TRG / LFO).
   **6b** ✅ global compact toggle (⚙ in the tab strip) hides every card's mod drawers while mods
   stay armed; per-row ▾ deferred (not requested).
   **6c** ✅ LFO drawer redesign (grid kept + standardised to uniform cells, feel segment
   Straight/Dotted/Triplet, Rev→Invert, **free period (pro)** via beats type-in; blanket
   grids→dropdowns *reversed* per Peter's eyeball).
   **6d** ✅ chrome header merged to one row (layer + master; clip N/A — content panel).
   Card-panel reuse (mod-tab snap-back fix) already shipped. **Whole phase still needs Peter's
   running-app eyeball** — custom renderer, can't screenshot here.
7. **Verify across the variety + roll through the inspector.** The single reference card can't
   show everything: effects with many params, multiple enums, string params, generators (purple
   tint), macros, clip params. Check the generic redesign against that spread and fix edge cases —
   **no new design work** — then the rest of the inspector chrome.

Each visual pass is verified by running the app and screenshotting — truth over speed, since
the renderer is custom.

---

## 14. Padding & layout rules — the sub-element grid 📏

**Status:** **A–F all shipped 2026-06-25 (Phase 3, automated).** Spacing + radii snapped to
the scale, the inspector inset unified to one column, the right gutter shared, the inter-card
gap single-owned, and the header rhythm unified — each verified by tests (a layout-geometry
column-alignment test replaced the running-app eyeball gate). The rules below are the SSOT for
every spatial constant in the UI.

### 14.1 The problem (grounded)
The scale was locked in Phase 3 (§4.2) but **the layout code never consumed it.** A parallel
set of hand-picked magic numbers lives in the panel files, most of them *off* the 4px grid.
Same disease for radius (§4.3 tokens exist; raw `corner_radius: 1.0/2.0/4.0/7.0/8.0` literals
scattered everywhere). The visible symptoms in the inspector:

- **Insets nest, so columns stagger.** A section label starts at `CONTENT_PADDING_H` 8; an
  effect-card param label starts at `8 (section) + 6 (card PADDING) = 14`; a clip label at 10.
  So "Amount" / "Zoom" don't share a left column with "Bloom" / "Position X" — off by 6px.
- **No fixed right column.** Row value+`T/∿/A` icons right-align within a card, but section-header
  trailing controls (Change, ON, chevron, cog) right-align to a *different* edge.
- **Four row-band heights** — `HEADER_HEIGHT` 27.5 / content 24 (20+4) / section header 22 /
  small row 18 → uneven striping.
- **Off-scale repeat offenders:** `5` (`GROUP_Y_PAD`, `ITEM_SPACING`, `LAYER_CTRL_PADDING`),
  `3` (`EFFECT_CONTAINER_SPACING`, `ELEM_Y_PAD`), `6` (`PADDING`, `CARD_BOTTOM_MARGIN`,
  `CONTENT_PADDING_V`).

This pass does **not** make the UI prettier in a taste sense (colour / hierarchy / density live
in §4–§7). It removes the drift: same insets, same columns, same radii. That's the win.

### 14.2 The eight rules

1. **One inset, one owner.** Horizontal inset = **8 (`SPACE_M`)**, owned by the card.
   Section/clip containers contribute **zero** horizontal padding. Insets never nest — a bare
   section header and an effect-card param label start at the *same* x.
2. **Every spatial constant snaps to the scale.** `SPACE_XS 2 / S 4 / M 8 / L 12 / XL 16 /
   XXL 24` is the SSOT. No constant lives off it (vertical row heights are the one tolerated
   exception — see rule 5).
3. **One affordance grid.** The inspector row is fixed columns:
   `[inset 8][label][slider flex][value][mod-icon lane][inset 8]`. The value+icon gutter is one
   fixed width, and section-header trailing controls right-align to the **same** gutter x.
   (Resolve's right column, §9 — made literal.)
4. **Three vertical gaps, max.** In-card row spacing **4**; between cards **8**; between major
   sections **12**. One owner per gap — `CARD_BOTTOM_MARGIN` → 0, the container owns the
   inter-card gap.
5. **One row rhythm.** Content row **24** (20 + 4). Card header **28**. Section header **24**.
   Small/macro rows are a documented second tier (18) — heights are about visual rhythm, not the
   horizontal grid, so fewer-distinct-values is the rule, not strict mult-of-4.
6. **Radius = four tokens.** Controls/buttons **`BUTTON_RADIUS` 3**; cards/sections
   **`CARD_RADIUS` 5**; chips/dots/small handles **`SMALL_RADIUS` 2**; popups **`POPUP_RADIUS` 6**.
   No raw literals. (Sub-pixel-thin overlay bars ≤6px wide may keep `1.0` as a documented hairline
   exception — eyeball call.)
7. **Hit target ≠ draw width** (carry-over from §11). Snapping draw sizes never shrinks a grab
   zone below the Fitts floor.
8. **Tokens, not local copies.** Per-file constants (`LH_BTN_RADIUS`, `SECTION_RADIUS`,
   `CELL_RADIUS`, `LAYER_CTRL_PADDING`, …) become thin aliases onto the global tokens, or are
   deleted. One edit shifts every surface.

### 14.3 Constant → token map (inspector — the pain)

> **⚠️ §2.5 audit correction (2026-06-25, Phase 3).** The eight `inspector_layout::*` rows below
> were **dead** — a Unity-port leftover (`InspectorLayout.cs`) the live inspector grew past;
> nothing in the workspace referenced `inspector_layout::`. **The whole module was deleted in
> Phase 3.** The rows are kept here only to record that the map pointed at the wrong code. The
> *real* inspector insets that produce the column stagger are:
> `panels/inspector.rs::SECTION_INSET` (4), the section-card 1px border, `param_card::PADDING` (6),
> and each chrome panel's own header inset (`master_chrome`/`layer_chrome`/`clip_chrome`). **The
> inset-unification (C/D) must be rewritten against those before execution** — and because the
> dependent `slider_w`/`label_width`/`cog_x` already *derive* from `PADDING` (they cascade for
> free), the risk isn't the recompute; it's that `PADDING`→8 *alone* worsens the nest
> (`SECTION_INSET 4 + border 1 + 8`). Rule 1 is a **coupled** move — `SECTION_INSET`→0 **and**
> `PADDING`→8 **and** the three chrome header insets, re-aligned together. That pairs naturally
> with **Phase 4's `section_header` template** (D right-aligns header controls to the shared
> gutter, which needs the template to exist). Verify with a layout-geometry test asserting the
> effect/gen card param label, the chrome header label, and the section content share one x —
> no GPU needed; the columns are node bounds, not pixels.

| File · const | Now | → Target | Note |
|---|---|---|---|
| `inspector_layout` · `CONTENT_PADDING_H` | 8 | `SPACE_M` 8 | the canonical inset |
| `inspector_layout` · `CONTENT_PADDING_V` | 6 | `SPACE_S` 4 | |
| `inspector_layout` · `CONTENT_SPACING` | 4 | `SPACE_S` 4 | ✓ |
| `inspector_layout` · `CLIP_PADDING_H` | 10 | `SPACE_M` 8 | unify to inset |
| `inspector_layout` · `CLIP_PADDING_V` | 8 | `SPACE_M` 8 | ✓ |
| `inspector_layout` · `CLIP_SPACING` | 6 | `SPACE_S` 4 | |
| `inspector_layout` · `EFFECT_CONTAINER_SPACING` | 3 | `SPACE_M` 8 | owns inter-card gap |
| `inspector_layout` · `SECTION_HEADER_HEIGHT` | 22 | 24 | row rhythm (eyeball) |
| `param_slider_shared` · `PADDING` | 6 | `SPACE_M` 8 | card inner inset |
| `param_slider_shared` · `GAP` | 4 | `SPACE_S` 4 | ✓ |
| `param_slider_shared` · `ROW_SPACING` | 4 | `SPACE_S` 4 | ✓ |
| `param_slider_shared` · `DE_BUTTON_GAP` | 2 | `SPACE_XS` 2 | ✓ |
| `param_slider_shared` · `corner_radius` 1.0/2.0 | 1/2 | `SMALL_RADIUS` 2 | hairline exception ok |
| `param_card` · `HEADER_HEIGHT` | 27.5 | 28 | |
| `param_card` · `CARD_BOTTOM_MARGIN` | 6 | 0 | gap owned by container |
| `param_card` · `CHEVRON_W` / `COG_W` | 18 | 16 or 20 | pick one (eyeball) |
| `param_card` · `corner_radius` 2.0 | 2 | `SMALL_RADIUS` 2 | dots/chips ✓ |

> Recomputing `PADDING` cascades into `slider_w`, `label_width`, and the header trailing-x math
> (`cog_x`/`chevron_x`/`toggle_x`) in `param_card`. That's Phase C, the one risky step.

### 14.4 Constant → token map (chrome — second pass)

| File · const | Now | → Target |
|---|---|---|
| `header` · `GROUP_Y_PAD`, `GROUP_SPACING` | 5 | `SPACE_S` 4 |
| `transport` · `ITEM_SPACING` | 5 | `SPACE_S` 4 |
| `transport` · `GROUP_Y_PAD`, `RIGHT_SPACING` | 4 | `SPACE_S` 4 ✓ |
| `footer` · `ELEM_Y_PAD` | 3 | `SPACE_S` 4 |
| `footer` · `PAD` | 8 | `SPACE_M` 8 ✓ |
| `layer_header` · `PAD` (`LAYER_CTRL_PADDING`) | 5 | `SPACE_S` 4 |
| `layer_header` · `REC_PAD` | 6 | `SPACE_S` 4 |
| `layer_header` · `LH_BTN_RADIUS` | 2 | `SMALL_RADIUS` 2 (alias) |
| `macros_panel` · `SECTION_RADIUS` | 4 | `CARD_RADIUS` 5 (and `-1.0` → `BUTTON_RADIUS` 3) |
| `browser_popup` · `corner_radius` 8/7 | 8/7 | `POPUP_RADIUS` 6 |
| `browser_popup` · `corner_radius` 4/2 | 4/2 | `BUTTON_RADIUS` 3 / `SMALL_RADIUS` 2 |

### 14.5 Build order

- **A — Spec & freeze (this section).** ✅ no code; the maps above are the freeze.
- **B — Spacing snap (mechanical, low-risk).** ✅ 2026-06-25. Tokenised the live §14.4 spacing
  constants onto `SPACE_*` (`param_slider_shared`, `header`, `transport`, `footer`, `layer_header`);
  value changes: section-header 22→24, chrome gaps 6/5/3 → 4, card header 27.5→28. The §14.3
  `inspector_layout` rows turned out **dead and were deleted** (see §14.3 correction). Deferred the
  `EFFECT_CONTAINER_SPACING`/`CARD_BOTTOM_MARGIN` gap pair (→E). All golden-layout oracles green.
- **B′ — Radius snap (sibling of B).** ✅ 2026-06-25. All 53 raw `corner_radius`/`.radius()`
  literals → radius tokens; local copies (`SECTION_RADIUS`, `LH_BTN_RADIUS`) aliased/inlined. Added
  `HAIRLINE_RADIUS` (1px) as the named rule-6 hairline exception for thin bars/tracks/fills. One
  survivor: a `// design-token-exempt:` circular status dot. **§16 RADIUS_BASELINE lowered 53 → 0
  — the radius guard is now absolute.**
- **C — Unify the inset.** ✅ 2026-06-25. The §14.3 map pointed at dead code; the live stagger was
  border-less chrome content (`PAD_H` 2) vs bordered card param labels (`BORDER 1 + PADDING 6` = 7),
  three different left edges. New token `color::SECTION_CONTENT_INSET` (`SPACE_M + 1px` border
  compensation) is the one column; `param_card::PADDING` 6→`SPACE_M` (card owns the canonical inset,
  `slider_w`/`label_width`/`cog_x` cascade from it); `master`/`layer`/`clip` chrome `PAD_H` 2→
  `SECTION_CONTENT_INSET`. **Not** the §14.3-literal `SECTION_INSET`→0 — that erases the visible
  section-card frame; keeping the frame and aligning *content* to one column is the right call. New
  test `param_label_column_aligns_to_section_inset` pins it on node bounds (no GPU). Visible change:
  chrome controls shift right ~7px to meet the card label column.
- **D — Shared right column.** ✅ 2026-06-25. The generator header padded `r: 0` (trailing controls
  flush to the inner edge) while the effect header and the param rows' value/mod-icon lane already
  right-align to `inner_right - PADDING`. Gave the gen header `r: PADDING` → one shared right gutter.
  Golden oracle re-frozen to the gutter position. (The cross-panel header-*title* x-alignment, a
  bigger restyle of every chrome header, is the only piece left for a future `section_header` pass.)
- **E — Row rhythm + gaps.** ✅ 2026-06-25. Inter-card gap single-owned: `CARD_BOTTOM_MARGIN` 6→0,
  `inspector::SECTION_GAP` 6→`SPACE_M` (was 6+6=12, now one 8). One header height: new token
  `color::HEADER_ROW_HEIGHT` (28) wired into the card header and the master/layer chrome headers
  (was a 27.5-vs-28 half-pixel split). `SECTION_CARD_RADIUS` 4→`CARD_RADIUS` (rule 6). Content row
  (24) and the 18/22.5 second tier kept (rule 5 tolerates a documented second tier).
- **F — Roll across variety.** ✅ 2026-06-25. 419 manifold-ui lib tests (which build every panel
  variety — effect/gen cards, master/layer/clip chrome, macros) + the geometry test + the token
  ratchet + workspace build, all green. clippy `-D warnings` clean.

Each structural phase ends: `cargo clippy --workspace -- -D warnings` + `cargo test -p manifold-ui
--lib` + the layout-geometry alignment test (replaces the old "Peter's running-app eyeball" gate —
the columns are node bounds we can assert headlessly).

### 14.6 Out of scope
- The floating **"Bloom" / "Highlight Boost"** text seen over the Text-section rows is a
  *rendering-overlap bug*, not a padding issue (same family as the macros-panel overlap fix).
  Separate ticket.

---

## 15. Semantic colour ramp 🎨

**Status:** spec. The grey ramp (§4.1) fixed the *neutrals*. The *chromatic* state colours never
got the same treatment — they're the same pre-ramp muddle, one hue-step lower.

### 15.1 The problem (grounded in `color.rs`)
State colours are hand-picked per spot, so each hue has many near-identical copies:
- **~7 reds** — `PLAYHEAD_RED` 217,64,56 · `STOP_RED` 128,51,51 · `RECORD_RED` 107,38,38 ·
  `RECORD_ACTIVE` 209,46,46 · `EXPORT_ACTIVE` 184,56,56 · `BPM_CLEAR_ACTIVE` 133,51,51 ·
  `MUTED_COLOR` **255,0,0** (pure red, off any sane ramp).
- **~7 greens** (`PLAY_GREEN`/`PLAY_ACTIVE`/`STATUS_DOT_GREEN`/`SAVE_FLASH_GREEN`/
  `BPM_RESET_ACTIVE`/`MONITOR_ACTIVE`/`SYNC_ACTIVE`), **~4 ambers**, **~3 oranges**.
- So "active red" is a different red in every widget. On a dark stage under coloured wash,
  inconsistent hue + brightness washes out — this is a *live-performance* legibility bug, not
  just untidiness.

### 15.2 The fix — one ramp per role-hue, three steps each
Define **idle · base · active** for each hue once; map roles onto hues. The point isn't *fewer*
colours — it's *one definition per hue*, so the same red means the same thing everywhere.

| Hue | idle | base | active | Roles |
|---|---|---|---|---|
| **RED** | 107,38,38 | 184,56,56 | 217,64,56 | record · stop · destructive · mute-warn |
| **GREEN** | 51,107,61 | 64,158,89 | 64,184,82 | play · monitor · confirm · save |
| **AMBER** | 156,128,40 | 204,166,38 | 217,191,64 | warn · solo · paused |
| **ORANGE** | 140,82,30 | 199,102,56 | 209,115,56 | envelope mod · mute-active · status |
| **BLUE** | 77,122,199 | 89,148,235 | 120,170,245 | accent · selection · active control |
| **CYAN** | 40,120,140 | 20,166,191 | 64,200,224 | driver/LFO mod · link/sync |
| **PURPLE** | 90,72,120 | 115,115,191 | 150,130,210 | generator identity / gen-card tint |

Values are a starting point — **tune on the running app** (the warm trio red/amber/orange must
stay distinguishable when mute/solo sit adjacent). Mute and envelope can *share* orange because
they never collide in one widget; the rule is consistent steps, not artificial collapse.

Collapses ~25 hand-picked constants → 7 hues × 3 steps. The old names persist as thin aliases
onto the ramp (same approach as the grey re-point in Phase 3), so call sites don't churn.

§11 still holds: **shape + colour, never colour alone** — the ramp makes hue consistent; armed
state still also changes fill/icon.

---

## 16. Enforcing the system — the systemic root ⚙️

**Status:** BUILT (2026-06-25, Phase 2). **Highest-leverage item in this whole doc.**

Tokens exist (§4) and still drift (§14, §15) because **nothing stops a raw literal.** That's why
the cleanup sections have to exist — and why they re-drift in weeks without a guard. A design
*system* makes violations fail CI; right now they're only discouraged. This is the difference
between a cleanup and a system.

### 16.1 The rule
**All colours and radii are defined in `color.rs`; call sites reference tokens only.** No raw
`Color32::new(` and no raw `corner_radius:`/`radius(` float literals anywhere outside the token
module. Spacing constants reference `SPACE_*`.

### 16.2 The guard
A `manifold-ui` unit test that walks `src/**` and **fails** on:
- `Color32::new(` outside `color.rs`,
- `corner_radius: <float>` / `.radius(<float>)` outside `color.rs`,
- (stretch) numeric spacing literals in layout structs not traceable to `SPACE_*`.

Cheap, deterministic, runs in the existing `cargo test -p manifold-ui --lib`. An allowlist
comment (`// design-token-exempt: <reason>`) covers the rare honest exception (e.g. the ≤6px
hairline bars in §14.2 rule 6). Once green, the system is *enforced*, not aspirational.

### 16.3 Built — the ratchet (Phase 2, 2026-06-25)
[`crates/manifold-ui/tests/design_tokens.rs`](../crates/manifold-ui/tests/design_tokens.rs)
(`cargo test -p manifold-ui --test design_tokens`). It scans `src/**` (excluding `color.rs` and
`node.rs`, the token/type homes) and counts two categories: raw `Color32::new(` and raw
`corner_radius:`/`.radius(` *numeric* literals (a `color::` token or computed expression starts with
a letter, so it isn't flagged). `// design-token-exempt: <reason>` on a line clears it.

Because colour (§15) and radii (§14 B′) aren't cleaned yet, a hard "zero raw literals" rule would
fail today. So it's a **ratchet** keyed on per-category baselines (high-water marks):
- count **rises above** baseline → fail (new drift — use a token or exempt it);
- count **drops below** baseline → fail (a cleanup landed — *lower the baseline* to lock it in).

So the number can only go down. **Baselines: `COLOR_BASELINE = 145` (still grandfathered, pending
§15); `RADIUS_BASELINE = 0` (Phase 3 B′ tokenised all 53 — the radius guard is now absolute).**
§15 drives the colour count down next. The classifier (detection
+ exempt) is unit-tested directly (`classifier_detects_and_exempts`), so the guard's own logic is
trusted, not just the baseline. Spacing-literal enforcement (§16.2 stretch) is deferred — noisier to
detect; revisit after §14's spacing snap lands.

---

## 17. Elevation & separation 🪟

**Status:** spec. The UI is flat fills + a muddle of **5 near-identical border greys**
(`CARD_BORDER` 46 · `CARD_BORDER_C32` 55 · `RACK_BORDER` 56 · `DROPDOWN_BORDER` 58 ·
`GEN_CARD_BORDER_C32` 58 purple-tinted). Floating things — dropdown, browser popup, mod drawer —
read as *glued* to the panel; there's no language that says "this is above."

**Fix — a 2-level elevation language:**
- **Flat (in-panel):** the §4.1 fill ramp only, no border. Cards/sections separate by fill level,
  as already decided (§4.4 "grouping = fill level, not boxes").
- **Raised (floating):** one **`BORDER`** hairline token (collapse the 5 greys → one, ≈ `DIVIDER`
  56) **plus a single soft drop-shadow** under popovers/dropdowns/drawers. One shadow step, not a
  Material-style ramp — just enough to lift off the panel.

Keep it subtle: a live tool in a dark room shouldn't glow. Borders/shadow are for *floating*
elements only; in-panel grouping stays fill-level.

---

## 18. Apply the component kit everywhere 🧩

**Status:** chrome bars DONE (2026-06-26); popups + dialogs remain. Added **`state_button`** — the
standalone latching/momentary button (on = filled semantic hue + lighten(30)/darken(20) hover/press;
off = neutral `BUTTON_DIM` chip), the generalisation of `toggle` (the accent special-case). The
button mechanic had been copy-pasted six times across the chrome (transport `button_style`, the
layer-card `mute/solo/led/analysis` quartet, header `action/zoom`); it now lives once in the kit.
**Migrated:** footer, transport, layer-card mixer (carve-out hues unchanged), header (and fixed a
within-bar 59-vs-71 neutral-grey split). Each verified by a headless render in `ui_color_swatches.rs`.
**Remaining:** the three popups (`dropdown`/`browser_popup`/`ableton_picker`) onto a shared
`popup_shell`; the param card's `*_btn_style` one-offs in `param_slider_shared`.

**Rule:** no bespoke button / toggle / dropdown / segmented styling. Every instance is a kit
component on the §4 tokens + §15 ramp. Audit each panel; replace one-offs; delete the local style
helpers (`*_btn_style` in `param_slider_shared`, per-file `LH_BTN_*`, etc.). After this, the kit —
not the panels — owns how a control looks.

**Two neutral greys, by background tier (intentional, not drift):** `button_secondary` sits at
`BG_3` (42) for controls on the *lighter* inspector/dialog cards; `state_button` off sits at
`BUTTON_DIM` (71) for standalone chips on the *dark* chrome bars + colour-card mixer. Value-relative
depth — a control is one step off *its own* background, so the two tiers carry two chips.

---

## 19. Layout hierarchy & micro-motion 🎬

**Status:** spec. Two gaps the grid (§14) doesn't address.

- **Flat hierarchy.** Every card is equal visual weight; the object you're *editing* isn't
  emphasised and the rest doesn't recede. SOTA inspectors lift the focused section (fill +1,
  subtle accent edge) and dim the rest. Pairs with collapse-by-default (§6).
- **Micro-motion (restrained).** No feedback on press / arm / collapse / value-commit. At 60fps in
  the custom renderer this is cheap — and for a *live* tool the SOTA call is restraint: a fast
  button-press flash, an arm-state pulse, a collapse ease. **No** decorative animation (distracting
  on stage). Motion confirms an action; it never idles.
- **Undesigned states.** Empty (no effects), error (load failure), loading (export/decode) likely
  have no considered treatment. Define them once.

---

## 20. Roadmap — system to SOTA

§14–§19 in leverage order. §16 (enforcement) underwrites all the cleanup — do it early so the
rest can't re-drift.

| # | Work | Kind | Risk |
|---|---|---|---|
| §16 | Token-enforcement guard | systemic | low |
| §14 | Padding / layout grid | cleanup | C is structural |
| §15 | Semantic colour ramp ✅ | cleanup | low (aliases) |
| §17 | Elevation / separation ✅ | additive | low |
| §18 | Apply component kit everywhere ◑ (chrome bars done; popups left) | coverage | medium (broad) |
| §19 | Hierarchy + micro-motion | additive | medium |
| §24 | Timeline visual upgrade (clips) | additive + structural | clips→GPU is the gate |

**Honest caveat:** §14–§18 get the *system* to SOTA-grade — consistent, enforced, complete. They
do **not** guarantee the *look* is best-in-class; that's a taste/tuning pass (the ramp values, the
hierarchy emphasis, the shadow weight) settled only by eyeballing the running app. The system can
be perfect and still look ordinary — these fix the system; taste is the layer on top.

---

## 21. Duplication audit 🔁

**Status:** spec (from a 2026-06-25 targeted scan of `manifold-ui/src`). The §14/§15/§18 cleanups
are all instances of **one root pattern**, found everywhere once you look:

> **A shared primitive exists, but only some call sites use it. The rest reimplement it.**

Sliders prove the codebase *can* do this right — there is one slider engine and every panel routes
through it. Buttons, headers, popups, and some drag handlers just never followed that example.

### 21.1 Reimplemented (fix these)

| Domain | Shared home | Reimplemented by | Severity |
|---|---|---|---|
| **Buttons / toggles** | `chrome/components.rs` (the kit) | `transport`, `header`, `footer`, `layer_header` — own draw + style | **HIGH** |
| **Card / section header row** | *none — should be one template (§6.1)* | `master_chrome` `header_row`, `param_card` `effect_header_row`, `clip_chrome` `section_label`, `macros_panel` | **HIGH** |
| **Popup chrome** | `overlay.rs` (positioning only) | `dropdown`, `browser_popup`, `ableton_picker` — own border / radius / shadow / item rows | MED |
| **State colours** | `color.rs` | ~25 hand-picked reds/greens/ambers (§15) | MED |
| **Radii / spacing** | `color.rs` tokens | raw literals (§14) | MED |
| **Widget drag** | `slider.rs::SliderDragState` | `macros_panel::handle_drag`, `layer_header` gain-drag — own drag math | MED |
| **Word-wrap** | *none* | `graph_canvas/model::wrap_text` **and** `graph_editor::wrap_words` (two copies) | LOW |
| **Rect-contains-point** | `node.rs::contains` / `hit.rs` | `mapping_popover::point_in` + inline `x>=…&&…` checks | LOW |

### 21.2 Good citizens (already shared — don't touch)
- **Sliders** — `slider.rs` + `SliderSpec`; every panel routes through it. The model to copy.
- **Text measurement** — one `TextMeasure` trait via `tree.rs`; `truncate_with_ellipsis` shared.
- **Tree hit-test** — `tree.rs::hit_test` is the one widget hit path.
- **1D range contains** — `hit.rs`. **Low-level drag detection** — `input.rs`.
- **Timeline clip hit/drag** (`clip_hit_tester`, `interaction_overlay`) — legitimately its own
  domain, not duplication.

### 21.3 The fix is the same as §16 + §18
There's no new pattern to invent — every row above is "lift/keep one primitive, migrate the call
sites, delete the copies":
- **HIGH** — finish the kit migration (§18) and build the **one** card-header template (§6.1),
  used by master / layer / clip / effect / macros.
- **MED** — give popups one shared chrome (border + §17 shadow + item row); route `macros` and
  `layer_header` drag through `SliderDragState`; the colour/radii/spacing ones are §14/§15.
- **LOW** — merge the two word-wrap fns; one `point_in_rect` helper.
- The §16 guard catches the literal-level ones (colour, radius) automatically once on.

### 21.4 Limits of this scan
Targeted at widgets / popups / headers / drag / text / hit-test. **Not** audited: icon rendering,
event-dispatch wiring, per-panel sync paths, the graph-canvas internals. A full pass would cover
those.

---

## 22. Full duplication audit — 5-agent pass 🔬

**Status:** complete (2026-06-25). Five parallel agents, one per crate slice, read every file in
`manifold-ui/src` (924k tokens, 118 tool calls). This **supersedes §21** (the preliminary scan) —
§21's findings all confirmed, plus much more. One finding is a **live correctness bug**, not tidiness.

### 22.1 Headline: a real bug, not just duplication ⚠️
> **✅ FIXED 2026-06-26 (`da7811f7`).** Hover now routes through the canonical
> `ClipHitTester::hit_test` ([viewport/interaction.rs:32](../crates/manifold-ui/src/panels/viewport/interaction.rs#L32));
> duplicate types removed; tests `hit_test_clip_delegates_to_shared_hit_tester` +
> `hit_test_clip_skips_group_layers` pin it. The original finding kept below for the record.

**Two clip hit-testers disagree.** *Confirmed by direct read.*
- Hover / cursor → `viewport/interaction.rs::hit_test_clip` (called `app.rs:801`, `interaction.rs:89`)
  uses **fixed-width** trim handles (`TRIM_HANDLE_THRESHOLD_PX`, gated by `TRIM_HANDLE_MIN_CLIP_WIDTH_PX`).
- Click / drag → `clip_hit_tester.rs::ClipHitTester::hit_test` (via `interaction_overlay.rs:1109`)
  uses **proportional** handles (`MAX_TRIM_HANDLE_PX 8 .min(width*0.15)`), **and** skips group layers.

Effect on stage: a clip edge can **hover-as-body but grab-as-trim** (and the hover path mis-handles
group layers). The two diverged because `HitRegion`/`ClipHitResult` are **defined twice**
(`clip_hit_tester.rs:16-30` *and* `viewport/model.rs:42-55`) — the type wall hid that they're one op.
**Fix:** delete `hit_test_clip`'s bespoke math; route it through `ClipHitTester::hit_test` like
`hit_test_at` does. Delete the duplicate types; `viewport.rs` re-exports the hit-tester's.
`marker_flag_rect` (draw==hit, unit-tested) is the model to copy.

### 22.2 The recurring families (a shared primitive exists; call sites bypass it)
This is the *whole* disease, now fully enumerated. Sliders, `TextMeasure`, `transform::Axis`,
`CoordinateMapper::layer_height` prove the codebase *can* do this right — these didn't.

| Family | Shared home | Bypassed by | Sev |
|---|---|---|---|
| **Colour lighten/darken** | *none — add `Color32::lighten/darken` to color.rs* | identical `fn lighten/darken` in `clip_chrome`, `layer_header`, `transport`; inline +40 marker (`render.rs` ×2), +30/+15 (`bitmap_painter`), +40 swatch (`dropdown`) — **~7 copies** | MED |
| **Buttons / toggles** | `chrome/components.rs` kit | `transport`, `header`, `footer`, `audio_setup_panel` (own `*_btn_style`); inspector chevrons (`macros`/`master`/`layer`/`param_card`); LED toggle (`master_chrome`), loop toggle (`clip_chrome`) | HIGH |
| **Card/section header row** | *none — add `components::section_header`* | `master_chrome`, `layer_chrome`, `macros_panel`, `clip_chrome` (label-only), `param_card` (×2 w/ extra furniture) | MED |
| **Drag lifecycle** | `drag.rs::DragController<T>` (its own doc lists the 5 machines it replaces) | `macros_panel` (`i32 = -1` sentinel), `layer_header` (redundant `active_gain_drag` beside `SliderDragState`), `audio_setup` band-divider | MED |
| **Hit-test (half-open interval)** | `hit::Span` | `view.rs:30`, `cursor_nav.rs:121`, graph-canvas (`mapping_popover`, `hit.rs`, `interaction.rs` — inclusive `<=`, a latent edge-bug), + the clip hit-tester (§22.1) | MED |
| **Popup shell** (backdrop+border+inner+radius) | *none — add `popup_shell()` + tokens* | `browser_popup`, `ableton_picker`, `dropdown`, `audio_setup` — `BG_BORDER/BG_INNER` consts **already drifted** (19,19,**22** vs 19,19,**20**) | HIGH |
| **Popup edge-clamp** | `overlay::compute_overlay_rect` | `dropdown`, `browser_popup`, `ableton_picker` (SelfManaged → opt out of the shared clamp) | LOW |
| **Char-width estimate** | `text::TextMeasure` | `dropdown` (×7.0), `browser_popup` (×0.6), `graph render` (×0.55) — three magic factors | LOW |
| **Compact float fmt** | *none — add `fmt_trimmed`* | `mapping_popover`, `graph_editor` (×2), + `fmt_opacity`/`fmt_macro`/`fmt_value` scattered | LOW |
| **Angle/Freq value fmt** | *none* | canvas `model.rs:369` vs `graph_editor.rs:1798` — duplicated **per** mirror enum (`ParamSnapshotKind` vs `GraphEditorParamKind`) | MED |
| **Raw colour literals** | `color.rs` tokens | `stem_lane`, `waveform_lane` write `(255,255,255)`, `(173,173,179)` etc. that *equal* existing tokens | LOW |

### 22.3 Build-vs-update desync (a distinct, dangerous class)
The in-place update discipline created **parallel walks that must stay in lockstep or silently desync**:
- **Ruler ticks/labels** built in `render.rs::build_ruler` **and** re-derived in
  `try_update_horizontal_scroll` (which itself walks twice — count then update). Comment admits
  *"same logic as build_ruler."* **HIGH** — scroll silently diverges from a fresh build.
- **Grid subdivision + bar_skip ladder** in `coordinate.rs` **and** re-derived in
  `bitmap_renderer.rs::paint_grid_lines` (*"matches GridOverlay exactly"*). **MED** — painted grid
  drifts from ruler ticks. Fix: hoist `bar_skip_for(px)` / `subdivisions_for(ppb)` to one grid module.

### 22.4 Also found (smaller, real)
- `audio_setup` builds the same `[-] value [+]` stepper **4×**; `reposition_trim_bars` reimplemented in
  `macros_panel`; bezier sample-loop copied (`draw_wire`/`draw_ghost_wire`); BPM button style ×3
  (`clip_chrome`); mute/solo style identical modulo one colour; hamburger 3-bar handle ×2; word-wrap ×2
  (`wrap_text`/`wrap_words`); waveform draw-clamp ×3 (redundant with the painter's own clamp).
- **Root cause behind two of these:** `graph_canvas::Rect` is a *separate* struct from `node::Rect`, so
  `Rect::contains` (via `hit::Span`) can't be reused on the canvas → inline `point_in` ×3.

### 22.5 Good citizens — already shared, do NOT touch
Sliders (`SliderDragState`); `TextMeasure`; `transform::Axis` (canvas + timeline); `CoordinateMapper`
(`layer_height` = "the single rule", tested); `waveform_renderer` + `draw_waveform` + `bitmap_painter`
primitives; `hit::Span` + `node::Rect::contains`; the `chrome` View/Host/components stack;
`drawer` DrawerSpec; `intent` registry; `scroll_container`; `overlay::compute_overlay_rect`;
`marker_flag_rect` (draw==hit). The primitives are good — the bypasses are the bug.

### 22.6 Fix order
1. **§22.1 clip hit-test bug** — ✅ DONE (`da7811f7`): routed through `ClipHitTester`, types unified.
2. **`Color32::lighten/darken`** — ✅ DONE (`e8b92e90`, Phase 1 dedups): one home in `color.rs`, ~7 copies gone.
3. **§16 guard** — turns the literal-level families (colour, radius, button styles) into CI failures.
4. **Buttons kit (§18)** + **`section_header`** — the two HIGH structural ones.
5. **Build-vs-update desync (§22.3)** — extract the shared ruler/grid iterators.
6. The MED/LOW dedups as the relevant files are touched.

---

## 23. Headless render + interaction harness (Phase -1) 📸

**Status:** spec. Build this **first** — it removes ~80% of the "Peter must look" gating that
otherwise blocks every visual phase (§14, §17, §18). The renderer is custom, so this is the only
way I can self-verify visual/interaction changes without a running window.

### 23.1 Why it works
The app is **event-driven**: input → state change → tree rebuild → render. Both ends are reachable
without a window:
- The UI rasters into a **CPU pixel buffer** (`bitmap_renderer.rs`: `pixel_buffer: Vec<Color32>`).
- Text goes through the **real** rasterizer (`ui.draw_text` → `manifold_renderer::text_rasterizer::TextRasterizer`,
  CoreText). The harness reuses it, so fonts / metrics / AA **match the live app by construction** — we
  reimplement nothing.
- Input is a state machine (`input.rs::UIInputSystem`, `UIEvent`); clicks resolve via
  `tree.rs::hit_test(pos)` → the `intent.rs` registry. Synthetic events drive the same path winit does.

### 23.2 What it does
1. **Render-to-PNG.** Build a UI state, render one frame through the real renderer, write a PNG.
   I can `Read` PNGs → I see what I changed.
2. **Interaction injection.** Feed a synthetic mouse down/up at a coordinate into the real input
   entrypoint → `hit_test` → intent dispatch → state mutation → rebuild. No window, no mouse.
3. **Two assertion layers:**
   - **Tree assertions** (deterministic, no pixels, CI-able) — after an action, query the in-memory
     tree: node exists, rect is where expected, **nothing overlaps**, label column x matches. Catches
     alignment/overlap better than eyeballing.
   - **PNG snapshot** — for what the tree can't show (colour, text, AA). Golden-image diff on regression.

### 23.3 Reference test — chevron → drawer
1. Build inspector + collapsed effect card → render PNG #1.
2. Query tree → find chevron node (by `KEY_CHEVRON`), take its center.
3. Inject down+up at that point.
4. Step the app (process event → rebuild).
5. Render PNG #2.
6. Assert: drawer node now present, positioned below the row, no overlap; `Read` PNG #2 to confirm it drew.

Multi-step (drag a slider) = a list of `(event, pos, time)` steps, snapshot at the end.

### 23.4 Seams to confirm in the spike
1. **Painter CPU vs GPU — RESOLVED (doesn't block).** Headless works either way: the GPU parity
   harness already spins up a real `GpuDevice::new()` windowless in `cargo test` and reads textures
   back to CPU. CPU path → read `pixel_buffer` directly; GPU path → `readback()`. Needs a Metal device
   present (always true on Peter's Mac; only a no-GPU CI container would care).
2. **Winit-less input.** `window_input.rs` does the winit→action translation, but it's typed in winit
   (`MouseButton`/`ElementState`). The clean seam is one level lower: panels consume `UIEvent`
   (`input.rs::UIInputSystem`). The spike builds a thin driver that synthesizes `UIEvent`s at a
   coordinate (mirroring `window_input`'s mapping, minus winit). Confirm that layer is reachable
   without a `Window`/`Workspace`.
3. **Injectable clock.** Time-based behaviour (double-click window, drag threshold) must take a passed-in
   timestamp, not wall-clock, so sequences are deterministic.

### 23.5 Accuracy & limits (honest)
- **Accurate:** yes for correctness — same renderer, same rasterizer, same layout. It does **not**
  replace Peter's eye for **taste** (does the colour feel pro, §8/§9 hierarchy) — only for correctness
  (aligned, no overlap, rendered, drawer opened).
- **Scope:** the UI chrome (panels, inspector, popups, timeline). The **video viewport** is GPU/Metal
  (manifold-renderer, IOSurface) — a different offscreen path, not covered by this harness.

### 23.6 Payoff against the 11 phases
- Visual phases (§14 grid, §17 elevation, §18 kit) flip from "Peter-gated each iteration" → "I
  self-check via snapshot + tree assertions; Peter signs off once at the end."
- Pairs with the §16 token guard: **guard catches bad tokens, snapshots catch bad layout, tree
  assertions catch bad structure.** Together they make the per-phase automation (§ build order) safe.

### 23.7 Existing infra — reuse, don't rebuild (inventoried 2026-06-25)
Peter's instinct was right: the GPU/node side already solved the hard half. Phase -1 is **~40% new,
60% reuse.**

**Reuse (already exists):**
- **Headless Metal device** — `GpuDevice::new()` runs windowless in `cargo test`
  ([`tests/parity/harness.rs`](../crates/manifold-renderer/tests/parity/harness.rs)). The whole
  "can we even render with no window" question is already answered yes.
- **Texture → CPU readback** — two impls: the harness `readback()` and
  [`gpu_readback.rs`](../crates/manifold-renderer/src/gpu_readback.rs) (`ReadbackRequest::submit/try_read`).
- **PNG encode** — `image` 0.25 (png feature) in `manifold-media`; `RgbaImage::save()` already
  round-trips PNGs in `image_renderer.rs` tests. (`png 0.18` also in `manifold-app`.)
- **Golden compare** — `assert_bytewise_equal` + the deterministic-fixture / fixed-`ctx` pattern.

**Build new (the genuinely missing 40%):**
- A **UI render entrypoint** that builds a `UITree` for a given state and renders one frame to a
  buffer/texture (the GPU parity harness renders an *effect graph*, not the UI tree — that's the gap).
- A **headless input driver** (synthesize `UIEvent`s at a coordinate + injectable clock; §23.4.2/3).
- **Glue**: tree-assertion helpers (find node by key, rect, overlap) + snapshot save/diff.

**Caveat (Peter's "don't get misled into old infra"):** reuse the harness *bones* (device, readback,
fixtures, compare) — **not** its `run_legacy` / `EffectChain` path, which is the dead Phase-4a legacy
side. The bones are current; the legacy effect path is not.

### 23.8 Spike result — PROVEN (2026-06-25) ✅
All three seams confirmed by a working test:
[`crates/manifold-renderer/tests/headless_ui_spike.rs`](../crates/manifold-renderer/tests/headless_ui_spike.rs)
(`cargo test -p manifold-renderer --test headless_ui_spike`). Compiled first try, runs in ~2.5s.

What it does, fully headless (no winit Window):
1. `GpuDevice::new()` + `UIRenderer::new(&device, Rgba8Unorm)`.
2. Build a `ParamCardPanel` (effect "Blur", collapsed) into a `UITree`, render to a `RenderTarget`,
   readback → **PNG #1 (collapsed)** — header only.
3. Inject `process_pointer(Down)` + `(Up)` at the chevron's center → `drain_events()` yields a
   `UIEvent::Click` on the chevron → `panel.handle_click()` returns **`EffectCollapseToggle(0)`**
   (asserted). The synthetic click really resolves and dispatches.
4. Apply the toggle (`set_collapsed(false)` — the bridge's write, replayed), rebuild, render →
   **PNG #2 (expanded)**: chevron flips ▶→▼, two real param rows (Radius/Strength sliders, values,
   T/∿/A) appear. 54,888 bytes differ — asserted.

Text is real CoreText, sliders/layout/glyphs are the production renderer. Confirms: **render path,
input injection, and the build→click→re-render loop all work with zero window.**

**Two small production additions the harness needed (clean, kept):**
- `ParamCardPanel::chevron_node_id() -> Option<NodeId>` — a public accessor for the keyed chevron
  (mirrors the already-public `mapping_chevron_rect`), so a harness can target it without guessing
  pixels.
- `manifold-foundation` as a **dev-dependency** of `manifold-renderer` (the card config needs an
  `EffectId`).

**Remaining to turn the spike into the harness:** generalize beyond one card (arbitrary panels /
the full `InspectorCompositePanel`), add tree-assertion helpers (find-by-key, rect, overlap), and a
golden-snapshot save/diff. The hard unknowns are now all answered.

---

## 24. Timeline visual upgrade — the clips 🎞️

**Status:** spec (captured 2026-06-26 from a mockup-driven session). **Not covered by §1–§23** —
this whole doc has been the *inspector + chrome*; the timeline lanes were never in scope. The
clip *hit/drag* domain is correctly its own thing (§22.5) and the two hit-testers were already
unified (`da7811f7`), so this chapter is **purely visual** — it adds nothing to hit-testing.

The mockups that drove it (neutral chrome, colour = identity, value-based depth, readable clips)
live in the session scratchpad — direction only, not the spec (§12: prototype in-renderer).

### 24.1 The problem (grounded in code)
The timeline is the most-played surface and it tells you almost nothing:
- **Clips are featureless bars.** [`bitmap_painter::draw_clip`](../crates/manifold-ui/src/bitmap_painter.rs)
  fills a rect + 1px borders. **No label** (names exist in the model, never drawn), **no preview**.
- **Clip colour == layer colour.** [`get_clip_color`](../crates/manifold-ui/src/bitmap_painter.rs#L244)
  uses the exact layer colour for every clip, so clips in a track fuse into one continuous smear —
  you can't see where one ends and the next begins.
- **Only audio has a preview** — [`waveform_renderer`](../crates/manifold-ui/src/waveform_renderer.rs)
  (good, keep it). Video clips and generators draw as solid colour.
- **Four track-height grammars** — `TRACK_HEIGHT` 140 / `COLLAPSED` 48 / `COLLAPSED_GEN` 62 /
  `COLLAPSED_GROUP` 70 in `coordinate_mapper::layer_height`. Headers restructure by type instead of
  badging it.
- **Two cursors** — playhead (red) and insert cursor (blue), both drawn, competing for "where am I."
- **Nav gaps** — button-only zoom (10 fixed levels), horizontal scroll with **no scrollbar thumb**.

### 24.2 Why the clips look flat — the rendering path
Clips are **CPU-painted into per-layer pixel buffers and blitted as flat quads**:
`bitmap_painter` → [`layer_bitmap_gpu::upload_layer`](../crates/manifold-renderer/src/layer_bitmap_gpu.rs#L165)
(`Rgba8UnormSrgb`) → `render_layers` draws them as textured quads. That path has **no rounded
corners, no gradient, no shadow, no image blit — by construction.**

Meanwhile the **GPU UITree path already has the hard primitive**:
[`draw_rounded_rect`](../crates/manifold-renderer/src/ui_renderer.rs#L418) is an SDF rounded-rect
with AA, scissor-depth layers, and CoreText text. The inspector/chrome use it; the clips don't.
**That gap is the whole reason the timeline looks flat.** What the GPU path is *missing* for the
full look: a **gradient fill** (no `gradient` in `ui_renderer.rs` today) and the **one soft shadow**
§17 already wants.

### 24.3 The upgrades (what the mockups showed)
- **A — Readable clips.** Name strip at the top of every clip, a real boundary, a content-preview
  area below. **Clip colour becomes independent of layer** — defaults to the track colour, can be
  overridden per clip. (New: clips need an optional colour-override field; today there's none.)
- **B — Content previews.**
  - *Audio* — done (`waveform_renderer`).
  - *Generator* — scaffolding exists:
    [`preview_request`](../crates/manifold-renderer/src/layer_compositor.rs#L471) is an
    authoring-time node-output preview (editor), **not** wired to timeline clips. Reuse the
    render-to-texture, cache a small preview per generator clip.
  - *Video* — **no infrastructure today.** New: extract a representative decoded frame → downscale →
    cache per clip → upload → sample in the clip quad. The heaviest piece.
- **C — One header grammar + type badges.** Collapse the four height grammars into one with a few
  height presets (collapsed / normal / tall) applied the same way to every type; push type into a
  badge (video / text / generator / group / audio) via icon glyph slots (the renderer's PUA glyph
  system, the one the LFO arm button already uses).
- **D — One clear "now."** Resolve playhead vs insert-cursor so the playback position is
  unmissable — the rule that matters most live.
- **E — Navigation.** Scroll-to-zoom, a draggable scrollbar thumb. (Minimap = optional, §24.6.)

### 24.4 The gating move — clips onto the GPU SDF pipeline
Migrate clip drawing off the CPU bitmap onto the **same GPU SDF pipeline the chrome already uses.**
Then rounded body, name strip, gradient body, lift-on-select, and a thumbnail texture-slot all come
from primitives that exist or are added once in §24.5(1).

It's also a **perf win at show scale** (a real project is 2928 clips, `project_typical_project_scale`):
GPU instanced quads drop the per-frame CPU paint entirely and retire the pixel-shift scroll
optimisation that only exists *because* CPU painting is expensive.

### 24.5 Build order (depends on §15 + §17)
1. **Gradient primitive** in the shared GPU rect shader (`ui_renderer.rs` `RectCommand` +
   fragment). Pairs with §17's single shadow term. Small, contained, benefits chrome too.
2. **Clips → GPU SDF quads** — rounded body, name strip, gradient body, lift-on-select. Verify via
   the §23 headless harness (node-bounds assertions + PNG snapshot), not Peter-gated per iteration.
3. **Thumbnail pipeline** — generator previews first (scaffolding exists), then video poster frames
   (new). Cache per clip; invalidate on trim / source change.
4. **One-grammar headers + type badges** — collapse the four heights; type → icon badge.
5. **Playhead/insert-cursor clarity + nav** (scroll-zoom, scrollbar thumb).

### 24.6 Out of scope / not now
- **A dedicated perform-mode timeline** (bigger targets, stripped chrome, "what's playing now").
  There *is* a Perform button; whether perform gets its own timeline treatment vs one shared surface
  is an **open design fork** — decide before investing past §24.5(2).
- **Minimap / arrangement overview** — optional; revisit if jump-around-live still feels slow after E.
- **Clip hit/drag** — already its own unified domain (§22.5, `da7811f7`); untouched here.

### 24.7 Dependencies on the rest of the doc
- **§15 (colour ramp)** — clip colours desaturate onto identity hues; chrome stays neutral.
- **§17 (elevation)** — the one shadow term is shared between floating popups and clip lift.
- **§19 (hierarchy)** — focused-track emphasis is the timeline echo of focused-card emphasis.
- **§23 (headless harness)** — the verifier for every step here.

---

## Sources (complaint research)
- Ableton — [UI needs an overhaul](https://forum.ableton.com/viewtopic.php?t=225454),
  [text too small](https://forum.ableton.com/viewtopic.php?t=204754)
- Resolume — [minimise layers / screen real estate](https://resolume.com/forum/viewtopic.php?t=21335)
- TouchDesigner — [the problem with TD UIs](https://interactiveimmersive.io/blog/controlling-touchdesigner/the-problem-with-touchdesigner-uis/)
- Blender — [HN: "simply bad UI design"](https://news.ycombinator.com/item?id=12893620),
  [2.8 UI feedback](https://devtalk.blender.org/t/blender-2-8-ui-feedback/558)
- Knobs — [Slashdot](https://tech.slashdot.org/story/17/08/25/1550203/why-are-there-so-many-knobs-in-audio-software),
  [HN sliders vs knobs](https://news.ycombinator.com/item?id=41965058)
- Contrast — [MuseScore low-contrast theme text](https://github.com/musescore/MuseScore/issues/26267)
- Nested menus — [multi-level menu UX](https://www.boundev.ai/blog/multilevel-menu-design-ux-guide)
