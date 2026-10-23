//! A model file as the scene sees it (SCENE_DOCUMENT_DESIGN.md section 7.7
//! (Imports)): a tree of file nodes that carry meshes, the file's materials,
//! lights and cameras, ready to become items. The importer in
//! `manifold-nodes-scene` reads the file and fills this in; the commands in
//! `manifold-editing` turn it into items and reconcile it with an existing
//! import on a Model File swap.

use super::{
    CameraFields, CameraRig, GroupFields, Item, ItemFields, ItemId, LightFields, LightType, MaterialFields,
    MeshPrimitives, ObjectFields, SceneDoc, Shape, TextureSource,
};

/// One part of an imported model: the primitives of one material on a
/// mesh-bearing file node (an Object), or a file node that holds several
/// (a Group).
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedNode {
    /// What the item's id derives from: the file node path, which survives a
    /// file swap while the node keeps its place. A Group is keyed by the path
    /// and an Object by the path plus `#mesh` (plus the material when the
    /// node's mesh carries several), so a node that gains or loses children
    /// keeps its Object.
    pub key: String,
    pub name: String,
    /// The file node whose own mesh an Object draws; `None` for a Group.
    pub mesh_node: Option<String>,
    /// Which of that mesh's primitives the Object draws.
    pub primitives: MeshPrimitives,
    /// The key of the [`ImportedMaterial`] an Object draws with.
    pub material: Option<String>,
    /// That mesh's vertex count once triangles are expanded; 0 for a Group.
    pub vertex_count: u32,
    /// The file node's own transform in the frame of the item above it,
    /// where the item's Transform starts. An Object draws its mesh in this
    /// node's space, so the node's origin is its pivot.
    pub transform: ImportedTransform,
    pub children: Vec<ImportedNode>,
}

/// A file node's transform as an item's Transform rows: position in metres,
/// XYZ Euler rotation in radians, scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImportedTransform {
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

impl ImportedTransform {
    pub const IDENTITY: Self = Self { position: [0.0; 3], rotation: [0.0; 3], scale: [1.0; 3] };

    /// `(property, value)` for every Transform row away from its rest.
    pub fn values(&self) -> impl Iterator<Item = (&'static str, f32)> + '_ {
        const ROWS: [[&str; 3]; 3] = [["pos_x", "pos_y", "pos_z"], ["rot_x", "rot_y", "rot_z"], ["scale_x", "scale_y", "scale_z"]];
        [(self.position, 0.0), (self.rotation, 0.0), (self.scale, 1.0)]
            .into_iter()
            .zip(ROWS)
            .flat_map(|((values, rest), names)| names.into_iter().zip(values).filter(move |(_, value)| *value != rest))
    }
}

/// One glTF material of the file: a Material item whose texture source is
/// the file's material.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedMaterial {
    pub key: String,
    pub name: String,
    /// The glTF material index; `None` is glTF's default material.
    pub material: Option<u32>,
}

/// One light of the file (`KHR_lights_punctual`): a Light item and the card
/// values it starts at, in the file's space.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedLight {
    pub key: String,
    pub name: String,
    pub light: LightType,
    /// `(property, value)` for the Light table's card rows.
    pub values: Vec<(&'static str, f32)>,
}

/// One perspective camera of the file: a Free Camera item and the card
/// values it starts at, in the file's space.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedCamera {
    pub key: String,
    pub name: String,
    /// `(property, value)` for the Camera table's card rows.
    pub values: Vec<(&'static str, f32)>,
}

/// A parsed model file: its parts, materials, lights and cameras, and its
/// animation clips. It imports at the file's own size and place, in metres
/// (glTF's unit): nothing rescales or recentres it.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedModel {
    /// The file's name without its extension.
    pub name: String,
    pub roots: Vec<ImportedNode>,
    pub materials: Vec<ImportedMaterial>,
    pub lights: Vec<ImportedLight>,
    pub cameras: Vec<ImportedCamera>,
    /// The file's animation clips by name, in file order.
    pub clips: Vec<String>,
}

/// The id of the import child with file key `key` under import `group`.
/// Stable across reloads and swaps, and different for two imports of one
/// file. Holds no `.`, so a param id still splits at its first dot.
pub fn import_child_id(group: &ItemId, key: &str) -> ItemId {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    ItemId::new(format!("{group}-{:012x}", hash & 0xffff_ffff_ffff))
}

/// Whether `id` was derived from import `group` by [`import_child_id`].
pub fn is_import_child(group: &ItemId, id: &ItemId) -> bool {
    id.as_str().strip_prefix(group.as_str()).is_some_and(|rest| rest.starts_with('-'))
}

impl ImportedModel {
    fn mesh_nodes(&self) -> Vec<&ImportedNode> {
        fn visit<'a>(nodes: &'a [ImportedNode], out: &mut Vec<&'a ImportedNode>) {
            for node in nodes {
                if node.mesh_node.is_some() {
                    out.push(node);
                }
                visit(&node.children, out);
            }
        }
        let mut out = Vec::new();
        visit(&self.roots, &mut out);
        out
    }

    /// The model as items `id` and what sits beside it: a Group whose
    /// Source is `file`, holding one Object per mesh part (nested where the
    /// file nests), one Material per glTF material, and the file's lights
    /// and cameras; or, for a file with one mesh part and no lights, cameras
    /// or animation, that Object and its Material beside it. Each starts at
    /// its file node's transform ([`Self::starting_values`]).
    pub fn into_items(&self, file: &str, id: ItemId) -> Vec<Item> {
        if self.is_single_mesh() {
            let only = self.mesh_nodes()[0];
            let mut object = node_item(&id, only, file);
            object.id = id.clone();
            object.name = self.name.clone();
            let materials = self.materials.iter().filter(|m| only.material.as_ref() == Some(&m.key));
            return std::iter::once(object).chain(materials.map(|m| material_item(&id, m, file))).collect();
        }
        let fields = ItemFields::Group(GroupFields { source: Some(file.to_owned()), clip_names: self.clips.clone(), ..GroupFields::default() });
        let mut group = Item::new(id.clone(), self.name.clone(), fields);
        group.children = self.group_children().iter().map(|node| node_item(&id, node, file)).collect();
        group.children.extend(self.materials.iter().map(|m| material_item(&id, m, file)));
        group.children.extend(self.lights.iter().map(|l| light_item(&id, l)));
        group.children.extend(self.cameras.iter().map(|c| camera_item(&id, c)));
        vec![group]
    }

    /// A new scene around the model: the base scene without its cube and the
    /// cube's Material, and the model as `id` (what a dropped model on a
    /// project with no scene makes).
    pub fn scene_doc(&self, file: &str, id: ItemId) -> SceneDoc {
        let mut doc = SceneDoc::base();
        doc.items.retain(|item| ![super::BASE_CUBE, super::BASE_CUBE_MATERIAL].contains(&item.id.as_str()));
        for mut item in self.into_items(file, id) {
            doc.number_cameras(&mut item);
            doc.items.push(item);
        }
        doc
    }

    /// The nodes below an import Group, as the Group holds them: a lone
    /// root group that does not move its parts is the import Group itself.
    pub fn group_children(&self) -> &[ImportedNode] {
        match self.roots.as_slice() {
            [only] if only.mesh_node.is_none() && only.transform == ImportedTransform::IDENTITY => &only.children,
            roots => roots,
        }
    }

    /// Whether the model is one Object rather than a Group: one mesh part,
    /// under no group that moves it, and no lights, cameras or animation
    /// clips to hold beside it.
    pub fn is_single_mesh(&self) -> bool {
        fn unmoved(nodes: &[ImportedNode]) -> bool {
            nodes.iter().all(|node| {
                node.mesh_node.is_some() || (node.transform == ImportedTransform::IDENTITY && unmoved(&node.children))
            })
        }
        self.mesh_nodes().len() == 1
            && unmoved(&self.roots)
            && self.lights.is_empty()
            && self.cameras.is_empty()
            && self.clips.is_empty()
    }

    /// The one mesh part of a model that has exactly one.
    pub fn only_part(&self) -> Option<&ImportedNode> {
        match self.mesh_nodes().as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// Every mesh part's vertex count, summed.
    pub fn vertex_count(&self) -> u32 {
        self.mesh_nodes().iter().map(|node| node.vertex_count).fold(0, u32::saturating_add)
    }

    /// The card values the import `id` starts at: each part's and group's
    /// file node transform, and the file's lights and cameras.
    /// `(item, property, value)`.
    pub fn starting_values(&self, id: &ItemId) -> Vec<(ItemId, &'static str, f32)> {
        let mut out = Vec::new();
        if self.is_single_mesh() {
            let only = self.mesh_nodes()[0];
            out.extend(only.transform.values().map(|(property, value)| (id.clone(), property, value)));
        } else {
            for node in self.group_children() {
                node.starting_values(id, &mut out);
            }
        }
        let lights = self.lights.iter().map(|l| (&l.key, &l.values));
        let cameras = self.cameras.iter().map(|c| (&c.key, &c.values));
        for (key, values) in lights.chain(cameras) {
            let item = import_child_id(id, key);
            out.extend(values.iter().map(|(property, value)| (item.clone(), *property, *value)));
        }
        out
    }
}

impl ImportedNode {
    /// The Transform values this node's item under import `group` starts
    /// at, and its children's.
    pub fn starting_values(&self, group: &ItemId, out: &mut Vec<(ItemId, &'static str, f32)>) {
        let item = import_child_id(group, &self.key);
        out.extend(self.transform.values().map(|(property, value)| (item.clone(), property, value)));
        for child in &self.children {
            child.starting_values(group, out);
        }
    }
}

/// The item for `node` under import group `group`.
pub fn node_item(group: &ItemId, node: &ImportedNode, file: &str) -> Item {
    let id = import_child_id(group, &node.key);
    match &node.mesh_node {
        Some(path) if node.children.is_empty() => {
            let fields = ObjectFields {
                shape: Shape::Mesh {
                    file: file.to_owned(),
                    node_path: path.clone(),
                    vertex_count: node.vertex_count,
                    primitives: node.primitives,
                },
                material: node.material.as_ref().map(|key| import_child_id(group, key)),
                ..ObjectFields::default()
            };
            Item::new(id, node.name.clone(), ItemFields::Object(fields))
        }
        _ => {
            let mut item = Item::new(id, node.name.clone(), ItemFields::Group(GroupFields::default()));
            item.children = node.children.iter().map(|child| node_item(group, child, file)).collect();
            item
        }
    }
}

/// The Material item for the file's material `material` under import `group`.
pub fn material_item(group: &ItemId, material: &ImportedMaterial, file: &str) -> Item {
    let texture = TextureSource::Model { file: file.to_owned(), material: material.material };
    let fields = ItemFields::Material(MaterialFields { texture, ..MaterialFields::default() });
    Item::new(import_child_id(group, &material.key), material.name.clone(), fields)
}

/// The Light item for the file's light `light` under import `group`.
pub fn light_item(group: &ItemId, light: &ImportedLight) -> Item {
    let fields = ItemFields::Light(LightFields { light: light.light.clone() });
    Item::new(import_child_id(group, &light.key), light.name.clone(), fields)
}

/// The Camera item for the file's camera `camera` under import `group`.
pub fn camera_item(group: &ItemId, camera: &ImportedCamera) -> Item {
    let fields = ItemFields::Camera(CameraFields { rig: CameraRig::Free, ..CameraFields::default() });
    Item::new(import_child_id(group, &camera.key), camera.name.clone(), fields)
}
