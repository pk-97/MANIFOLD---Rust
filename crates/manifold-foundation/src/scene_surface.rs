//! The scene surface vocabulary shared by core and ui
//! (SCENE_SURFACES_DESIGN.md section 3.1 (vocabulary)). Declaration order is
//! display order.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// How scene gizmos meet the scene's depth, in the preview, the editor
/// viewport and an export alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GizmoDrawMode {
    /// Depth-tested: hidden behind scene geometry.
    Occluded,
    /// Drawn over everything, as gizmos always were.
    #[default]
    OnTop,
    /// Visible parts drawn normally, hidden parts drawn faded.
    Ghost,
}

impl GizmoDrawMode {
    pub const ALL: [Self; 3] = [Self::Occluded, Self::OnTop, Self::Ghost];

    pub fn label(self) -> &'static str {
        match self {
            Self::Occluded => "Occluded",
            Self::OnTop => "On Top",
            Self::Ghost => "Ghost",
        }
    }

    /// The next mode in `ALL`, wrapping.
    pub fn next(self) -> Self {
        Self::ALL[(Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0) + 1) % Self::ALL.len()]
    }
}

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
    RenderMode,
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
            Self::RenderMode => "Render Mode",
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
    /// A fluid source's role: 0 Initial Fill, 1 Inflow, 2 Outflow, 3
    /// Collider.
    SourceRole,
}

impl Feature {
    /// Whether the switch picks a mode rather than turning the feature on.
    pub fn has_modes(self) -> bool {
        matches!(self, Self::LightType | Self::EnvironmentMode | Self::SourceRole)
    }

    /// Why a row gated on this feature does nothing right now.
    pub fn off_reason(self) -> &'static str {
        match self {
            Self::DepthOfField => "Depth of field is off",
            Self::MotionBlur => "Motion blur is off",
            Self::LightType => "This light type does not use it",
            Self::EnvironmentMode => "This environment mode does not use it",
            Self::SourceRole => "This source role does not use it",
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
    /// A rate, such as a drag rate: how much changes per second.
    PerSecond,
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
            Self::PerSecond => " /s",
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
    /// A placed fluid source, listed under the water it feeds.
    Source(SourceRole),
}

/// What a placed fluid source does to its water.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceRole {
    /// The box of water the liquid starts with and resets to.
    InitialFill,
    Inflow,
    Drain,
    Collider,
}

/// Where a scene object row sits (PHYSICS_ENGINE_BOUNDARY_DESIGN.md section
/// 5.2 (One scene-object model)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SceneObjectRowKind {
    /// An ordinary object, a placed fluid source, or a part of an imported
    /// model.
    #[default]
    Object,
    /// The row that owns a simulation or a shared body for its children:
    /// Water's real object, or an imported model's group.
    PhysicsParent,
    /// One of Water's looks (Foam, Spray, Bubbles, Physics View's dots): it
    /// owns its material, Size and visibility only.
    LookChild,
}

/// What a scene object row's commands accept. The tree, the header, the
/// shortcuts and the commands read the same facts, so a shortcut cannot do
/// what a hidden button would.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SceneObjectCapabilities {
    pub rename: bool,
    /// The header's Hide action (a look child's own eye).
    pub hide: bool,
    pub delete: bool,
    pub duplicate: bool,
    /// The row has its own placement transform.
    pub transform: bool,
    pub enable_physics: bool,
    /// The row can feed a liquid as a fluid role.
    pub fluid_role: bool,
    /// The row takes mesh modifiers.
    pub modifiers: bool,
}

/// A Physics World's body slots. Core asserts at compile time that the
/// runtime body capacity (`RigidImpulseTargets::BODY_CAPACITY`) matches.
pub const PHYSICS_WORLD_BODY_SLOTS: u32 = 64;

/// The most copies one Duplicate makes, of any object: one per body slot, so
/// every copy of a physics object can carry its own body.
pub const MAX_DUPLICATE_COPIES: u32 = PHYSICS_WORLD_BODY_SLOTS;

/// Why `copies` copies of a physics object cannot fit a world with `free`
/// body slots; `None` when they fit.
pub fn copies_shortfall(copies: u32, free: u32) -> Option<String> {
    (free < copies).then(|| {
        let slots = if free == 1 { "slot" } else { "slots" };
        let need = if copies == 1 { "copy needs" } else { "copies need" };
        format!("The Physics World has {free} free body {slots}; {copies} {need} {copies}")
    })
}

/// What the one Add menu inserts into a scene through a template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SceneTemplateKind {
    Object,
    Plane,
    Light,
    Water,
    /// A swarm of GPU instances pushed by the scene's forces.
    Swarm,
    /// The placed fluid sources, inserted inside their water.
    InitialFill,
    Inflow,
    Drain,
    Collider,
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

/// Three param ids, one per axis.
pub type ParamTriple = [Cow<'static, str>; 3];

/// A scene object's extent and handles, by the param ids that place it
/// (PHYSICS_ENGINE_BOUNDARY_DESIGN.md section 5.2 (One scene-object model)).
/// Handles, the out-of-domain check and the card all read this one
/// declaration; no kind has handle code of its own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SceneShape {
    /// A solid oriented box: `center` moves, `rotation` rotates, `size`
    /// scales per axis.
    Box {
        center: ParamTriple,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rotation: Option<ParamTriple>,
        size: ParamTriple,
    },
    /// A solid ball: `center` moves, `radius` scales uniformly.
    Sphere { center: ParamTriple, radius: Cow<'static, str> },
    /// An infinite plane through `center`; rotating writes `normal`
    /// normalized.
    Plane { center: ParamTriple, normal: ParamTriple },
    /// A point with a reach: `center` moves, `radius` scales uniformly.
    PointRadius { center: ParamTriple, radius: Cow<'static, str> },
    /// A direction, optionally placed and bounded. Rotating writes
    /// `direction` with its length kept.
    Axis {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<ParamTriple>,
        direction: ParamTriple,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radius: Option<Cow<'static, str>>,
    },
}

impl SceneShape {
    /// Every param id the shape names.
    pub fn param_ids(&self) -> impl Iterator<Item = &str> {
        let (triples, singles): (Vec<&ParamTriple>, Vec<&Cow<'static, str>>) = match self {
            Self::Box { center, rotation, size } => {
                (std::iter::once(center).chain(rotation).chain(std::iter::once(size)).collect(), Vec::new())
            }
            Self::Sphere { center, radius } | Self::PointRadius { center, radius } => (vec![center], vec![radius]),
            Self::Plane { center, normal } => (vec![center, normal], Vec::new()),
            Self::Axis { origin, direction, radius } => {
                (origin.iter().chain(std::iter::once(direction)).collect(), radius.iter().collect())
            }
        };
        triples.into_iter().flatten().chain(singles).map(|id| id.as_ref())
    }
}

/// One declared shape of a scene object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneShapeDecl {
    #[serde(flatten)]
    pub shape: SceneShape,
    /// The input port whose upstream `node.transform_3d` owns the params;
    /// `None` when the declaring node or card owns them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Cow<'static, str>>,
    /// The hard limit on every size and radius the shape names, in their
    /// storage unit. A scale handle stops at it as a knob stops at its
    /// param's limit. `None` keeps only the gizmo's smallest extent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extent_limit: Option<crate::param_range::HardLimit>,
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    #[test]
    fn shape_declaration_reads_the_manifest_form() {
        let decl: SceneShapeDecl = serde_json::from_str(
            r#"{"kind": "point_radius", "center": ["center_x", "center_y", "center_z"], "radius": "radius"}"#,
        )
        .unwrap();
        assert_eq!(
            decl,
            SceneShapeDecl {
                shape: SceneShape::PointRadius {
                    center: [Cow::Borrowed("center_x"), Cow::Borrowed("center_y"), Cow::Borrowed("center_z")],
                    radius: Cow::Borrowed("radius"),
                },
                via: None,
                extent_limit: None,
            }
        );
        let axis: SceneShapeDecl =
            serde_json::from_str(r#"{"kind": "axis", "direction": ["x", "y", "z"], "via": "transform"}"#).unwrap();
        assert!(matches!(axis.shape, SceneShape::Axis { origin: None, radius: None, .. }));
        assert_eq!(axis.via.as_deref(), Some("transform"));
        let round: SceneShapeDecl = serde_json::from_str(&serde_json::to_string(&axis).unwrap()).unwrap();
        assert_eq!(round, axis);
    }
}
