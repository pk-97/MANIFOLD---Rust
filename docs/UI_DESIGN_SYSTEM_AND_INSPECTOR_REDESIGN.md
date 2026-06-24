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
Retire the three divider constants.

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
- Cards **collapse by default**; expand to see controls.
- Each slider gets a **collapsible drawer** holding its modulation config.
- The modulation buttons **E (envelope) / → (driver/LFO) / A (audio)** move *into* the drawer
  as **tabs** — a row never shows three button clusters at once.
- A **glance badge** on the collapsed row shows modulation state (filled = armed).
- **Reset (↺)** lives in a fixed right-side column — same spot every row (Resolve pattern).
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

### 6.4 Toggle/trigger rows (e.g. "Clip Trigger")
Today toggle/trigger rows ([param_card.rs:1529](../crates/manifold-ui/src/panels/param_card.rs#L1529))
**left-align** their label while slider rows **right-align** in a column, and the button is
pinned far-right — so the row doesn't line up with the slider grid and reads as bolted-on.
Fix is the same row template: left-align *all* labels in one column, put the toggle/button in
the same right control column as slider values. **Open question:** "Clip Trigger" is a card
*behaviour* setting (does this generator react to clip launches), not a look param — consider
moving it to a card-settings/header area rather than the tail of the param list.

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
- **Button grids** for option params (→ dropdowns).
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
- Exact token values (grey ramp steps, radii, spacing) — §4 is a proposal to lock.
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
3. **Design tokens** — audit `color.rs`, lock the grey ramp / spacing / radius / divider tokens.
4. **Components** — build Toggle / Dropdown / SegmentedControl / IconButton / Button / ParamRow
   on the tokens.
5. **Prototype on Edge Detect only** — assemble the new card; run the app and screenshot each
   pass (real bitmap, not a mockup). Lock the look. Fix Clip Trigger row alignment here (§6.4).
6. **Roll out** — apply the template to every card, then the rest of the inspector.

Each visual pass is verified by running the app and screenshotting — truth over speed, since
the renderer is custom.

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
