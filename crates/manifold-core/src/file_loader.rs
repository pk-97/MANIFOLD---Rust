//! The single list of file-loading node types (PROJECT_FOLDERS_DESIGN P5).
//!
//! P1's `is_file_path` flag made collect opt-in per preset author; embedded
//! presets saved before the flag existed silently skipped their GLBs
//! (BUG-gqne). Peter's ruling (2026-09-02): the human never chooses what is
//! collected — it just works. So the flag's meaning flips from opt-in to
//! redundant: a string param whose `stringBinding` targets a node whose
//! `type_id` is in this table IS a collected path, whatever the metadata says.
//!
//! This table is the ONE list of "node type loads a filesystem path":
//! `collect_asset_paths` in `manifold-io` follows a binding's node to this
//! table for family + file-vs-folder, and a renderer-side exhaustiveness test
//! asserts every file-reading primitive with a string param has an entry here
//! (a new file-loading primitive without an entry is a red test, not a silent
//! skip).

/// The `Media/` subfolder a collected asset's family maps to (D2): the video
/// library and layer video folders differ from the mesh/HDRI/image families in
/// that the string-param inventory only ever produces `Mesh` / `Hdri` /
/// `Images` (the library / folder families come from dedicated model fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetFamily {
    /// A GLB mesh or anything decoded from a model/GLB file.
    Mesh,
    /// An environment map image (EXR / HDR read by `node.hdri_source`).
    Hdri,
    /// A folder (or still image) of image files.
    Images,
}

/// What a file-loading node reads: a single file, or a folder tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeFileLoad {
    File(AssetFamily),
    Folder(AssetFamily),
}

/// The table: `type_id` → what that node reads from disk. Names only nodes
/// whose path comes via an outer-card string binding (`stringParams` +
/// `stringBindings`), per PROJECT_FOLDERS_DESIGN P5.
macro_rules! table {
    ($($type_id:literal => $load:expr),* $(,)?) => {
        /// Look up what a node type loads from disk, if it loads anything.
        ///
        /// The one enumeration the whole design reads. A string param whose
        /// binding targets a node with a `Some` here is a collected path; its
        /// family and file-vs-folder come from the returned value, never from
        /// `is_file_path` (P5).
        pub fn file_loader_kind(type_id: &str) -> Option<NodeFileLoad> {
            match type_id {
                $($type_id => Some($load),)*
                _ => None,
            }
        }

        /// All type_ids in the table — the renderer-side exhaustiveness test
        /// asserts each names a real registered primitive.
        pub const ALL_FILE_LOADER_TYPE_IDS: &[&str] = &[$($type_id),*];
    };
}

table! {
    // GLB model / skinned / morph / animation sources. Mesh family, single file.
    "node.gltf_mesh_source" => NodeFileLoad::File(AssetFamily::Mesh),
    "node.gltf_skinned_mesh_source" => NodeFileLoad::File(AssetFamily::Mesh),
    "node.gltf_morph_deltas_source" => NodeFileLoad::File(AssetFamily::Mesh),
    "node.gltf_morph_weights" => NodeFileLoad::File(AssetFamily::Mesh),
    "node.gltf_skeleton_pose" => NodeFileLoad::File(AssetFamily::Mesh),
    "node.gltf_animation_source" => NodeFileLoad::File(AssetFamily::Mesh),
    "node.gltf_texture_source" => NodeFileLoad::File(AssetFamily::Mesh),
    // HDRI envmap, single file.
    "node.hdri_source" => NodeFileLoad::File(AssetFamily::Hdri),
    // A folder of still images, copied as a tree.
    "node.image_folder" => NodeFileLoad::Folder(AssetFamily::Images),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_file_loader_types_are_covered() {
        assert_eq!(
            file_loader_kind("node.gltf_mesh_source"),
            Some(NodeFileLoad::File(AssetFamily::Mesh))
        );
        assert_eq!(
            file_loader_kind("node.image_folder"),
            Some(NodeFileLoad::Folder(AssetFamily::Images))
        );
        assert_eq!(
            file_loader_kind("node.hdri_source"),
            Some(NodeFileLoad::File(AssetFamily::Hdri))
        );
    }

    #[test]
    fn non_file_loaders_map_to_none() {
        assert_eq!(file_loader_kind("node.does_not_exist"), None);
        assert_eq!(file_loader_kind("node.layer_source"), None);
        assert_eq!(file_loader_kind("node.skin_mesh"), None);
        assert_eq!(file_loader_kind("system.generator_input"), None);
    }
}