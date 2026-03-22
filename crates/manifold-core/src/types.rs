use serde::{Deserialize, Serialize};
use serde::de::Deserializer;
use serde::ser::Serializer;

// ─── Blend Modes ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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
        BlendMode::Normal, BlendMode::Additive, BlendMode::Multiply,
        BlendMode::Screen, BlendMode::Overlay, BlendMode::Stencil,
        BlendMode::Opaque, BlendMode::Difference, BlendMode::Exclusion,
        BlendMode::Subtract, BlendMode::ColorDodge, BlendMode::Lighten,
        BlendMode::Darken,
    ];

    pub fn from_index(i: usize) -> Self {
        Self::try_from(i as i32).unwrap_or(Self::Normal)
    }
}

impl Serialize for BlendMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for BlendMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::Number(n) => match n.as_i64().unwrap_or(0) as i32 {
                0 => BlendMode::Normal,
                1 => BlendMode::Additive,
                2 => BlendMode::Multiply,
                3 => BlendMode::Screen,
                4 => BlendMode::Overlay,
                5 => BlendMode::Stencil,
                6 => BlendMode::Opaque,
                7 => BlendMode::Difference,
                8 => BlendMode::Exclusion,
                9 => BlendMode::Subtract,
                10 => BlendMode::ColorDodge,
                11 => BlendMode::Lighten,
                12 => BlendMode::Darken,
                _ => BlendMode::Normal,
            },
            serde_json::Value::String(s) => match s.as_str() {
                "Normal" => BlendMode::Normal,
                "Additive" => BlendMode::Additive,
                "Multiply" => BlendMode::Multiply,
                "Screen" => BlendMode::Screen,
                "Overlay" => BlendMode::Overlay,
                "Stencil" => BlendMode::Stencil,
                "Opaque" => BlendMode::Opaque,
                "Difference" => BlendMode::Difference,
                "Exclusion" => BlendMode::Exclusion,
                "Subtract" => BlendMode::Subtract,
                "ColorDodge" => BlendMode::ColorDodge,
                "Lighten" => BlendMode::Lighten,
                "Darken" => BlendMode::Darken,
                _ => BlendMode::Normal,
            },
            _ => BlendMode::Normal,
        })
    }
}

// ─── Effect Types ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum EffectType {
    #[default]
    Transform = 0,
    InvertColors = 1,
    Feedback = 10,
    PixelSort = 11,
    Bloom = 12,
    InfiniteZoom = 13,
    Kaleidoscope = 14,
    EdgeStretch = 15,
    VoronoiPrism = 16,
    QuadMirror = 17,
    Dither = 18,
    Strobe = 19,
    StylizedFeedback = 20,
    Mirror = 21,
    BlobTracking = 22,
    CRT = 23,
    FluidDistortion = 24,
    EdgeGlow = 25,
    Datamosh = 26,
    SlitScan = 27,
    ColorGrade = 28,
    WireframeDepth = 29,
    ChromaticAberration = 30,
    GradientMap = 31,
    Glitch = 32,
    FilmGrain = 33,
    Halation = 34,
    Microscope = 35,
    Corruption = 36,
    Infrared = 37,
    Surveillance = 38,
    Redaction = 39,
}

impl EffectType {
    /// Human-readable name for UI display.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Transform => "Transform",
            Self::InvertColors => "Invert Colors",
            Self::Feedback => "Feedback",
            Self::PixelSort => "Pixel Sort",
            Self::Bloom => "Bloom",
            Self::InfiniteZoom => "Infinite Zoom",
            Self::Kaleidoscope => "Kaleidoscope",
            Self::EdgeStretch => "Edge Stretch",
            Self::VoronoiPrism => "Voronoi Prism",
            Self::QuadMirror => "Quad Mirror",
            Self::Dither => "Dither",
            Self::Strobe => "Strobe",
            Self::StylizedFeedback => "Stylized Feedback",
            Self::Mirror => "Mirror",
            Self::BlobTracking => "Blob Tracking",
            Self::CRT => "CRT",
            Self::FluidDistortion => "Fluid Distortion",
            Self::EdgeGlow => "Edge Glow",
            Self::Datamosh => "Datamosh",
            Self::SlitScan => "Slit Scan",
            Self::ColorGrade => "Color Grade",
            Self::WireframeDepth => "Wireframe Depth",
            Self::ChromaticAberration => "Chromatic Aberration",
            Self::GradientMap => "Gradient Map",
            Self::Glitch => "Glitch",
            Self::FilmGrain => "Film Grain",
            Self::Halation => "Halation",
            Self::Microscope => "Microscope",
            Self::Corruption => "Corruption",
            Self::Infrared => "Infrared",
            Self::Surveillance => "Surveillance",
            Self::Redaction => "Redaction",
        }
    }

    /// All effect types available for the "Add Effect" dropdown.
    pub const ALL: &'static [EffectType] = &[
        EffectType::InvertColors, EffectType::ColorGrade, EffectType::Mirror,
        EffectType::Feedback, EffectType::Bloom, EffectType::Transform,
        EffectType::Kaleidoscope, EffectType::EdgeStretch, EffectType::PixelSort,
        EffectType::Strobe, EffectType::ChromaticAberration, EffectType::FilmGrain,
        EffectType::Glitch, EffectType::Dither, EffectType::CRT,
        EffectType::Halation, EffectType::VoronoiPrism,
        EffectType::QuadMirror, EffectType::StylizedFeedback, EffectType::BlobTracking,
        EffectType::EdgeGlow,
        EffectType::WireframeDepth,
        EffectType::Infrared,
    ];

    pub fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }

    /// Convert from the integer discriminant value (matches Unity enum values).
    /// Used by the browser popup which stores `et as i32` as the item key.
    pub fn from_discriminant(v: i32) -> Option<Self> {
        Self::try_from(v).ok()
    }
}

impl Serialize for EffectType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for EffectType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::Number(n) => match n.as_i64().unwrap_or(0) as i32 {
                0 => EffectType::Transform,
                1 => EffectType::InvertColors,
                10 => EffectType::Feedback,
                11 => EffectType::PixelSort,
                12 => EffectType::Bloom,
                13 => EffectType::InfiniteZoom,
                14 => EffectType::Kaleidoscope,
                15 => EffectType::EdgeStretch,
                16 => EffectType::VoronoiPrism,
                17 => EffectType::QuadMirror,
                18 => EffectType::Dither,
                19 => EffectType::Strobe,
                20 => EffectType::StylizedFeedback,
                21 => EffectType::Mirror,
                22 => EffectType::BlobTracking,
                23 => EffectType::CRT,
                24 => EffectType::FluidDistortion,
                25 => EffectType::EdgeGlow,
                26 => EffectType::Datamosh,
                27 => EffectType::SlitScan,
                28 => EffectType::ColorGrade,
                29 => EffectType::WireframeDepth,
                30 => EffectType::ChromaticAberration,
                31 => EffectType::GradientMap,
                32 => EffectType::Glitch,
                33 => EffectType::FilmGrain,
                34 => EffectType::Halation,
                35 => EffectType::Microscope,
                36 => EffectType::Corruption,
                37 => EffectType::Infrared,
                38 => EffectType::Surveillance,
                39 => EffectType::Redaction,
                _ => EffectType::Transform,
            },
            serde_json::Value::String(s) => match s.as_str() {
                "Transform" => EffectType::Transform,
                "InvertColors" => EffectType::InvertColors,
                "Feedback" => EffectType::Feedback,
                "PixelSort" => EffectType::PixelSort,
                "Bloom" => EffectType::Bloom,
                "InfiniteZoom" => EffectType::InfiniteZoom,
                "Kaleidoscope" => EffectType::Kaleidoscope,
                "EdgeStretch" => EffectType::EdgeStretch,
                "VoronoiPrism" => EffectType::VoronoiPrism,
                "QuadMirror" => EffectType::QuadMirror,
                "Dither" => EffectType::Dither,
                "Strobe" => EffectType::Strobe,
                "StylizedFeedback" => EffectType::StylizedFeedback,
                "Mirror" => EffectType::Mirror,
                "BlobTracking" => EffectType::BlobTracking,
                "CRT" => EffectType::CRT,
                "FluidDistortion" => EffectType::FluidDistortion,
                "EdgeGlow" => EffectType::EdgeGlow,
                "Datamosh" => EffectType::Datamosh,
                "SlitScan" => EffectType::SlitScan,
                "ColorGrade" => EffectType::ColorGrade,
                "WireframeDepth" => EffectType::WireframeDepth,
                "ChromaticAberration" => EffectType::ChromaticAberration,
                "GradientMap" => EffectType::GradientMap,
                "Glitch" => EffectType::Glitch,
                "FilmGrain" => EffectType::FilmGrain,
                "Halation" => EffectType::Halation,
                "Microscope" => EffectType::Microscope,
                "Corruption" => EffectType::Corruption,
                "Infrared" => EffectType::Infrared,
                "Surveillance" => EffectType::Surveillance,
                "Redaction" => EffectType::Redaction,
                _ => EffectType::Transform,
            },
            _ => EffectType::Transform,
        })
    }
}

// ─── Generator Types ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum GeneratorType {
    #[default]
    None = 0,
    BasicShapesSnap = 2,
    Duocylinder = 3,
    Tesseract = 4,
    ConcentricTunnel = 5,
    Plasma = 6,
    Lissajous = 7,
    FractalZoom = 8,
    OscilloscopeXY = 9,
    WireframeZoo = 10,
    ReactionDiffusion = 11,
    Flowfield = 12,
    ParametricSurface = 13,
    StrangeAttractor = 14,
    FluidSimulation = 15,
    NumberStation = 16,
    Mycelium = 17,
    ComputeStrangeAttractor = 18,
    FluidSimulation3D = 19,
    MriVolume = 20,
}

impl GeneratorType {
    /// Human-readable name for UI display.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::None => "None",
            Self::BasicShapesSnap => "Basic Shapes",
            Self::Duocylinder => "Duocylinder",
            Self::Tesseract => "Tesseract",
            Self::ConcentricTunnel => "Concentric Tunnel",
            Self::Plasma => "Plasma",
            Self::Lissajous => "Lissajous",
            Self::FractalZoom => "Fractal Zoom",
            Self::OscilloscopeXY => "Oscilloscope XY",
            Self::WireframeZoo => "Wireframe Zoo",
            Self::ReactionDiffusion => "Reaction Diffusion",
            Self::Flowfield => "Flowfield",
            Self::ParametricSurface => "Parametric Surface",
            Self::StrangeAttractor => "Strange Attractor",
            Self::FluidSimulation => "Fluid Simulation",
            Self::NumberStation => "Number Station",
            Self::Mycelium => "Mycelium",
            Self::ComputeStrangeAttractor => "Compute Attractor",
            Self::FluidSimulation3D => "Fluid Sim 3D",
            Self::MriVolume => "MRI Volume",
        }
    }

    pub const ALL: &[Self] = &[
        Self::Plasma, Self::ConcentricTunnel, Self::Lissajous,
        Self::FractalZoom, Self::Flowfield, Self::ReactionDiffusion,
        Self::FluidSimulation, Self::FluidSimulation3D,
        Self::BasicShapesSnap, Self::Duocylinder, Self::Tesseract,
        Self::OscilloscopeXY, Self::WireframeZoo, Self::ParametricSurface,
        Self::StrangeAttractor, Self::ComputeStrangeAttractor,
        Self::NumberStation, Self::Mycelium,
        Self::MriVolume,
    ];

    pub fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }
}

// GeneratorType serializes as integer in JSON
impl Serialize for GeneratorType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for GeneratorType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::Number(n) => match n.as_i64().unwrap_or(0) as i32 {
                0 => GeneratorType::None,
                2 => GeneratorType::BasicShapesSnap,
                3 => GeneratorType::Duocylinder,
                4 => GeneratorType::Tesseract,
                5 => GeneratorType::ConcentricTunnel,
                6 => GeneratorType::Plasma,
                7 => GeneratorType::Lissajous,
                8 => GeneratorType::FractalZoom,
                9 => GeneratorType::OscilloscopeXY,
                10 => GeneratorType::WireframeZoo,
                11 => GeneratorType::ReactionDiffusion,
                12 => GeneratorType::Flowfield,
                13 => GeneratorType::ParametricSurface,
                14 => GeneratorType::StrangeAttractor,
                15 => GeneratorType::FluidSimulation,
                16 => GeneratorType::NumberStation,
                17 => GeneratorType::Mycelium,
                18 => GeneratorType::ComputeStrangeAttractor,
                19 => GeneratorType::FluidSimulation3D,
                20 => GeneratorType::MriVolume,
                _ => GeneratorType::None,
            },
            serde_json::Value::String(s) => match s.as_str() {
                "None" => GeneratorType::None,
                "BasicShapesSnap" => GeneratorType::BasicShapesSnap,
                "Duocylinder" => GeneratorType::Duocylinder,
                "Tesseract" => GeneratorType::Tesseract,
                "ConcentricTunnel" => GeneratorType::ConcentricTunnel,
                "Plasma" => GeneratorType::Plasma,
                "Lissajous" => GeneratorType::Lissajous,
                "FractalZoom" => GeneratorType::FractalZoom,
                "OscilloscopeXY" => GeneratorType::OscilloscopeXY,
                "WireframeZoo" => GeneratorType::WireframeZoo,
                "ReactionDiffusion" => GeneratorType::ReactionDiffusion,
                "Flowfield" => GeneratorType::Flowfield,
                "ParametricSurface" => GeneratorType::ParametricSurface,
                "StrangeAttractor" => GeneratorType::StrangeAttractor,
                "FluidSimulation" => GeneratorType::FluidSimulation,
                "NumberStation" => GeneratorType::NumberStation,
                "Mycelium" => GeneratorType::Mycelium,
                "ComputeStrangeAttractor" => GeneratorType::ComputeStrangeAttractor,
                "FluidSimulation3D" => GeneratorType::FluidSimulation3D,
                "MriVolume" => GeneratorType::MriVolume,
                _ => GeneratorType::None,
            },
            _ => GeneratorType::None,
        })
    }
}

// ─── Layer Type ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LayerType {
    #[default]
    Video = 0,
    Generator = 1,
    Group = 2,
}

impl Serialize for LayerType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for LayerType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::Number(n) => match n.as_i64().unwrap_or(0) as i32 {
                0 => LayerType::Video,
                1 => LayerType::Generator,
                2 => LayerType::Group,
                _ => LayerType::Video,
            },
            serde_json::Value::String(s) => match s.as_str() {
                "Video" => LayerType::Video,
                "Generator" => LayerType::Generator,
                "Group" => LayerType::Group,
                _ => LayerType::Video,
            },
            _ => LayerType::Video,
        })
    }
}

// ─── Clock Authority ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ClockAuthority {
    #[default]
    Internal = 0,
    Link = 1,
    MidiClock = 2,
    Osc = 3,
}

impl ClockAuthority {
    pub fn next(&self) -> Self {
        match self {
            Self::Internal => Self::Link,
            Self::Link => Self::MidiClock,
            Self::MidiClock => Self::Osc,
            Self::Osc => Self::Internal,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Internal => "INT",
            Self::Link => "LINK",
            Self::MidiClock => "MIDI",
            Self::Osc => "OSC",
        }
    }

    /// Transport bar label matching Unity's PushClockAuthorityToPanel format.
    pub fn transport_label(&self) -> &'static str {
        match self {
            Self::Internal => "SRC:INT",
            Self::Link => "SRC:LNK",
            Self::MidiClock => "SRC:CLK",
            Self::Osc => "SRC:OSC",
        }
    }
}

impl Serialize for ClockAuthority {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for ClockAuthority {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::Number(n) => match n.as_i64().unwrap_or(0) as i32 {
                0 => ClockAuthority::Internal,
                1 => ClockAuthority::Link,
                2 => ClockAuthority::MidiClock,
                3 => ClockAuthority::Osc,
                _ => ClockAuthority::Internal,
            },
            serde_json::Value::String(s) => match s.as_str() {
                "Internal" => ClockAuthority::Internal,
                "Link" => ClockAuthority::Link,
                "MidiClock" => ClockAuthority::MidiClock,
                "Osc" => ClockAuthority::Osc,
                _ => ClockAuthority::Internal,
            },
            _ => ClockAuthority::Internal,
        })
    }
}

// ─── Quantize Mode ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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

impl Serialize for QuantizeMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for QuantizeMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::Number(n) => match n.as_i64().unwrap_or(0) as i32 {
                0 => QuantizeMode::Off,
                1 => QuantizeMode::QuarterBeat,
                2 => QuantizeMode::Beat,
                3 => QuantizeMode::Bar,
                _ => QuantizeMode::Off,
            },
            serde_json::Value::String(s) => match s.as_str() {
                "Off" => QuantizeMode::Off,
                "QuarterBeat" => QuantizeMode::QuarterBeat,
                "Beat" => QuantizeMode::Beat,
                "Bar" => QuantizeMode::Bar,
                _ => QuantizeMode::Off,
            },
            _ => QuantizeMode::Off,
        })
    }
}

// ─── Resolution Preset ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ResolutionPreset {
    HD720p = 0,
    #[default]
    FHD1080p = 1,
    QHD1440p = 2,
    UHD4K = 3,
    Square1080 = 4,
    Portrait720 = 5,
    Portrait1080 = 6,
    Portrait1440 = 7,
}

impl Serialize for ResolutionPreset {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for ResolutionPreset {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::Number(n) => match n.as_i64().unwrap_or(1) as i32 {
                0 => ResolutionPreset::HD720p,
                1 => ResolutionPreset::FHD1080p,
                2 => ResolutionPreset::QHD1440p,
                3 => ResolutionPreset::UHD4K,
                4 => ResolutionPreset::Square1080,
                5 => ResolutionPreset::Portrait720,
                6 => ResolutionPreset::Portrait1080,
                7 => ResolutionPreset::Portrait1440,
                _ => ResolutionPreset::FHD1080p,
            },
            serde_json::Value::String(s) => match s.as_str() {
                "HD720p" => ResolutionPreset::HD720p,
                "FHD1080p" => ResolutionPreset::FHD1080p,
                "QHD1440p" => ResolutionPreset::QHD1440p,
                "UHD4K" => ResolutionPreset::UHD4K,
                "Square1080" => ResolutionPreset::Square1080,
                "Portrait720" => ResolutionPreset::Portrait720,
                "Portrait1080" => ResolutionPreset::Portrait1080,
                "Portrait1440" => ResolutionPreset::Portrait1440,
                _ => ResolutionPreset::FHD1080p,
            },
            _ => ResolutionPreset::FHD1080p,
        })
    }
}

impl ResolutionPreset {
    pub const ALL: &[Self] = &[
        Self::HD720p, Self::FHD1080p, Self::QHD1440p, Self::UHD4K,
        Self::Square1080, Self::Portrait720, Self::Portrait1080, Self::Portrait1440,
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
        }
    }

    /// Dropdown label with dimensions. Matches Unity: "{label}  ({w}x{h})".
    pub fn dropdown_label(&self) -> String {
        let (w, h) = self.dimensions();
        format!("{}  ({}x{})", self.display_name(), w, h)
    }

    pub fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TempoPointSource {
    #[default]
    Unknown = 0,
    Manual = 1,
    Link = 2,
    MidiClock = 3,
    Recorded = 4,
}

impl Serialize for TempoPointSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for TempoPointSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::Number(n) => match n.as_i64().unwrap_or(0) as i32 {
                0 => TempoPointSource::Unknown,
                1 => TempoPointSource::Manual,
                2 => TempoPointSource::Link,
                3 => TempoPointSource::MidiClock,
                4 => TempoPointSource::Recorded,
                _ => TempoPointSource::Unknown,
            },
            serde_json::Value::String(s) => match s.as_str() {
                "Unknown" => TempoPointSource::Unknown,
                "Manual" => TempoPointSource::Manual,
                "Link" => TempoPointSource::Link,
                "MidiClock" => TempoPointSource::MidiClock,
                "Recorded" => TempoPointSource::Recorded,
                _ => TempoPointSource::Unknown,
            },
            _ => TempoPointSource::Unknown,
        })
    }
}

// ─── Beat Division (for parameter drivers / LFO) ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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

impl Serialize for BeatDivision {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for BeatDivision {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::Number(n) => match n.as_i64().unwrap_or(3) as i32 {
                0 => BeatDivision::ThirtySecond,
                1 => BeatDivision::Sixteenth,
                2 => BeatDivision::Eighth,
                3 => BeatDivision::Quarter,
                4 => BeatDivision::Half,
                5 => BeatDivision::Whole,
                6 => BeatDivision::TwoWhole,
                7 => BeatDivision::FourWhole,
                8 => BeatDivision::EightWhole,
                9 => BeatDivision::SixteenWhole,
                10 => BeatDivision::ThirtyTwoWhole,
                11 => BeatDivision::EighthDotted,
                12 => BeatDivision::QuarterDotted,
                13 => BeatDivision::HalfDotted,
                14 => BeatDivision::WholeDotted,
                15 => BeatDivision::TwoWholeDotted,
                16 => BeatDivision::EighthTriplet,
                17 => BeatDivision::QuarterTriplet,
                18 => BeatDivision::HalfTriplet,
                19 => BeatDivision::WholeTriplet,
                _ => BeatDivision::Quarter,
            },
            serde_json::Value::String(s) => match s.as_str() {
                "ThirtySecond" => BeatDivision::ThirtySecond,
                "Sixteenth" => BeatDivision::Sixteenth,
                "Eighth" => BeatDivision::Eighth,
                "Quarter" => BeatDivision::Quarter,
                "Half" => BeatDivision::Half,
                "Whole" => BeatDivision::Whole,
                "TwoWhole" => BeatDivision::TwoWhole,
                "FourWhole" => BeatDivision::FourWhole,
                "EightWhole" => BeatDivision::EightWhole,
                "SixteenWhole" => BeatDivision::SixteenWhole,
                "ThirtyTwoWhole" => BeatDivision::ThirtyTwoWhole,
                "EighthDotted" => BeatDivision::EighthDotted,
                "QuarterDotted" => BeatDivision::QuarterDotted,
                "HalfDotted" => BeatDivision::HalfDotted,
                "WholeDotted" => BeatDivision::WholeDotted,
                "TwoWholeDotted" => BeatDivision::TwoWholeDotted,
                "EighthTriplet" => BeatDivision::EighthTriplet,
                "QuarterTriplet" => BeatDivision::QuarterTriplet,
                "HalfTriplet" => BeatDivision::HalfTriplet,
                "WholeTriplet" => BeatDivision::WholeTriplet,
                _ => BeatDivision::Quarter,
            },
            _ => BeatDivision::Quarter,
        })
    }
}

impl BeatDivision {
    /// Convert integer value to BeatDivision. Returns None if invalid.
    pub fn from_i32(val: i32) -> Option<Self> {
        match val {
            0 => Some(Self::ThirtySecond),
            1 => Some(Self::Sixteenth),
            2 => Some(Self::Eighth),
            3 => Some(Self::Quarter),
            4 => Some(Self::Half),
            5 => Some(Self::Whole),
            6 => Some(Self::TwoWhole),
            7 => Some(Self::FourWhole),
            8 => Some(Self::EightWhole),
            9 => Some(Self::SixteenWhole),
            10 => Some(Self::ThirtyTwoWhole),
            11 => Some(Self::EighthDotted),
            12 => Some(Self::QuarterDotted),
            13 => Some(Self::HalfDotted),
            14 => Some(Self::WholeDotted),
            15 => Some(Self::TwoWholeDotted),
            16 => Some(Self::EighthTriplet),
            17 => Some(Self::QuarterTriplet),
            18 => Some(Self::HalfTriplet),
            19 => Some(Self::WholeTriplet),
            _ => None,
        }
    }

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
            BeatDivision::ThirtyTwoWhole, // "64" — clamp to max
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
        matches!(self,
            Self::EighthDotted | Self::QuarterDotted | Self::HalfDotted
            | Self::WholeDotted | Self::TwoWholeDotted)
    }

    pub fn is_triplet(self) -> bool {
        matches!(self,
            Self::EighthTriplet | Self::QuarterTriplet
            | Self::HalfTriplet | Self::WholeTriplet)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DriverWaveform {
    #[default]
    Sine = 0,
    Triangle = 1,
    Sawtooth = 2,
    Square = 3,
    Random = 4,
}

impl Serialize for DriverWaveform {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for DriverWaveform {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::Number(n) => match n.as_i64().unwrap_or(0) as i32 {
                0 => DriverWaveform::Sine,
                1 => DriverWaveform::Triangle,
                2 => DriverWaveform::Sawtooth,
                3 => DriverWaveform::Square,
                4 => DriverWaveform::Random,
                _ => DriverWaveform::Sine,
            },
            serde_json::Value::String(s) => match s.as_str() {
                "Sine" => DriverWaveform::Sine,
                "Triangle" => DriverWaveform::Triangle,
                "Sawtooth" => DriverWaveform::Sawtooth,
                "Square" => DriverWaveform::Square,
                "Random" => DriverWaveform::Random,
                _ => DriverWaveform::Sine,
            },
            _ => DriverWaveform::Sine,
        })
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ClipDurationMode {
    #[default]
    NoteOff = 2,
}

impl Serialize for ClipDurationMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for ClipDurationMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Only one variant exists; accept any integer or string representation.
        let _value = serde_json::Value::deserialize(deserializer)?;
        Ok(ClipDurationMode::NoteOff)
    }
}

// ─── Display impls ───

impl std::fmt::Display for EffectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

impl std::fmt::Display for GeneratorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

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

impl TryFrom<i32> for EffectType {
    type Error = ();

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Transform),
            1 => Ok(Self::InvertColors),
            10 => Ok(Self::Feedback),
            11 => Ok(Self::PixelSort),
            12 => Ok(Self::Bloom),
            13 => Ok(Self::InfiniteZoom),
            14 => Ok(Self::Kaleidoscope),
            15 => Ok(Self::EdgeStretch),
            16 => Ok(Self::VoronoiPrism),
            17 => Ok(Self::QuadMirror),
            18 => Ok(Self::Dither),
            19 => Ok(Self::Strobe),
            20 => Ok(Self::StylizedFeedback),
            21 => Ok(Self::Mirror),
            22 => Ok(Self::BlobTracking),
            23 => Ok(Self::CRT),
            24 => Ok(Self::FluidDistortion),
            25 => Ok(Self::EdgeGlow),
            26 => Ok(Self::Datamosh),
            27 => Ok(Self::SlitScan),
            28 => Ok(Self::ColorGrade),
            29 => Ok(Self::WireframeDepth),
            30 => Ok(Self::ChromaticAberration),
            31 => Ok(Self::GradientMap),
            32 => Ok(Self::Glitch),
            33 => Ok(Self::FilmGrain),
            34 => Ok(Self::Halation),
            35 => Ok(Self::Microscope),
            36 => Ok(Self::Corruption),
            37 => Ok(Self::Infrared),
            38 => Ok(Self::Surveillance),
            39 => Ok(Self::Redaction),
            _ => Err(()),
        }
    }
}

impl TryFrom<i32> for GeneratorType {
    type Error = ();

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::None),
            2 => Ok(Self::BasicShapesSnap),
            3 => Ok(Self::Duocylinder),
            4 => Ok(Self::Tesseract),
            5 => Ok(Self::ConcentricTunnel),
            6 => Ok(Self::Plasma),
            7 => Ok(Self::Lissajous),
            8 => Ok(Self::FractalZoom),
            9 => Ok(Self::OscilloscopeXY),
            10 => Ok(Self::WireframeZoo),
            11 => Ok(Self::ReactionDiffusion),
            12 => Ok(Self::Flowfield),
            13 => Ok(Self::ParametricSurface),
            14 => Ok(Self::StrangeAttractor),
            15 => Ok(Self::FluidSimulation),
            16 => Ok(Self::NumberStation),
            17 => Ok(Self::Mycelium),
            18 => Ok(Self::ComputeStrangeAttractor),
            19 => Ok(Self::FluidSimulation3D),
            20 => Ok(Self::MriVolume),
            _ => Err(()),
        }
    }
}

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
