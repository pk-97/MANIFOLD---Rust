use serde::{Deserialize, Serialize};

// ─── Blend Modes ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum BlendMode {
    #[default]
    Normal = 0,
    Additive = 1,
    Multiply = 2,
    Screen = 3,
    Overlay = 4,
    Stencil = 5,
    Opaque = 6,
    Difference = 7,
    Exclusion = 8,
    Subtract = 9,
    ColorDodge = 10,
    Lighten = 11,
    Darken = 12,
}

impl BlendMode {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Additive => "Additive",
            Self::Multiply => "Multiply",
            Self::Screen => "Screen",
            Self::Overlay => "Overlay",
            Self::Stencil => "Stencil",
            Self::Opaque => "Opaque",
            Self::Difference => "Difference",
            Self::Exclusion => "Exclusion",
            Self::Subtract => "Subtract",
            Self::ColorDodge => "Color Dodge",
            Self::Lighten => "Lighten",
            Self::Darken => "Darken",
        }
    }

    pub const ALL: &'static [BlendMode] = &[
        BlendMode::Normal,
        BlendMode::Additive,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Stencil,
        BlendMode::Opaque,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Subtract,
        BlendMode::ColorDodge,
        BlendMode::Lighten,
        BlendMode::Darken,
    ];

    pub fn from_index(i: usize) -> Self {
        Self::try_from(i as i32).unwrap_or(Self::Normal)
    }
}

// ─── Layer Type ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum LayerType {
    #[default]
    Video = 0,
    Generator = 1,
    Group = 2,
    /// An audio track: plays an imported audio file synced to the transport and
    /// can drive audio modulation. No visual output (the compositor skips it).
    Audio = 3,
    /// A direct-drive DMX/LED layer: renders its generator like any layer but
    /// routes ONLY to the LED composite — screen-invisible by construction.
    ///
    Dmx = 4,
    /// A non-rendering child lane whose clips provide independent trigger timing.
    Trigger = 5,
}

/// The kind of clip a layer holds. A clip may only live on a layer of the
/// same kind — the one rule clip move, paste, duplicate and create read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipKind {
    Video,
    /// Generator and DMX layers share this kind: both host a generator, and a
    /// clip adopts its target layer's generator.
    Generator,
    Audio,
    Trigger,
}

impl LayerType {
    /// `None` for groups, which hold no clips.
    pub fn clip_kind(self) -> Option<ClipKind> {
        match self {
            LayerType::Video => Some(ClipKind::Video),
            LayerType::Generator | LayerType::Dmx => Some(ClipKind::Generator),
            LayerType::Audio => Some(ClipKind::Audio),
            LayerType::Trigger => Some(ClipKind::Trigger),
            LayerType::Group => None,
        }
    }

    /// Whether clips living on a `from` layer may be placed on this one.
    pub fn accepts_clips_from(self, from: LayerType) -> bool {
        self.clip_kind().is_some() && self.clip_kind() == from.clip_kind()
    }

    /// Whether a layer of this type may own a child of `child`.
    /// Trigger lanes are leaves owned by any non-trigger layer; ordinary
    /// layers may be parented only under groups.
    pub fn accepts_child(self, child: LayerType) -> bool {
        if child == LayerType::Trigger {
            self != LayerType::Trigger
        } else {
            self == LayerType::Group
        }
    }

    /// Whether this layer kind can play timeline clip content.
    pub fn supports_clip_playback(self) -> bool {
        !matches!(self, LayerType::Group | LayerType::Trigger)
    }

    /// Whether this layer kind should produce timeline clip thumbnails.
    pub fn supports_clip_thumbnails(self) -> bool {
        matches!(
            self,
            LayerType::Video | LayerType::Generator | LayerType::Dmx
        )
    }
}

// ─── Clock Authority ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ClockAuthority {
    #[default]
    Internal = 0,
    Link = 1,
    MidiClock = 2,
    Osc = 3,
    /// Tempo and bar 1 heard from the first audio send.
    Audio = 4,
}

impl ClockAuthority {
    pub fn next(&self) -> Self {
        match self {
            Self::Internal => Self::Link,
            Self::Link => Self::MidiClock,
            Self::MidiClock => Self::Osc,
            Self::Osc => Self::Audio,
            Self::Audio => Self::Internal,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Internal => "INT",
            Self::Link => "LINK",
            Self::MidiClock => "MIDI",
            Self::Osc => "OSC",
            Self::Audio => "AUDIO",
        }
    }

    /// Transport bar label matching Unity's PushClockAuthorityToPanel format.
    pub fn transport_label(&self) -> &'static str {
        match self {
            Self::Internal => "SRC:INT",
            Self::Link => "SRC:LNK",
            Self::MidiClock => "SRC:CLK",
            Self::Osc => "SRC:OSC",
            Self::Audio => "SRC:AUD",
        }
    }
}

// ─── OSC Sync Mode ───

/// Selects which OSC transport sync path to use when `ClockAuthority::Osc` is active.
/// - `M4L`: Legacy Max4Live MANIFOLD Sync device (timecode on port 9000/9001)
/// - `AbletonOsc`: AbletonOSC remote script (listener-based on port 11000/11001)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum OscSyncMode {
    #[default]
    M4L = 0,
    AbletonOsc = 1,
}

impl OscSyncMode {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::M4L => "M4L",
            Self::AbletonOsc => "AbletonOSC",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::M4L => Self::AbletonOsc,
            Self::AbletonOsc => Self::M4L,
        }
    }
}

// ─── Quantize Mode ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum QuantizeMode {
    #[default]
    Off = 0,
    QuarterBeat = 1,
    Beat = 2,
    Bar = 3,
}

impl QuantizeMode {
    pub fn next(&self) -> Self {
        match self {
            Self::Off => Self::QuarterBeat,
            Self::QuarterBeat => Self::Beat,
            Self::Beat => Self::Bar,
            Self::Bar => Self::Off,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::QuarterBeat => "1/4",
            Self::Beat => "BEAT",
            Self::Bar => "BAR",
        }
    }
}

// ─── Resolution Preset ───

/// Named output sizes offered in the resolution dropdown. The project stores
/// only `output_width`/`output_height`; a preset is a shortcut to a size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolutionPreset {
    HD720p = 0,
    FHD1080p = 1,
    QHD1440p = 2,
    UHD4K = 3,
    Square1080 = 4,
    Portrait720 = 5,
    Portrait1080 = 6,
    Portrait1440 = 7,
    Portrait4K = 8,
    Square4K = 9,
}

impl ResolutionPreset {
    pub const ALL: &[Self] = &[
        Self::HD720p,
        Self::FHD1080p,
        Self::QHD1440p,
        Self::UHD4K,
        Self::Square1080,
        Self::Portrait720,
        Self::Portrait1080,
        Self::Portrait1440,
        Self::Portrait4K,
        Self::Square4K,
    ];

    pub fn dimensions(&self) -> (i32, i32) {
        match self {
            ResolutionPreset::HD720p => (1280, 720),
            ResolutionPreset::FHD1080p => (1920, 1080),
            ResolutionPreset::QHD1440p => (2560, 1440),
            ResolutionPreset::UHD4K => (3840, 2160),
            ResolutionPreset::Square1080 => (1080, 1080),
            ResolutionPreset::Portrait720 => (720, 1280),
            ResolutionPreset::Portrait1080 => (1080, 1920),
            ResolutionPreset::Portrait1440 => (1440, 2560),
            ResolutionPreset::Portrait4K => (2160, 3840),
            ResolutionPreset::Square4K => (4000, 4000),
        }
    }

    /// Short label for footer display. Matches Unity ProjectSettings.GetResolutionLabel().
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::HD720p => "720p",
            Self::FHD1080p => "1080p",
            Self::QHD1440p => "1440p",
            Self::UHD4K => "4K",
            Self::Square1080 => "1080sq",
            Self::Portrait720 => "720v",
            Self::Portrait1080 => "1080v",
            Self::Portrait1440 => "1440v",
            Self::Portrait4K => "4Kv",
            Self::Square4K => "4Ksq",
        }
    }

    /// Dropdown label with dimensions. Matches Unity: "{label}  ({w}x{h})".
    pub fn dropdown_label(&self) -> String {
        let (w, h) = self.dimensions();
        format!("{}  ({}x{})", self.display_name(), w, h)
    }

    /// The preset with exactly this size, if any.
    pub fn matching(width: i32, height: i32) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|p| p.dimensions() == (width, height))
    }
}

// ─── Playback State ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PlaybackState {
    #[default]
    Stopped,
    Playing,
    Paused,
}

// ─── Tempo Point Source ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TempoPointSource {
    #[default]
    Unknown = 0,
    Manual = 1,
    Link = 2,
    MidiClock = 3,
    Recorded = 4,
    AbletonOsc = 5,
    Audio = 6,
}

// ─── Beat Division (for parameter drivers / LFO) ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum BeatDivision {
    ThirtySecond = 0,
    Sixteenth = 1,
    Eighth = 2,
    #[default]
    Quarter = 3,
    Half = 4,
    Whole = 5,
    TwoWhole = 6,
    FourWhole = 7,
    EightWhole = 8,
    SixteenWhole = 9,
    ThirtyTwoWhole = 10,
    EighthDotted = 11,
    QuarterDotted = 12,
    HalfDotted = 13,
    WholeDotted = 14,
    TwoWholeDotted = 15,
    EighthTriplet = 16,
    QuarterTriplet = 17,
    HalfTriplet = 18,
    WholeTriplet = 19,
}

impl BeatDivision {
    /// Duration in beats for this division.
    pub fn beats(&self) -> f32 {
        match self {
            BeatDivision::ThirtySecond => 0.125,
            BeatDivision::Sixteenth => 0.25,
            BeatDivision::Eighth => 0.5,
            BeatDivision::Quarter => 1.0,
            BeatDivision::Half => 2.0,
            BeatDivision::Whole => 4.0,
            BeatDivision::TwoWhole => 8.0,
            BeatDivision::FourWhole => 16.0,
            BeatDivision::EightWhole => 32.0,
            BeatDivision::SixteenWhole => 64.0,
            BeatDivision::ThirtyTwoWhole => 128.0,
            BeatDivision::EighthDotted => 0.75,
            BeatDivision::QuarterDotted => 1.5,
            BeatDivision::HalfDotted => 3.0,
            BeatDivision::WholeDotted => 6.0,
            BeatDivision::TwoWholeDotted => 12.0,
            BeatDivision::EighthTriplet => 1.0 / 3.0,
            BeatDivision::QuarterTriplet => 2.0 / 3.0,
            BeatDivision::HalfTriplet => 4.0 / 3.0,
            BeatDivision::WholeTriplet => 8.0 / 3.0,
        }
    }

    /// Map UI button index (0..=10) to a base BeatDivision.
    /// Button labels: "1/16", "1/8", "1/4", "1/2", "1", "2", "4", "8", "16", "32", "64"
    pub fn from_button_index(idx: usize) -> Option<Self> {
        const MAP: [BeatDivision; 11] = [
            BeatDivision::ThirtySecond,
            BeatDivision::Sixteenth,
            BeatDivision::Eighth,
            BeatDivision::Quarter,
            BeatDivision::Half,
            BeatDivision::Whole,
            BeatDivision::TwoWhole,
            BeatDivision::FourWhole,
            BeatDivision::EightWhole,
            BeatDivision::SixteenWhole,
            BeatDivision::ThirtyTwoWhole,
        ];
        MAP.get(idx).copied()
    }

    /// Strip dotted/triplet modifier, returning the base division.
    pub fn base_division(self) -> Self {
        match self {
            Self::EighthDotted | Self::EighthTriplet => Self::Eighth,
            Self::QuarterDotted | Self::QuarterTriplet => Self::Quarter,
            Self::HalfDotted | Self::HalfTriplet => Self::Half,
            Self::WholeDotted | Self::WholeTriplet => Self::Whole,
            Self::TwoWholeDotted => Self::TwoWhole,
            other => other,
        }
    }

    pub fn is_dotted(self) -> bool {
        matches!(
            self,
            Self::EighthDotted
                | Self::QuarterDotted
                | Self::HalfDotted
                | Self::WholeDotted
                | Self::TwoWholeDotted
        )
    }

    pub fn is_triplet(self) -> bool {
        matches!(
            self,
            Self::EighthTriplet | Self::QuarterTriplet | Self::HalfTriplet | Self::WholeTriplet
        )
    }

    /// Toggle dotted modifier. Returns None if no dotted variant exists for this base.
    pub fn toggle_dotted(self) -> Option<Self> {
        if self.is_dotted() {
            return Some(self.base_division());
        }
        let base = self.base_division();
        match base {
            Self::Eighth => Some(Self::EighthDotted),
            Self::Quarter => Some(Self::QuarterDotted),
            Self::Half => Some(Self::HalfDotted),
            Self::Whole => Some(Self::WholeDotted),
            Self::TwoWhole => Some(Self::TwoWholeDotted),
            _ => None,
        }
    }

    /// Toggle triplet modifier. Returns None if no triplet variant exists for this base.
    pub fn toggle_triplet(self) -> Option<Self> {
        if self.is_triplet() {
            return Some(self.base_division());
        }
        let base = self.base_division();
        match base {
            Self::Eighth => Some(Self::EighthTriplet),
            Self::Quarter => Some(Self::QuarterTriplet),
            Self::Half => Some(Self::HalfTriplet),
            Self::Whole => Some(Self::WholeTriplet),
            _ => None,
        }
    }
}

// ─── Driver Waveform ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum DriverWaveform {
    #[default]
    Sine = 0,
    Triangle = 1,
    Sawtooth = 2,
    Square = 3,
    Random = 4,
}

impl DriverWaveform {
    pub fn from_index(idx: usize) -> Option<Self> {
        const ALL: [DriverWaveform; 5] = [
            DriverWaveform::Sine,
            DriverWaveform::Triangle,
            DriverWaveform::Sawtooth,
            DriverWaveform::Square,
            DriverWaveform::Random,
        ];
        ALL.get(idx).copied()
    }
}

// ─── Clip Duration Mode ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ClipDurationMode {
    #[default]
    NoteOff = 2,
}

// ─── MIDI Trigger Mode ───

/// How a layer responds to incoming MIDI notes.
/// `SingleNote`: only `layer.midi_note` triggers a clip.
/// `AllNotes`: every NoteOn on the selected device fires a clip (drum-pad style).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MidiTriggerMode {
    #[default]
    SingleNote,
    AllNotes,
}

// ─── Display impls ───

impl std::fmt::Display for BlendMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

impl std::fmt::Display for PlaybackState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => f.write_str("Stopped"),
            Self::Playing => f.write_str("Playing"),
            Self::Paused => f.write_str("Paused"),
        }
    }
}

// ─── TryFrom<i32> impls ───

impl TryFrom<i32> for BlendMode {
    type Error = ();

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Normal),
            1 => Ok(Self::Additive),
            2 => Ok(Self::Multiply),
            3 => Ok(Self::Screen),
            4 => Ok(Self::Overlay),
            5 => Ok(Self::Stencil),
            6 => Ok(Self::Opaque),
            7 => Ok(Self::Difference),
            8 => Ok(Self::Exclusion),
            9 => Ok(Self::Subtract),
            10 => Ok(Self::ColorDodge),
            11 => Ok(Self::Lighten),
            12 => Ok(Self::Darken),
            _ => Err(()),
        }
    }
}

// ─── Marker Color ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum MarkerColor {
    Red = 0,
    Orange = 1,
    Yellow = 2,
    Green = 3,
    Cyan = 4,
    Blue = 5,
    #[default]
    Purple = 6,
    White = 7,
}

impl MarkerColor {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Red => "Red",
            Self::Orange => "Orange",
            Self::Yellow => "Yellow",
            Self::Green => "Green",
            Self::Cyan => "Cyan",
            Self::Blue => "Blue",
            Self::Purple => "Purple",
            Self::White => "White",
        }
    }

    pub const ALL: &'static [MarkerColor] = &[
        MarkerColor::Red,
        MarkerColor::Orange,
        MarkerColor::Yellow,
        MarkerColor::Green,
        MarkerColor::Cyan,
        MarkerColor::Blue,
        MarkerColor::Purple,
        MarkerColor::White,
    ];

    pub fn from_index(i: usize) -> Self {
        Self::try_from(i as i32).unwrap_or(Self::Red)
    }
}

impl std::fmt::Display for MarkerColor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

impl TryFrom<i32> for MarkerColor {
    type Error = ();

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Red),
            1 => Ok(Self::Orange),
            2 => Ok(Self::Yellow),
            3 => Ok(Self::Green),
            4 => Ok(Self::Cyan),
            5 => Ok(Self::Blue),
            6 => Ok(Self::Purple),
            7 => Ok(Self::White),
            _ => Err(()),
        }
    }
}

// ─── Tonemap Curve ───

/// Tonemapping curve selected for final SDR presentation after master effects.
/// Application is controlled by `ProjectSettings::tonemap_enabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TonemapCurve {
    /// ACES Narkowicz 2015 fit — simple, warm highlight rolloff. Unity parity.
    #[default]
    AcesNarkowicz = 0,
    /// ACES Stephen Hill fit — proper AP1 color space, RRT+ODT desaturation.
    /// More accurate than Narkowicz, better highlight hue preservation.
    AcesHill = 1,
    /// AgX (Troy Sobotka) — designed for saturated/generative content.
    /// Minimal hue shift on neons, graceful highlight desaturation.
    Agx = 2,
    /// Khronos PBR Neutral — minimal hue shift, low saturation loss in highlights.
    /// Designed for cross-display consistency (glTF standard tonemapper).
    KhronosPbrNeutral = 3,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_only_accept_clips_of_their_own_kind() {
        use LayerType::*;
        assert!(Video.accepts_clips_from(Video));
        assert!(Audio.accepts_clips_from(Audio));
        // Generator and DMX layers both host a generator; clips move freely.
        assert!(Dmx.accepts_clips_from(Generator));
        assert!(Generator.accepts_clips_from(Dmx));
        for (to, from) in [
            (Audio, Video),
            (Video, Audio),
            (Video, Dmx),
            (Dmx, Video),
            (Generator, Video),
            (Group, Video),
            (Group, Group),
            (Video, Group),
        ] {
            assert!(
                !to.accepts_clips_from(from),
                "{to:?} must refuse clips from {from:?}"
            );
        }
    }

    /// Saved as the variant name. A bare number, the retired "Led" alias or an
    /// unknown name is a load error.
    #[test]
    fn layer_type_dmx_wire_form_round_trip() {
        assert_eq!(serde_json::to_string(&LayerType::Dmx).unwrap(), "\"Dmx\"");
        assert_eq!(
            serde_json::from_str::<LayerType>("\"Dmx\"").unwrap(),
            LayerType::Dmx
        );
        assert!(serde_json::from_str::<LayerType>("4").is_err());
        assert!(serde_json::from_str::<LayerType>("\"Led\"").is_err());
        assert!(serde_json::from_str::<LayerType>("\"Nonsense\"").is_err());
    }

    #[test]
    fn layer_type_names_and_trigger_capabilities() {
        use LayerType::*;

        for (name, kind) in [
            ("Video", Video),
            ("Generator", Generator),
            ("Group", Group),
            ("Audio", Audio),
            ("Dmx", Dmx),
            ("Trigger", Trigger),
        ] {
            let wire = format!("\"{name}\"");
            assert_eq!(serde_json::to_string(&kind).unwrap(), wire);
            assert_eq!(serde_json::from_str::<LayerType>(&wire).unwrap(), kind);
        }

        assert!(Trigger.accepts_clips_from(Trigger));
        assert!(!Trigger.accepts_clips_from(Video));
        assert!(!Video.accepts_clips_from(Trigger));
        assert!(!Group.supports_clip_playback());
        assert!(!Trigger.supports_clip_playback());
        assert!(Video.supports_clip_playback());
        assert!(Generator.supports_clip_playback());
        assert!(Audio.supports_clip_playback());
        assert!(Dmx.supports_clip_playback());
        assert!(Video.supports_clip_thumbnails());
        assert!(Generator.supports_clip_thumbnails());
        assert!(Dmx.supports_clip_thumbnails());
        assert!(!Group.supports_clip_thumbnails());
        assert!(!Audio.supports_clip_thumbnails());
        assert!(!Trigger.supports_clip_thumbnails());
    }
}
