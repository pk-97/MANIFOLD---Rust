//! THE single definition of the scope overlay-scalar layout — what rides
//! alongside each spectrogram column from the analyzer to the shader.
//!
//! Every producer and consumer (`manifold-audio`'s analyzer, the content
//! thread, `app_render`, the GPU renderer in [`crate::spectrogram`], and the
//! `mod_harness` CPU port) speaks [`ScopeColumn`] — never a hand-packed float
//! strip. The GPU buffer is the raw bytes of a `[ScopeColumn]` slice (all-`f32`
//! `repr(C)`, no padding — const-asserted below), and the shader receives the
//! stride, onset lane base/count, and lane colours through its uniform params,
//! so adding an overlay scalar is a change to THIS FILE plus the one analyzer
//! push site, and nothing else.

/// Onset tick lanes drawn as stacked ribbons at the scope's bottom edge.
/// **Field order is lane order, bottom-up**: the first field is the lowest
/// lane. [`Self::LANE_COLORS`] and [`Self::lanes`] follow the same order —
/// both are length-checked against the field count at compile time.
///
/// This struct is the scope's own tick-lane
/// display only — the underlying ridge-only kick detector
/// (`crates/manifold-audio/src/analysis.rs`'s `kick_ridges`/`KickRidges`,
/// `AudioFeatureKind::Kick`, the drawer's Kick feature button) is completely
/// untouched; only the scope's visual lane for it is gone. Not conditional —
/// deleted outright, per Peter's "never sometimes there and sometimes not."
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScopeOnsets {
    /// Low-band transient fire (0/1 impulse; only the fired hop, not the tail).
    /// Bottom lane.
    pub low: f32,
    /// Mid-band transient fire.
    pub mid: f32,
    /// High-band transient fire.
    pub high: f32,
}

impl ScopeOnsets {
    /// Lane count, derived from the struct size so it can never drift from the
    /// fields (all-`f32` `repr(C)` — const-asserted below).
    pub const COUNT: usize = size_of::<Self>() / size_of::<f32>();

    /// Lane colours (linear rgb), same bottom-up order as the fields. Low/Mid/
    /// High match their centroid-trace colours.
    pub const LANE_COLORS: [[f32; 3]; Self::COUNT] = [
        [1.0, 0.35, 0.30], // low — red
        [0.35, 1.0, 0.45], // mid — green
        [0.40, 0.62, 1.0], // high — blue
    ];

    /// Human-readable lane names, same bottom-up order as the fields — the
    /// Audio Setup scope's gutter legend and any other UI naming a lane.
    /// Length-checked against the field count at compile time.
    pub const LANE_LABELS: [&'static str; Self::COUNT] = ["Low", "Mid", "High"];

    /// The lanes as an array, bottom-up (for consumers that iterate lanes, e.g.
    /// the `mod_harness` CPU renderer). The return length is [`Self::COUNT`],
    /// so forgetting to list a new field here is a compile error.
    pub fn lanes(&self) -> [f32; Self::COUNT] {
        [self.low, self.mid, self.high]
    }
}

/// Each onset tick lane's height as a fraction of the scope's height —
/// owned here with the rest of the lane definition; the shader receives it
/// through its uniforms, the UI's gutter legend and the `mod_harness` CPU
/// port position labels/lanes with the same value.
pub const LANE_HEIGHT_FRAC: f32 = 0.014;

/// Per-band spectral-centroid count: `[full, low, mid, high]`, each a
/// normalised height-from-bottom (0..1); `< 0` hides that band's trace.
pub const SCOPE_CENTROID_COUNT: usize = 4;

/// Fixed capacity of the shader's onset lane-colour uniform array (WGSL array
/// lengths are compile-time literals, so the uniform is sized to this max and
/// the live count rides in a uniform field). Bump only alongside the WGSL.
pub const MAX_ONSET_LANES: usize = 8;

/// One column's overlay scalars, stored in a ring parallel to the magnitude
/// columns. The GPU sees this struct's raw bytes; the shader indexes it with
/// [`Self::STRIDE`]/[`Self::ONSET_BASE`] passed through its uniforms.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScopeColumn {
    /// Per-band spectral centroids `[full, low, mid, high]` as normalised
    /// height-from-bottom (0..1); `< 0` hides that band's trace this column.
    pub centroids: [f32; SCOPE_CENTROID_COUNT],
    pub onsets: ScopeOnsets,
}

impl ScopeColumn {
    /// Floats per column — the GPU-side stride.
    pub const STRIDE: usize = size_of::<Self>() / size_of::<f32>();

    /// Float index of the first onset lane within a column.
    pub const ONSET_BASE: usize = std::mem::offset_of!(ScopeColumn, onsets) / size_of::<f32>();

    /// The "nothing recorded" column: hidden centroid traces, no onset ticks.
    pub const EMPTY: Self = Self {
        centroids: [-1.0; SCOPE_CENTROID_COUNT],
        onsets: ScopeOnsets { low: 0.0, mid: 0.0, high: 0.0 },
    };
}

// The GPU contract: all-f32, no padding, onsets contiguous after centroids,
// and the lane count within the shader uniform's fixed capacity. A non-f32
// field or reordering breaks these at compile time instead of desyncing the
// shader silently.
const _: () = {
    assert!(size_of::<ScopeColumn>() == (SCOPE_CENTROID_COUNT + ScopeOnsets::COUNT) * size_of::<f32>());
    assert!(std::mem::offset_of!(ScopeColumn, onsets) == SCOPE_CENTROID_COUNT * size_of::<f32>());
    assert!(align_of::<ScopeColumn>() == align_of::<f32>());
    assert!(ScopeOnsets::COUNT <= MAX_ONSET_LANES);
};

#[cfg(test)]
mod tests {
    use super::*;

    /// `lanes()` must present the fields in memory (= lane) order — the same
    /// order the shader reads them at `ONSET_BASE + i`.
    #[test]
    fn lanes_match_memory_order() {
        let col = ScopeColumn {
            centroids: [0.1, 0.2, 0.3, 0.4],
            onsets: ScopeOnsets { low: 1.0, mid: 2.0, high: 3.0 },
        };
        // SAFETY: repr(C), all-f32, no padding (const-asserted above).
        let raw: [f32; ScopeColumn::STRIDE] = unsafe { std::mem::transmute(col) };
        assert_eq!(&raw[..SCOPE_CENTROID_COUNT], &col.centroids);
        assert_eq!(&raw[ScopeColumn::ONSET_BASE..], &col.onsets.lanes());
    }
}
