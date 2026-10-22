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
mod import;
mod tree;

pub use base::{BASE_CUBE, BASE_CUBE_MATERIAL};
pub use manifold_foundation::id::ItemId;
pub use import::{
    ImportPlacement, ImportedCamera, ImportedLight, ImportedMaterial, ImportedModel, ImportedNode, camera_item, import_child_id,
    is_import_child, light_item, material_item, node_item,
};
pub use tree::{SceneEditError, can_hold};

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

pub use manifold_foundation::scene_surface::ItemKind;

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
    Modifier(ModifierFields),
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
            ItemFields::Modifier(_) => ItemKind::Modifier,
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
            ItemKind::Modifier => ItemFields::Modifier(ModifierFields::default()),
            ItemKind::World => ItemFields::World(WorldFields::default()),
        }
    }

    /// The material this item binds itself, before inheritance.
    pub fn material(&self) -> Option<&ItemId> {
        match self {
            ItemFields::Group(g) => g.material.as_ref(),
            ItemFields::Object(o) => o.material.as_ref(),
            ItemFields::Swarm(s) => s.material.as_ref(),
            _ => None,
        }
    }

    /// Whether an Object or Group has Copies on.
    pub fn copies(&self) -> bool {
        match self {
            ItemFields::Group(g) => g.copies,
            ItemFields::Object(o) => o.copies,
            _ => false,
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
    /// An import's animation clip: which of the file's clips every
    /// animated part below plays. Structural, so it is never mapped; Rate,
    /// Loop Mode and Retrigger are the live rows.
    pub clip: u32,
    /// An import's clip names, in file order: what Clip picks from.
    pub clip_names: Vec<String>,
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
    Sphere,
    Plane,
    Tetrahedron,
    Octahedron,
    Icosahedron,
    Dodecahedron,
    Ocean,
    #[serde(rename_all = "camelCase")]
    Mesh {
        file: String,
        node_path: String,
        /// Vertices after the file's triangles are expanded, read at import: the
        /// mesh node's buffer is sized from it (0 until known).
        #[serde(default)]
        vertex_count: u32,
        /// Which of the node's primitives this Object draws: a node whose mesh
        /// carries several materials is one Object per material.
        #[serde(default, skip_serializing_if = "MeshPrimitives::is_all")]
        primitives: MeshPrimitives,
    },
}

/// The primitives of a file node's mesh that one Object draws.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MeshPrimitives {
    /// Every primitive.
    #[default]
    All,
    /// The primitives of one glTF material, by its index in the file.
    #[serde(rename_all = "camelCase")]
    Material { index: u32 },
    /// The primitives with no material (glTF's default material).
    NoMaterial,
}

impl MeshPrimitives {
    pub fn is_all(&self) -> bool {
        *self == MeshPrimitives::All
    }

    /// The glTF material these primitives use (`None` is the default
    /// material); `None` for every primitive.
    pub fn material(&self) -> Option<Option<u32>> {
        match self {
            MeshPrimitives::All => None,
            MeshPrimitives::Material { index } => Some(Some(*index)),
            MeshPrimitives::NoMaterial => Some(None),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct CameraFields {
    pub rig: CameraRig,
    /// The Object or Group a Look At camera aims at, as Blender's Track To
    /// aims at an object; its Target X/Y/Z then offset from that item's
    /// origin. `None` aims at the Target X/Y/Z point.
    pub target: Option<ItemId>,
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
    /// The shading model: lit PBR, flat Unlit or banded Cel.
    pub material_type: MaterialType,
    pub texture: TextureSource,
    /// Which surface a layer texture feeds.
    pub texture_feeds: TextureFeeds,
    /// The features added from "+ Add feature", in the inspector's order,
    /// each with its mode (section 7.6).
    pub features: Vec<FeatureSetting>,
}

/// How a Material shades: `node.pbr_material`, `node.unlit_material` (flat
/// colour, no lights) or `node.cel_material` (toon bands).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaterialType {
    #[default]
    Pbr,
    Unlit,
    Cel,
}

impl MaterialType {
    /// The material node this type compiles to.
    pub const fn node_type(self) -> &'static str {
        match self {
            Self::Pbr => "node.pbr_material",
            Self::Unlit => "node.unlit_material",
            Self::Cel => "node.cel_material",
        }
    }
}

/// The surface a layer texture feeds on a Material.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextureFeeds {
    #[default]
    BaseColour,
    Emission,
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
    /// A material of a model file (section 7.7): its factors, every map and
    /// every extension, read from the file when the scene compiles.
    /// `material` is the glTF material index; `None` is glTF's default
    /// material, for primitives with none.
    #[serde(rename_all = "camelCase")]
    Model { file: String, material: Option<u32> },
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
    /// What every particle draws with; none draws the scene's plain material.
    pub material: Option<ItemId>,
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

/// One card of the scene's modifier chain (SCENE_DOCUMENT_DESIGN.md section
/// 7.19 (Modifiers)). The chain is the scene's Modifier items in tree order,
/// which is the order they apply. Type, On, Targets, Pieces and the place in
/// the chain are structural; every other row is a live param.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ModifierFields {
    pub modifier: ModifierType,
    /// Off leaves the card in the chain acting on nothing.
    pub on: bool,
    pub targets: ModifierTargets,
    /// How many pieces Shatter breaks a body into. Setup: it decides which
    /// bodies exist.
    pub pieces: u32,
}

/// Shatter's Pieces when a card is new.
pub const DEFAULT_PIECES: u32 = 16;

impl Default for ModifierFields {
    fn default() -> Self {
        Self { modifier: ModifierType::default(), on: true, targets: ModifierTargets::default(), pieces: DEFAULT_PIECES }
    }
}

/// What a modifier card does. Each type's recipe, rows and label are one
/// entry of `scene_schema::modifier::MODIFIER_TYPES`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModifierType {
    #[default]
    SurfacePeel,
    OrderedRecon,
    SurfaceWaves,
    SpatialEchoes,
    ElasticSculpture,
    VortexFragments,
    Shatter,
}

/// What a modifier card acts on: the whole scene, as one group of
/// everything, or chosen Objects and Groups.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModifierTargets {
    #[default]
    Scene,
    Items(Vec<ItemId>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct WorldFields {
    /// Fog adds the atmosphere pass; off in the base scene.
    pub fog: bool,
    /// Ambient occlusion adds the screen-space contact shadow pass; on in the
    /// base scene.
    pub ambient_occlusion: bool,
    /// The scene's default active camera; the first camera when unset.
    pub active_camera: Option<ItemId>,
}

impl Default for WorldFields {
    fn default() -> Self {
        Self { fog: false, ambient_occlusion: true, active_camera: None }
    }
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
