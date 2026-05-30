//! Shared musical note-rate vocabulary. The cycles-per-beat table that
//! beat-locked primitives (`node.beat_gate`, `node.lfo`, …) map a note-rate
//! enum index onto. Previously lived inside the legacy `node.strobe`
//! monolith; lifted here when that bundle was decomposed so the table
//! survives as neutral shared data rather than effect-specific state.

/// Note-rate selector labels (UI surface) — indices into
/// [`NOTE_RATE_VALUES`]. The two slices must stay length-aligned.
pub const NOTE_RATE_LABELS: &[&str] = &[
    "1/1", "1/2", "1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32", "1/64",
];

/// Cycles-per-beat values indexed by the corresponding entry in
/// [`NOTE_RATE_LABELS`]. Pure data — kept `pub` for parity tests.
pub const NOTE_RATE_VALUES: [f32; 10] =
    [0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 16.0];
