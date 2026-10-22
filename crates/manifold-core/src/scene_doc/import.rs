//! A model file as the scene sees it (SCENE_DOCUMENT_DESIGN.md section 7.7
//! (Imports)): a tree of file nodes that carry meshes, ready to become items.
//! The importer in `manifold-nodes-scene` reads the file and fills this in;
//! the commands in `manifold-editing` turn it into items and reconcile it
//! with an existing import on a Model File swap.

use super::{GroupFields, Item, ItemFields, ItemId, ObjectFields, SceneDoc, Shape};

/// One part of an imported model: a mesh-bearing file node (an Object), or a
/// file node that holds several (a Group).
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedNode {
    /// What the item's id derives from: the file node path, which survives a
    /// file swap while the node keeps its place. A Group is keyed by the path
    /// and an Object by the path plus `#mesh`, so a node that gains or loses
    /// children keeps its Object.
    pub key: String,
    pub name: String,
    /// The file node whose own mesh an Object draws; `None` for a Group.
    pub mesh_node: Option<String>,
    /// That mesh's vertex count once triangles are expanded; 0 for a Group.
    pub vertex_count: u32,
    pub children: Vec<ImportedNode>,
}

/// A parsed model file: its parts and where it sits in space.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedModel {
    /// The file's name without its extension.
    pub name: String,
    pub roots: Vec<ImportedNode>,
    /// The world-space box around every mesh, when the file has geometry.
    pub bounds: Option<([f32; 3], [f32; 3])>,
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

/// Where an import sits so the whole model is in view: scaled to span two
/// metres along its longest side, centred on the origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImportPlacement {
    pub scale: f32,
    pub position: [f32; 3],
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

    /// The model as items under `id`: a Group whose Source is `file`, with
    /// one Object per mesh-bearing node and a nested Group where the file
    /// nests; or one Object when the file has a single mesh.
    pub fn into_item(&self, file: &str, id: ItemId) -> Item {
        let meshes = self.mesh_nodes();
        if let [only] = meshes.as_slice() {
            let node_path = only.mesh_node.clone().unwrap_or_default();
            return mesh_object(id, &self.name, file, node_path, only.vertex_count);
        }
        let mut roots: &[ImportedNode] = &self.roots;
        if let [only] = roots
            && only.mesh_node.is_none()
        {
            roots = &only.children;
        }
        let fields = ItemFields::Group(GroupFields { source: Some(file.to_owned()), ..GroupFields::default() });
        let mut group = Item::new(id.clone(), self.name.clone(), fields);
        group.children = roots.iter().map(|node| node_item(&id, node, file)).collect();
        group
    }

    /// A new scene around the model: the base scene without its cube, and the
    /// model as `id` (what a dropped model on a project with no scene makes).
    pub fn scene_doc(&self, file: &str, id: ItemId) -> SceneDoc {
        let mut doc = SceneDoc::base();
        doc.items.retain(|item| item.id.as_str() != "cube");
        doc.items.push(self.into_item(file, id));
        doc
    }

    /// The nodes below an import Group, as the Group holds them.
    pub fn group_children(&self) -> &[ImportedNode] {
        match self.roots.as_slice() {
            [only] if only.mesh_node.is_none() => &only.children,
            roots => roots,
        }
    }

    /// Whether the model is one Object rather than a Group.
    pub fn is_single_mesh(&self) -> bool {
        self.mesh_nodes().len() == 1
    }

    /// Scale and position that bring the model into view, none for a model
    /// with no geometry or a degenerate box.
    pub fn placement(&self) -> Option<ImportPlacement> {
        let (lo, hi) = self.bounds?;
        let longest = (0..3).map(|axis| hi[axis] - lo[axis]).fold(0.0_f32, f32::max);
        if !longest.is_finite() || longest <= 1e-6 {
            return None;
        }
        let scale = 2.0 / longest;
        let position = [0, 1, 2].map(|axis| -(lo[axis] + hi[axis]) * 0.5 * scale);
        position.iter().all(|v| v.is_finite()).then_some(ImportPlacement { scale, position })
    }
}

/// The item for `node` under import group `group`.
pub fn node_item(group: &ItemId, node: &ImportedNode, file: &str) -> Item {
    let id = import_child_id(group, &node.key);
    match &node.mesh_node {
        Some(path) if node.children.is_empty() => mesh_object(id, &node.name, file, path.clone(), node.vertex_count),
        _ => {
            let mut item = Item::new(id, node.name.clone(), ItemFields::Group(GroupFields::default()));
            item.children = node.children.iter().map(|child| node_item(group, child, file)).collect();
            item
        }
    }
}

fn mesh_object(id: ItemId, name: &str, file: &str, node_path: String, vertex_count: u32) -> Item {
    let fields = ObjectFields { shape: Shape::Mesh { file: file.to_owned(), node_path, vertex_count }, ..ObjectFields::default() };
    Item::new(id, name, ItemFields::Object(fields))
}
