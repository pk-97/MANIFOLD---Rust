//! String-keyed generator type identifier.
//!
//! Replaces the old `GeneratorType` enum with a `Cow<'static, str>` newtype.
//! Built-in generators use compile-time constants; future plugins can register
//! their own IDs at runtime.

use std::borrow::Cow;
use std::fmt;
use serde::{Serialize, Serializer, Deserialize, Deserializer};

/// Identifies a generator type by name.
///
/// Built-in generators use the associated constants (e.g. `GeneratorTypeId::PLASMA`).
/// Serializes as a human-readable string; deserializes from string or legacy
/// integer discriminant for backward compatibility with old project files.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GeneratorTypeId(Cow<'static, str>);

// ── Construction ────────────────────────────────────────────────────────

impl GeneratorTypeId {
    /// Create from a runtime string (e.g. plugin-provided ID).
    pub fn from_string(s: String) -> Self {
        Self(Cow::Owned(s))
    }

    /// The underlying string value.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True if this is the "no generator" sentinel.
    pub fn is_none(&self) -> bool {
        *self == Self::NONE
    }
}

// ── Built-in constants ──────────────────────────────────────────────────

impl GeneratorTypeId {
    /// Sentinel: layer has no generator (video layer).
    pub const NONE: Self = Self(Cow::Borrowed("None"));
    pub const BASIC_SHAPES_SNAP: Self = Self(Cow::Borrowed("BasicShapesSnap"));
    pub const DUOCYLINDER: Self = Self(Cow::Borrowed("Duocylinder"));
    pub const TESSERACT: Self = Self(Cow::Borrowed("Tesseract"));
    pub const CONCENTRIC_TUNNEL: Self = Self(Cow::Borrowed("ConcentricTunnel"));
    pub const PLASMA: Self = Self(Cow::Borrowed("Plasma"));
    pub const LISSAJOUS: Self = Self(Cow::Borrowed("Lissajous"));
    pub const FRACTAL_ZOOM: Self = Self(Cow::Borrowed("FractalZoom"));
    pub const OSCILLOSCOPE_XY: Self = Self(Cow::Borrowed("OscilloscopeXY"));
    pub const WIREFRAME_ZOO: Self = Self(Cow::Borrowed("WireframeZoo"));
    pub const REACTION_DIFFUSION: Self = Self(Cow::Borrowed("ReactionDiffusion"));
    pub const FLOWFIELD: Self = Self(Cow::Borrowed("Flowfield"));
    pub const PARAMETRIC_SURFACE: Self = Self(Cow::Borrowed("ParametricSurface"));
    pub const STRANGE_ATTRACTOR: Self = Self(Cow::Borrowed("StrangeAttractor"));
    pub const FLUID_SIMULATION: Self = Self(Cow::Borrowed("FluidSimulation"));
    pub const NUMBER_STATION: Self = Self(Cow::Borrowed("NumberStation"));
    pub const MYCELIUM: Self = Self(Cow::Borrowed("Mycelium"));
    pub const COMPUTE_STRANGE_ATTRACTOR: Self =
        Self(Cow::Borrowed("ComputeStrangeAttractor"));
    pub const FLUID_SIMULATION_3D: Self = Self(Cow::Borrowed("FluidSimulation3D"));
    pub const MRI_VOLUME: Self = Self(Cow::Borrowed("MriVolume"));
}

// ── Legacy discriminant mapping ─────────────────────────────────────────

impl GeneratorTypeId {
    /// Convert a legacy integer discriminant (from old project files) to a
    /// `GeneratorTypeId`. Unknown values map to `NONE`.
    pub fn from_legacy_discriminant(v: i32) -> Self {
        match v {
            0 => Self::NONE,
            2 => Self::BASIC_SHAPES_SNAP,
            3 => Self::DUOCYLINDER,
            4 => Self::TESSERACT,
            5 => Self::CONCENTRIC_TUNNEL,
            6 => Self::PLASMA,
            7 => Self::LISSAJOUS,
            8 => Self::FRACTAL_ZOOM,
            9 => Self::OSCILLOSCOPE_XY,
            10 => Self::WIREFRAME_ZOO,
            11 => Self::REACTION_DIFFUSION,
            12 => Self::FLOWFIELD,
            13 => Self::PARAMETRIC_SURFACE,
            14 => Self::STRANGE_ATTRACTOR,
            15 => Self::FLUID_SIMULATION,
            16 => Self::NUMBER_STATION,
            17 => Self::MYCELIUM,
            18 => Self::COMPUTE_STRANGE_ATTRACTOR,
            19 => Self::FLUID_SIMULATION_3D,
            20 => Self::MRI_VOLUME,
            _ => Self::NONE,
        }
    }
}

// ── Trait impls ──────────────────────────────────────────────────────────

impl Default for GeneratorTypeId {
    fn default() -> Self {
        Self::NONE
    }
}

impl fmt::Debug for GeneratorTypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GeneratorTypeId({})", self.0)
    }
}

impl fmt::Display for GeneratorTypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for GeneratorTypeId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// ── Serde ───────────────────────────────────────────────────────────────

impl Serialize for GeneratorTypeId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for GeneratorTypeId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::String(s) => Self(Cow::Owned(s.clone())),
            serde_json::Value::Number(n) => {
                Self::from_legacy_discriminant(n.as_i64().unwrap_or(0) as i32)
            }
            _ => Self::NONE,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_equal() {
        assert_eq!(GeneratorTypeId::PLASMA, GeneratorTypeId::PLASMA);
        assert_ne!(GeneratorTypeId::PLASMA, GeneratorTypeId::NONE);
    }

    #[test]
    fn legacy_discriminant_roundtrip() {
        assert_eq!(GeneratorTypeId::from_legacy_discriminant(0), GeneratorTypeId::NONE);
        assert_eq!(GeneratorTypeId::from_legacy_discriminant(6), GeneratorTypeId::PLASMA);
        assert_eq!(GeneratorTypeId::from_legacy_discriminant(20), GeneratorTypeId::MRI_VOLUME);
        assert_eq!(GeneratorTypeId::from_legacy_discriminant(999), GeneratorTypeId::NONE);
    }

    #[test]
    fn serde_string_roundtrip() {
        let id = GeneratorTypeId::PLASMA;
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"Plasma\"");
        let back: GeneratorTypeId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, GeneratorTypeId::PLASMA);
    }

    #[test]
    fn serde_legacy_integer() {
        let back: GeneratorTypeId = serde_json::from_str("6").unwrap();
        assert_eq!(back, GeneratorTypeId::PLASMA);
    }

    #[test]
    fn serde_unknown_integer() {
        let back: GeneratorTypeId = serde_json::from_str("999").unwrap();
        assert_eq!(back, GeneratorTypeId::NONE);
    }

    #[test]
    fn is_none() {
        assert!(GeneratorTypeId::NONE.is_none());
        assert!(!GeneratorTypeId::PLASMA.is_none());
    }

    #[test]
    fn display() {
        assert_eq!(format!("{}", GeneratorTypeId::PLASMA), "Plasma");
    }
}
