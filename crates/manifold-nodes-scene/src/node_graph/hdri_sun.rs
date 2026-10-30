//! The sun in an HDRI: where it is, how much light it gives and how wide it
//! looks, measured once per file on `node.hdri_source`'s decode thread.
//!
//! The map is averaged down to a coarse grid first, so one hot texel (a
//! sensor speck) never wins over a real light source. Around the brightest
//! cell, the sky in a ring just outside the sun (`SUN_REACH`..`RING_OUTER`)
//! is the background, and the light above it inside the ring is the sun's.
//! A clipped sun is flat on top, so the brightest cell can sit at its edge:
//! the measure runs again around the first answer's centre.
//! A sun is small: a bright patch wider than `MAX_RADIUS` (a softbox, a
//! window, a sun glowing through cloud) is no sun, and neither is a speck
//! that gives almost none of the map's light (`MIN_SHARE`). A low sunset sun
//! counts even though the sky gives most of the light.

use half::f16;
use manifold_node_engine::scene::light::{Light, LightMode};

/// The grid the map is averaged down to: columns across the full 360 degrees.
const GRID_COLUMNS: usize = 128;
/// How far from the brightest cell's centre a texel can be the sun, radians.
const SUN_REACH: f64 = 8.0 * std::f64::consts::PI / 180.0;
/// The outer edge of the background ring, radians.
const RING_OUTER: f64 = 12.0 * std::f64::consts::PI / 180.0;
/// A texel is the sun's when it stands above the background by at least this
/// fraction of the brightest texel's excess.
const REGION_FLOOR: f64 = 0.01;
/// The widest a sun can be, as an angular radius: a clear sun measures
/// about half a degree with its glow, a lit window or softbox several.
const MAX_RADIUS: f64 = 1.5 * std::f64::consts::PI / 180.0;
/// The least share of the whole map's light the sun must give.
const MIN_SHARE: f64 = 0.01;

/// An HDRI's sun, in the file's own frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HdriSun {
    /// Unit direction toward the sun, the renderer's environment convention
    /// with no rotation (`env_lookup_uv`): x = cos(lat)cos(lon), y =
    /// sin(lat), z = cos(lat)sin(lon).
    pub toward: [f32; 3],
    /// The sun's light above the sky around it, per channel: its radiance
    /// summed over its solid angle, the irradiance it gives a surface facing
    /// it. That is what a Light's colour means.
    pub irradiance: [f32; 3],
    /// Angular radius of the region counted as the sun, radians.
    pub radius: f32,
    /// Angular radius of the even disc that spreads its light the same way,
    /// radians: what sets how soft its shadows are. A disc of radius R has
    /// a mean squared angle from its centre of R²/2; a glow around the disc
    /// counts by the light it gives, so it widens this far less than it
    /// widens `radius`.
    pub disc: f32,
}

impl HdriSun {
    /// The sun as a Sun light shining from `toward`, its colour the
    /// irradiance and its `sun_radius` the disc's. The region's radius rides
    /// in `outer_cone_angle`, which a Sun has no other use for, so the
    /// Environment can paint the region out of its lighting map. No sun is a
    /// black Sun: a light slot keeps its last value, so "none" has to be a
    /// value too.
    pub(crate) fn light(found: Option<Self>) -> Light {
        let Some(sun) = found else {
            return Light {
                color: [0.0, 0.0, 0.0, 1.0],
                ..Light::default_sun()
            };
        };
        let [x, y, z] = sun.toward;
        let [r, g, b] = sun.irradiance;
        Light {
            mode: LightMode::Sun,
            pos: [x * 30.0, y * 30.0, z * 30.0],
            aim: [0.0; 3],
            dir: [-x, -y, -z],
            color: [r, g, b, 1.0],
            outer_cone_angle: sun.radius,
            sun_radius: sun.disc,
            ..Light::default_sun()
        }
    }

    /// [`Self::light`]'s sun back from the light, `None` for a black one.
    pub(crate) fn from_light(light: Option<Light>) -> Option<Self> {
        let light = light.filter(|l| matches!(l.mode, LightMode::Sun))?;
        let [r, g, b, _] = light.color;
        if r.max(g).max(b) <= 0.0 {
            return None;
        }
        let [x, y, z] = light.dir;
        Some(Self {
            toward: [-x, -y, -z],
            irradiance: [r, g, b],
            radius: light.outer_cone_angle,
            disc: light.sun_radius,
        })
    }
}

/// The sun of a map of `Rgba16Float` texels, row 0 at the nadir (how
/// `hdri_source` stores them). `None` when the map has no distinct sun:
/// overcast, a studio, a night sky, or no light at all.
pub(crate) fn measure(width: usize, height: usize, rgba16: &[u8]) -> Option<HdriSun> {
    if width == 0 || height == 0 || rgba16.len() < width * height * 8 {
        return None;
    }
    let cols = GRID_COLUMNS.min(width);
    let rows = (cols / 2).clamp(1, height);
    let (luminance, total) = block_luminance(width, height, rgba16, cols, rows);
    let peak = (0..rows * cols).max_by(|&a, &b| luminance[a].total_cmp(&luminance[b]))?;
    if luminance[peak] <= 0.0 || total <= 0.0 {
        return None;
    }
    let centre = cell_direction(peak / cols, peak % cols, rows, cols);
    let (first, _) = around(width, height, rgba16, centre)?;
    let (sun, energy) = around(width, height, rgba16, first.toward.map(f64::from))?;
    (energy / total >= MIN_SHARE && f64::from(sun.radius) <= MAX_RADIUS).then_some(sun)
}

/// The sun measured around `centre`, and its light: excess luminance summed
/// over solid angle.
fn around(width: usize, height: usize, rgba16: &[u8], centre: [f64; 3]) -> Option<(HdriSun, f64)> {
    // Every texel within the ring's outer edge, by latitude band first.
    let pi = std::f64::consts::PI;
    let lat = centre[1].clamp(-1.0, 1.0).asin();
    let row_of = |lat: f64| (lat / pi + 0.5) * height as f64;
    let first = row_of(lat - RING_OUTER).floor().max(0.0) as usize;
    let last = (row_of(lat + RING_OUTER).ceil() as usize).min(height - 1);
    let (cos_outer, cos_reach) = (RING_OUTER.cos(), SUN_REACH.cos());
    let mut ring = [0.0f64; 3];
    let mut ring_weight = 0.0f64;
    let mut inside: Vec<([f64; 3], [f64; 3], f64)> = Vec::new();
    for y in first..=last {
        let solid_angle = texel_solid_angle(y, width, height);
        for x in 0..width {
            let dir = texel_direction(x, y, width, height);
            let along = dot(dir, centre);
            if along < cos_outer {
                continue;
            }
            let rgb = texel_rgb(rgba16, (y * width + x) * 8);
            if along >= cos_reach {
                inside.push((dir, rgb, solid_angle));
            } else {
                for k in 0..3 {
                    ring[k] += rgb[k] * solid_angle;
                }
                ring_weight += solid_angle;
            }
        }
    }
    if ring_weight <= 0.0 {
        return None;
    }
    let background = ring.map(|c| c / ring_weight);
    let excess =
        |rgb: [f64; 3]| -> [f64; 3] { [0, 1, 2].map(|k| (rgb[k] - background[k]).max(0.0)) };
    let brightest = inside
        .iter()
        .map(|&(_, rgb, _)| luma(excess(rgb)))
        .fold(0.0f64, f64::max);
    if brightest <= 0.0 {
        return None;
    }
    let floor = brightest * REGION_FLOOR;
    let mut irradiance = [0.0f64; 3];
    let mut weighted = [0.0f64; 3];
    let mut energy = 0.0f64;
    for &(dir, rgb, solid_angle) in &inside {
        let light = excess(rgb);
        let strength = luma(light);
        if strength < floor {
            continue;
        }
        for k in 0..3 {
            irradiance[k] += light[k] * solid_angle;
            weighted[k] += strength * solid_angle * dir[k];
        }
        energy += strength * solid_angle;
    }
    let toward = normalize(weighted)?;
    let mut widest = 0.0f64;
    let mut spread = 0.0f64;
    for &(dir, rgb, solid_angle) in &inside {
        let strength = luma(excess(rgb));
        if strength < floor {
            continue;
        }
        let angle = dot(dir, toward).clamp(-1.0, 1.0).acos();
        widest = widest.max(angle);
        spread += strength * solid_angle * angle * angle;
    }
    let sun = HdriSun {
        toward: toward.map(|c| c as f32),
        irradiance: irradiance.map(|c| c as f32),
        radius: (widest + pi / height as f64) as f32,
        disc: (2.0 * spread / energy).sqrt() as f32,
    };
    Some((sun, energy))
}

/// Mean luminance of each `cols` x `rows` block, and the map's whole light:
/// luminance summed over solid angle. A non-finite texel counts as black.
fn block_luminance(
    width: usize,
    height: usize,
    rgba16: &[u8],
    cols: usize,
    rows: usize,
) -> (Vec<f32>, f64) {
    let mut sums = vec![0.0f64; cols * rows];
    let mut counts = vec![0u32; cols * rows];
    let mut total = 0.0f64;
    for y in 0..height {
        let row = y * rows / height;
        let solid_angle = texel_solid_angle(y, width, height);
        for x in 0..width {
            let value = luma(texel_rgb(rgba16, (y * width + x) * 8));
            let cell = row * cols + x * cols / width;
            sums[cell] += value;
            counts[cell] += 1;
            total += value * solid_angle;
        }
    }
    let means = sums
        .iter()
        .zip(&counts)
        .map(|(sum, &n)| {
            if n == 0 {
                0.0
            } else {
                (sum / f64::from(n)) as f32
            }
        })
        .collect();
    (means, total)
}

/// A texel's linear RGB; a non-finite or negative channel counts as black.
fn texel_rgb(rgba16: &[u8], at: usize) -> [f64; 3] {
    [0, 1, 2].map(|k| {
        let value = f16::from_le_bytes([rgba16[at + 2 * k], rgba16[at + 2 * k + 1]]).to_f32();
        if value.is_finite() {
            f64::from(value.max(0.0))
        } else {
            0.0
        }
    })
}

fn luma(rgb: [f64; 3]) -> f64 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: [f64; 3]) -> Option<[f64; 3]> {
    let len = dot(v, v).sqrt();
    (len.is_finite() && len > 0.0).then(|| v.map(|c| c / len))
}

/// The solid angle of one texel in row `y` of an equirect map.
fn texel_solid_angle(y: usize, width: usize, height: usize) -> f64 {
    let pi = std::f64::consts::PI;
    let lat = ((y as f64 + 0.5) / height as f64 - 0.5) * pi;
    (2.0 * pi / width as f64) * (pi / height as f64) * lat.cos()
}

fn texel_direction(x: usize, y: usize, width: usize, height: usize) -> [f64; 3] {
    cell_direction(y, x, height, width)
}

/// The unit direction through the centre of grid cell (`row`, `col`).
fn cell_direction(row: usize, col: usize, rows: usize, cols: usize) -> [f64; 3] {
    let lon = ((col as f64 + 0.5) / cols as f64 - 0.5) * std::f64::consts::TAU;
    let lat = ((row as f64 + 0.5) / rows as f64 - 0.5) * std::f64::consts::PI;
    [lat.cos() * lon.cos(), lat.sin(), lat.cos() * lon.sin()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKY: f64 = 0.2;
    const SUN: f64 = 400.0;
    /// The disc's angular radius in the synthetic sky, under `MAX_RADIUS`.
    const DISC: f64 = 0.02;
    /// The synthetic sky's size: enough texels across the disc to sum it.
    const W: usize = 2048;
    const H: usize = 1024;

    /// A dim sky with one bright disc centred on `toward`, plus a one-texel
    /// speck far brighter than the disc's mean.
    fn sky_with_sun(width: usize, height: usize, toward: [f64; 3]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(width * height * 8);
        for y in 0..height {
            for x in 0..width {
                let along = dot(texel_direction(x, y, width, height), toward);
                let mut value = if along > DISC.cos() { SUN } else { SKY };
                if (x, y) == (3, 3) {
                    value = 3000.0;
                }
                for channel in [value, value, value, 1.0] {
                    bytes.extend_from_slice(&f16::from_f64(channel).to_le_bytes());
                }
            }
        }
        bytes
    }

    fn unit(v: [f64; 3]) -> [f64; 3] {
        normalize(v).unwrap()
    }

    fn along(found: &HdriSun, toward: [f64; 3]) -> f64 {
        dot(found.toward.map(f64::from), toward)
    }

    #[test]
    fn the_sun_is_the_bright_disc_not_a_speck() {
        let toward = unit([0.6, 0.5, -0.4]);
        let found = measure(W, H, &sky_with_sun(W, H, toward)).unwrap();
        assert!(along(&found, toward) > 0.99995, "{found:?} for {toward:?}");
        // The disc gives its radiance above the sky over its solid angle.
        let expected = (SUN - SKY) * std::f64::consts::TAU * (1.0 - DISC.cos());
        let got = f64::from(found.irradiance[1]);
        assert!(
            (got / expected - 1.0).abs() < 0.08,
            "irradiance {got}, expected {expected}"
        );
        let radius = f64::from(found.radius);
        assert!((DISC..DISC + 0.008).contains(&radius), "radius {radius}");
        let disc = f64::from(found.disc);
        assert!((disc / DISC - 1.0).abs() < 0.05, "disc {disc}");
    }

    #[test]
    fn a_sun_across_the_seam_is_found() {
        let toward = unit([-1.0, 0.1, 0.01]);
        let found = measure(W, H, &sky_with_sun(W, H, toward)).unwrap();
        assert!(along(&found, toward) > 0.99995, "{found:?} for {toward:?}");
    }

    #[test]
    fn a_sky_without_a_sun_has_none() {
        assert_eq!(measure(64, 32, &vec![0u8; 64 * 32 * 8]), None);
        assert_eq!(measure(0, 0, &[]), None);
        // A wide bright patch fills its own ring: no sun.
        let mut bytes = Vec::with_capacity(256 * 128 * 8);
        for y in 0..128 {
            for x in 0..256 {
                let up = texel_direction(x, y, 256, 128)[1];
                let value = if up > 0.5 { 20.0 } else { SKY };
                for channel in [value, value, value, 1.0] {
                    bytes.extend_from_slice(&f16::from_f64(channel).to_le_bytes());
                }
            }
        }
        assert_eq!(measure(256, 128, &bytes), None);
    }

    #[test]
    fn the_light_carries_the_sun_and_none_reads_back_as_none() {
        let sun = HdriSun {
            toward: [0.0, 0.6, 0.8],
            irradiance: [5.0, 4.0, 3.0],
            radius: 0.05,
            disc: 0.01,
        };
        assert_eq!(
            HdriSun::from_light(Some(HdriSun::light(Some(sun)))),
            Some(sun)
        );
        assert_eq!(HdriSun::from_light(Some(HdriSun::light(None))), None);
        assert_eq!(HdriSun::from_light(None), None);
    }
}
