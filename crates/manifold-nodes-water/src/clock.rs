//! The water family's fixed simulation tick and display-time helpers.

pub const TICK: f64 = 1.0 / 60.0;

/// GPU_FLUID_SURFACE_DESIGN.md D10: the blend presenting display time `s`
/// between frames at `t_a` and `t_b`, and their span. Display time never
/// passes the newest frame; one frame (`t_a == t_b`) presents it fully.
pub(crate) fn display_blend(s: f64, t_a: f64, t_b: f64) -> (f32, f32) {
    let span = t_b - t_a;
    if span <= 0.0 {
        return (1.0, 0.0);
    }
    (((s - t_a) / span).clamp(0.0, 1.0) as f32, span as f32)
}

/// A whitewater particle's draw scale from its remaining lifetime: the last
/// 0.2 seconds shrink instead of leaving a full-sized particle until removal.
pub(crate) fn whitewater_fade(lifetime: f32) -> f32 {
    (lifetime / 0.2).clamp(0.0, 1.0).sqrt()
}
