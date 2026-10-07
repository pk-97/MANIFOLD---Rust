//! Family-owned decoding and placement of source meshes.
use std::path::Path;
use crate::mesh::MeshVertex;
use super::physics_mesh::MeshSelection;

pub struct MeshAssetSource {
    pub load: fn(&Path, &MeshSelection) -> Result<Vec<MeshVertex>, String>,
}
inventory::collect!(MeshAssetSource);

pub fn load_mesh(path: &Path, selection: &MeshSelection) -> Result<Vec<MeshVertex>, String> {
    let mut sources = inventory::iter::<MeshAssetSource>.into_iter();
    let source = sources.next().ok_or("mesh asset provider must be linked")?;
    assert!(sources.next().is_none(), "duplicate mesh asset provider");
    (source.load)(path, selection)
}
