//! 1D affine coordinate axis — the shared seed behind every pan/zoom mapping.
//!
//! Two surfaces map a logical coordinate to a screen pixel and back:
//!
//! - the **timeline** maps beats↔pixels (`screen = beat·pixels_per_beat − scroll`),
//! - the **graph canvas** maps graph-space↔screen per dimension
//!   (`screen = (graph + pan)·zoom + origin`).
//!
//! Those are the same idea twice — a 1D affine map `screen = logical·scale +
//! offset`. `Axis` is that map, once. [`CoordinateMapper`](crate::coordinate_mapper::CoordinateMapper)
//! builds one for its X conversions; the canvas builds one per dimension (X and
//! Y share the zoom scale and differ only in offset). Both *express* this type
//! instead of re-deriving the arithmetic, so a sign error or a forgotten scroll
//! term can't drift between them.
//!
//! The two surfaces parameterise the map differently — the timeline carries the
//! offset in screen space (a subtracted scroll), the canvas carries it in
//! logical space (a pan added before scaling). [`Axis::new`] and
//! [`Axis::from_pan`] are the two constructors for those two forms; both land on
//! the same canonical `scale`/`offset` pair.

/// A 1D affine map between a logical coordinate and a screen pixel.
///
/// Canonical form: `screen = logical * scale + offset`, where `offset` is the
/// screen position of logical zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axis {
    /// Screen units per logical unit — pixels-per-beat on the timeline, zoom on
    /// the canvas.
    scale: f32,
    /// Screen-space position of logical zero.
    offset: f32,
}

impl Axis {
    /// Build directly from the canonical `scale`/`offset`. This is the
    /// timeline's form: `screen = beat·pixels_per_beat − scroll`, so
    /// `Axis::new(pixels_per_beat, -scroll)`.
    #[inline]
    pub fn new(scale: f32, offset: f32) -> Self {
        Self { scale, offset }
    }

    /// Build from a logical-space pan plus a screen-space origin — the canvas's
    /// form: `screen = (logical + pan)·scale + screen_origin`. Folds to the same
    /// canonical pair (`offset = scale·pan + screen_origin`).
    #[inline]
    pub fn from_pan(scale: f32, pan: f32, screen_origin: f32) -> Self {
        Self {
            scale,
            offset: scale * pan + screen_origin,
        }
    }

    /// Screen units per logical unit.
    #[inline]
    pub fn scale(self) -> f32 {
        self.scale
    }

    /// Screen position of logical zero.
    #[inline]
    pub fn offset(self) -> f32 {
        self.offset
    }

    /// logical → screen.
    #[inline]
    pub fn to_screen(self, logical: f32) -> f32 {
        logical * self.scale + self.offset
    }

    /// screen → logical.
    #[inline]
    pub fn to_logical(self, screen: f32) -> f32 {
        (screen - self.offset) / self.scale
    }

    /// A logical span (duration / width) → screen span. Offset-free: a length
    /// scales but does not translate.
    #[inline]
    pub fn span_to_screen(self, logical_span: f32) -> f32 {
        logical_span * self.scale
    }

    /// A screen span → logical span. Offset-free.
    #[inline]
    pub fn span_to_logical(self, screen_span: f32) -> f32 {
        screen_span / self.scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let a = Axis::new(120.0, -50.0);
        let s = a.to_screen(7.5);
        assert!((a.to_logical(s) - 7.5).abs() < 1e-4);
    }

    #[test]
    fn span_ignores_offset() {
        let a = Axis::new(120.0, 999.0);
        // a length of 3.5 logical units is 3.5*scale screen units regardless of offset
        assert!((a.span_to_screen(3.5) - 3.5 * 120.0).abs() < 1e-4);
        assert!((a.span_to_logical(420.0) - 3.5).abs() < 1e-4);
    }

    #[test]
    fn reproduces_timeline_form() {
        // screen = beat·pixels_per_beat − scroll
        let ppb = 120.0;
        let scroll = 100.0;
        let a = Axis::new(ppb, -scroll);
        let beat = 4.0;
        assert!((a.to_screen(beat) - (beat * ppb - scroll)).abs() < 1e-4);
    }

    #[test]
    fn reproduces_canvas_form() {
        // screen = (graph + pan)·zoom + origin
        let zoom = 0.75;
        let pan = 30.0;
        let origin = 12.0;
        let a = Axis::from_pan(zoom, pan, origin);
        let g = 200.0;
        assert!((a.to_screen(g) - ((g + pan) * zoom + origin)).abs() < 1e-4);
        // and the inverse matches the canvas's hand-written to_graph
        let s = a.to_screen(g);
        assert!((a.to_logical(s) - ((s - origin) / zoom - pan)).abs() < 1e-4);
    }
}
