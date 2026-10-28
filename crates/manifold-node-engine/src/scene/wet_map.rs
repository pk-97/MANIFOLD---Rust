//! The wet map's layout: one array of u32 words over the water's object-space
//! XZ, written by `node.wet_map` and read by every mesh shader's wet-surface
//! look. It lives here, beside its WGSL helpers ([`LAYOUT`]), because the
//! water crate that marks it and the scene renderer that reads it do not
//! depend on each other.

/// Header words before the first texel: origin x and z, texel size, side,
/// the display time of the last mark, the drying rate, two unused.
pub const HEADER_WORDS: u64 = 8;

/// Words per texel: touch key, lowest and highest touched height, the touched
/// height bins.
pub const TEXEL_WORDS: u64 = 4;

/// Texels per side of the map.
pub const SIDE: u32 = 1024;

/// The WGSL layout helpers (`wet_map_layout.wgsl`): header indices, the touch
/// and height keys, whether a texel speaks for a height, and its wetness.
pub const LAYOUT: &str = include_str!("wet_map_layout.wgsl");

/// Bytes of a map `side` texels a side.
pub fn map_bytes(side: u32) -> u64 {
    4 * (HEADER_WORDS + TEXEL_WORDS * u64::from(side) * u64::from(side))
}

/// The touch key of display time `now`, as the shader writes it.
pub fn touch_key(now: f32) -> u32 {
    now.max(0.0).to_bits() + 1
}

/// A height as a u32 that orders like the float, as the shader writes it.
pub fn height_key(y: f32) -> u32 {
    let bits = y.to_bits();
    if bits & 0x8000_0000 != 0 {
        !bits
    } else {
        bits | 0x8000_0000
    }
}

/// The bit of height `y`'s bin, bins `bin` metres tall wrapping every 16, in
/// the high half for a touch on an up-facing surface, as the shader sets it.
pub fn height_bit(y: f32, bin: f32, up: bool) -> u32 {
    (1 << (((y / bin).floor() as i32) & 15)) << if up { 16 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_keys_order_like_the_floats() {
        let heights = [-1.0e6_f32, -2.5, -0.0, 0.0, 1.0e-6, 0.3, 7.0, 1.0e6];
        for pair in heights.windows(2) {
            assert!(height_key(pair[0]) <= height_key(pair[1]), "{pair:?}");
        }
        // Zero is below every key: an untouched word never wins a max.
        assert!(
            heights
                .iter()
                .all(|&y| height_key(y) > 0 && !height_key(y) > 0)
        );
    }

    #[test]
    fn touch_keys_order_like_time_and_never_read_zero() {
        assert_eq!(touch_key(0.0), 1);
        assert!(touch_key(0.5) < touch_key(12.0));
        assert_eq!(touch_key(-3.0), touch_key(0.0));
    }

    #[test]
    fn a_full_map_is_sixteen_megabytes_and_a_header() {
        assert_eq!(map_bytes(SIDE), 32 + 16 * 1024 * 1024);
    }
}
