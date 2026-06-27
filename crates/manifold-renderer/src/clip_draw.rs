//! GPU clip-body emission (§24 5b). Turns a clip's on-screen rect + state into
//! `UIRenderer` rounded-rect / gradient / shadow draws, reusing the shared SDF
//! rect pipeline. Replaces the CPU `bitmap_painter::draw_clip` fills — clips are
//! GPU tiles (rounded, gradient body, lift-on-select) instead of baked pixels.
//!
//! Layering: emit the shadows for the whole visible set FIRST (so a selected
//! clip's lift sits under every neighbour), then the bodies. Both are scissored
//! to the tracks rect by the caller, and run in their own `UIRenderer`
//! prepare/render cycle BETWEEN the background bitmap (grid) and the front
//! bitmap (waveform / overlays).

use crate::ui_renderer::UIRenderer;
use manifold_ui::bitmap_painter::get_clip_color;
use manifold_ui::color;
use manifold_ui::node::{Color32, Rect};
use manifold_ui::panels::viewport::ClipScreenRect;

/// Per-clip inputs the emitter needs. `rect` is screen-space; the caller
/// resolves selection/hover (it owns the selection state) and supplies the
/// effective `base_color` (per-clip override or layer colour).
#[derive(Clone, Copy)]
pub struct ClipBody {
    pub rect: Rect,
    pub base_color: Color32,
    pub selected: bool,
    pub hovered: bool,
    pub muted: bool,
    pub locked: bool,
    pub generator: bool,
}

impl ClipBody {
    /// Final body colour after state (select / hover / mute / lock). Routed
    /// through the same `get_clip_color` the bitmap path used, so colours do
    /// not shift on the cutover.
    fn resolved_color(&self) -> Color32 {
        get_clip_color(
            self.selected,
            self.hovered,
            self.muted,
            self.locked,
            self.generator,
            self.base_color,
        )
    }

    fn visible(&self) -> bool {
        self.rect.width > 0.0 && self.rect.height > 0.0
    }
}

/// Emit the lift shadow under every selected clip. Call before the bodies so a
/// selected clip's shadow never lands on top of a neighbouring body.
pub fn emit_clip_shadows(ui: &mut UIRenderer, clips: &[ClipBody]) {
    for c in clips {
        if !c.selected || !c.visible() {
            continue;
        }
        ui.draw_shadow(
            c.rect.x,
            c.rect.y + color::CLIP_SHADOW_OFFSET_Y,
            c.rect.width,
            c.rect.height,
            color::CLIP_RADIUS,
            color::CLIP_SHADOW_BLUR,
            color::CLIP_SHADOW,
        );
    }
}

/// Emit one clip body: a rounded gradient fill, then its border on top
/// (transparent-filled so only the outline draws).
pub fn emit_clip_body(ui: &mut UIRenderer, c: &ClipBody) {
    if !c.visible() {
        return;
    }
    let r = color::CLIP_RADIUS;
    let body = c.resolved_color();

    // Vertical body gradient: top edge lightened, fading to the base at the
    // bottom — a soft top-lit roundness. Locked clips are already dim and inert,
    // so they stay flat.
    let top = if c.locked {
        body
    } else {
        color::lighten(body, color::CLIP_GRADIENT_LIGHTEN)
    };
    ui.draw_gradient_rect(
        c.rect.x,
        c.rect.y,
        c.rect.width,
        c.rect.height,
        r,
        top,
        body,
        [0.0, 1.0],
    );

    // Border over the body — transparent fill, so only the outline shows.
    let (bw, bc) = if c.selected {
        (color::CLIP_BORDER_SELECTED_WIDTH, color::CLIP_BORDER_SELECTED)
    } else {
        (color::CLIP_BORDER_NORMAL_WIDTH, color::CLIP_BORDER_NORMAL)
    };
    if bw > 0.0 {
        ui.draw_bordered_rect(
            c.rect.x,
            c.rect.y,
            c.rect.width,
            c.rect.height,
            Color32::new(0, 0, 0, 0),
            r,
            bw,
            bc,
        );
    }
}

/// Emit a whole visible set: shadows first (all selected), then every body.
pub fn emit_clips(ui: &mut UIRenderer, clips: &[ClipBody]) {
    emit_clip_shadows(ui, clips);
    for c in clips {
        emit_clip_body(ui, c);
    }
}

/// Perceived luminance of a colour (0–255 scale), for picking label contrast.
fn luminance(c: Color32) -> f32 {
    0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32
}

/// Emit clip name labels in the overlay pass, on top of the bodies + waveforms.
/// Each label is scissor-clipped to its clip's interior (a hard cut at the right
/// edge for now; ellipsis is a Phase-6 polish) and uses dark-on-light /
/// light-on-dark text chosen by body luminance so it reads on any layer colour.
/// Narrow clips are skipped — a label there is illegible noise.
pub fn emit_clip_names(ui: &mut UIRenderer, clips: &[ClipScreenRect]) {
    let font = color::FONT_LABEL as f32;
    for c in clips {
        if c.name.is_empty() || c.rect.width < color::CLIP_LABEL_MIN_WIDTH {
            continue;
        }
        let text_color = if luminance(c.base_color) > 140.0 {
            color::CLIP_LABEL_ON_LIGHT
        } else {
            color::CLIP_LABEL_ON_DARK
        };
        // Clip the label to the body interior so a long name cuts at the edge
        // instead of bleeding over the next clip.
        let pad = color::CLIP_LABEL_PAD_X;
        let inner_w = (c.rect.width - pad * 2.0).max(0.0);
        if inner_w <= 0.0 {
            continue;
        }
        ui.push_immediate_clip(c.rect.x + pad, c.rect.y, inner_w, c.rect.height);
        // Anchor the label to the BOTTOM of the clip (title-bottom — keeps the
        // name out of the thumbnail/content above it; Ableton/Premiere/FCP style).
        // Clamp so a very short clip never pushes the text above its own top.
        let bottom_pad = 3.0;
        let ty = (c.rect.y + c.rect.height - font - bottom_pad).max(c.rect.y);
        ui.draw_text(c.rect.x + pad, ty, &c.name, font, text_color);
        ui.pop_immediate_clip();
    }
}
