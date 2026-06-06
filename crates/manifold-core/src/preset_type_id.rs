//! String-keyed preset type identifier — the unified id for effects and
//! generators.
//!
//! Replaces the two parallel newtypes (the former `EffectTypeId` / `GeneratorTypeId`)
//! (Phase 1 of `docs/PRESET_INSTANCE_COLLAPSE_PLAN.md`). A preset is a preset;
//! whether it transforms an input (effect) or produces from nothing (generator)
//! is carried by [`crate::preset_def::PresetKind`], not by a separate id type.
//!
//! Built-in presets use the associated constants. Serializes as a
//! human-readable string. The **legacy integer discriminant** form (pre-string
//! project files) is kind-specific — the same integer means different things for
//! an effect vs a generator — so it is decoded through the two explicit
//! [`PresetTypeId::from_legacy_effect_discriminant`] /
//! [`PresetTypeId::from_legacy_generator_discriminant`] functions at the one
//! place the kind is statically known (the `PresetInstance` / generator
//! deserializers). The bare `Deserialize` handles the modern string form.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::fmt;

/// Identifies a preset (effect or generator) type by name.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PresetTypeId(Cow<'static, str>);

// ── Construction ────────────────────────────────────────────────────────

impl PresetTypeId {
    /// Create from a static string (compile-time constant).
    pub const fn new(s: &'static str) -> Self {
        Self(Cow::Borrowed(s))
    }

    /// Create from a runtime string (e.g. plugin-provided ID).
    pub fn from_string(s: String) -> Self {
        Self(Cow::Owned(s))
    }

    /// The underlying string value.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True if this is the effect "unrecognized / removed type" sentinel.
    pub fn is_unknown(&self) -> bool {
        *self == Self::UNKNOWN
    }

    /// True if this is the "no generator" sentinel (a video layer).
    pub fn is_none(&self) -> bool {
        *self == Self::NONE
    }
}

// ── Built-in constants: effects ─────────────────────────────────────────

impl PresetTypeId {
    pub const TRANSFORM: Self = Self(Cow::Borrowed("Transform"));
    pub const INVERT_COLORS: Self = Self(Cow::Borrowed("InvertColors"));
    pub const FEEDBACK: Self = Self(Cow::Borrowed("Feedback"));
    pub const PIXEL_SORT: Self = Self(Cow::Borrowed("PixelSort"));
    pub const BLOOM: Self = Self(Cow::Borrowed("Bloom"));
    pub const INFINITE_ZOOM: Self = Self(Cow::Borrowed("InfiniteZoom"));
    pub const KALEIDOSCOPE: Self = Self(Cow::Borrowed("Kaleidoscope"));
    pub const EDGE_STRETCH: Self = Self(Cow::Borrowed("EdgeStretch"));
    pub const VORONOI_PRISM: Self = Self(Cow::Borrowed("VoronoiPrism"));
    pub const QUAD_MIRROR: Self = Self(Cow::Borrowed("QuadMirror"));
    pub const DITHER: Self = Self(Cow::Borrowed("Dither"));
    pub const STROBE: Self = Self(Cow::Borrowed("Strobe"));
    pub const STYLIZED_FEEDBACK: Self = Self(Cow::Borrowed("StylizedFeedback"));
    pub const MIRROR: Self = Self(Cow::Borrowed("Mirror"));
    pub const BLOB_TRACKING: Self = Self(Cow::Borrowed("BlobTracking"));
    pub const CRT: Self = Self(Cow::Borrowed("CRT"));
    pub const FLUID_DISTORTION: Self = Self(Cow::Borrowed("FluidDistortion"));
    /// Serialized as "EdgeGlow" for backward compatibility with project files.
    pub const EDGE_DETECT: Self = Self(Cow::Borrowed("EdgeGlow"));
    pub const DATAMOSH: Self = Self(Cow::Borrowed("Datamosh"));
    pub const SLIT_SCAN: Self = Self(Cow::Borrowed("SlitScan"));
    pub const COLOR_GRADE: Self = Self(Cow::Borrowed("ColorGrade"));
    pub const WIREFRAME_DEPTH: Self = Self(Cow::Borrowed("WireframeDepth"));
    pub const CHROMATIC_ABERRATION: Self = Self(Cow::Borrowed("ChromaticAberration"));
    pub const GRADIENT_MAP: Self = Self(Cow::Borrowed("GradientMap"));
    pub const GLITCH: Self = Self(Cow::Borrowed("Glitch"));
    pub const HALATION: Self = Self(Cow::Borrowed("Halation"));
    pub const CORRUPTION: Self = Self(Cow::Borrowed("Corruption"));
    pub const INFRARED: Self = Self(Cow::Borrowed("Infrared"));
    pub const SURVEILLANCE: Self = Self(Cow::Borrowed("Surveillance"));
    pub const REDACTION: Self = Self(Cow::Borrowed("Redaction"));
    pub const DEPTH_OF_FIELD: Self = Self(Cow::Borrowed("DepthOfField"));
    pub const HDR_BOOST: Self = Self(Cow::Borrowed("HdrBoost"));
    pub const AUTO_GAIN: Self = Self(Cow::Borrowed("AutoGain"));
    pub const WATERCOLOR: Self = Self(Cow::Borrowed("Watercolor"));

    /// Test effect proving the node-graph runtime renders to a real layer
    /// target. `Source × 2 → Mix → FinalOutput`.
    pub const NODE_GRAPH_TEST: Self = Self(Cow::Borrowed("NodeGraphTest"));

    /// Soft Focus, graph-backed — first graph-backed effect with branching
    /// topology.
    pub const SOFT_FOCUS_GRAPH: Self = Self(Cow::Borrowed("SoftFocusGraph"));

    /// Placeholder for unrecognized/removed effect types. Renderers skip this.
    pub const UNKNOWN: Self = Self(Cow::Borrowed("Unknown"));
}

// ── Built-in constants: generators ──────────────────────────────────────

impl PresetTypeId {
    /// Sentinel: layer has no generator (video layer).
    pub const NONE: Self = Self(Cow::Borrowed("None"));
    pub const BASIC_SHAPES: Self = Self(Cow::Borrowed("BasicShapes"));
    pub const DUOCYLINDER: Self = Self(Cow::Borrowed("Duocylinder"));
    pub const TESSERACT: Self = Self(Cow::Borrowed("Tesseract"));
    pub const CONCENTRIC_TUNNEL: Self = Self(Cow::Borrowed("ConcentricTunnel"));
    pub const PLASMA: Self = Self(Cow::Borrowed("Plasma"));
    pub const LISSAJOUS: Self = Self(Cow::Borrowed("Lissajous"));
    pub const FRACTAL_ZOOM: Self = Self(Cow::Borrowed("FractalZoom"));
    pub const WIREFRAME_ZOO: Self = Self(Cow::Borrowed("WireframeZoo"));
    pub const REACTION_DIFFUSION: Self = Self(Cow::Borrowed("ReactionDiffusion"));
    pub const FLOWFIELD: Self = Self(Cow::Borrowed("Flowfield"));
    pub const STRANGE_ATTRACTOR: Self = Self(Cow::Borrowed("StrangeAttractor"));
    pub const FLUID_SIMULATION: Self = Self(Cow::Borrowed("FluidSimulation"));
    pub const NUMBER_STATION: Self = Self(Cow::Borrowed("NumberStation"));
    pub const COMPUTE_STRANGE_ATTRACTOR: Self = Self(Cow::Borrowed("ComputeStrangeAttractor"));
    pub const FLUID_SIMULATION_3D: Self = Self(Cow::Borrowed("FluidSimulation3D"));
    pub const MRI_VOLUME: Self = Self(Cow::Borrowed("MriVolume"));
    pub const BLACK_HOLE: Self = Self(Cow::Borrowed("BlackHole"));
    pub const METALLIC_GLASS: Self = Self(Cow::Borrowed("MetallicGlass"));
    pub const OILY_FLUID: Self = Self(Cow::Borrowed("OilyFluid"));
    pub const NESTED_CUBES: Self = Self(Cow::Borrowed("NestedCubes"));
    pub const STAR_FIELD: Self = Self(Cow::Borrowed("StarField"));
    pub const TEXT: Self = Self(Cow::Borrowed("Text"));
    pub const PARTICLE_TEXT: Self = Self(Cow::Borrowed("ParticleText"));
    pub const DIGITAL_PLANTS: Self = Self(Cow::Borrowed("DigitalPlants"));
}

// ── Legacy discriminant mapping (kind-specific) ─────────────────────────

impl PresetTypeId {
    /// Decode a legacy integer **effect** discriminant (from pre-string project
    /// files). Unknown values map to [`Self::UNKNOWN`]. Callable only where the
    /// kind is known to be an effect — the integer namespace overlaps with the
    /// generator one.
    pub fn from_legacy_effect_discriminant(v: i32) -> Self {
        match v {
            0 => Self::TRANSFORM,
            1 => Self::INVERT_COLORS,
            10 => Self::FEEDBACK,
            11 => Self::PIXEL_SORT,
            12 => Self::BLOOM,
            13 => Self::INFINITE_ZOOM,
            14 => Self::KALEIDOSCOPE,
            15 => Self::EDGE_STRETCH,
            16 => Self::VORONOI_PRISM,
            17 => Self::QUAD_MIRROR,
            18 => Self::DITHER,
            19 => Self::STROBE,
            20 => Self::STYLIZED_FEEDBACK,
            21 => Self::MIRROR,
            22 => Self::BLOB_TRACKING,
            23 => Self::CRT,
            24 => Self::FLUID_DISTORTION,
            25 => Self::EDGE_DETECT,
            26 => Self::DATAMOSH,
            27 => Self::SLIT_SCAN,
            28 => Self::COLOR_GRADE,
            29 => Self::WIREFRAME_DEPTH,
            30 => Self::CHROMATIC_ABERRATION,
            31 => Self::GRADIENT_MAP,
            32 => Self::GLITCH,
            33 => Self::UNKNOWN,
            34 => Self::HALATION,
            36 => Self::CORRUPTION,
            37 => Self::INFRARED,
            38 => Self::SURVEILLANCE,
            39 => Self::REDACTION,
            40 => Self::DEPTH_OF_FIELD,
            41 => Self::AUTO_GAIN,
            _ => {
                for meta in inventory::iter::<crate::effect_registration::EffectMetadata> {
                    if meta.legacy_discriminant == Some(v) {
                        return meta.id.clone();
                    }
                }
                Self::UNKNOWN
            }
        }
    }

    /// Decode a legacy integer **generator** discriminant (from pre-string
    /// project files). Unknown values map to [`Self::NONE`]. Callable only where
    /// the kind is known to be a generator.
    pub fn from_legacy_generator_discriminant(v: i32) -> Self {
        match v {
            0 => Self::NONE,
            2 => Self::BASIC_SHAPES,
            3 => Self::DUOCYLINDER,
            4 => Self::TESSERACT,
            5 => Self::CONCENTRIC_TUNNEL,
            6 => Self::PLASMA,
            7 => Self::LISSAJOUS,
            8 => Self::FRACTAL_ZOOM,
            // Legacy discriminant 9 (OscilloscopeXY) → Lissajous: OscXY was a
            // variant of Lissajous with worse param defaults, removed 2026-05-25.
            9 => Self::LISSAJOUS,
            10 => Self::WIREFRAME_ZOO,
            11 => Self::REACTION_DIFFUSION,
            12 => Self::FLOWFIELD,
            14 => Self::STRANGE_ATTRACTOR,
            15 => Self::FLUID_SIMULATION,
            16 => Self::NUMBER_STATION,
            18 => Self::COMPUTE_STRANGE_ATTRACTOR,
            19 => Self::FLUID_SIMULATION_3D,
            20 => Self::MRI_VOLUME,
            21 => Self::BLACK_HOLE,
            23 => Self::METALLIC_GLASS,
            24 => Self::OILY_FLUID,
            25 => Self::NESTED_CUBES,
            _ => {
                for meta in inventory::iter::<crate::generator_registration::GeneratorMetadata> {
                    if meta.legacy_discriminant == Some(v) {
                        return meta.id.clone();
                    }
                }
                Self::NONE
            }
        }
    }
}

/// String-form rename map for project-load compatibility. Old projects stored
/// the legacy ids verbatim; renames after ship must be remapped here so saved
/// layers continue to resolve. Folded in from the old generator decoder — the
/// effect id space has no entries, so applying it globally is a no-op for them.
pub(crate) fn remap_legacy_string(s: &str) -> &str {
    match s {
        "BasicShapesSnap" => "BasicShapes",
        other => other,
    }
}

// ── Trait impls ──────────────────────────────────────────────────────────

impl Default for PresetTypeId {
    /// The empty/sentinel id. Generator param state defaults to "no generator";
    /// effect instances are always constructed with an explicit type, so the
    /// default is only ever observed on the generator side.
    fn default() -> Self {
        Self::NONE
    }
}

impl fmt::Debug for PresetTypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PresetTypeId({})", self.0)
    }
}

impl fmt::Display for PresetTypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for PresetTypeId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// ── Serde ───────────────────────────────────────────────────────────────

impl Serialize for PresetTypeId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PresetTypeId {
    /// The modern string form. The legacy integer form is kind-specific and is
    /// decoded by [`deserialize_effect_type`] / [`deserialize_generator_type`]
    /// at the call sites where the kind is known; a bare integer reaching here
    /// (no kind context) cannot be disambiguated and maps to [`Self::UNKNOWN`].
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::String(s) => Self(Cow::Owned(remap_legacy_string(s).to_string())),
            _ => Self::UNKNOWN,
        })
    }
}

/// `deserialize_with` helper for an **effect** type field: string form, or the
/// kind-specific legacy integer discriminant. Use on the `effect_type` field of
/// any struct deserialized in an effect context.
pub fn deserialize_effect_type<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<PresetTypeId, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match &value {
        serde_json::Value::String(s) => {
            PresetTypeId(Cow::Owned(remap_legacy_string(s).to_string()))
        }
        serde_json::Value::Number(n) => {
            PresetTypeId::from_legacy_effect_discriminant(n.as_i64().unwrap_or(0) as i32)
        }
        _ => PresetTypeId::UNKNOWN,
    })
}

/// `deserialize_with` helper for a **generator** type field: string form, or the
/// kind-specific legacy integer discriminant. Use on the `generator_type` field
/// of any struct deserialized in a generator context.
pub fn deserialize_generator_type<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<PresetTypeId, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match &value {
        serde_json::Value::String(s) => {
            PresetTypeId(Cow::Owned(remap_legacy_string(s).to_string()))
        }
        serde_json::Value::Number(n) => {
            PresetTypeId::from_legacy_generator_discriminant(n.as_i64().unwrap_or(0) as i32)
        }
        _ => PresetTypeId::NONE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_disjoint_and_equal() {
        assert_eq!(PresetTypeId::BLOOM, PresetTypeId::BLOOM);
        assert_ne!(PresetTypeId::BLOOM, PresetTypeId::TRANSFORM);
        assert_ne!(PresetTypeId::BLOOM, PresetTypeId::OILY_FLUID);
    }

    #[test]
    fn legacy_effect_discriminant() {
        assert_eq!(PresetTypeId::from_legacy_effect_discriminant(0), PresetTypeId::TRANSFORM);
        assert_eq!(PresetTypeId::from_legacy_effect_discriminant(12), PresetTypeId::BLOOM);
        assert_eq!(PresetTypeId::from_legacy_effect_discriminant(36), PresetTypeId::CORRUPTION);
        assert_eq!(PresetTypeId::from_legacy_effect_discriminant(999), PresetTypeId::UNKNOWN);
    }

    #[test]
    fn legacy_generator_discriminant() {
        assert_eq!(PresetTypeId::from_legacy_generator_discriminant(0), PresetTypeId::NONE);
        assert_eq!(PresetTypeId::from_legacy_generator_discriminant(6), PresetTypeId::PLASMA);
        assert_eq!(PresetTypeId::from_legacy_generator_discriminant(24), PresetTypeId::OILY_FLUID);
        assert_eq!(PresetTypeId::from_legacy_generator_discriminant(999), PresetTypeId::NONE);
    }

    #[test]
    fn legacy_discriminant_overlap_is_kind_specific() {
        // 10 means Feedback (effect) but WireframeZoo (generator).
        assert_eq!(PresetTypeId::from_legacy_effect_discriminant(10), PresetTypeId::FEEDBACK);
        assert_eq!(PresetTypeId::from_legacy_generator_discriminant(10), PresetTypeId::WIREFRAME_ZOO);
    }

    #[test]
    fn serde_string_roundtrip() {
        let id = PresetTypeId::BLOOM;
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"Bloom\"");
        let back: PresetTypeId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, PresetTypeId::BLOOM);
    }

    #[test]
    fn deserialize_with_helpers_decode_legacy_int() {
        let mut de = serde_json::Deserializer::from_str("12");
        assert_eq!(deserialize_effect_type(&mut de).unwrap(), PresetTypeId::BLOOM);
        let mut de = serde_json::Deserializer::from_str("6");
        assert_eq!(deserialize_generator_type(&mut de).unwrap(), PresetTypeId::PLASMA);
    }

    #[test]
    fn string_remap_applies() {
        let back: PresetTypeId = serde_json::from_str("\"BasicShapesSnap\"").unwrap();
        assert_eq!(back, PresetTypeId::BASIC_SHAPES);
    }

    #[test]
    fn unknown_string_preserved() {
        let back: PresetTypeId = serde_json::from_str("\"SomeFuturePreset\"").unwrap();
        assert_eq!(back.as_str(), "SomeFuturePreset");
        assert!(!back.is_unknown());
    }

    #[test]
    fn display() {
        assert_eq!(format!("{}", PresetTypeId::BLOOM), "Bloom");
        assert_eq!(format!("{}", PresetTypeId::OILY_FLUID), "OilyFluid");
    }
}
