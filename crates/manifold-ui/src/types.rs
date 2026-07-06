//! UI-local mirrors of engine domain enums and small data structs.
//!
//! Phase 5 layering inversion: `manifold-ui` no longer depends on
//! `manifold-core`. The genuinely-shared *primitives* (ids, `Beats`, `ParamId`)
//! live in `manifold-foundation` and are shared verbatim. The types here are
//! the *domain semantics* the UI needs to render and to name in its events —
//! they are mirrored, and `manifold-app` translates them to/from the engine at
//! the one boundary (`ui_translate.rs`). See `docs/UI_LAYERING_INVERSION.md`.
//!
//! These mirrors are kept faithful to the core definitions (same variants, same
//! `index()`/`label()` results) so the round-trip through the app translation
//! layer is lossless. They carry no serde — UI events are never serialized.

/// What a layer renders. Mirror of `manifold_core::types::LayerType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayerType {
    #[default]
    Video,
    Generator,
    Group,
    /// An audio track: no visual output (the compositor skips it).
    Audio,
}

/// How a MIDI-triggered layer interprets incoming notes.
/// Mirror of `manifold_core::types::MidiTriggerMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MidiTriggerMode {
    #[default]
    SingleNote,
    AllNotes,
}

/// HDR output tonemapping curve. Mirror of `manifold_core::TonemapCurve`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TonemapCurve {
    #[default]
    AcesNarkowicz,
    AcesHill,
    Agx,
    KhronosPbrNeutral,
}

/// LFO driver waveform shape. Mirror of `manifold_core::DriverWaveform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DriverWaveform {
    #[default]
    Sine,
    Triangle,
    Sawtooth,
    Square,
    Random,
}

/// Timeline marker swatch color. Mirror of `manifold_core::MarkerColor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MarkerColor {
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    #[default]
    Purple,
    White,
}

/// Card-slider response curve for a mapped parameter.
/// Mirror of `manifold_core::macro_bank::MacroCurve`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MacroCurve {
    #[default]
    Linear,
    Exponential,
    Logarithmic,
    SCurve,
}

impl MacroCurve {
    /// Map a normalized 0–1 input through this curve, returning 0–1. Value-exact
    /// mirror of `manifold_core::macro_bank::MacroCurve::apply` — the mapping
    /// popover plots the response curve UI-side and must match the engine.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::Exponential => t * t,
            Self::Logarithmic => t.sqrt(),
            // Hermite S-curve: 3t² - 2t³
            Self::SCurve => t * t * (3.0 - 2.0 * t),
        }
    }
}

/// Apply the card-binding reshape pipeline to a raw value. Value-exact mirror of
/// `manifold_core::effects::apply_card_reshape`, kept UI-side so the mapping
/// popover's live response preview evaluates the same transform the engine does
/// at the write boundary (the popover renders in `manifold-ui` and can't reach
/// into `manifold-core`).
///
/// Two stages: a slider response (normalize within `[min, max]` → invert → curve
/// → scale back, clamped to `[0, 1]`), applied only when `invert` or a non-Linear
/// `curve` is set; then the card→consumer affine `out = v * scale + offset`
/// (unclamped). Identity inputs return `value` unchanged.
pub fn apply_card_reshape(
    value: f32,
    min: f32,
    max: f32,
    invert: bool,
    curve: MacroCurve,
    scale: f32,
    offset: f32,
) -> f32 {
    let mut v = value;
    if invert || curve != MacroCurve::Linear {
        let range = max - min;
        if range.abs() >= f32::EPSILON {
            let mut n = ((v - min) / range).clamp(0.0, 1.0);
            if invert {
                n = 1.0 - n;
            }
            n = curve.apply(n);
            v = min + range * n;
        }
    }
    v * scale + offset
}

/// Number of macro slots in the fixed bank. Mirror of
/// `manifold_core::macro_bank::MACRO_COUNT`.
pub const MACRO_COUNT: usize = 8;

/// The "off" sentinel for a per-send noise floor, in dB. Mirror of
/// `manifold_core::audio_setup::FLOOR_DB_OFF`.
pub const FLOOR_DB_OFF: f32 = -120.0;

/// A frequency band of an audio send. Mirror of
/// `manifold_core::audio_mod::AudioBand`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioBand {
    #[default]
    Full,
    Low,
    Mid,
    High,
}

impl AudioBand {
    /// All bands in feature-storage order.
    pub const ALL: [AudioBand; 4] = [AudioBand::Full, AudioBand::Low, AudioBand::Mid, AudioBand::High];

    /// Index into a send's per-band feature array.
    pub fn index(self) -> usize {
        match self {
            AudioBand::Full => 0,
            AudioBand::Low => 1,
            AudioBand::Mid => 2,
            AudioBand::High => 3,
        }
    }

    /// User-facing label.
    pub fn label(self) -> &'static str {
        match self {
            AudioBand::Full => "Full",
            AudioBand::Low => "Low",
            AudioBand::Mid => "Mid",
            AudioBand::High => "High",
        }
    }
}

/// Which scalar feature of an audio band drives modulation. Mirror of
/// `manifold_core::audio_mod::AudioFeatureKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioFeatureKind {
    #[default]
    Amplitude,
    Centroid,
    Noisiness,
    Flux,
    Transients,
    /// Tracked pitch of the band's dominant object (P4). Holds on dropout;
    /// gate with Presence.
    Pitch,
    /// Confidence the tracked pitch is a real object, 0..1 (P4).
    Presence,
}

impl AudioFeatureKind {
    /// All kinds in drawer-button order.
    pub const ALL: [AudioFeatureKind; 7] = [
        AudioFeatureKind::Amplitude,
        AudioFeatureKind::Centroid,
        AudioFeatureKind::Noisiness,
        AudioFeatureKind::Flux,
        AudioFeatureKind::Transients,
        AudioFeatureKind::Pitch,
        AudioFeatureKind::Presence,
    ];

    /// Position in `ALL`.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&k| k == self).unwrap_or(0)
    }

    /// User-facing label.
    pub fn label(self) -> &'static str {
        match self {
            AudioFeatureKind::Amplitude => "Amplitude",
            AudioFeatureKind::Centroid => "Centroid",
            AudioFeatureKind::Noisiness => "Noisiness",
            AudioFeatureKind::Flux => "Flux",
            AudioFeatureKind::Transients => "Transients",
            AudioFeatureKind::Pitch => "Pitch",
            AudioFeatureKind::Presence => "Presence",
        }
    }
}

/// A feature-on-a-band selection. Mirror of `manifold_core::audio_mod::AudioFeature`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioFeature {
    pub kind: AudioFeatureKind,
    pub band: AudioBand,
}

impl AudioFeature {
    pub fn new(kind: AudioFeatureKind, band: AudioBand) -> Self {
        Self { kind, band }
    }
}

/// Which kind of source an audio device reference points at. Mirror of
/// `manifold_core::audio_setup::AudioSourceKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioSourceKind {
    #[default]
    InputDevice,
    SystemAudio,
    App,
}

/// A reference to an audio capture source. Mirror of
/// `manifold_core::AudioDeviceRef`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioDeviceRef {
    /// Stable identity (device UID or app bundle id). Empty for system audio.
    pub uid: String,
    /// Display name + fallback match key.
    pub name: String,
    /// Which kind of source this points at.
    pub kind: AudioSourceKind,
}

impl AudioDeviceRef {
    /// An input-device reference by UID + display name.
    pub fn new(uid: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            name: name.into(),
            kind: AudioSourceKind::InputDevice,
        }
    }

    /// The system-audio tap source (whole-system output mix).
    pub fn system_audio() -> Self {
        Self {
            uid: String::new(),
            name: "System Audio".to_string(),
            kind: AudioSourceKind::SystemAudio,
        }
    }

    /// An application-audio tap source, keyed by stable bundle id + display name.
    pub fn app(bundle_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uid: bundle_id.into(),
            name: name.into(),
            kind: AudioSourceKind::App,
        }
    }

    /// The UID for resolution, or `None` if this is a legacy name-only ref or a
    /// kind that carries no UID (system audio).
    pub fn uid_opt(&self) -> Option<&str> {
        (!self.uid.is_empty()).then_some(self.uid.as_str())
    }

    /// Whether this ref points at a tap source (system or app) rather than a
    /// hardware input device.
    pub fn is_tap(&self) -> bool {
        !matches!(self.kind, AudioSourceKind::InputDevice)
    }
}

/// Conversion applied to a card slider's value before the node sees it.
/// Mirror of `manifold_core::effects::ParamConvert`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParamConvert {
    #[default]
    Float,
    IntRound,
    BoolThreshold,
    EnumRound,
    Trigger,
}

/// A typed parameter value carried in a graph edit. Mirror of
/// `manifold_core::effect_graph_def::SerializedParamValue`.
#[derive(Debug, Clone, PartialEq)]
pub enum SerializedParamValue {
    Float { value: f32 },
    Int { value: i32 },
    Bool { value: bool },
    Vec2 { value: [f32; 2] },
    Vec3 { value: [f32; 3] },
    Vec4 { value: [f32; 4] },
    Color { value: [f32; 4] },
    Enum { value: u32 },
    Table { rows: Vec<Vec<f32>> },
}

/// Stable id of a registered effect / generator preset type. Mirror of
/// `manifold_core::PresetTypeId` — the UI keeps the string newtype but not the
/// registry-querying methods (those stay in the engine).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct PresetTypeId(String);

impl PresetTypeId {
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Structural identity of an Ableton rack device. Mirror of
/// `manifold_core::ableton_mapping::AbletonDeviceIdentity`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AbletonDeviceIdentity {
    pub device_class_name: String,
}

/// Full address of a single Ableton rack macro parameter. Mirror of
/// `manifold_core::ableton_mapping::AbletonMacroAddress`.
#[derive(Debug, Clone, Default)]
pub struct AbletonMacroAddress {
    pub track_id: i32,
    pub device_id: i32,
    pub param_id: i32,
    pub device_identity: AbletonDeviceIdentity,
    pub track_name: String,
    pub device_name: String,
    pub macro_name: String,
}

/// Runtime status of an Ableton mapping. Mirror of
/// `manifold_core::ableton_mapping::AbletonMappingStatus`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AbletonMappingStatus {
    #[default]
    Dormant,
    Active,
    Ambiguous,
}

/// True if `name` is a default, un-renamed Ableton macro name ("Macro 1".."Macro 8").
/// Mirror of `manifold_core::ableton_mapping::is_default_macro_name`.
pub fn is_default_macro_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("Macro ")
        && let Ok(n) = rest.parse::<u32>()
    {
        return (1..=8).contains(&n);
    }
    false
}

/// Format a MIDI note number as a name like "C3". Mirror of
/// `manifold_core::midi::note_number_to_name`.
pub fn note_number_to_name(note: i32) -> String {
    if note < 0 {
        return "None".into();
    }
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let octave = (note / 12) - 2;
    let name = NAMES[(note % 12) as usize];
    format!("{name}{octave}")
}
