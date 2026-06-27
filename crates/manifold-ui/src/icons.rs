//! Atlas-icon vocabulary — the one contract between the UI build path (which
//! emits icon chars in node `text`) and the renderer's glyph atlas (which
//! rasterises the bitmaps and dispatches them).
//!
//! Each icon is a Private-Use-Area codepoint `U+E000 + id`; `UIRenderer` draws
//! the atlas bitmap for any text node whose first char is an icon. Defining the
//! id↔char mapping ONCE here keeps the two crates from drifting on a bare
//! `0xE000` literal — before this they each hard-coded it (the renderer's
//! dispatch range, the LFO arm button, and the modulation drawer's shape row).

/// Private-Use-Area base codepoint for atlas icons.
const PUA_BASE: u32 = 0xE000;

/// One glyph in the renderer's icon atlas. The discriminant is the atlas id and
/// the PUA offset; declaration order is the atlas injection order in
/// `manifold-renderer`'s `native_text::generate_atlas_icons`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Icon {
    WaveSine = 0,
    WaveTriangle = 1,
    WaveSawtooth = 2,
    WaveSquare = 3,
    WaveRandom = 4,
    /// Cog / gear — the "hide modulation settings" toggle (the UI font has no ⚙).
    Cog = 5,
    /// Layer type badges (§24 5d) — drawn in the layer-header name row so type is
    /// read from a glyph, not from the header restructuring by type.
    LayerVideo = 6,
    LayerGenerator = 7,
    LayerGroup = 8,
    LayerAudio = 9,
    /// Playhead head marker (§24 5e) — a downward triangle at the top of the
    /// ruler so the "now" position is unmissable next to the insert cursor.
    Playhead = 10,
}

impl Icon {
    /// Number of distinct atlas icons (the atlas slot count).
    pub const COUNT: usize = 11;

    /// The atlas id (== PUA offset).
    #[inline]
    pub const fn id(self) -> u8 {
        self as u8
    }

    /// The PUA codepoint a node carries in its `text` to render this icon.
    #[inline]
    pub fn ch(self) -> char {
        // Every id is < COUNT, well inside the PUA block — never fails.
        char::from_u32(PUA_BASE + self as u32).expect("icon id is in the PUA range")
    }

    /// One-char `String` for a node's text (icon nodes carry exactly one char).
    #[inline]
    pub fn text(self) -> String {
        self.ch().to_string()
    }

    /// Decode a char to its atlas id, if it is an icon codepoint.
    #[inline]
    pub fn id_from_char(c: char) -> Option<u8> {
        let cp = c as u32;
        (PUA_BASE..PUA_BASE + Self::COUNT as u32)
            .contains(&cp)
            .then(|| (cp - PUA_BASE) as u8)
    }

    /// True when `c` renders as an atlas icon — its presence as a node's first
    /// char routes the renderer to the icon path instead of text shaping.
    #[inline]
    pub fn is_icon_char(c: char) -> bool {
        Self::id_from_char(c).is_some()
    }
}

/// The five waveform-shape icons in driver order (`DriverWaveform as usize`).
/// The LFO arm button and the modulation drawer's shape row index this.
pub const WAVEFORMS: [Icon; 5] = [
    Icon::WaveSine,
    Icon::WaveTriangle,
    Icon::WaveSawtooth,
    Icon::WaveSquare,
    Icon::WaveRandom,
];

/// Waveform icon char for a driver-waveform index (clamped to the valid range).
#[inline]
pub fn waveform_icon_char(wave_idx: i32) -> char {
    let i = wave_idx.clamp(0, WAVEFORMS.len() as i32 - 1) as usize;
    WAVEFORMS[i].ch()
}

/// The type badge for a layer, from its [`crate::types::LayerType`].
#[inline]
pub fn layer_badge(layer_type: crate::types::LayerType) -> Icon {
    use crate::types::LayerType;
    match layer_type {
        LayerType::Video => Icon::LayerVideo,
        LayerType::Generator => Icon::LayerGenerator,
        LayerType::Group => Icon::LayerGroup,
        LayerType::Audio => Icon::LayerAudio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_char_round_trip() {
        for (i, icon) in [
            Icon::WaveSine,
            Icon::WaveTriangle,
            Icon::WaveSawtooth,
            Icon::WaveSquare,
            Icon::WaveRandom,
            Icon::Cog,
            Icon::LayerVideo,
            Icon::LayerGenerator,
            Icon::LayerGroup,
            Icon::LayerAudio,
            Icon::Playhead,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(icon.id() as usize, i, "id == declaration index");
            assert_eq!(Icon::id_from_char(icon.ch()), Some(icon.id()));
            assert!(Icon::is_icon_char(icon.ch()));
        }
    }

    #[test]
    fn count_matches_highest_id() {
        // COUNT must cover every variant — Playhead is the last id.
        assert_eq!(Icon::Playhead.id() as usize, Icon::COUNT - 1);
    }

    #[test]
    fn non_icon_chars_reject() {
        assert!(!Icon::is_icon_char('A'));
        assert!(!Icon::is_icon_char(' '));
        // One past the last icon is not an icon.
        let past = char::from_u32(0xE000 + Icon::COUNT as u32).unwrap();
        assert!(!Icon::is_icon_char(past));
        assert_eq!(Icon::id_from_char(past), None);
    }

    #[test]
    fn waveform_index_clamps() {
        assert_eq!(waveform_icon_char(0), Icon::WaveSine.ch());
        assert_eq!(waveform_icon_char(4), Icon::WaveRandom.ch());
        assert_eq!(waveform_icon_char(-1), Icon::WaveSine.ch());
        assert_eq!(waveform_icon_char(99), Icon::WaveRandom.ch());
    }

    #[test]
    fn layer_badges_are_distinct() {
        use crate::types::LayerType;
        let badges = [
            layer_badge(LayerType::Video),
            layer_badge(LayerType::Generator),
            layer_badge(LayerType::Group),
            layer_badge(LayerType::Audio),
        ];
        for i in 0..badges.len() {
            for j in (i + 1)..badges.len() {
                assert_ne!(badges[i], badges[j], "each layer type gets a distinct badge");
            }
        }
    }
}
