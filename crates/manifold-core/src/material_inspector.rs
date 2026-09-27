//! Persisted semantic descriptors for the material inspector.
//!
//! These types describe the meaning of a material parameter at the manifest
//! boundary.  The renderer owns the classification table; core owns this
//! small, dependency-free vocabulary so descriptors can be saved and restored
//! without depending on renderer or UI types.

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaterialFeature {
    Coat,
    Iridescence,
    Emission,
    Glass,
    Sheen,
    Anisotropy,
    Translucency,
    Subsurface,
}

/// Optional material features in their stable inspector order.
pub const MATERIAL_FEATURES: &[MaterialFeature] = &[
    MaterialFeature::Coat,
    MaterialFeature::Iridescence,
    MaterialFeature::Emission,
    MaterialFeature::Glass,
    MaterialFeature::Sheen,
    MaterialFeature::Anisotropy,
    MaterialFeature::Translucency,
    MaterialFeature::Subsurface,
];

/// Feature-mode parameter names in the same order as [`MATERIAL_FEATURES`].
pub const MATERIAL_FEATURE_MODE_PARAMS: &[&str] = &[
    "coat_mode",
    "iridescence_mode",
    "emission_mode",
    "glass_mode",
    "sheen_mode",
    "anisotropy_mode",
    "translucency_mode",
    "subsurface_feature_mode",
];
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MaterialGroup {
    Surface,
    Opacity,
    Feature(MaterialFeature),
    Advanced,
}

/// Wire compatibility for projects written while Subsurface was presented as
/// its own material group. The runtime vocabulary is unified under
/// `Feature(Subsurface)`; legacy values are normalized as they deserialize.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum MaterialGroupWire {
    Surface,
    Opacity,
    Subsurface,
    Feature(MaterialFeature),
    Advanced,
}

impl<'de> Deserialize<'de> for MaterialGroup {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match MaterialGroupWire::deserialize(deserializer)? {
            MaterialGroupWire::Surface => Self::Surface,
            MaterialGroupWire::Opacity => Self::Opacity,
            MaterialGroupWire::Subsurface => Self::Feature(MaterialFeature::Subsurface),
            MaterialGroupWire::Feature(feature) => Self::Feature(feature),
            MaterialGroupWire::Advanced => Self::Advanced,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaterialColour {
    Base,
    Specular,
    Emission,
    Sheen,
    Attenuation,
    VolumeScattering,
    Subsurface,
    Translucency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RgbChannel {
    R,
    G,
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaterialMapFamily {
    Base,
    Normal,
    MetallicRoughness,
    Occlusion,
    Emission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UvComponent {
    M00,
    M01,
    M10,
    M11,
    Tx,
    Ty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SamplerComponent {
    WrapU,
    WrapV,
    MagFilter,
    MinFilter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaterialParamRole {
    Scalar(MaterialGroup),
    Colour(MaterialGroup, MaterialColour, RgbChannel),
    FeatureMode(MaterialFeature),
    Placement(MaterialMapFamily, UvComponent),
    Sampler(MaterialMapFamily, SamplerComponent),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_subsurface_roles_normalize_on_load_and_resave() {
        let scalar_json = serde_json::json!({ "scalar": "subsurface" });
        let colour_json = serde_json::json!({
            "colour": ["subsurface", "subsurface", "r"]
        });
        let scalar: MaterialParamRole = serde_json::from_value(scalar_json.clone()).unwrap();
        let colour: MaterialParamRole = serde_json::from_value(colour_json.clone()).unwrap();
        let expected_group = MaterialGroup::Feature(MaterialFeature::Subsurface);
        assert_eq!(scalar, MaterialParamRole::Scalar(expected_group));
        assert_eq!(
            colour,
            MaterialParamRole::Colour(expected_group, MaterialColour::Subsurface, RgbChannel::R)
        );

        for (role, legacy) in [(scalar, scalar_json), (colour, colour_json)] {
            let canonical = serde_json::to_value(role).unwrap();
            assert_ne!(canonical, legacy);
            assert_eq!(
                serde_json::from_value::<MaterialParamRole>(canonical.clone()).unwrap(),
                role
            );
            assert!(!canonical.to_string().contains("\"scalar\":\"subsurface\""));
            assert!(!canonical.to_string().contains("\"colour\":[\"subsurface\""));
        }
    }
}
