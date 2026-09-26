//! Persisted semantic descriptors for the material inspector.
//!
//! These types describe the meaning of a material parameter at the manifest
//! boundary.  The renderer owns the classification table; core owns this
//! small, dependency-free vocabulary so descriptors can be saved and restored
//! without depending on renderer or UI types.

use serde::{Deserialize, Serialize};

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
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaterialGroup {
    Surface,
    Opacity,
    Subsurface,
    Feature(MaterialFeature),
    Advanced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaterialColour {
    Base,
    Specular,
    Emission,
    Sheen,
    Attenuation,
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
