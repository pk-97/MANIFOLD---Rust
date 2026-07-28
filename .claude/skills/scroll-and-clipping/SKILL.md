---
name: scroll-and-clipping
description: ScrollContainer usage, GPU scissor clipping, text clipping. Invoke before modifying any scrollable panel.
---
# Scroll & Clipping Architecture Guide

## ScrollContainer — The Canonical Scroll Primitive

All scrollable panels use `ScrollContainer` (`manifold-ui/src/scroll_container.rs`).
It owns: clip region lifecycle, scroll offset + clamping, optional scrollbar, content reparenting.

### Panels using ScrollContainer

| Panel | Instances | Scrollbar | Scroll driver |
|-------|-----------|-----------|---------------|
| Inspector | 2 (master + layer columns) | Yes | Self (mouse wheel on panel) |
| BrowserPopup | 1 (grid) | No | Self (mouse wheel on popup) |
| LayerHeader | 1 (layer rows) | No | External (mirrors viewport Y scroll) |

### Usage pattern

```rust
// 1. Create clip region
let clip_id = self.scroll.begin(tree, viewport_rect);

// 2. Build content (parent to clip_id, or reparent after)
let start = tree.count();
// ... add nodes ...
self.scroll.reparent_content(tree, start);

// 3. Set content height (REQUIRED — without this, scroll clamps to 0)
self.scroll.set_content_height(total_content_h);

// 4. Optional: scrollbar
self.scroll.build_scrollbar(tree, sb_x, &SCROLLBAR_STYLE);
self.scroll.update_scrollbar(tree);
```

### Critical rules

- **Always call `set_content_height()`** — even for externally-driven scroll. Without it,
  `max_scroll()` returns 0 and `set_scroll_offset()` clamps everything to 0.
- **`SCROLL_SPEED` is 1.0** — the app layer already normalizes scroll delta
  (`LineDelta × 20px` in `app.rs`). Don't add another multiplier.
- **Content Y positioning:** Use `self.scroll.content_y(local_offset)` which computes
  `viewport.y + local_offset - scroll_offset`.

## GPU Scissor Rect Clipping (Rects)

UI rects are clipped via Metal GPU scissor rects, NOT geometry clamping.
This was changed from the original mathematical clipping approach which caused
elements to "squish" (compress height, re-center content) at clip boundaries.

### How it works

1. During tree traversal, `PushClip`/`PopClip` events flush scissor batches in `UIRenderer`
2. Each batch records its logical-coordinate scissor rect
3. `prepare()` converts batches to physical-pixel scissor rects
4. `render_in_pass()` issues one draw call per batch with `encoder.set_scissor_rect()`
5. Elements maintain their original bounds — the GPU discards pixels outside the scissor

### Key files

- `manifold-gpu/src/metal/encoder.rs` — `set_scissor_rect()`, `draw_in_render_pass()` with `index_buffer_offset`
- `manifold-renderer/src/ui_renderer.rs` — `ScissorBatch`, `PreparedBatch`, batched rendering
- `manifold-ui/src/tree.rs` — `traverse_flat_range()` pre-pushes ancestor CLIPS_CHILDREN nodes

### Sub-region rendering

`traverse_flat_range` (used for incremental inspector rendering) now walks the parent
chain to find ancestor `CLIPS_CHILDREN` nodes and pushes them before the traversal range.
Without this, sub-region renders miss the column's clip context and draw outside bounds.

## Text & Icon Clipping

Text and icons clip at the pixel level by adjusting quad positions AND atlas UVs
at the clip boundary (`native_text.rs`). This is separate from the GPU scissor —
text is rendered in its own draw call after the scissor is reset.

For each glyph/icon that partially overlaps the clip boundary:
- Quad position is clamped to the clip edge
- UV coordinates are proportionally adjusted so the visible portion maps correctly
- Glyphs fully outside the clip are skipped entirely

## Viewport ↔ LayerHeader Scroll Coupling

The viewport owns the vertical scroll position. LayerHeader mirrors it:

```
Mouse wheel on viewport → viewport.set_scroll(x, new_y)
                         → layer_headers.set_scroll_y(viewport.scroll_y_px())
```

LayerHeader uses ScrollContainer for the clip region, but the scroll offset is
externally driven. It still needs `set_content_height()` so the clamping works.
The viewport and layer header independently clamp from the same layer data —
two clamps from the same data can never disagree.
