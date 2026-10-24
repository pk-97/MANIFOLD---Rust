//! Family-owned decoding and placement of source meshes.
use super::mesh_selection::MeshSelection;
use crate::mesh::MeshVertex;
use std::path::Path;

/// Load `path`'s mesh: the one file node `node` names (an imported part),
/// else what the selection picks from the whole file.
pub type LoadMesh = fn(&Path, Option<&str>, &MeshSelection) -> Result<Vec<MeshVertex>, String>;

pub struct MeshAssetSource {
    pub load: LoadMesh,
}
inventory::collect!(MeshAssetSource);

pub fn load_mesh(path: &Path, selection: &MeshSelection) -> Result<Vec<MeshVertex>, String> {
    load_mesh_node(path, None, selection)
}

pub fn load_mesh_node(
    path: &Path,
    node: Option<&str>,
    selection: &MeshSelection,
) -> Result<Vec<MeshVertex>, String> {
    let mut sources = inventory::iter::<MeshAssetSource>.into_iter();
    let source = sources.next().ok_or("mesh asset provider must be linked")?;
    assert!(sources.next().is_none(), "duplicate mesh asset provider");
    (source.load)(path, node, selection)
}
