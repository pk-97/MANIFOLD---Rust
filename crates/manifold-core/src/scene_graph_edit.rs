//! Shared result and error types for pure authored scene-graph edits.

use std::fmt;

use crate::effect_graph_def::EffectGraphDef;
use crate::scene_modifier_preset::SceneNodeRef;

/// Result of one stack edit. `graph` is always a complete cloned owner;
/// runtime mapping cleanup consumes the reported parameter IDs after commit.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneGraphEdit {
    pub graph: EffectGraphDef,
    pub removed_param_ids: Vec<String>,
    /// Host parameter ids whose runtime state should be copied to a duplicated
    /// modifier. Each pair is `(source_id, duplicate_id)`.
    pub parameter_id_remaps: Vec<(String, String)>,
}

/// A pure scene graph edit could not be prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneGraphEditError {
    pub at: Option<SceneNodeRef>,
    pub message: String,
}

impl fmt::Display for SceneGraphEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.at {
            Some(at) => write!(f, "{} at {at:?}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for SceneGraphEditError {}
