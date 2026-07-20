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
    /// P2 motion (`UI_CRAFT_AND_MOTION_PLAN.md` D17 "duplicate-drag ghost"):
    /// 0..1 body alpha multiplier. `1.0` (opaque) is the resting value every
    /// clip uses outside an alt-duplicate drag — `emit_clip_body` only
    /// departs from the usual hard-`opaque()` force when this is < 1.0, so a
    /// normal clip's body/well/strip is unaffected and the lane grid still
    /// never bleeds through it.
    pub alpha: f32,
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
/// selected clip's shadow never lands on top of a neighbouring body. (An *ambient*
/// shadow under every clip — the mockup's `.clip` box-shadow — was tried and
/// dropped: on MANIFOLD's near-black lanes a black drop-shadow doesn't read, so it
/// was pure per-frame cost. The clips' depth comes from the inset card + identity
/// border, not a shadow.)
pub fn emit_clip_shadows(ui: &mut UIRenderer, clips: &[ClipBody]) {
    if !color::SHADOWS_ENABLED {
        return;
    }
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

/// Name-strip band height for a clip of height `h`: `Some(strip_h)` when the clip
/// has room for a preview + a name strip (§E / §K15), `None` only for a clip too
/// short to carry a legible strip at all. Collapsed/short clips get a *proportional*
/// strip (capped at `CLIP_STRIP_HEIGHT`) so the name always reads on a solid band
/// instead of floating over the thumbnail — the thumbnail reserves this band.
pub fn clip_strip_height(h: f32) -> Option<f32> {
    (h >= color::CLIP_STRIP_MIN_CLIP_HEIGHT).then(|| color::CLIP_STRIP_HEIGHT.min(h * 0.45))
}

/// The preview-well colour: the identity colour scaled toward black (hue-
/// preserving), standing in for the thumbnail until §F populates it. Carries
/// the input's own alpha through unchanged — a preview well is a solid
/// backstop against the lane grid EXCEPT during a P2
/// duplicate-drag ghost (`ClipBody::alpha` < 1.0, see [`body_alpha`]), where
/// the whole clip — well included — is meant to read as translucent.
fn well_color(c: Color32) -> Color32 {
    let s = color::CLIP_PREVIEW_WELL_SCALE;
    Color32::new(
        (c.r as f32 * s) as u8,
        (c.g as f32 * s) as u8,
        (c.b as f32 * s) as u8,
        c.a,
    )
}

/// A clip colour's body alpha: fully opaque (255) unless `alpha` is
/// mid-fade (P2 duplicate-drag ghost, `ClipBody::alpha`), in which case it
/// scales down instead of being force-opaqued. A clip body is solid by
/// default — if the identity colour ever carried alpha < 255 outside this
/// one deliberate case, the lane grid would bleed through.
#[inline]
fn body_alpha(c: Color32, alpha: f32) -> Color32 {
    if alpha >= 0.999 {
        Color32::new(c.r, c.g, c.b, 255)
    } else {
        Color32::new(c.r, c.g, c.b, (255.0 * alpha.clamp(0.0, 1.0)) as u8)
    }
}

/// Emit one clip body. Tall clips render the §E anatomy — a darker preview WELL
/// on top (the thumbnail's home once §F lands) + a solid identity NAME STRIP on
/// the bottom — then the border on top. Short/collapsed clips stay a single
/// rounded identity bar. The border is transparent-filled so only the outline draws.
pub fn emit_clip_body(ui: &mut UIRenderer, c: &ClipBody) {
    if !c.visible() {
        return;
    }
    let r = color::CLIP_RADIUS;
    let body = body_alpha(c.resolved_color(), c.alpha);

    if let Some(strip_h) = clip_strip_height(c.rect.height) {
        // Preview well (full-height, rounded) — the identity colour darkened so
        // the strip below reads as a distinct band. A subtle top-lit gradient.
        let well = if c.locked { body } else { well_color(body) };
        let well_top = if c.locked {
            well
        } else {
            color::lighten(well, color::CLIP_GRADIENT_LIGHTEN)
        };
        ui.draw_gradient_rect(
            c.rect.x,
            c.rect.y,
            c.rect.width,
            c.rect.height,
            r,
            well_top,
            well,
            [0.0, 1.0],
        );
        // Name strip (bottom band, rounded) — the full identity colour. Its top
        // corners round into the well (a few px), which reads cleanly.
        ui.draw_gradient_rect(
            c.rect.x,
            c.rect.y + c.rect.height - strip_h,
            c.rect.width,
            strip_h,
            r,
            body,
            body,
            [0.0, 1.0],
        );
    } else {
        // Short/collapsed clip: one solid identity bar (top-lit gradient).
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
    }

    // Border over the whole clip — transparent fill, so only the outline shows.
    // §E: a normal clip's frame is the LAYER's IDENTITY colour (Ableton clip-colour),
    // tying the full-bleed thumbnail + name strip to the layer; the selected clip
    // keeps the bright focus ring. Locked clips stay on the dim neutral edge.
    let (bw, bc) = if c.selected {
        (color::CLIP_BORDER_SELECTED_WIDTH, color::CLIP_BORDER_SELECTED)
    } else if c.locked {
        (color::CLIP_BORDER_NORMAL_WIDTH, color::CLIP_BORDER_NORMAL)
    } else {
        (color::CLIP_BORDER_NORMAL_WIDTH, c.base_color)
    };
    // The border fades with the same ghost alpha as the body — otherwise a
    // fully-faded duplicate-drag ghost would still show a solid outline.
    let bc = body_alpha(bc, c.alpha);
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
///
/// `tracks` is the timeline tracks viewport. It is pushed as the outer clip so a
/// clip scrolled left under the header column can never draw its name over the
/// layer controls — the per-clip clip below intersects it, so the bound holds at
/// any scroll position or zoom. Taking the rect here (rather than relying on the
/// caller to wrap the call in `push_immediate_clip`) keeps the function
/// self-contained: every emitter sibling that *did* depend on the caller wrapping
/// it had already drifted out of sync, which is exactly the bleed this fixes.
pub fn emit_clip_names(ui: &mut UIRenderer, clips: &[ClipScreenRect], tracks: Rect) {
    let font = color::FONT_LABEL as f32;
    ui.push_immediate_clip(tracks.x, tracks.y, tracks.width, tracks.height);
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
        // The name lives on the bottom NAME STRIP (§K15): centred in the strip
        // band on a tall clip, or bottom-anchored on a short solid bar. Either
        // way the strip carries the identity colour, so the luminance-picked text
        // colour above already reads against it. Clamp so a tiny clip can't push
        // the text above its own top.
        let ty = match clip_strip_height(c.rect.height) {
            Some(strip_h) => (c.rect.y + c.rect.height - strip_h + (strip_h - font) * 0.5)
                .max(c.rect.y),
            None => (c.rect.y + c.rect.height - font - 3.0).max(c.rect.y),
        };
        ui.draw_text(c.rect.x + pad, ty, &c.name, font, text_color);
        ui.pop_immediate_clip();
    }
    ui.pop_immediate_clip();
}
