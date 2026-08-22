//! The Skin row's view-model payload (SCENE_FX P4b). Lives apart from
//! `scene_setup_panel.rs` — that file is under the godfile-decomposition
//! line ceiling, so new payload types for its rows go in sibling modules
//! like this one, not into the capped file.

use manifold_foundation::LayerId;

/// P4b: which material map port the layer skin drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkinTargetMap {
    Emissive,
    BaseColor,
}

impl SkinTargetMap {
    pub fn label(self) -> &'static str {
        match self {
            SkinTargetMap::Emissive => "Emissive",
            SkinTargetMap::BaseColor => "Base Color",
        }
    }
}

/// P4b: the Skin row state for one Known object. `source` is the currently
/// bound layer id (`None` = "None"), `source_options` lists every project
/// layer for the dropdown, and `source_missing` flags a deleted source so the
/// panel can render the D8 chip.
#[derive(Clone, Debug, PartialEq)]
pub struct SkinRowVm {
    /// `node.layer_source` doc id when a skin exists; `None` when the user is
    /// picking a source for the first time (the editing command will mint one).
    pub source_node_id: Option<u32>,
    /// Scope path to the level that contains `source_node_id` (the object's
    /// own group or root).
    pub source_scope_path: Vec<u32>,
    pub source: Option<LayerId>,
    /// All project layers, ordered, so the panel can render the source picker.
    pub source_options: Vec<(LayerId, String)>,
    pub source_missing: bool,
    pub target_map: SkinTargetMap,
}
