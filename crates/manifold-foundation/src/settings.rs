//! RT Quality settings types that can be used across UI, core, and editing.
//! These types were originally added to manifold_core::settings but need to be
//! accessible to the UI layer (which can only depend on foundation).

use serde::{Deserialize, Serialize};

/// Six-step spp ladder, shared by all RT features (RT_QUALITY_SETTINGS_DESIGN.md D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RtQualityTier {
    UltraLow,
    Low,
    #[default]
    Medium,
    High,
    ExtraHigh,
    Ultra,
}

impl RtQualityTier {
    pub fn spp(self) -> u32 {
        match self {
            RtQualityTier::UltraLow => 1,
            RtQualityTier::Low => 2,
            RtQualityTier::Medium => 4,
            RtQualityTier::High => 8,
            RtQualityTier::ExtraHigh => 16,
            RtQualityTier::Ultra => 32,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RtQualityTier::UltraLow => "Ultra Low (1 spp)",
            RtQualityTier::Low => "Low (2 spp)",
            RtQualityTier::Medium => "Medium (4 spp)",
            RtQualityTier::High => "High (8 spp)",
            RtQualityTier::ExtraHigh => "Extra High (16 spp)",
            RtQualityTier::Ultra => "Ultra (32 spp)",
        }
    }

    /// UI order (worst to best).
    pub const ALL: [RtQualityTier; 6] = [
        RtQualityTier::UltraLow,
        RtQualityTier::Low,
        RtQualityTier::Medium,
        RtQualityTier::High,
        RtQualityTier::ExtraHigh,
        RtQualityTier::Ultra,
    ];
}

/// RT dispatch resolution relative to native canvas (RT_QUALITY_SETTINGS_DESIGN.md D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RtRayResolution {
    Quarter,
    #[default]
    Half,
    ThreeQuarter,
    Native,
}

impl RtRayResolution {
    /// (numerator, denominator) — integer fraction, same discipline as
    /// `output_canvas_scale`'s (num, den) at render_scene.rs:284+.
    pub fn fraction(self) -> (u32, u32) {
        match self {
            RtRayResolution::Quarter => (1, 4),
            RtRayResolution::Half => (1, 2),
            RtRayResolution::ThreeQuarter => (3, 4),
            RtRayResolution::Native => (1, 1),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RtRayResolution::Quarter => "25% (quarter)",
            RtRayResolution::Half => "50% (half)",
            RtRayResolution::ThreeQuarter => "75% (three-quarter)",
            RtRayResolution::Native => "100% (native)",
        }
    }

    /// UI order (lowest to highest quality).
    pub const ALL: [RtRayResolution; 4] = [
        RtRayResolution::Quarter,
        RtRayResolution::Half,
        RtRayResolution::ThreeQuarter,
        RtRayResolution::Native,
    ];
}

/// Which spp-tier row a quality control targets — by identity, never by
/// matching the current value (two rows sharing a tier would misroute the
/// edit). Shared by the RT quality panel (manifold-ui) and the dropdown
/// builder (manifold-app), so it lives here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtTierField {
    Shadows,
    Ao,
    Gi,
    Reflections,
}

impl RtTierField {
    pub fn get(self, col: &RtQualityColumn) -> RtQualityTier {
        match self {
            RtTierField::Shadows => col.shadows,
            RtTierField::Ao => col.ao,
            RtTierField::Gi => col.gi,
            RtTierField::Reflections => col.reflections,
        }
    }

    pub fn set(self, col: &mut RtQualityColumn, tier: RtQualityTier) {
        match self {
            RtTierField::Shadows => col.shadows = tier,
            RtTierField::Ao => col.ao = tier,
            RtTierField::Gi => col.gi = tier,
            RtTierField::Reflections => col.reflections = tier,
        }
    }
}

/// One column of the grid — the values one usage mode (live or export) runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RtQualityColumn {
    pub shadows: RtQualityTier,
    pub ao: RtQualityTier,
    pub gi: RtQualityTier,
    pub reflections: RtQualityTier,
    pub ray_resolution: RtRayResolution,
}

impl Default for RtQualityColumn {
    fn default() -> Self {
        Self {
            shadows: RtQualityTier::UltraLow,
            ao: RtQualityTier::Medium,
            gi: RtQualityTier::Medium,
            reflections: RtQualityTier::High,
            ray_resolution: RtRayResolution::Half,
        }
    }
}

impl RtQualityColumn {
    /// Export default column: shadows High, ao/gi High, reflections ExtraHigh, ray Native.
    pub fn export_default() -> Self {
        Self {
            shadows: RtQualityTier::High,
            ao: RtQualityTier::High,
            gi: RtQualityTier::High,
            reflections: RtQualityTier::ExtraHigh,
            ray_resolution: RtRayResolution::Native,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RtQualitySettings {
    pub realtime: RtQualityColumn,
    #[serde(default = "RtQualityColumn::export_default")]
    pub export: RtQualityColumn,
}

impl Default for RtQualitySettings {
    fn default() -> Self {
        Self {
            realtime: RtQualityColumn::default(),
            export: RtQualityColumn::export_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rt_quality_tier_spp_ladder() {
        assert_eq!(RtQualityTier::UltraLow.spp(), 1);
        assert_eq!(RtQualityTier::Low.spp(), 2);
        assert_eq!(RtQualityTier::Medium.spp(), 4);
        assert_eq!(RtQualityTier::High.spp(), 8);
        assert_eq!(RtQualityTier::ExtraHigh.spp(), 16);
        assert_eq!(RtQualityTier::Ultra.spp(), 32);
    }

    #[test]
    fn rt_quality_tier_spp_always_positive() {
        for tier in [
            RtQualityTier::UltraLow,
            RtQualityTier::Low,
            RtQualityTier::Medium,
            RtQualityTier::High,
            RtQualityTier::ExtraHigh,
            RtQualityTier::Ultra,
        ] {
            assert!(tier.spp() >= 1, "spp() must be >= 1 for {:?}", tier);
        }
    }

    #[test]
    fn rt_quality_live_defaults_match_constants() {
        let live = RtQualityColumn::default();
        assert_eq!(live.shadows, RtQualityTier::UltraLow);
        assert_eq!(live.ao, RtQualityTier::Medium);
        assert_eq!(live.gi, RtQualityTier::Medium);
        assert_eq!(live.reflections, RtQualityTier::High);
        assert_eq!(live.ray_resolution, RtRayResolution::Half);
        assert_eq!(live.shadows.spp(), 1);
        assert_eq!(live.ao.spp(), 4);
        assert_eq!(live.gi.spp(), 4);
        assert_eq!(live.reflections.spp(), 8);
        assert_eq!(live.ray_resolution.fraction(), (1, 2));
    }

    #[test]
    fn rt_quality_export_defaults() {
        let export = RtQualityColumn::export_default();
        assert_eq!(export.shadows, RtQualityTier::High);
        assert_eq!(export.ao, RtQualityTier::High);
        assert_eq!(export.gi, RtQualityTier::High);
        assert_eq!(export.reflections, RtQualityTier::ExtraHigh);
        assert_eq!(export.ray_resolution, RtRayResolution::Native);
        assert_eq!(export.shadows.spp(), 8);
        assert_eq!(export.ao.spp(), 8);
        assert_eq!(export.gi.spp(), 8);
        assert_eq!(export.reflections.spp(), 16);
        assert_eq!(export.ray_resolution.fraction(), (1, 1));
    }

    #[test]
    fn rt_quality_serde_round_trip() {
        let settings = RtQualitySettings {
            realtime: RtQualityColumn {
                shadows: RtQualityTier::Low,
                ao: RtQualityTier::ExtraHigh,
                gi: RtQualityTier::High,
                reflections: RtQualityTier::Ultra,
                ray_resolution: RtRayResolution::Quarter,
            },
            export: RtQualityColumn {
                shadows: RtQualityTier::ExtraHigh,
                ao: RtQualityTier::Ultra,
                gi: RtQualityTier::ExtraHigh,
                reflections: RtQualityTier::Ultra,
                ray_resolution: RtRayResolution::ThreeQuarter,
            },
        };

        let json = serde_json::to_string(&settings).unwrap();
        let deserialized: RtQualitySettings = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.realtime.shadows, RtQualityTier::Low);
        assert_eq!(deserialized.realtime.ao, RtQualityTier::ExtraHigh);
        assert_eq!(deserialized.realtime.gi, RtQualityTier::High);
        assert_eq!(deserialized.realtime.reflections, RtQualityTier::Ultra);
        assert_eq!(deserialized.realtime.ray_resolution, RtRayResolution::Quarter);

        assert_eq!(deserialized.export.shadows, RtQualityTier::ExtraHigh);
        assert_eq!(deserialized.export.ao, RtQualityTier::Ultra);
        assert_eq!(deserialized.export.gi, RtQualityTier::ExtraHigh);
        assert_eq!(deserialized.export.reflections, RtQualityTier::Ultra);
        assert_eq!(deserialized.export.ray_resolution, RtRayResolution::ThreeQuarter);
    }

    #[test]
    fn rt_ray_resolution_fraction() {
        assert_eq!(RtRayResolution::Quarter.fraction(), (1, 4));
        assert_eq!(RtRayResolution::Half.fraction(), (1, 2));
        assert_eq!(RtRayResolution::ThreeQuarter.fraction(), (3, 4));
        assert_eq!(RtRayResolution::Native.fraction(), (1, 1));
    }
}