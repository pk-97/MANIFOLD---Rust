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
    ShadowsAdvanced,
    Transform,
    Material,
    Body,
    Simulation,
    Whitewater,
    WhitewaterAdvanced,
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
    /// the card label of a `CardLabel::GroupName` row. A title starting
    /// "Advanced · " starts folded in the Scene tab.
    pub fn title(self) -> &'static str {
        match self {
            Self::Camera => "Camera",
            Self::Lens => "Lens",
            Self::DepthOfField => "Depth of Field",
            Self::MotionBlur => "Motion Blur",
            Self::Light => "Light",
            Self::Shadows => "Shadows",
            Self::ShadowsAdvanced => "Advanced · Shadows",
            Self::Transform => "Transform",
            Self::Material => "Material",
            Self::Body => "Body",
            Self::Simulation => "Simulation",
            Self::Whitewater => "Whitewater",
            Self::WhitewaterAdvanced => "Advanced · Whitewater",
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
    /// The owning item's name, then the param's label in lower case
    /// ("Sun intensity").
    ItemName,
    GroupName,
}

/// What a switch row controls. A plain feature is on above 0.5; a feature
/// with modes reads its switch's whole value as the mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Feature {
    DepthOfField,
    MotionBlur,
    /// A light's type: 0 Sun, 1 Point, 2 Spot.
    LightType,
    /// An environment bake's mode: 0 Gradient, 1 Softbox.
    EnvironmentMode,
}

impl Feature {
    /// Whether the switch picks a mode rather than turning the feature on.
    pub fn has_modes(self) -> bool {
        matches!(self, Self::LightType | Self::EnvironmentMode)
    }

    /// Why a row gated on this feature does nothing right now.
    pub fn off_reason(self) -> &'static str {
        match self {
            Self::DepthOfField => "Depth of field is off",
            Self::MotionBlur => "Motion blur is off",
            Self::LightType => "This light type does not use it",
            Self::EnvironmentMode => "This environment mode does not use it",
        }
    }
}

/// The modes of a feature with modes that read a row, one bit per mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Modes(pub u8);

impl Modes {
    pub const fn contains(self, mode: u32) -> bool {
        mode < 8 && self.0 & (1 << mode) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "feature")]
pub enum Gate {
    Dim(Feature),
    Fold(Feature),
    /// Read only in these modes of the owning item's switch. The Scene tab
    /// leaves the row out in any other mode.
    Only(Feature, Modes),
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
    /// This row turns the feature on and off, or picks its mode.
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
