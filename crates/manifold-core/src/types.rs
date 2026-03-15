use serde::{Deserialize, Serialize};
use serde::de::Deserializer;
use serde::ser::Serializer;

// ─── Blend Modes ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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
        Self::ALL.get(i).copied().unwrap_or(Self::Normal)
    }
}

impl Serialize for BlendMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for BlendMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = i32::deserialize(deserializer)?;
        Ok(match v {
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
        })
    }
}

// ─── Effect Types ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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

    /// Parameter definitions for this effect type.
    /// Returns (name, min, max, default, whole_numbers).
    pub fn param_defs(&self) -> &'static [(&'static str, f32, f32, f32, bool)] {
        match self {
            Self::InvertColors => &[("Intensity", 0.0, 1.0, 1.0, false)],
            Self::Feedback => &[("Amount", 0.0, 1.0, 0.5, false)],
            Self::Bloom => &[
                ("Threshold", 0.0, 2.0, 0.8, false),
                ("Intensity", 0.0, 2.0, 0.5, false),
            ],
            Self::Mirror => &[("Mode", 0.0, 2.0, 0.0, true)],
            Self::ColorGrade => &[
                ("Hue Shift", -1.0, 1.0, 0.0, false),
                ("Saturation", 0.0, 2.0, 1.0, false),
                ("Gain", 0.0, 3.0, 1.0, false),
                ("Contrast", 0.0, 2.0, 1.0, false),
            ],
            Self::Transform => &[
                ("Scale", 0.1, 5.0, 1.0, false),
                ("Rotation", -180.0, 180.0, 0.0, false),
                ("Offset X", -1.0, 1.0, 0.0, false),
                ("Offset Y", -1.0, 1.0, 0.0, false),
            ],
            Self::Kaleidoscope => &[
                ("Amount", 0.0, 1.0, 1.0, false),
                ("Segments", 2.0, 16.0, 6.0, true),
            ],
            Self::EdgeStretch => &[
                ("Amount", 0.0, 1.0, 0.5, false),
                ("Source Width", 0.01, 0.5, 0.1, false),
            ],
            Self::PixelSort => &[
                ("Amount", 0.0, 1.0, 0.5, false),
                ("Threshold", 0.0, 1.0, 0.3, false),
            ],
            Self::Strobe => &[
                ("Rate", 0.0, 1.0, 0.5, false),
                ("Intensity", 0.0, 1.0, 1.0, false),
            ],
            Self::ChromaticAberration => &[("Amount", 0.0, 50.0, 5.0, false)],
            Self::FilmGrain => &[("Amount", 0.0, 1.0, 0.3, false)],
            Self::Glitch => &[("Amount", 0.0, 1.0, 0.5, false)],
            Self::Dither => &[("Amount", 0.0, 1.0, 0.5, false)],
            Self::CRT => &[("Amount", 0.0, 1.0, 0.5, false)],
            Self::Halation => &[
                ("Amount", 0.0, 1.0, 0.5, false),
                ("Radius", 0.0, 50.0, 10.0, false),
            ],
            _ => &[],
        }
    }

    /// All effect types available for the "Add Effect" dropdown.
    pub const ALL: &'static [EffectType] = &[
        EffectType::InvertColors, EffectType::ColorGrade, EffectType::Mirror,
        EffectType::Feedback, EffectType::Bloom, EffectType::Transform,
        EffectType::Kaleidoscope, EffectType::EdgeStretch, EffectType::PixelSort,
        EffectType::Strobe, EffectType::ChromaticAberration, EffectType::FilmGrain,
        EffectType::Glitch, EffectType::Dither, EffectType::CRT,
        EffectType::Halation, EffectType::InfiniteZoom, EffectType::VoronoiPrism,
        EffectType::QuadMirror, EffectType::StylizedFeedback, EffectType::BlobTracking,
        EffectType::FluidDistortion, EffectType::EdgeGlow, EffectType::Datamosh,
        EffectType::SlitScan, EffectType::WireframeDepth, EffectType::GradientMap,
        EffectType::Microscope, EffectType::Corruption, EffectType::Infrared,
        EffectType::Surveillance, EffectType::Redaction,
    ];

    pub fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }
}

impl Serialize for EffectType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for EffectType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = i32::deserialize(deserializer)?;
        Ok(match v {
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
        })
    }
}

// ─── Generator Types ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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
        }
    }

    /// Parameter definitions for this generator type.
    /// Returns (name, min, max, default, whole_numbers, is_toggle).
    pub fn param_defs(&self) -> &'static [(&'static str, f32, f32, f32, bool, bool)] {
        match self {
            Self::Plasma => &[
                ("Speed", 0.0, 5.0, 1.0, false, false),
                ("Scale", 0.1, 10.0, 3.0, false, false),
                ("Complexity", 1.0, 8.0, 4.0, true, false),
                ("Color Shift", 0.0, 1.0, 0.0, false, false),
            ],
            Self::ConcentricTunnel => &[
                ("Speed", 0.0, 5.0, 1.0, false, false),
                ("Rings", 2.0, 20.0, 8.0, true, false),
                ("Twist", 0.0, 10.0, 1.0, false, false),
            ],
            Self::Lissajous => &[
                ("Freq X", 1.0, 12.0, 3.0, true, false),
                ("Freq Y", 1.0, 12.0, 2.0, true, false),
                ("Phase", 0.0, 6.28, 1.57, false, false),
                ("Trail", 0.0, 1.0, 0.95, false, false),
            ],
            Self::FractalZoom => &[
                ("Zoom Speed", 0.0, 3.0, 0.5, false, false),
                ("Iterations", 10.0, 500.0, 100.0, true, false),
                ("Color Cycle", 0.0, 5.0, 1.0, false, false),
            ],
            Self::Flowfield => &[
                ("Speed", 0.0, 5.0, 1.0, false, false),
                ("Scale", 0.1, 10.0, 3.0, false, false),
                ("Particle Count", 100.0, 10000.0, 2000.0, true, false),
            ],
            Self::ReactionDiffusion => &[
                ("Feed Rate", 0.01, 0.1, 0.055, false, false),
                ("Kill Rate", 0.04, 0.08, 0.062, false, false),
                ("Diffusion A", 0.5, 1.5, 1.0, false, false),
                ("Diffusion B", 0.1, 1.0, 0.5, false, false),
            ],
            Self::FluidSimulation | Self::FluidSimulation3D => &[
                ("Viscosity", 0.0, 1.0, 0.1, false, false),
                ("Diffusion", 0.0, 1.0, 0.0, false, false),
                ("Force", 0.0, 500.0, 100.0, false, false),
                ("Color Decay", 0.9, 1.0, 0.99, false, false),
            ],
            _ => &[],
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
        let v = i32::deserialize(deserializer)?;
        match v {
            0 => Ok(GeneratorType::None),
            2 => Ok(GeneratorType::BasicShapesSnap),
            3 => Ok(GeneratorType::Duocylinder),
            4 => Ok(GeneratorType::Tesseract),
            5 => Ok(GeneratorType::ConcentricTunnel),
            6 => Ok(GeneratorType::Plasma),
            7 => Ok(GeneratorType::Lissajous),
            8 => Ok(GeneratorType::FractalZoom),
            9 => Ok(GeneratorType::OscilloscopeXY),
            10 => Ok(GeneratorType::WireframeZoo),
            11 => Ok(GeneratorType::ReactionDiffusion),
            12 => Ok(GeneratorType::Flowfield),
            13 => Ok(GeneratorType::ParametricSurface),
            14 => Ok(GeneratorType::StrangeAttractor),
            15 => Ok(GeneratorType::FluidSimulation),
            16 => Ok(GeneratorType::NumberStation),
            17 => Ok(GeneratorType::Mycelium),
            18 => Ok(GeneratorType::ComputeStrangeAttractor),
            19 => Ok(GeneratorType::FluidSimulation3D),
            _ => Ok(GeneratorType::None),
        }
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
        let v = i32::deserialize(deserializer)?;
        match v {
            0 => Ok(LayerType::Video),
            1 => Ok(LayerType::Generator),
            2 => Ok(LayerType::Group),
            _ => Ok(LayerType::Video),
        }
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
}

impl Serialize for ClockAuthority {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for ClockAuthority {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = i32::deserialize(deserializer)?;
        match v {
            0 => Ok(ClockAuthority::Internal),
            1 => Ok(ClockAuthority::Link),
            2 => Ok(ClockAuthority::MidiClock),
            3 => Ok(ClockAuthority::Osc),
            _ => Ok(ClockAuthority::Internal),
        }
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
        let v = i32::deserialize(deserializer)?;
        match v {
            0 => Ok(QuantizeMode::Off),
            1 => Ok(QuantizeMode::QuarterBeat),
            2 => Ok(QuantizeMode::Beat),
            3 => Ok(QuantizeMode::Bar),
            _ => Ok(QuantizeMode::Off),
        }
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
        let v = i32::deserialize(deserializer)?;
        match v {
            0 => Ok(ResolutionPreset::HD720p),
            1 => Ok(ResolutionPreset::FHD1080p),
            2 => Ok(ResolutionPreset::QHD1440p),
            3 => Ok(ResolutionPreset::UHD4K),
            4 => Ok(ResolutionPreset::Square1080),
            5 => Ok(ResolutionPreset::Portrait720),
            6 => Ok(ResolutionPreset::Portrait1080),
            7 => Ok(ResolutionPreset::Portrait1440),
            _ => Ok(ResolutionPreset::FHD1080p),
        }
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

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::HD720p => "720p",
            Self::FHD1080p => "1080p",
            Self::QHD1440p => "1440p",
            Self::UHD4K => "4K",
            Self::Square1080 => "1080×1080",
            Self::Portrait720 => "720×1280",
            Self::Portrait1080 => "1080×1920",
            Self::Portrait1440 => "1440×2560",
        }
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
        let v = i32::deserialize(deserializer)?;
        match v {
            0 => Ok(TempoPointSource::Unknown),
            1 => Ok(TempoPointSource::Manual),
            2 => Ok(TempoPointSource::Link),
            3 => Ok(TempoPointSource::MidiClock),
            4 => Ok(TempoPointSource::Recorded),
            _ => Ok(TempoPointSource::Unknown),
        }
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
        let v = i32::deserialize(deserializer)?;
        Ok(match v {
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
        })
    }
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
        let v = i32::deserialize(deserializer)?;
        Ok(match v {
            0 => DriverWaveform::Sine,
            1 => DriverWaveform::Triangle,
            2 => DriverWaveform::Sawtooth,
            3 => DriverWaveform::Square,
            4 => DriverWaveform::Random,
            _ => DriverWaveform::Sine,
        })
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
        let _v = i32::deserialize(deserializer)?;
        Ok(ClipDurationMode::NoteOff)
    }
}
