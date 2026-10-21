//! The frame-aligned driver pipeline reads the project clock and keeps
//! legacy timing and invert.

use super::*;
use super::composition::driver_target_value;
use manifold_core::effects::ParameterDriver;
use manifold_core::{Bpm, types::{BeatDivision, DriverWaveform}};

#[test]
fn frame_align_pipeline_uses_project_clock_and_preserves_legacy_and_invert() {
    let mut d = ParameterDriver::new("amount", BeatDivision::Sixteenth, DriverWaveform::Square);
    let beat = Beats(0.24);
    let time = Seconds(2.0 / 24.0);
    assert_eq!(driver_target_value(&d, beat, time, Bpm(164.0), 24.0, 0.0, 1.0), 0.0);
    d.frame_aligned = true;
    assert_eq!(driver_target_value(&d, beat, time, Bpm(164.0), 24.0, 0.0, 1.0), 1.0);
    d.reversed = true;
    assert_eq!(driver_target_value(&d, beat, time, Bpm(164.0), 24.0, 0.0, 1.0), 0.0);
    d.reversed = false;
    d.trim_min = 0.2;
    d.trim_max = 0.8;
    assert_eq!(driver_target_value(&d, beat, time, Bpm(164.0), 24.0, 0.0, 10.0), 8.0);
}
