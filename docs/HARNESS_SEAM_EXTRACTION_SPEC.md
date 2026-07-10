# `render_main_ui_passes` — extraction spec (HARNESS_FIDELITY_INVARIANT §4 step 2)

**Status: BUILD SPEC · 2026-07-10 · Opus (1M).** The executable design for the
seam that folds the main-window immediate-pass assembly into shared code, so the
live app (`app_render.rs::present_all_windows`) and the headless harness
(`ui_snapshot/render.rs::render_ui_to_png` + `script.rs`'s `Runner`) run the
**identical** pass sequence and per-pass render-call choices. Deletes
`draw_immediate_passes` and the harness overlay pass; closes BUG-097 by
construction. Read `HARNESS_FIDELITY_INVARIANT_PROPOSAL.md` §3–§4 first — this is
its build sheet.

## The one owner rule

Today the pass **assembly** — *which* passes run, in *what* order, with *which*
per-pass render call — has two owners: `present_all_windows` (live) and
`draw_immediate_passes` (harness). That duplication is the drift (BUG-097: the
harness chose `render_tree_range` where the live app chose `render_sub_region`).

After this change the assembly has **one owner**: `render_main_ui_passes` in
`crates/manifold-app/src/ui_frame.rs`. Both callers call it. Nothing re-sequences.

**What stays caller-side (input *resolution*, per §3's caller test):** building the
`Vec<ClipBody>` from live drag state, resolving thumbnail quads from the
content-thread atlas layout, resolving automation lanes from latched params. These
are *inputs*, resolved rich live / simple headless, and handed to the seam as plain
data. The seam never asks "am I the harness?"; it asks "is this input present?"

## Shape — two functions, called in sequence

`composite_main_ui_frame` (P1, verified, **unchanged**) stays as-is: dirty-panel
atlas composite + clear + atlas blit + video-band blit, its own committed encoder.

**New:** `render_main_ui_passes` owns everything after it — Passes 4a→5 + the VQT
waterfall + the overlay-region dirty-clear — on its own `"Frame"` encoder (mirroring
the encoder `present_all_windows` creates at the old :3922 and commits at :4677).

Both callers do: `composite_main_ui_frame(...)` then `render_main_ui_passes(...)`.

## Signature

`ui_root: &mut UIRoot` is the workhorse: it carries `viewport`, `overlay_draw`,
`tree`, `browser_popup`, `audio_setup_panel`, `dropdown`, `inspector`,
`overlay_region_start` — so ~60% of the old `self.ws.ui_root.*` accesses come free.
`&mut` for the trailing `tree.clear_dirty_range` (the overlay-dirty-clear).

```rust
/// Owns the full main-window immediate-pass assembly (Passes 4a→5 + VQT +
/// overlay dirty-clear) on its own encoder, AFTER composite_main_ui_frame. The
/// single owner of pass order + per-pass render-call choice, called by the live
/// app and the harness. Inputs resolved by the caller (rich live / simple
/// headless); the seam branches only on input presence, never caller identity
/// (HARNESS_FIDELITY_INVARIANT §3).
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_main_ui_passes(
    device: &GpuDevice,
    ui_renderer: &mut UIRenderer,
    ui_root: &mut UIRoot,
    offscreen: &GpuTexture,
    logical_w: u32,
    logical_h: u32,
    scale: f64,
    inputs: MainUiPassInputs<'_>,
);

/// Caller-resolved per-pass data. Every `Option`/empty field is a legitimate
/// "input absent" (§3): the live app fills what it has this frame; the harness
/// fills the subset it can resolve headless and leaves the rest None/empty. A
/// pass whose input is absent skips itself — the live app skips the same pass on
/// a frame whose input is absent (no open modal → no overlay pass, etc.).
pub(crate) struct MainUiPassInputs<'a> {
    // Pass 4a grid + 4c lane/overview/collapsed bitmaps.
    pub layer_bitmap_gpu: Option<&'a mut LayerBitmapGpu>,
    // Pass 4b clip bodies — resolved WITH live drag lift/settle/ghost/split-flick
    // (harness: plain rects, no drag). `clip_rects` carries the SAME drag-adjusted
    // rects reused by waveforms/thumbnails/names so the whole clip moves together.
    pub clip_bodies: &'a [ClipBody],
    pub clip_rects: &'a [ClipScreenRect],
    // Pass 4b' waveforms.
    pub clip_content_gpu: Option<&'a mut ClipContentGpu>,
    // Pass 4b" thumbnails — atlas + quads resolved caller-side (content-thread
    // atlas live; thumbs::make_test_atlas headless via --thumbs; both None if
    // absent). Renderer is the one primitive the harness DOES build for thumbs.
    pub thumb: Option<ThumbPass<'a>>,
    // Pass 5 timeline overlays + names + lanes + playhead + scrollbar.
    pub timeline_overlays: TimelineOverlays,
    pub markers: &'a [(f32, Color32)],
    pub landing_flash: Option<LandingFlash>,
    pub automation_lanes: &'a [AutomationLaneScreen],
    pub cursor_pos: manifold_ui::node::Vec2, // scrollbar hover
    // Pass 5 text-input overlay (card-drag ghost + overlay_draw come off ui_root).
    pub text_input: &'a crate::text_input::TextInputState,
    pub frame_timer: &'a crate::frame_timer::FrameTimer,
    // VQT waterfall — live-only spectrogram state bundled; None headless.
    pub vqt: Option<&'a mut VqtPassState<'a>>,
    // Shared blit resources (VQT blit).
    pub blit_pipeline: &'a GpuRenderPipeline,
    pub blit_sampler: &'a GpuSampler,
}
```

`ThumbPass<'a>` = `{ gpu: &'a mut ClipThumbGpu, atlas: &'a GpuTexture, quads: &'a [ThumbQuad] }`.
`VqtPassState<'a>` bundles the six Application fields the VQT pass mutates
(`spectrogram`, `spectrogram_pane`, `spectrogram_num_bins`, `spectrogram_tex_dims`,
`pending_spectrogram_columns`, `pending_spectrogram_scalars`) plus the
`content_state` spectrogram scalars + `scope_cursor_y` (resolved caller-side from
`self.scope_hover_uv()` — a live-only method). `LandingFlash` /
`AutomationLaneScreen` reuse the existing tuple/struct types.

## Pass-by-pass move map (old `app_render.rs` line → new seam)

Each pass moves VERBATIM (behavior-preserving), rewriting `self.ws.ui_root.X` →
`ui_root.X`, `self.clip_body_scratch` → `inputs.clip_bodies`, etc. The **§3
classification** column says why each caller-provided input is input-presence, not
caller-identity.

| Pass | Old lines | Renderer / input | §3 note |
|---|---|---|---|
| 4a grid bitmaps | ~3932–3945 | `inputs.layer_bitmap_gpu` + `viewport.layer_bitmap_rects()` | absent headless (no bitmap gpu) |
| 4b clip bodies | ~4057–4067 (emit only) | `ui_renderer` + `inputs.clip_bodies` | bodies resolved caller-side (drag) |
| 4b' waveforms | ~4079–4093 | `inputs.clip_content_gpu` + `inputs.clip_rects` | absent headless |
| 4b" thumbnails | ~4120–4253 (emit only) | `inputs.thumb` | atlas/quads resolved caller-side |
| 4c lane bitmaps | ~4263–4284 | `inputs.layer_bitmap_gpu` + `viewport.overview_rect/collapsed_group_rects` | absent headless |
| 5 region/cursor/markers + landing flash | ~4318–4348 | `ui_renderer` + `inputs.timeline_overlays/markers/landing_flash` | resolved caller-side |
| 5 clip names | ~4353–4357 | `emit_clip_names(ui_renderer, inputs.clip_rects, tracks)` | shared today |
| 5 automation lanes | ~4363–4372 | `emit_automation_lanes(ui_renderer, inputs.automation_lanes, tracks)` | shared today |
| 5 playhead | ~4378–4400 | `ui_renderer` + `viewport.playhead_pixel/ruler_rect` | on ui_root |
| 5 scrollbar | ~4407–4431 | `ui_renderer` + `viewport.scrollbar_h_layout` + `inputs.cursor_pos` | on ui_root |
| 5 browser popup thumbs | ~4440–4453 | `device` + `ui_renderer` + `ui_root.browser_popup` | on ui_root; None-safe |
| 5 top-level overlays | ~4463–4498 | `ui_renderer.render_sub_region` @ `Depth::OVERLAY` + shadow | **BUG-097** — this call choice is the drift |
| 5 card-drag ghost + text input | ~4502–4510 | `ui_root.inspector.card_drag_first_node()` + `inputs.text_input/frame_timer` | on ui_root / caller |
| 5 prepare+render flush | ~4513–4515 | `ui_renderer` | — |
| VQT waterfall | ~4528–4662 | `inputs.vqt` + `ui_root.audio_setup_panel/dropdown` + `inputs.blit_*` | absent headless (None) |
| commit | ~4664–4677 | — | seam commits its own encoder |
| overlay dirty-clear | ~4681–4695 | `ui_root.overlay_region_start` + `tree.clear_dirty_range` | on ui_root |

Everything from the old encoder-create (:3922) through the overlay dirty-clear
(:4695) moves in. What STAYS in `present_all_windows`: the panel-cache ensure,
fast path, `composite_main_ui_frame` call, the caller-side input resolution
(clip-body/thumb-quad/overlay/lane/vqt resolution), and the drawable
acquire→blit→present tail (:4697+). The seam ends at the offscreen (Decided #8).

## Caller wiring

**Live (`present_all_windows`):** keep all input resolution (the drag-adjusted
`clip_rect_scratch`/`clip_body_scratch` loops, thumbnail quad build, timeline
overlay resolve, automation lane resolve, scope-cursor resolve). Replace the old
inline pass block with one `render_main_ui_passes(...)` call, filling
`MainUiPassInputs` from `self.*`. The `SetClipAtlasVisible` content-command send
(~4100–4114) is a live-only side effect that is NOT a render pass — it stays in
`present_all_windows`, before the seam call.

**Harness (`render_ui_to_png`):** after the existing `composite_frame`, resolve the
simple inputs (clip rects from `ui.viewport.visible_clip_rects`, bodies from
selection, automation lanes, `--thumbs` test atlas+quads) and call
`render_main_ui_passes` with `layer_bitmap_gpu: None`, `clip_content_gpu: None`,
`vqt: None`, empty `markers`/`landing_flash`, default `TimelineOverlays`, an empty
`TextInputState`. `draw_immediate_passes` is **deleted**.

**Harness (`script.rs` Runner):** repoint its immediate-pass draw (the
`draw_immediate_passes` call over its persistent offscreen) at
`render_main_ui_passes` the same way.

## Acceptance gates

1. **Byte-identical no-overlay frame** — a harness PNG for a no-overlay scene is
   pixel-identical before vs after (the extraction changes no pixel that already
   rendered). Verify: render on the `784b369b` parent, render after, `sha256` match.
2. **RED→GREEN overlay proof** — a harness PNG with an open overlay (dropdown or
   perf HUD) renders the overlay **blank** on `main` (via the old
   `render_tree_range` path) and **drawn** after (via the seam's `render_sub_region`
   @ `Depth::OVERLAY`). Keep the green side as a permanent regression test.
3. **No caller-identity parameter** — grep the seam: no `is_harness`/`headless`/
   `skip_*` param only a harness caller sets. Every branch is `Option::is_some` /
   slice-empty on a real input.
4. **Deletion proof** — `rg "fn draw_immediate_passes|render_tree_range" ui_snapshot/render.rs`
   returns zero hits (the parallel assembly and the BUG-097 call are gone).
5. Clippy clean; workspace sweep; GPU-proof run of `manifold-app --features ui-snapshot`.
