pub mod ableton;
pub mod audio_mod;
pub mod audio_setup;
pub mod automation;
pub mod clip;
pub mod clip_detection;
pub mod drivers;
pub mod effect_groups;
pub mod effect_target;
pub mod effects;
pub mod envelopes;
pub mod graph;
pub mod layer;
pub mod marker;
pub mod preset;
pub mod scene_doc;
pub mod selection;
pub mod session_commands;
pub mod settings;
pub mod stage;
pub mod trigger_source;

use manifold_core::change::{ChangeSet, Part};

/// Every part a whole `PresetInstance` spans. Adding, removing or reordering
/// instances moves all of them: the instance carries its values, shapes,
/// lanes, Ableton mappings and override copy with it.
pub const WHOLE_INSTANCES: ChangeSet = ChangeSet::of(&[
    Part::Effects,
    Part::Overrides,
    Part::Values,
    Part::Shapes,
    Part::Automation,
    Part::Mappings,
    Part::MappingRanges,
]);

/// An edit to one instance's graph document. Its structure is Effects, lifting
/// the catalog default into a materialized copy is Overrides, and every node's
/// unbound param values and every binding reshape are Shapes, so adding,
/// removing or lifting nodes moves Shapes too.
pub(crate) const GRAPH_EDIT: ChangeSet =
    ChangeSet::of(&[Part::Effects, Part::Overrides, Part::Shapes]);

/// A graph edit that also adds or removes card params, carrying their values
/// and their drivers, envelopes, audio mods and Ableton mappings with them.
/// Automation lanes stay: only `WHOLE_INSTANCES` edits move them.
pub(crate) const GRAPH_AND_CONTROLS: ChangeSet = GRAPH_EDIT.union(ChangeSet::of(&[
    Part::Values,
    Part::Mappings,
    Part::MappingRanges,
]));

/// A scene-modifier edit rewrites the owner graph and reconciles the owner's
/// controls through `prune_instance_params`, which drops automation lanes too,
/// so it moves `WHOLE_INSTANCES`. Any other target moves `otherwise`.
pub(crate) fn graph_edit_touches(target: &manifold_core::GraphTarget, otherwise: ChangeSet) -> ChangeSet {
    if matches!(target, manifold_core::GraphTarget::SceneModifier { .. }) {
        WHOLE_INSTANCES
    } else {
        otherwise
    }
}

/// Every part a whole `Layer` spans: its own fields, its clips and its
/// generator and effect instances.
pub(crate) const WHOLE_LAYERS: ChangeSet =
    WHOLE_INSTANCES.union(ChangeSet::of(&[Part::Layers, Part::Clips]));

#[cfg(test)]
use crate::command::Command;
#[cfg(test)]
pub(crate) mod setter_roundtrip;
