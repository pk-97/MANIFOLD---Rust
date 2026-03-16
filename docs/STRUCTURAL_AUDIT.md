# MANIFOLD Rust Port — Structural Audit

> **Date:** 2026-03-17
> **Purpose:** Systematic bug-finding checklist for AI agents working on the Rust port. These are bug *patterns* that can be found by reading code — no runtime testing required. Run this audit after every major porting pass.

---

## How to Use This Document

Each section defines a **bug pattern**, explains **why it happens**, provides **grep/search strategies** to find instances, and lists **known findings** from the 2026-03-17 audit. Agents should:

1. Read the pattern description
2. Run the suggested searches
3. Verify each finding against the Unity source
4. Fix or flag for review

---

## Pattern 1: Divergent Duplicates

**What:** The same conceptual value (hit rect, threshold, color, layout dimension) is computed or defined in two different places using different logic or constants. One gets updated, the other doesn't.

**Why it happens:** Agent ports a feature, then later ports a different feature that needs the same value. Instead of importing the existing function/constant, it re-derives or hardcodes it.

**How to find:**
- Grep for magic numbers that match named constants (e.g., `6.0` near handle code → should be `INSPECTOR_RESIZE_HANDLE_WIDTH`)
- Grep for the same constant name defined in multiple files (e.g., `SNAP_THRESHOLD_PX`)
- Grep for inline bounds checks that replicate a `layout.*()` or `hit_test_*()` function
- Search for `Color32::new(` in panel files and cross-reference against `color.rs`

### Known Findings (2026-03-17)

| Issue | Severity | Location | Fix |
|-------|----------|----------|-----|
| Inspector min width: 196 in `color.rs:305` vs 200 in `ui_root.rs:432` | **BUG** | `color.rs`, `ui_root.rs` | Use single constant from `color.rs` |
| `SNAP_THRESHOLD_PX` defined in both `snap.rs:9` and `viewport.rs:512` | **BUG** | `snap.rs`, `viewport.rs` | Import from `snap.rs`, delete duplicate |
| Export marker color defined in both `color.rs:228` and `transport.rs:49` | Style | `color.rs`, `transport.rs` | Use `color::EXPORT_MARKER_COLOR` |
| Playhead centering `px - PLAYHEAD_WIDTH * 0.5` computed in 4 places | Style | `viewport.rs:607,618,1565,1575` | Extract helper method |
| Insert cursor ruler marker `6.0`/`3.0` hardcoded in 2 places | Style | `viewport.rs:644-648,1599-1604` | Extract constants |
| `DEFAULT_INSPECTOR_WIDTH` (280.0) hardcoded in `layout.rs:33` instead of using `color.rs:307` | Style | `layout.rs` | Use `color::DEFAULT_INSPECTOR_WIDTH` |

---

## Pattern 2: State Destroyed on Rebuild

**What:** A `build()`, `rebuild()`, or `refresh()` method creates a new instance (`Type::new()`) or resets fields, destroying user-mutable state that should survive rebuilds.

**Why it happens:** The agent writes `build()` as a "start from scratch" function (clear + reconstruct), not realizing some state was set by user interaction and must persist.

**How to find:**
- Search for `::new(` inside any `fn build`, `fn rebuild`, `fn rebuild_scroll_panels`
- Search for `= vec![]` or `= Vec::new()` or `.clear()` inside build methods
- Search for `= false` or `= 0.0` or `= -1` resets inside build methods
- For each mutable field on a panel struct, trace: "Is this set by a user action? Does `build()` reset it?"

### Known Findings (2026-03-17)

| Issue | Severity | Location | Fix |
|-------|----------|----------|-----|
| ~~`UIRoot::build()` called `ScreenLayout::new()` resetting split ratio + inspector width~~ | **FIXED** | `ui_root.rs:97` | Changed to `layout.resize()` |
| `LayerHeaderPanel::build()` resets `cached_mute`, `cached_solo`, `cached_selected` to all-false | **BUG** | `layer_header.rs:1012-1016` | Only resize vectors if layer count changes; preserve existing values |

---

## Pattern 3: Missing Persistence Roundtrip

**What:** User-mutable state exists in memory and works during a session, but is never saved to `ProjectSettings` on change and/or never restored from `ProjectSettings` on project load.

**Why it happens:** The agent implements the interaction (drag to resize) but forgets to wire the persistence (save on drag-end, restore on load). The `ProjectSettings` field exists because it was ported from Unity's data model, but the read/write code paths were never connected.

**How to find:**
- List every field in `ProjectSettings` (`settings.rs`). For each:
  - Search for writes: `settings.{field} =` or `.{field} =`
  - Search for reads: `settings.{field}` used in UI/layout code
  - Flag any field that is defined but never written or never read
- Check the project load path (`app.rs` after `load_project`): is there an `apply_saved_layout()` step?
- Check every drag-end / toggle handler: does it persist the value?

### Known Findings (2026-03-17)

| State | Saved on Change? | Restored on Load? | Fix Needed |
|-------|-----------------|-------------------|------------|
| `inspector_width` | **NO** — drag end doesn't write to settings | **NO** — `UIRoot::new()` hardcodes 280 | Write on drag end + apply on load |
| `timeline_height_percent` | **NO** — split drag doesn't write to settings | **NO** — hardcoded 0.30 | Write on drag end + apply on load |
| `effect_browser_width` | **NO** — not yet implemented | **NO** | Wire when feature is ported |
| `effect_browser_open` | **NO** — not yet implemented | **NO** | Wire when feature is ported |
| `saved_playhead_time` | **NO** — never written on save | **NO** — field exists but never applied | Write on save + seek on load |

**Root cause:** Rust has no equivalent of Unity's `WorkspaceController.ApplySavedLayout()`. Need an `apply_project_layout()` method called after every project load.

---

## Pattern 4: Hover / Click / Drag Disagreement

**What:** An interactive element's hover detection, click detection, and drag handling check different rects or bounds, causing the cursor to show one thing but the interaction to do another.

**Why it happens:** Hover is written in one function (e.g., `update_cursor_for_position`), click in another (e.g., `MouseInput::Pressed`), and drag in a third (e.g., `CursorMoved`). The agent hand-rolls bounds in each instead of calling the shared hit-test.

**How to find:**
- For each interactive element, trace the three code paths:
  1. Hover cursor change (usually in `update_cursor_for_position` or `UIInputSystem`)
  2. Click initiation (usually in `MouseInput::Pressed` handler)
  3. Drag handling (usually in `CursorMoved` handler)
- Verify all three call the same function or reference the same rect
- Red flag: inline bounds math (`pos.y >= something && pos.y <= something`) instead of a method call

### Known Findings (2026-03-17)

| Issue | Severity | Location | Fix |
|-------|----------|----------|-----|
| ~~Split handle: hover checked `timeline_body.y`, click checked `timeline_area().y`~~ | **FIXED** | `app.rs:278` | Unified to `layout.is_near_split_handle()` |
| All other elements (inspector edge, clips, trim handles, sliders, buttons, ruler) | **CLEAN** | Various | No issues found — architecture is sound |

---

## Pattern 5: Hardcoded Magic Numbers

**What:** Numeric literals in interaction/layout code that duplicate named constants. When the constant changes, the hardcoded value doesn't.

**Why it happens:** Agent writes code quickly, hardcodes the value instead of importing the constant. Or the constant didn't exist yet when the code was written, and was added later without updating the hardcoded site.

**How to find:**
- Grep for common values and cross-reference with `color.rs` constants:
  - `72.0` → should be `DRAG_EDGE_ZONE_PX` (doesn't exist yet — create it)
  - `900.0` → should be `DRAG_SCROLL_SPEED_PX_PER_SEC` (doesn't exist yet)
  - `0.15` / `0.70` → should be `MIN_TIMELINE_SPLIT_RATIO` / `MAX_TIMELINE_SPLIT_RATIO`
  - `0.25` for min clip duration → should import `MIN_CLIP_DURATION_BEATS` from `trim.rs`
  - `8.0` for trim handles → should be `TRIM_HANDLE_THRESHOLD_PX`
  - `16.0` for min trim width → should be `TRIM_HANDLE_MIN_CLIP_WIDTH`
  - `280.0` for inspector width → should use `color::DEFAULT_INSPECTOR_WIDTH`
  - `0.30` for default split ratio → should be `DEFAULT_TIMELINE_SPLIT_RATIO`
- Grep for `Color32::new(` in panel files — each should have a named constant in `color.rs`
- Grep for `const.*f32` in panel files — local constants that should be centralized

### Known Findings (2026-03-17)

| Literal | Where | Should Be | Count |
|---------|-------|-----------|-------|
| `72.0` (auto-scroll edge zone) | `app.rs:636` | New constant `DRAG_EDGE_ZONE_PX` | 1 |
| `900.0` (auto-scroll speed) | `app.rs:637` | New constant `DRAG_SCROLL_SPEED_PX_PER_SEC` | 1 |
| `0.15` / `0.70` (split clamps) | `layout.rs:206` | New constants `MIN/MAX_TIMELINE_SPLIT_RATIO` | 1 |
| `0.30` (default split) | `layout.rs:35` | New constant `DEFAULT_TIMELINE_SPLIT_RATIO` | 1 |
| `280.0` (default inspector) | `layout.rs:33` | Use `color::DEFAULT_INSPECTOR_WIDTH` | 1 + 30 in tests |
| `0.25` (min clip duration) | `ui_bridge.rs:357-358` | Import `trim::MIN_CLIP_DURATION_BEATS` | ~7 |
| `8.0` (trim handle) | `viewport.rs:442,444` | New constant `TRIM_HANDLE_THRESHOLD_PX` | 2 |
| `16.0` (min trim width) | `viewport.rs:442` | New constant `TRIM_HANDLE_MIN_CLIP_WIDTH` | 1 |
| `56.0` (waveform lane height) | `layout.rs:160,166` | New constants | 2 |
| `4.0` / `0.3` (drag/dblclick) | `input.rs:128-129` | Move to `color.rs` | 2 |
| `SNAP_THRESHOLD_PX` redefined | `viewport.rs:512` | Import from `snap.rs` | 1 duplicate |
| 30+ inline `Color32::new()` | Various panels | Named constants in `color.rs` | ~30 |

---

## Running This Audit

### Quick Checks (< 5 minutes)

```bash
# Pattern 1: Find duplicate constant definitions
rg "const.*: f32 = " crates/ | sort -t= -k2 | uniq -d -f1

# Pattern 2: Find ::new() in build methods
rg "::new\(" crates/ -A2 -B5 | rg -B7 "fn build"

# Pattern 3: Find ProjectSettings fields and their usage
rg "pub (inspector_width|timeline_height|effect_browser|saved_playhead)" crates/

# Pattern 4: Find inline bounds checks (red flag for divergent hit-tests)
rg "cursor_pos\.(x|y) >= .* && .*(x|y) <=" crates/manifold-app/

# Pattern 5: Find hardcoded numeric literals in interaction code
rg "\b(72\.0|900\.0|0\.15|0\.70|280\.0|0\.30)\b" crates/
```

### Full Audit (30-60 minutes)

For each pattern above:
1. Run the grep commands
2. Read each match in context
3. Cross-reference against Unity source for intended behavior
4. Categorize as BUG (will break), STYLE (maintenance risk), or FALSE POSITIVE
5. Fix BUGs immediately, log STYLE issues for batch cleanup

---

## Architectural Invariants (Prevent Future Bugs)

These rules, if followed, prevent all five patterns from occurring:

1. **One rect, one function:** Every interactive element's bounds MUST be computed by exactly one function. Hover, click, and drag all call that function.

2. **No `::new()` in `build()`:** If a struct holds user-mutable state, `build()` must update dimensions only (call `resize()` or equivalent), never reconstruct.

3. **Persist on mutation, restore on load:** Every user-set value that should survive restart MUST be written to `ProjectSettings` in the mutation handler AND read in `apply_project_layout()`.

4. **Constants live in `color.rs`:** All numeric thresholds, dimensions, and colors are defined once in `color.rs` (or a dedicated `constants.rs`). Panel code imports, never redefines.

5. **No inline `Color32::new()`:** Every color used in UI code has a named constant. Inline construction is only acceptable in tests.
