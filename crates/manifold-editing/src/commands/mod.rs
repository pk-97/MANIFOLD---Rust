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
pub mod material;
pub mod preset;
pub mod selection;
pub mod session_commands;
pub mod settings;
pub mod stage;
pub mod trigger_source;

#[cfg(test)]
use crate::command::Command;
#[cfg(test)]
pub(crate) mod setter_roundtrip;
