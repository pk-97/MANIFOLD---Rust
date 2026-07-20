# UI Transform Stack — general 2D affine capability for the bitmap UI

**Status:** SHIPPED 2026-07-04 (`transform2d.rs`, `UIRenderer.transform_stack`, `push_transform`/`pop_transform`, `Painter::push_transform` all live in-tree) · approved by Peter 2026-07-04, scoped from live investigation
**Motivating consumer:** caret rotate (P2). **Longer arc:** UI-wide scale/rotation for motion + future tilted/rotated chrome.

## The decision this records
The motion layer's four pieces (`AnimF32`/`Transient`/`FlipList`/exit-state) produce *scalars*; they can't place a rotated glyph or a scaled-about-pivot quad on screen because the draw path is axis-aligned. Rather than approximate (glyph-swap) or bolt a rotation field onto one command type (the point-solution rejected this session), we add a **general 2D affine transform as a first-class draw-path capability**, peer to the depth and clip stacks the renderer already keeps.

## Grounding (verified in this session — anchors for the worker)
- One renderer, not two paths: `impl Painter for UIRenderer` (`crates/manifold-renderer/src/ui_renderer.rs:1465`) is a thin forwarder to `UIRenderer`'s inherent `draw_*` methods; the retained-mode chrome tree-walk and the immediate-mode canvas both emit through the same `UIRenderer`. A stack on `UIRenderer` covers both.
- Rects/lines/text are stored as **commands** (`RectCommand`, `ui_renderer.rs:196`) and expanded to 4 corner vertices in `prepare()` at `ui_renderer.rs:~1128` (`position:[x0,y0]…`), carrying `uv`∈[0,1] and `rect_params:[w,h,corner,border]`.
- The fragment shader (`ui_renderer.rs:75-153`) computes **every** shape feature — rounded-rect SDF, border ring, gradient, drop shadow — from `uv` and `rect_params` (local-space), never from screen position. The vertex position only maps to NDC (`ui_renderer.rs:63`). AA uses `fwidth(d)` (`:128`), self-correcting at any screen footprint.
- `UIStyle` (`crates/manifold-ui/src/node.rs:410-439`) has no transform field. The disclosure/dropdown caret is renderer-drawn via `UIStyle.dropdown_caret` (`node.rs:426`), pinned after the main text.

## Design
### 1. Affine type + stack
- Add a small `Affine2` (2×3: rotation/scale/skew + translation) — a plain `[f32;6]` struct with `identity`, `mul`, `translate`, `scale`, `rotate_about(pivot)`. New module `crates/manifold-ui/src/transform2d.rs` (do NOT reuse `transform.rs` — that's 1D pan/zoom axis math, unrelated).
- `UIRenderer` gains `transform_stack: Vec<Affine2>` (peer to `depth_stack`/`clip_stack`), current = composed top (or identity when empty). `push_transform(Affine2)`/`pop_transform()` flush the immediate run first, exactly like `push_depth`/`push_immediate_clip` do (`ui_renderer.rs:437,469`).
- `Painter` trait (`crates/manifold-ui/src/draw.rs:46`) gains `push_transform`/`pop_transform`; the `impl Painter for UIRenderer` forwarder (`ui_renderer.rs:1465`) forwards them.

### 2. Application — vertex positions only, zero WGSL change
- Capture the current affine per command at draw time (like depth/clip are captured), then in the `prepare()` expansion (`ui_renderer.rs:~1128`) multiply each of the 4 corner **positions** by that affine before pushing the `UIVertex`. Leave `uv` and `rect_params` untouched.
- Lines (already oriented quads) and text/glyph quads get the same corner-position multiply.
- Result: rounded corners, borders, gradients, shadows, and glyphs rotate/scale correctly with **no shader edit**, because the SDF runs in local `uv` space.

### 3. Retained-mode nodes
- Add `transform: Option<Affine2>` to `UIStyle` (default `None`). During the tree-walk draw, when a node's style carries a transform, `push_transform` around that node's draws (and its subtree, if we want transforms to inherit — decide: **node-local only** for v1, no subtree inheritance, to keep it simple and match the caret use). Pivot defaults to the node's rect center.
- The renderer-drawn dropdown/disclosure caret honors the node's transform so the caret rotates.

### 4. Honest boundaries (documented, not hidden)
- **Clip under rotation:** hardware scissor (`encoder.set_scissor_rect`, `ui_renderer.rs:1408`) is axis-aligned. Under translate + axis-aligned scale the transformed clip rect stays an AABB → exact. Under rotation/skew, clip falls back to the **AABB of the transformed clip rect** (loose). v1 documents this; no rotated-clipped content in current consumers.
- **Non-uniform scale / skew:** the corner-radius/AA band is computed from local `uv`·`rect_params`, so non-uniform scale slightly warps corner AA (local uv-distance ≠ screen px). Rotation and *uniform* scale are exact. The caret and the P2 pops are rotation/uniform-scale only.

## Gate
- Unit: `Affine2` math (identity, compose, rotate_about pivot, associativity).
- **Visual (required):** headless `ui-snap` render proving (a) a rotated rounded rect keeps crisp AA'd rounded corners, (b) a rotated glyph renders rotated, (c) a scaled-about-center rect. Commit the PNGs. This is the capability's proof — a green unit test is not a look.
- `cargo test -p manifold-ui --lib` + `cargo test -p manifold-renderer --lib` (renderer touched) + `cargo clippy --workspace -- -D warnings`.

## Explicitly NOT in v1
- Subtree transform inheritance (node-local only).
- True rotated hardware clipping (AABB fallback is the boundary).
- Any consumer beyond the capability + its test. Caret-rotate wiring in `param_card` is a *separate* follow-up step (avoids colliding with the concurrent group-fold work in that file).
