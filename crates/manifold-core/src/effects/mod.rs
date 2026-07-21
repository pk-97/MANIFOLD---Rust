use crate::effect_graph_def::EffectGraphDef;
use crate::preset_type_id::PresetTypeId;
use crate::id::{EffectGroupId, EffectId, NodeId};
use crate::types::{BeatDivision, DriverWaveform};
use serde::{Deserialize, Serialize};

/// Stable string identifier for a host-visible parameter.
///
/// `Cow::Borrowed("amount")` for compile-time IDs (developer-defined
/// effects). `Cow::Owned(...)` for V2 user-exposed parameters allocated
/// at runtime. External mappings (OSC, Ableton, MIDI, modulation
/// drivers, envelopes) all key on this — never on positional indices.
///
/// See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 for the full design.
///
/// Defined in `manifold-foundation` (the shared primitive vocabulary) and
/// re-exported here at its historical path so the UI can share the identical
/// type without depending on the engine. See `docs/UI_LAYERING_INVERSION.md`.
pub use manifold_foundation::ParamId;

mod automation;
mod bindings;
mod driver;
mod envelope;
mod group;
mod instance;
mod instance_serde;
mod param_defs;
mod relight;
#[cfg(test)]
mod test_support;

pub use automation::{AutomationLane, AutomationPoint, RemovedAutomation, SegmentShape};
pub use bindings::{apply_card_reshape, binding_id_for_node_param_in, invert_card_reshape, ParamConvert, RemovedExposure, UserParamBinding};
pub use driver::{beat_division_helper, hash_to_float, hash_u32, ParameterDriver};
pub use envelope::{ParamEnvelope, DEFAULT_ENVELOPE_DECAY_BEATS};
pub use group::EffectGroup;
pub use instance::PresetInstance;
pub use instance_serde::{deserialize_generator_instance, deserialize_opt_generator_instance};
pub use param_defs::{RangeContract, RangeReason, RegistryParamDef};
pub use relight::{RelightField, RelightHeightFrom, RelightParams};



/// serde `skip_serializing_if` for [`crate::effect_graph_def::ParamSpecDef::curve`].
pub(crate) fn curve_is_linear(c: &crate::macro_bank::MacroCurve) -> bool {
    matches!(c, crate::macro_bank::MacroCurve::Linear)
}

/// serde `skip_serializing_if` for a defaulted `false` bool field.
pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

// ─── Traits ───

/// Shared contract for entities that own a modular effects list.
/// Port of Unity IEffectContainer.cs.
/// Implemented by TimelineClip, Layer, and ProjectSettings.
pub trait EffectContainer {
    fn effects(&self) -> &[PresetInstance];
    fn effects_mut(&mut self) -> &mut Vec<PresetInstance>;
    fn effect_groups(&self) -> &[EffectGroup];
    fn effect_groups_mut(&mut self) -> &mut Vec<EffectGroup>;
    fn has_modular_effects(&self) -> bool;
    fn find_effect(&self, effect_type: &PresetTypeId) -> Option<&PresetInstance>;
    fn find_effect_group(&self, group_id: &str) -> Option<&EffectGroup>;
}

/// Abstracts a "thing with named params, drivers, and ranges."
/// Port of Unity IParamSource.cs.
/// Both PresetInstance and generator params implement this.
pub trait ParamSource {
    fn display_name(&self) -> &str;
    fn param_count(&self) -> usize;
    fn get_param_def(&self, id: &str) -> crate::effect_graph_def::ParamSpecDef;
    fn get_param(&self, id: &str) -> f32;
    fn set_param(&mut self, id: &str, value: f32);
    fn get_base_param(&self, id: &str) -> f32;
    fn set_base_param(&mut self, id: &str, value: f32);
    fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver>;
    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>>;
    fn create_driver(&mut self, param_id: ParamId) -> &ParameterDriver;
    fn remove_driver(&mut self, param_id: &str);
}







// ─── Default helpers ───

fn default_true() -> bool {
    true
}
fn default_one() -> f32 {
    1.0
}
fn generate_effect_id() -> EffectId {
    EffectId::new(crate::math::short_id())
}

