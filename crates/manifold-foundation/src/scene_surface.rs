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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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

impl ControlGroup {
    /// The group's name, shown as its section title in the Scene tab and as
    /// the card label of a `CardLabel::GroupName` row.
    pub fn title(self) -> &'static str {
        match self {
            Self::Camera => "Camera",
            Self::Lens => "Lens",
            Self::DepthOfField => "Depth of Field",
            Self::MotionBlur => "Motion Blur",
            Self::Light => "Light",
            Self::Shadows => "Shadows",
            Self::Transform => "Transform",
            Self::Material => "Material",
            Self::Body => "Body",
            Self::Simulation => "Simulation",
            Self::Whitewater => "Whitewater",
            Self::Source => "Source",
            Self::Look => "Look",
            Self::Modifier => "Modifier",
            Self::Force => "Force",
            Self::PhysicsWorld => "Physics",
            Self::Environment => "Environment",
            Self::Fog => "Fog",
            Self::RayTracing => "Ray Tracing",
        }
    }
}

impl CardBand {
    /// The card section title of a band whose rows share one section.
    /// `Scene` and `Forces` are titled per item instead.
    pub fn title(self) -> &'static str {
        match self {
            Self::Camera => "Camera",
            Self::Scene => "Scene",
            Self::Forces => "Forces",
            Self::Lighting => "Lighting",
            Self::Look => "Look",
        }
    }
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

impl Feature {
    /// Why a row gated on this feature does nothing right now.
    pub fn off_reason(self) -> &'static str {
        match self {
            Self::DepthOfField => "Depth of field is off",
            Self::MotionBlur => "Motion blur is off",
        }
    }
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
    Nanometres,
    Beats,
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
            Self::Nanometres => " nm",
            Self::Beats => " beats",
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
    /// One channel of its item's colour. The card's colour swatch writes
    /// the item's three channels as one edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<RgbChannel>,
}

/// One channel of an RGB colour stored as three scalar params.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RgbChannel {
    R,
    G,
    B,
}

impl RgbChannel {
    /// Position in an `[r, g, b]` triple.
    pub fn index(self) -> usize {
        match self {
            Self::R => 0,
            Self::G => 1,
            Self::B => 2,
        }
    }
}
