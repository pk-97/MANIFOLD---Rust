//! The wet map's layout: one array of u32 words holding the soft wet marks
//! (decals) the water leaves on surfaces, written by `node.wet_map` and drawn
//! by the scene's wet pass. It lives here, beside its WGSL helpers
//! ([`layout`]), because the water crate that marks it and the scene renderer
//! that draws it do not depend on each other.

/// Header words: the decals' draw arguments (vertices, count, 0, 0), the
/// display time of the last mark, the drying rate, the half in use, the
/// contacts refused for want of room, the next half's count while compacting,
/// the decal radius and the merge distance (both metres), five unused.
pub const HEADER_WORDS: u64 = 16;

/// Slots of the merge hash a contact finds its decal through.
pub const HASH_SLOTS: u64 = 1 << 17;

/// Decals a half holds.
pub const CAPACITY: u32 = 1 << 16;

/// Words per decal: centre xyz, normal, touch key, amount, two unused.
pub const DECAL_WORDS: u64 = 8;

/// Seconds a surface the water left takes to dry to 1/e at Drying 1, so a
/// splash fades in about half a minute on stage. A real 0.1 mm film takes
/// about half an hour (FAO-56 evaporation), which reads as never drying.
pub const DRY_SECONDS: f32 = 15.0;

/// Words of the whole map.
pub fn map_words() -> u64 {
    HEADER_WORDS + HASH_SLOTS + 2 * u64::from(CAPACITY) * DECAL_WORDS
}

/// The WGSL layout: these constants, then the helpers
/// (`wet_map_layout.wgsl`): header and decal word indices, the merge hash,
/// the touch key, the normal's word, and a decal's strength and soft edge.
pub fn layout() -> String {
    format!(
        "const WD_HEADER_WORDS: u32 = {HEADER_WORDS}u;\n\
         const WD_HASH_SLOTS: u32 = {HASH_SLOTS}u;\n\
         const WD_CAPACITY: u32 = {CAPACITY}u;\n\
         const WD_DECAL_WORDS: u32 = {DECAL_WORDS}u;\n\
         const WD_DRY_SECONDS: f32 = {DRY_SECONDS:?};\n{}",
        include_str!("wet_map_layout.wgsl")
    )
}

/// The touch key of display time `now`, as the shader writes it.
pub fn touch_key(now: f32) -> u32 {
    now.max(0.0).to_bits() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_keys_order_like_time_and_never_read_zero() {
        assert_eq!(touch_key(0.0), 1);
        assert!(touch_key(0.5) < touch_key(12.0));
        assert_eq!(touch_key(-3.0), touch_key(0.0));
    }

    #[test]
    fn the_map_is_a_header_a_hash_and_two_halves_of_decals() {
        assert_eq!(map_words() * 4, 64 + 4 * (1 << 17) + 2 * 32 * (1 << 16));
        assert!(HASH_SLOTS.is_power_of_two());
    }
}
