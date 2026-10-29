//! The colour and brightness of a glowing body by temperature: Planck's law
//! integrated against the CIE 1931 2° observer and converted to linear
//! Rec.709. Fire reads it as a 256-entry table, made once on the CPU.

use std::sync::OnceLock;

/// Entries in the table.
pub const BLACKBODY_ENTRIES: usize = 256;
/// Entry 0's temperature, in kelvin.
pub const BLACKBODY_MIN_KELVIN: f32 = 500.0;
/// The last entry's temperature, in kelvin.
pub const BLACKBODY_MAX_KELVIN: f32 = 4000.0;
/// The temperature whose luminance is 1 in the table's `w`.
pub const BLACKBODY_REFERENCE_KELVIN: f32 = 1500.0;

const PLANCK_H: f64 = 6.626_070_15e-34;
const LIGHT_C: f64 = 299_792_458.0;
const BOLTZMANN_K: f64 = 1.380_649e-23;

/// The visible band the spectrum is summed over, in nanometres, in 1 nm steps.
const FIRST_NM: u32 = 380;
const LAST_NM: u32 = 780;

/// CIE XYZ to linear Rec.709 (sRGB primaries, D65 white).
const XYZ_TO_REC709: [[f64; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.9692660, 1.8760108, 0.0415560],
    [0.0556434, -0.2040259, 1.0572252],
];
/// Rec.709 luminance of a linear colour.
const LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// One lobe of the Wyman, Sloan and Shirley (2013) fit of the CIE 1931
/// observer: a Gaussian with one width below its mean and another above.
fn lobe(nm: f64, mean: f64, sigma_below: f64, sigma_above: f64) -> f64 {
    let sigma = if nm < mean { sigma_below } else { sigma_above };
    let t = (nm - mean) / sigma;
    (-0.5 * t * t).exp()
}

/// The observer's x̄, ȳ and z̄ at `nm`.
fn observer(nm: f64) -> [f64; 3] {
    [
        1.056 * lobe(nm, 599.8, 37.9, 31.0) + 0.362 * lobe(nm, 442.0, 16.0, 26.7)
            - 0.065 * lobe(nm, 501.1, 20.4, 26.2),
        0.821 * lobe(nm, 568.8, 46.9, 40.5) + 0.286 * lobe(nm, 530.9, 16.3, 31.1),
        1.217 * lobe(nm, 437.0, 11.8, 36.0) + 0.681 * lobe(nm, 459.0, 26.0, 13.8),
    ]
}

/// Planck's spectral radiance of a black body at `nm` and `kelvin`.
fn planck(nm: f64, kelvin: f64) -> f64 {
    let metres = nm * 1e-9;
    let exponent = PLANCK_H * LIGHT_C / (metres * BOLTZMANN_K * kelvin);
    2.0 * PLANCK_H * LIGHT_C * LIGHT_C / metres.powi(5) / exponent.exp_m1()
}

/// A black body at `kelvin`: its linear Rec.709 colour scaled to luminance 1
/// (channels the display cannot show clamped to 0 first), and its luminance Y
/// on an arbitrary absolute scale.
fn radiator(kelvin: f64) -> ([f64; 3], f64) {
    let mut xyz = [0.0; 3];
    for nm in FIRST_NM..=LAST_NM {
        let nm = f64::from(nm);
        let radiance = planck(nm, kelvin);
        for (sum, weight) in xyz.iter_mut().zip(observer(nm)) {
            *sum += radiance * weight;
        }
    }
    let linear = XYZ_TO_REC709.map(|row| {
        row.iter()
            .zip(xyz)
            .map(|(coefficient, value)| coefficient * value)
            .sum::<f64>()
            .max(0.0)
    });
    let luma: f64 = LUMA.iter().zip(linear).map(|(l, c)| l * c).sum();
    (linear.map(|channel| channel / luma), xyz[1])
}

fn kelvin_at(entry: usize) -> f64 {
    let span = f64::from(BLACKBODY_MAX_KELVIN - BLACKBODY_MIN_KELVIN);
    f64::from(BLACKBODY_MIN_KELVIN) + entry as f64 / (BLACKBODY_ENTRIES - 1) as f64 * span
}

/// Where `kelvin` falls in the table: the lower entry and the fraction
/// towards the next, with `kelvin` clamped to the table's range.
fn table_position(kelvin: f32) -> (usize, f32) {
    let span = BLACKBODY_MAX_KELVIN - BLACKBODY_MIN_KELVIN;
    let kelvin = kelvin.clamp(BLACKBODY_MIN_KELVIN, BLACKBODY_MAX_KELVIN);
    let position = (kelvin - BLACKBODY_MIN_KELVIN) / span * (BLACKBODY_ENTRIES - 1) as f32;
    let lower = (position.floor() as usize).min(BLACKBODY_ENTRIES - 2);
    (lower, position - lower as f32)
}

/// Entry k is `BLACKBODY_MIN_KELVIN + k/(ENTRIES-1)·(MAX-MIN)`.
/// rgb: linear Rec.709 colour scaled to luminance (Y) 1; w: the radiator's
/// luminance relative to 1500 K's, so reading the table at 1500 K gives
/// w = 1 (the reference is the table's own interpolation there, the way the
/// shader and [`blackbody_radiance`] read it).
pub fn blackbody_table() -> Vec<[f32; 4]> {
    table().to_vec()
}

fn table() -> &'static [[f32; 4]] {
    static TABLE: OnceLock<Vec<[f32; 4]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let spectra: Vec<([f64; 3], f64)> = (0..BLACKBODY_ENTRIES)
            .map(|entry| radiator(kelvin_at(entry)))
            .collect();
        let (lower, fraction) = table_position(BLACKBODY_REFERENCE_KELVIN);
        let fraction = f64::from(fraction);
        let reference = spectra[lower].1 * (1.0 - fraction) + spectra[lower + 1].1 * fraction;
        spectra
            .iter()
            .map(|&([r, g, b], luminance)| {
                [r as f32, g as f32, b as f32, (luminance / reference) as f32]
            })
            .collect()
    })
}

/// rgb·w at `kelvin`, linearly interpolated in the table, clamped to its range.
pub fn blackbody_radiance(kelvin: f32) -> [f32; 3] {
    let table = table();
    let (lower, fraction) = table_position(kelvin);
    let (a, b) = (table[lower], table[lower + 1]);
    let at = |i: usize| a[i] + (b[i] - a[i]) * fraction;
    let w = at(3);
    [at(0) * w, at(1) * w, at(2) * w]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(rgb: [f32; 3]) -> f32 {
        0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
    }

    #[test]
    fn six_thousand_five_hundred_kelvin_is_near_white() {
        let (rgb, _) = radiator(6500.0);
        let largest = rgb.iter().copied().fold(f64::MIN, f64::max);
        let smallest = rgb.iter().copied().fold(f64::MAX, f64::min);
        assert!(largest / smallest < 1.15, "{rgb:?}");
    }

    #[test]
    fn a_thousand_kelvin_glows_red_then_orange() {
        let [r, g, b] = blackbody_radiance(1000.0);
        assert!(r > g && g > b, "{r} {g} {b}");
    }

    #[test]
    fn brighter_with_every_degree_of_the_table() {
        let table = blackbody_table();
        assert_eq!(table.len(), BLACKBODY_ENTRIES);
        for pair in table.windows(2) {
            assert!(pair[0][3] < pair[1][3], "{:?} {:?}", pair[0], pair[1]);
        }
    }

    #[test]
    fn the_reference_temperature_has_luminance_one() {
        let y = luminance(blackbody_radiance(BLACKBODY_REFERENCE_KELVIN));
        assert!((y - 1.0).abs() < 1e-3, "{y}");
    }

    #[test]
    fn entries_have_unit_luminance_colour_and_the_range_clamps() {
        let table = blackbody_table();
        for entry in &table {
            let y = luminance([entry[0], entry[1], entry[2]]);
            assert!((y - 1.0).abs() < 1e-4, "{entry:?}");
        }
        assert_eq!(blackbody_radiance(10.0), blackbody_radiance(500.0));
        assert_eq!(blackbody_radiance(9000.0), blackbody_radiance(4000.0));
    }
}
