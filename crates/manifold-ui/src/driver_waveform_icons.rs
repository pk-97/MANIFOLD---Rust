//! Generates and caches anti-aliased procedural pixel buffers for each
//! DriverWaveform type. Uses signed-distance-field rendering for smooth
//! thick lines at any display size.
//!
//! Mechanical translation of `Assets/Scripts/UI/Timeline/DriverWaveformIcons.cs`.
//!
//! Used on the driver toggle button to show the active waveform shape.

use crate::node::Color32;
use manifold_core::DriverWaveform;
use std::sync::OnceLock;

// ── Constants (DriverWaveformIcons.cs lines 13-17) ──

const SIZE: usize = 128;
const PADDING: usize = 14;
const LINE_THICKNESS: f32 = 8.0;
const AA_WIDTH: f32 = 1.4;
const SINE_SAMPLES: usize = 64;

/// Cached icon pixel buffers (one per waveform variant).
/// Each entry is SIZE×SIZE pixels of Color32.
static CACHE: OnceLock<[Vec<Color32>; 5]> = OnceLock::new();

/// Get the procedural icon pixels for a waveform type.
///
/// Returns a SIZE×SIZE pixel buffer (row-major, bottom-to-top like Unity textures).
/// Cached after first generation.
///
/// Unity: `DriverWaveformIcons.Get(DriverWaveform wave)` (lines 25-87).
pub fn get(wave: DriverWaveform) -> &'static [Color32] {
    let cache = CACHE.get_or_init(|| {
        [
            generate(DriverWaveform::Sine),
            generate(DriverWaveform::Triangle),
            generate(DriverWaveform::Sawtooth),
            generate(DriverWaveform::Square),
            generate(DriverWaveform::Random),
        ]
    });
    let idx = wave as usize;
    if idx < cache.len() {
        &cache[idx]
    } else {
        &cache[0]
    }
}

/// Icon dimensions.
pub fn icon_size() -> usize {
    SIZE
}

fn generate(wave: DriverWaveform) -> Vec<Color32> {
    let points = fill_waveform_points(wave);

    let draw_size = (SIZE - PADDING * 2) as f32;
    let half_thick = LINE_THICKNESS * 0.5;
    let aa_outer = half_thick + AA_WIDTH * 0.5;
    let aa_inner = half_thick - AA_WIDTH * 0.5;

    let mut pixels = vec![Color32::TRANSPARENT; SIZE * SIZE];

    // Unity: nested loop py,px (lines 45-81)
    for py in 0..SIZE {
        let ny = (py as f32 - PADDING as f32) / draw_size;
        for px in 0..SIZE {
            let nx = (px as f32 - PADDING as f32) / draw_size;

            // Min distance to polyline in pixel space
            let mut min_dist = f32::MAX;
            for i in 0..points.len() - 1 {
                let d = dist_to_segment(nx, ny, points[i], points[i + 1]);
                if d < min_dist {
                    min_dist = d;
                }
            }
            let pixel_dist = min_dist * draw_size;

            // Smoothstep anti-aliasing (Unity lines 62-79)
            if pixel_dist > aa_outer {
                continue; // fully outside
            }

            let alpha = if pixel_dist <= aa_inner {
                1.0
            } else {
                let t = (pixel_dist - aa_inner) / (aa_outer - aa_inner);
                let t = t * t * (3.0 - 2.0 * t); // smoothstep
                1.0 - t
            };

            let a = (alpha * 255.0 + 0.5) as u8;
            pixels[py * SIZE + px] = Color32::new(255, 255, 255, a);
        }
    }

    pixels
}

/// Fill waveform points in normalized [0,1] space.
///
/// Unity: `FillWaveformPoints(DriverWaveform wave)` (lines 93-148).
fn fill_waveform_points(wave: DriverWaveform) -> Vec<[f32; 2]> {
    match wave {
        DriverWaveform::Sine => {
            // 1 full cycle, smooth (Unity lines 98-105)
            let mut points = Vec::with_capacity(SINE_SAMPLES);
            for i in 0..SINE_SAMPLES {
                let t = i as f32 / (SINE_SAMPLES - 1) as f32;
                let val = ((t * std::f32::consts::PI * 2.0).sin() + 1.0) * 0.5;
                points.push([t, val]);
            }
            points
        }
        DriverWaveform::Triangle => {
            // Unity lines 108-112
            vec![[0.0, 0.0], [0.5, 1.0], [1.0, 0.0]]
        }
        DriverWaveform::Sawtooth => {
            // Unity lines 115-119
            vec![[0.0, 0.0], [1.0, 1.0], [1.0, 0.0]]
        }
        DriverWaveform::Square => {
            // Unity lines 122-126
            vec![[0.0, 1.0], [0.5, 1.0], [0.5, 0.0], [1.0, 0.0]]
        }
        DriverWaveform::Random => {
            // 5-step S&H pattern (Unity lines 130-141)
            vec![
                [0.0, 0.3],
                [0.2, 0.3],
                [0.2, 0.85],
                [0.4, 0.85],
                [0.4, 0.1],
                [0.6, 0.1],
                [0.6, 0.65],
                [0.8, 0.65],
                [0.8, 0.45],
                [1.0, 0.45],
            ]
        }
    }
}

/// Shortest distance from point (px,py) to line segment (a, b) in normalized space.
///
/// Unity: `DistToSegment(float px, float py, Vector2 a, Vector2 b)` (lines 153-173).
fn dist_to_segment(px: f32, py: f32, a: [f32; 2], b: [f32; 2]) -> f32 {
    let abx = b[0] - a[0];
    let aby = b[1] - a[1];
    let len_sq = abx * abx + aby * aby;

    if len_sq < 0.000001 {
        let dx = px - a[0];
        let dy = py - a[1];
        return (dx * dx + dy * dy).sqrt();
    }

    let mut t = ((px - a[0]) * abx + (py - a[1]) * aby) / len_sq;
    if t < 0.0 {
        t = 0.0;
    } else if t > 1.0 {
        t = 1.0;
    }

    let cx = a[0] + abx * t - px;
    let cy = a[1] + aby * t - py;
    (cx * cx + cy * cy).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_all_waveforms() {
        for wave in [
            DriverWaveform::Sine,
            DriverWaveform::Triangle,
            DriverWaveform::Sawtooth,
            DriverWaveform::Square,
            DriverWaveform::Random,
        ] {
            let pixels = get(wave);
            assert_eq!(pixels.len(), SIZE * SIZE);
            // At least some pixels should be non-transparent (the waveform line)
            let drawn = pixels.iter().filter(|c| c.a > 0).count();
            assert!(drawn > 0, "{:?} should have drawn pixels", wave);
        }
    }

    #[test]
    fn icon_is_cached() {
        let a = get(DriverWaveform::Sine);
        let b = get(DriverWaveform::Sine);
        assert!(std::ptr::eq(a, b), "Should return same cached slice");
    }

    #[test]
    fn dist_to_segment_endpoints() {
        // Point on segment start
        let d = dist_to_segment(0.0, 0.0, [0.0, 0.0], [1.0, 0.0]);
        assert!(d < 0.001);

        // Point on segment end
        let d = dist_to_segment(1.0, 0.0, [0.0, 0.0], [1.0, 0.0]);
        assert!(d < 0.001);

        // Point above segment midpoint
        let d = dist_to_segment(0.5, 0.5, [0.0, 0.0], [1.0, 0.0]);
        assert!((d - 0.5).abs() < 0.001);
    }

    #[test]
    fn dist_to_degenerate_segment() {
        let d = dist_to_segment(1.0, 1.0, [0.0, 0.0], [0.0, 0.0]);
        assert!((d - std::f32::consts::SQRT_2).abs() < 0.001);
    }

    #[test]
    fn waveform_points_correct_count() {
        assert_eq!(fill_waveform_points(DriverWaveform::Sine).len(), SINE_SAMPLES);
        assert_eq!(fill_waveform_points(DriverWaveform::Triangle).len(), 3);
        assert_eq!(fill_waveform_points(DriverWaveform::Sawtooth).len(), 3);
        assert_eq!(fill_waveform_points(DriverWaveform::Square).len(), 4);
        assert_eq!(fill_waveform_points(DriverWaveform::Random).len(), 10);
    }
}
