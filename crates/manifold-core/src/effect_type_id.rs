//! String-keyed effect type identifier.
//!
//! Replaces the old `EffectType` enum with a `Cow<'static, str>` newtype.
//! Built-in effects use compile-time constants; future plugins can register
//! their own IDs at runtime.

use std::borrow::Cow;
use std::fmt;
use serde::{Serialize, Serializer, Deserialize, Deserializer};

/// Identifies an effect type by name.
///
/// Built-in effects use the associated constants (e.g. `EffectTypeId::BLOOM`).
/// Serializes as a human-readable string; deserializes from string or legacy
/// integer discriminant for backward compatibility with old project files.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct EffectTypeId(Cow<'static, str>);

// ── Construction ────────────────────────────────────────────────────────

impl EffectTypeId {
    /// Create from a runtime string (e.g. plugin-provided ID).
    pub fn from_string(s: String) -> Self {
        Self(Cow::Owned(s))
    }

    /// The underlying string value.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_unknown(&self) -> bool {
        *self == Self::UNKNOWN
    }
}

// ── Built-in constants ──────────────────────────────────────────────────

impl EffectTypeId {
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
    pub const EDGE_GLOW: Self = Self(Cow::Borrowed("EdgeGlow"));
    pub const DATAMOSH: Self = Self(Cow::Borrowed("Datamosh"));
    pub const SLIT_SCAN: Self = Self(Cow::Borrowed("SlitScan"));
    pub const COLOR_GRADE: Self = Self(Cow::Borrowed("ColorGrade"));
    pub const WIREFRAME_DEPTH: Self = Self(Cow::Borrowed("WireframeDepth"));
    pub const CHROMATIC_ABERRATION: Self = Self(Cow::Borrowed("ChromaticAberration"));
    pub const GRADIENT_MAP: Self = Self(Cow::Borrowed("GradientMap"));
    pub const GLITCH: Self = Self(Cow::Borrowed("Glitch"));
    pub const FILM_GRAIN: Self = Self(Cow::Borrowed("FilmGrain"));
    pub const HALATION: Self = Self(Cow::Borrowed("Halation"));
    pub const CORRUPTION: Self = Self(Cow::Borrowed("Corruption"));
    pub const INFRARED: Self = Self(Cow::Borrowed("Infrared"));
    pub const SURVEILLANCE: Self = Self(Cow::Borrowed("Surveillance"));
    pub const REDACTION: Self = Self(Cow::Borrowed("Redaction"));

    /// Placeholder for unrecognized/removed effect types.
    /// Renderers skip this — it never applies any GPU work.
    pub const UNKNOWN: Self = Self(Cow::Borrowed("Unknown"));
}

// ── Legacy discriminant mapping ─────────────────────────────────────────

impl EffectTypeId {
    /// Convert a legacy integer discriminant (from old project files) to an
    /// `EffectTypeId`. Unknown values map to `UNKNOWN`.
    pub fn from_legacy_discriminant(v: i32) -> Self {
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
            25 => Self::EDGE_GLOW,
            26 => Self::DATAMOSH,
            27 => Self::SLIT_SCAN,
            28 => Self::COLOR_GRADE,
            29 => Self::WIREFRAME_DEPTH,
            30 => Self::CHROMATIC_ABERRATION,
            31 => Self::GRADIENT_MAP,
            32 => Self::GLITCH,
            33 => Self::FILM_GRAIN,
            34 => Self::HALATION,
            36 => Self::CORRUPTION,
            37 => Self::INFRARED,
            38 => Self::SURVEILLANCE,
            39 => Self::REDACTION,
            _ => Self::UNKNOWN,
        }
    }
}

// ── Trait impls ──────────────────────────────────────────────────────────

impl Default for EffectTypeId {
    fn default() -> Self {
        Self::TRANSFORM
    }
}

impl fmt::Debug for EffectTypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EffectTypeId({})", self.0)
    }
}

impl fmt::Display for EffectTypeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for EffectTypeId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// ── Serde ───────────────────────────────────────────────────────────────

impl Serialize for EffectTypeId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for EffectTypeId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::String(s) => Self(Cow::Owned(s.clone())),
            serde_json::Value::Number(n) => {
                Self::from_legacy_discriminant(n.as_i64().unwrap_or(0) as i32)
            }
            _ => Self::UNKNOWN,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_equal() {
        assert_eq!(EffectTypeId::BLOOM, EffectTypeId::BLOOM);
        assert_ne!(EffectTypeId::BLOOM, EffectTypeId::TRANSFORM);
    }

    #[test]
    fn legacy_discriminant_roundtrip() {
        assert_eq!(EffectTypeId::from_legacy_discriminant(0), EffectTypeId::TRANSFORM);
        assert_eq!(EffectTypeId::from_legacy_discriminant(12), EffectTypeId::BLOOM);
        assert_eq!(EffectTypeId::from_legacy_discriminant(36), EffectTypeId::CORRUPTION);
        assert_eq!(EffectTypeId::from_legacy_discriminant(999), EffectTypeId::UNKNOWN);
        assert_eq!(EffectTypeId::from_legacy_discriminant(-1), EffectTypeId::UNKNOWN);
    }

    #[test]
    fn serde_string_roundtrip() {
        let id = EffectTypeId::BLOOM;
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"Bloom\"");
        let back: EffectTypeId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, EffectTypeId::BLOOM);
    }

    #[test]
    fn serde_legacy_integer() {
        let back: EffectTypeId = serde_json::from_str("12").unwrap();
        assert_eq!(back, EffectTypeId::BLOOM);
    }

    #[test]
    fn serde_unknown_integer() {
        let back: EffectTypeId = serde_json::from_str("999").unwrap();
        assert_eq!(back, EffectTypeId::UNKNOWN);
    }

    #[test]
    fn serde_unknown_string() {
        let back: EffectTypeId = serde_json::from_str("\"SomeFutureEffect\"").unwrap();
        assert_eq!(back.as_str(), "SomeFutureEffect");
        assert!(!back.is_unknown()); // Not UNKNOWN — it's a valid ID, just unregistered
    }

    #[test]
    fn display() {
        assert_eq!(format!("{}", EffectTypeId::BLOOM), "Bloom");
    }

    #[test]
    fn from_runtime_string() {
        let id = EffectTypeId::from_string("CustomPlugin".to_string());
        assert_eq!(id.as_str(), "CustomPlugin");
    }
}
