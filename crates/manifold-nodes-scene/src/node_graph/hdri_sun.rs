//! Where the sun is in an HDRI: the direction toward its brightest region,
//! the way Blender's Sun Position add-on aims a sun at an environment.
//!
//! The decoded map is averaged down to a small grid first, so one hot texel
//! (a sensor speck) never wins over a real light source, and the peak cell's
//! neighbours pull the answer to the centre of a sun disc wider than a cell.
//! It runs once per "Aim Sun at HDRI" click on the file's decoded bytes,
//! never per frame.

use std::path::Path;

use half::f16;

use crate::node_graph::decode_cache::cached_load_hdri;

/// The grid the map is averaged down to: columns across the full 360 degrees.
const GRID_COLUMNS: usize = 128;
/// Cells either side of the peak that join the centre-of-mass.
const REFINE_RADIUS: i64 = 2;

/// The unit direction toward the HDRI's brightest region, in the renderer's
/// environment convention (`pbr_equirect_uv`): x = cos(lat)cos(lon), y =
/// sin(lat), z = cos(lat)sin(lon). Decodes `path` through the shared decode
/// cache, so a file the scene already loaded costs a cache read.
pub fn hdri_sun_direction(path: &Path) -> Result<[f32; 3], String> {
    let (width, height, texels) = cached_load_hdri(path)?;
    sun_direction(width as usize, height as usize, &texels)
}

/// [`hdri_sun_direction`] over decoded `Rgba16Float` texels, row 0 at the
/// nadir (how `hdri_source` stores them).
pub(crate) fn sun_direction(
    width: usize,
    height: usize,
    rgba16: &[u8],
) -> Result<[f32; 3], String> {
    if width == 0 || height == 0 || rgba16.len() < width * height * 8 {
        return Err("the HDRI has no pixels".to_string());
    }
    let cols = GRID_COLUMNS.min(width);
    let rows = (cols / 2).clamp(1, height);
    let luminance = block_luminance(width, height, rgba16, cols, rows);
    let (peak_row, peak_col) = (0..rows)
        .flat_map(|r| (0..cols).map(move |c| (r, c)))
        .max_by(|&(ar, ac), &(br, bc)| {
            luminance[ar * cols + ac].total_cmp(&luminance[br * cols + bc])
        })
        .ok_or("the HDRI has no pixels")?;
    if luminance[peak_row * cols + peak_col] <= 0.0 {
        return Err("the HDRI has no bright region".to_string());
    }

    let mut sum = [0.0f64; 3];
    for dr in -REFINE_RADIUS..=REFINE_RADIUS {
        let row = peak_row as i64 + dr;
        if !(0..rows as i64).contains(&row) {
            continue;
        }
        for dc in -REFINE_RADIUS..=REFINE_RADIUS {
            let col = (peak_col as i64 + dc).rem_euclid(cols as i64) as usize;
            let weight = f64::from(luminance[row as usize * cols + col]);
            let dir = cell_direction(row as usize, col, rows, cols);
            for k in 0..3 {
                sum[k] += weight * dir[k];
            }
        }
    }
    let len = (sum[0] * sum[0] + sum[1] * sum[1] + sum[2] * sum[2]).sqrt();
    if !len.is_finite() || len <= 0.0 {
        return Err("the HDRI has no bright region".to_string());
    }
    Ok([
        (sum[0] / len) as f32,
        (sum[1] / len) as f32,
        (sum[2] / len) as f32,
    ])
}

/// Mean luminance of each `cols` x `rows` block; a non-finite texel counts as
/// black.
fn block_luminance(
    width: usize,
    height: usize,
    rgba16: &[u8],
    cols: usize,
    rows: usize,
) -> Vec<f32> {
    let mut sums = vec![0.0f64; cols * rows];
    let mut counts = vec![0u32; cols * rows];
    for y in 0..height {
        let row = y * rows / height;
        for x in 0..width {
            let at = (y * width + x) * 8;
            let channel = |k: usize| {
                f16::from_le_bytes([rgba16[at + 2 * k], rgba16[at + 2 * k + 1]]).to_f32()
            };
            let luma = 0.2126 * channel(0) + 0.7152 * channel(1) + 0.0722 * channel(2);
            let cell = row * cols + x * cols / width;
            if luma.is_finite() && luma > 0.0 {
                sums[cell] += f64::from(luma);
            }
            counts[cell] += 1;
        }
    }
    sums.iter()
        .zip(&counts)
        .map(|(sum, &n)| {
            if n == 0 {
                0.0
            } else {
                (sum / f64::from(n)) as f32
            }
        })
        .collect()
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

    /// A dim sky with one bright disc centred on `toward`, plus a one-texel
    /// speck far brighter than the disc's mean.
    fn sky_with_sun(width: usize, height: usize, toward: [f64; 3]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(width * height * 8);
        for y in 0..height {
            for x in 0..width {
                let lon = ((x as f64 + 0.5) / width as f64 - 0.5) * std::f64::consts::TAU;
                let lat = ((y as f64 + 0.5) / height as f64 - 0.5) * std::f64::consts::PI;
                let dir = [lat.cos() * lon.cos(), lat.sin(), lat.cos() * lon.sin()];
                let along = dir[0] * toward[0] + dir[1] * toward[1] + dir[2] * toward[2];
                let mut value = if along > 0.995 { 40.0 } else { 0.2 };
                if (x, y) == (3, 3) {
                    value = 300.0;
                }
                for channel in [value, value, value, 1.0] {
                    bytes.extend_from_slice(&f16::from_f32(channel).to_le_bytes());
                }
            }
        }
        bytes
    }

    fn unit(v: [f64; 3]) -> [f64; 3] {
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        v.map(|c| c / len)
    }

    #[test]
    fn the_sun_is_the_bright_disc_not_a_speck() {
        let toward = unit([0.6, 0.5, -0.4]);
        let found = sun_direction(512, 256, &sky_with_sun(512, 256, toward)).unwrap();
        let dot = found
            .iter()
            .zip(toward)
            .map(|(&a, b)| f64::from(a) * b)
            .sum::<f64>();
        assert!(dot > 0.995, "found {found:?} for {toward:?}, dot {dot}");
    }

    #[test]
    fn a_sun_across_the_seam_is_found() {
        let toward = unit([-1.0, 0.1, 0.01]);
        let found = sun_direction(512, 256, &sky_with_sun(512, 256, toward)).unwrap();
        let dot = found
            .iter()
            .zip(toward)
            .map(|(&a, b)| f64::from(a) * b)
            .sum::<f64>();
        assert!(dot > 0.995, "found {found:?} for {toward:?}, dot {dot}");
    }

    #[test]
    fn a_black_sky_has_no_sun() {
        let black = vec![0u8; 64 * 32 * 8];
        assert!(sun_direction(64, 32, &black).is_err());
        assert!(sun_direction(0, 0, &[]).is_err());
    }
}
