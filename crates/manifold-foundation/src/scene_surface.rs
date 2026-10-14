//! The scene surface vocabulary shared by core and ui
//! (SCENE_SURFACES_DESIGN.md section 3.1 (vocabulary)). Declaration order is
//! display order.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfaceRole {
    Performance,
    Setup,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CardBand {
    Camera,
    Scene,
    Forces,
    Lighting,
    Look,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlGroup {
    Camera,
    Lens,
    DepthOfField,
    MotionBlur,
    Light,
    Shadows,
    Transform,
    Material,
    Body,
    Simulation,
    Whitewater,
    Source,
    Look,
    Modifier,
    Force,
    PhysicsWorld,
    Environment,
    Fog,
    RayTracing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CardLabel {
    Param,
    ItemName,
    GroupName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Feature {
    DepthOfField,
    MotionBlur,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "feature")]
pub enum Gate {
    Dim(Feature),
    Fold(Feature),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Unit {
    Metres,
    MetresPerSecond,
    MetresPerSecondSquared,
    KilogramsPerCubicMetre,
    Ev,
    Pixels,
    MillionCells,
    Seconds,
}

impl Unit {
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Metres => " m",
            Self::MetresPerSecond => " m/s",
            Self::MetresPerSecondSquared => " m/s²",
            Self::KilogramsPerCubicMetre => " kg/m³",
            Self::Ev => " EV",
            Self::Pixels => " px",
            Self::MillionCells => " M cells",
            Self::Seconds => " s",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ObjectKind {
    Mesh,
    Plane,
    Water,
    Model,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "object")]
pub enum SceneItemKind {
    Camera,
    Light,
    Object(ObjectKind),
    Force,
    Physics,
    World,
}

/// A user's card pin on one control. `None` on the control follows its role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CardPin {
    Shown,
    Removed,
}

/// Where one scene control lives on the card and in the Scene tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneParamDesc {
    pub role: SurfaceRole,
    pub group: ControlGroup,
    /// `Some` iff `role == Performance`.
    pub band: Option<CardBand>,
    /// Order within (item, band).
    pub card_order: u8,
    pub card_label: CardLabel,
    pub gate: Option<Gate>,
    /// This row turns the feature on and off.
    pub switch: Option<Feature>,
}
