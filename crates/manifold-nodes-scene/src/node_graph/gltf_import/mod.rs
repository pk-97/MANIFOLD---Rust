//! Reads a model file into the scene's import tree (SCENE_DOCUMENT_DESIGN.md
//! section 7.7 (Imports)): one part per mesh-bearing file node, empty
//! pass-through nodes collapsed, nested where the file nests. The commands in
//! `manifold-editing` turn the tree into items; the compiler's Mesh shape
//! (`scene_compile/mesh.rs`) draws each part by its file node path.
//!
//! No graph is built here. The parse is the one the mesh loader uses
//! ([`gltf_load::parse_document_and_buffers`]).

use std::path::Path;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_doc::{ImportedModel, ImportedNode, ItemId};

use super::gltf_load::{self, FileNode};

/// Parse `path` and describe it as scene parts. Errors when the file cannot
/// be read or has no mesh.
pub fn import_model(path: &Path) -> Result<ImportedModel, String> {
    let (document, buffers) = gltf_load::parse_document_and_buffers(path)?;
    let roots: Vec<ImportedNode> = gltf_load::file_node_tree(&document, &buffers).iter().filter_map(part).collect();
    if roots.is_empty() {
        return Err(format!("{}: no meshes — nothing to import", path.display()));
    }
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "Model".to_owned());
    Ok(ImportedModel { name, roots, bounds: gltf_load::file_bounds(&document) })
}

/// The model at `path` in the base scene (minus its cube), compiled: the scene
/// a drop makes. For the render and soak tools, which draw a file without a
/// project around it.
pub fn compile_model_scene(path: &Path) -> Result<(EffectGraphDef, ImportedModel), String> {
    let model = import_model(path)?;
    let doc = model.scene_doc(&path.to_string_lossy(), ItemId::new("model"));
    let def = manifold_core::scene_doc::compile(&doc)?;
    Ok((def, model))
}

/// The part a file node becomes, or none when nothing under it has a mesh.
/// A node with no mesh and one part below it is that part; its path stays
/// the part's key, so the id survives the collapse.
fn part(node: &FileNode) -> Option<ImportedNode> {
    let mut children: Vec<ImportedNode> = node.children.iter().filter_map(part).collect();
    let object = |key: String| ImportedNode {
        key,
        name: node.name.clone(),
        mesh_node: Some(node.path.clone()),
        vertex_count: node.vertex_count,
        children: Vec::new(),
    };
    match (node.has_mesh, children.len()) {
        (false, 0) => None,
        (false, 1) => children.pop(),
        (true, 0) => Some(object(format!("{}#mesh", node.path))),
        (has_mesh, _) => {
            if has_mesh {
                children.insert(0, object(format!("{}#mesh", node.path)));
            }
            Some(ImportedNode { key: node.path.clone(), name: node.name.clone(), mesh_node: None, vertex_count: 0, children })
        }
    }
}

#[cfg(test)]
mod tests;
