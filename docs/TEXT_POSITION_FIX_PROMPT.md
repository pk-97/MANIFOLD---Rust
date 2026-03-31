# Fix: Text Positioned Too Low and Cut Off

Read `CLAUDE.md` before starting.

## Problem

Text renders with correct glyphs but is positioned too low vertically, causing it to be clipped by panel boundaries. The text content is readable but shifted downward from where it should be.

## Root Cause

CoreText uses Cartesian coordinates (Y increases upward, origin at baseline). The UI renderer uses screen coordinates (Y increases downward, origin at top-left). If the glyph bearing/ascent values from CoreText are applied with the wrong sign during vertex quad generation, all text shifts downward.

## Fix

**File:** `crates/manifold-renderer/src/native_text.rs`

Read the full file before making changes. Focus on these areas:

### 1. Glyph vertex positioning in `prepare()`

Find where glyph quads are positioned. The Y coordinate for each glyph should be computed as:

```
screen_y = cmd.y + glyph_run_position.y - bearing_y
```

NOT:

```
screen_y = cmd.y + glyph_run_position.y + bearing_y  // WRONG — shifts down
```

CoreText's `bearing_y` (from `CTFontGetBoundingRectsForGlyphs`) is the distance from the glyph origin to the TOP of the bitmap, measured UPWARD (positive). In screen coordinates (Y-down), you must SUBTRACT it to move the glyph UP from the baseline to its correct visual position.

### 2. CTFont coordinate system

When getting glyph bounding rects via `ct_font.get_bounding_rects_for_glyphs()`, the returned `CGRect`:
- `origin.x` = bearing_x (horizontal offset from glyph origin) — positive means rightward ✓
- `origin.y` = bearing_y (vertical offset from glyph origin) — positive means UPWARD in CoreText
- `size.width` = glyph bitmap width
- `size.height` = glyph bitmap height

For screen-coordinate rendering:
- Glyph quad X = `text_x + glyph_position.x + bearing_x` ✓
- Glyph quad Y = `text_y + glyph_position.y - bearing_y` (negate because Y is flipped)

Wait — it's more nuanced. The `text_y` passed in from UIRenderer is already the TOP of the text line (computed as `bounds.y + (bounds.height - text_height) * 0.5`). The glyph needs to be positioned relative to that top:

```
glyph_screen_y = text_y + ascent - bearing_y - glyph_height
```

Or equivalently, if using the baseline model:
```
baseline_y = text_y + ascent
glyph_top_y = baseline_y - bearing_y  // bearing_y measured UP from baseline
glyph_screen_y = glyph_top_y          // top of glyph bitmap in screen coords
```

### 3. Ascent handling

Check how the ascent value from CTFont is used. When UIRenderer calls `draw_text(x, y, ...)`, the `y` is the top of the text bounding box (not the baseline). The text renderer needs to:
1. Compute the baseline: `baseline = y + font_ascent`
2. Position each glyph relative to the baseline

If `font_ascent` is not being added (or is negative when it shouldn't be), text drops below the intended position.

### 4. Rasterization position

Also check the glyph rasterization in `rasterize_glyph()` — the draw position within the CGBitmapContext must account for CoreText's Y-up coordinate system. If the glyph is drawn at the wrong position within its bitmap, the UV coordinates will be correct but the bitmap content will be offset.

The bitmap context has Y=0 at the bottom (Core Graphics convention). To draw a glyph at the correct position:
```
draw_y = glyph_height - ascent + padding  // or similar, accounting for descent
```

### 5. Compare with glyphon behavior

The old glyphon-based UIRenderer positioned text using:
```rust
let text_y = bounds.y + (bounds.height - text_size.y) * 0.5;
```

Where `text_size.y` came from `measure_text_cached()`. Make sure `measure_text_cached()` returns a height that matches what glyphon returned. If the CoreText measurement returns a different height (e.g. just ascent vs ascent+descent), the vertical centering will be off.

Check: does `measure_text_cached()` return `(ascent + descent)` as the height? That's what glyphon returned. If it returns just `font_size` or just `ascent`, text will be mispositioned.

## Verification

1. `cargo clippy --workspace -- -D warnings` — must pass
2. `cargo test --workspace` — must pass
3. Launch the app. Text in all panels should be vertically centered within its containing rectangle — not clipped at the bottom, not floating at the top.
4. Check: header bar buttons, inspector labels, timeline ruler numbers, layer headers, transport display, perf HUD.

## Critical Rules

- Only modify `native_text.rs` — do not change UIRenderer or UICacheManager
- Do not change the public API of NativeTextRenderer
- `cargo clippy --workspace -- -D warnings` must pass
- Commit and push when done
