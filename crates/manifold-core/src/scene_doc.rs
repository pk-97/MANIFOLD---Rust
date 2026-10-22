//! The scene document: a scene layer's source of truth
//! (SCENE_DOCUMENT_DESIGN.md section 7.1 (The scene document)).
//!
//! A scene layer stores a tree of typed items on its generator instance
//! (`PresetInstance::scene`). The render graph is compiled from it by the
//! compiler the scene crate registers ([`SceneCompiler`]) and is never saved.
//! Structural properties are item fields, set by the scene document commands
//! in `manifold-editing`; every live property is a param in the layer's
//! manifest with id `<item id>.<property>` ([`param_id`]), created by the
//! compiler from the kind's table (`crate::scene_schema`).
//!
//! The format is closed: every type here denies unknown fields.

mod base;
mod tree;


pub use manifold_foundation::id::ItemId;
pub use tree::SceneEditError;

use serde::{Deserialize, Serialize};

use crate::effect_graph_def::EffectGraphDef;
use crate::material_inspector::MaterialFeature;

/// One scene layer's document: the items at the top of the tree, in order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SceneDoc {
    pub items: Vec<Item>,
}

/// One item: a stable id, a display name, its kind's structural fields, and
/// the items under it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Item {
    pub id: ItemId,
    pub name: String,
    pub fields: ItemFields,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Item>,
}

/// The ten kinds (ruling 4). A layer plane is an Object; Copies is a section;
/// an import is a Group whose Source is a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemKind {
    Group,
    Object,
    Camera,
    Light,
    Material,
    Water,
    WaterSource,
    Swarm,
    Force,
    World,
}

impl ItemKind {
    pub const ALL: [ItemKind; 10] = [
        ItemKind::Group,
        ItemKind::Object,
        ItemKind::Camera,
        ItemKind::Light,
        ItemKind::Material,
        ItemKind::Water,
        ItemKind::WaterSource,
        ItemKind::Swarm,
        ItemKind::Force,
        ItemKind::World,
    ];

    /// The name a new item of this kind takes.
    pub fn default_name(self) -> &'static str {
        match self {
            ItemKind::Group => "Group",
            ItemKind::Object => "Object",
            ItemKind::Camera => "Camera",
            ItemKind::Light => "Light",
            ItemKind::Material => "Material",
            ItemKind::Water => "Water",
            ItemKind::WaterSource => "Water Source",
            ItemKind::Swarm => "Swarm",
            ItemKind::Force => "Force",
            ItemKind::World => "World",
        }
    }
}

/// An item's structural fields, by kind. Set by commands, never mapped or
/// modulated (ruling 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemFields {
    Group(GroupFields),
    Object(ObjectFields),
    Camera(CameraFields),
    Light(LightFields),
    Material(MaterialFields),
    Water(WaterFields),
    WaterSource(WaterSourceFields),
    Swarm(SwarmFields),
    Force(ForceFields),
    World(WorldFields),
}

impl ItemFields {
    pub fn kind(&self) -> ItemKind {
        match self {
            ItemFields::Group(_) => ItemKind::Group,
            ItemFields::Object(_) => ItemKind::Object,
            ItemFields::Camera(_) => ItemKind::Camera,
            ItemFields::Light(_) => ItemKind::Light,
            ItemFields::Material(_) => ItemKind::Material,
            ItemFields::Water(_) => ItemKind::Water,
            ItemFields::WaterSource(_) => ItemKind::WaterSource,
            ItemFields::Swarm(_) => ItemKind::Swarm,
            ItemFields::Force(_) => ItemKind::Force,
            ItemFields::World(_) => ItemKind::World,
        }
    }

    /// A new item's fields.
    pub fn default_for(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Group => ItemFields::Group(GroupFields::default()),
            ItemKind::Object => ItemFields::Object(ObjectFields::default()),
            ItemKind::Camera => ItemFields::Camera(CameraFields::default()),
            ItemKind::Light => ItemFields::Light(LightFields::default()),
            ItemKind::Material => ItemFields::Material(MaterialFields::default()),
            ItemKind::Water => ItemFields::Water(WaterFields::default()),
            ItemKind::WaterSource => ItemFields::WaterSource(WaterSourceFields::default()),
            ItemKind::Swarm => ItemFields::Swarm(SwarmFields::default()),
            ItemKind::Force => ItemFields::Force(ForceFields::default()),
            ItemKind::World => ItemFields::World(WorldFields::default()),
        }
    }

    /// The material this item binds itself, before inheritance.
    pub fn material(&self) -> Option<&ItemId> {
        match self {
            ItemFields::Group(g) => g.material.as_ref(),
            ItemFields::Object(o) => o.material.as_ref(),
            _ => None,
        }
    }

    /// The physics setting of an Object or Group.
    pub fn physics(&self) -> PhysicsSetting {
        match self {
            ItemFields::Group(g) => g.physics,
            ItemFields::Object(o) => o.physics,
            _ => PhysicsSetting::None,
        }
    }
}

/// None, Collider or Body (ruling 9). There is no fourth setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PhysicsSetting {
    #[default]
    None,
    Collider,
    Body,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct GroupFields {
    /// Applies to every item below with no material of its own.
    pub material: Option<ItemId>,
    pub physics: PhysicsSetting,
    pub copies: bool,
    /// An import: the file this group's children come from (section 7.7).
    pub source: Option<String>,
    pub combine_meshes: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ObjectFields {
    pub shape: Shape,
    pub material: Option<ItemId>,
    pub physics: PhysicsSetting,
    pub copies: bool,
}

/// An Object's Shape: the built-in solids, Ocean, or a mesh from a file.
/// The built-ins are the solids a node generates today.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Shape {
    #[default]
    Cube,
    Plane,
    Tetrahedron,
    Octahedron,
    Icosahedron,
    Dodecahedron,
    Ocean,
    #[serde(rename_all = "camelCase")]
    Mesh { file: String, node_path: String },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct CameraFields {
    pub rig: CameraRig,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CameraRig {
    #[default]
    Orbit,
    Free,
    LookAt,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct LightFields {
    pub light: LightType,
}

/// A light's type. Environment carries where its image comes from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LightType {
    #[default]
    Sun,
    Point,
    Spot,
    #[serde(rename_all = "camelCase")]
    Environment { source: EnvironmentSource },
}

/// Where the environment light comes from (section 7.5): a procedural sky,
/// an HDRI file (the picker is always on the card in this mode, so the file
/// may be empty), or nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentSource {
    #[serde(rename_all = "camelCase")]
    Sky { style: SkyStyle },
    #[serde(rename_all = "camelCase")]
    Hdri { file: String },
    None,
}

impl Default for EnvironmentSource {
    fn default() -> Self {
        EnvironmentSource::Sky { style: SkyStyle::Softbox }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SkyStyle {
    Gradient,
    #[default]
    Softbox,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct MaterialFields {
    pub texture: TextureSource,
    /// The features added from "+ Add feature", in the inspector's order,
    /// each with its mode (section 7.6).
    pub features: Vec<FeatureSetting>,
}

/// A Material's image: none, a file, or another layer's output (today's
/// Skin Source).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextureSource {
    #[default]
    None,
    #[serde(rename_all = "camelCase")]
    File { path: String },
    #[serde(rename_all = "camelCase")]
    Layer { layer: crate::LayerId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FeatureSetting {
    pub feature: MaterialFeature,
    pub mode: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct WaterFields {}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct WaterSourceFields {
    pub role: SourceRole,
    pub shape: SourceShape,
}

/// An open list: mesh-shaped fills and absorb zones join here later
/// (section 7.18).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceRole {
    #[default]
    Fill,
    Inflow,
    Drain,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceShape {
    #[default]
    Box,
    Sphere,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct SwarmFields {
    pub emit_from: EmitFrom,
    pub draw_as: DrawAs,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EmitFrom {
    #[default]
    Box,
    Sphere,
    /// The surface of an Object or Group.
    Item(ItemId),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DrawAs {
    #[default]
    Tetrahedron,
    Cube,
    Octahedron,
    Icosahedron,
    Sphere,
    /// Any Object or Group, drawn per particle.
    Item(ItemId),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ForceFields {
    pub force: ForceType,
    pub targets: ForceTargets,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ForceType {
    #[default]
    Radial,
    Uniform,
    Vortex,
    Wind,
    Drag,
    Attractor,
    Explosion,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ForceTargets {
    /// Everything with physics.
    #[default]
    Physics,
    Items(Vec<ItemId>),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct WorldFields {
    /// Fog adds the atmosphere pass; off in the base scene.
    pub fog: bool,
    /// The scene's default active camera; the first camera when unset.
    pub active_camera: Option<ItemId>,
}

/// A live property's param id: `<item id>.<property>` (ruling 7). Renaming
/// or reparenting an item never changes it.
pub fn param_id(item: &ItemId, property: &str) -> String {
    format!("{item}.{property}")
}

/// The item and property a scene param id names.
pub fn split_param_id(id: &str) -> Option<(&str, &str)> {
    id.split_once('.')
}

/// The scene compiler, registered by the crate that owns the node vocabulary
/// (`manifold-nodes-scene`), so the project loader and the editing commands
/// compile without depending on it. A pure function of the document: the
/// same document always gives the same graph.
pub struct SceneCompiler {
    pub compile: fn(&SceneDoc) -> Result<EffectGraphDef, String>,
}
inventory::collect!(SceneCompiler);

/// Compile `doc` with the registered compiler.
pub fn compile(doc: &SceneDoc) -> Result<EffectGraphDef, String> {
    let mut compilers = inventory::iter::<SceneCompiler>.into_iter();
    let compiler = compilers.next().ok_or("no scene compiler is linked")?;
    debug_assert!(compilers.next().is_none(), "duplicate scene compiler");
    (compiler.compile)(doc)
}
