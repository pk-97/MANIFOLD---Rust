#![forbid(unsafe_code)]

mod testkit_visibility;

/// Build-time identity of core tempo sources used by recorded physics results.
pub const SOURCE_IDENTITY: &str = env!("MANIFOLD_CORE_SOURCE_IDENTITY");

/// Optional model lists have two in-memory forms of "nothing" (`None` and an
/// emptied `Some`). The wire has one: absent. Undo of an add leaves `Some([])`,
/// so serializing by `is_some` would change the saved file (BUG-rr482 (undo
/// leaves empty lists)).
pub(crate) fn has_items<T>(list: &Option<Vec<T>>) -> bool {
    list.as_ref().is_some_and(|items| !items.is_empty())
}

pub(crate) fn items<T>(list: &Option<Vec<T>>) -> Option<&Vec<T>> {
    list.as_ref().filter(|items| !items.is_empty())
}

pub(crate) fn has_no_items<T>(list: &Option<Vec<T>>) -> bool {
    !has_items(list)
}

pub mod ableton_mapping;
pub mod audio_clip_detection;
pub mod audio_features;
pub mod audio_mod;
pub mod audio_setup;
pub mod audio_stream;
pub mod audio_trigger;
pub mod audio_visual;
pub mod binding_identity;
pub mod change;
pub mod clip;
pub mod color;
pub mod control_history;
pub mod copies_body;
pub mod copy_layout;
pub mod effect_graph_def;
pub mod effect_registration;
pub mod effects;
pub mod file_loader;
pub mod flatten;
pub mod fluid_domain;
pub mod generator;
pub mod generator_metadata_submissions;
pub mod generator_registration;
pub mod graph_target;
pub mod group_edit;
pub mod id;
pub mod initial_fill;
pub mod layer;
pub mod liquid_domain;
pub mod liquid_setup;
pub mod macro_bank;
pub mod marker;
pub mod material_inspector;
pub mod math;
pub mod midi;
pub mod param_lookup;
pub mod params;
pub mod percussion_analysis;
pub mod percussion_binding;
pub mod percussion_settings;
pub mod phong_migration;
pub mod physics_body_slots;
pub mod physics_gizmo;
pub mod physics_members;
pub mod preset_def;
pub mod preset_definition_registry;
pub mod preset_type_id;
pub mod preset_type_registry;
pub mod project;
pub mod recording;
pub mod retired_cpu_flip;
pub mod scene_doc;
pub mod scene_impulse;
pub mod scene_index;
pub mod scene_modifier_math_view;
pub mod scene_modifier_preset;
pub mod scene_node;
pub mod scene_schema;
pub mod scene_source_identity;
pub mod scene_variadic;
pub mod scene_world_controls;
pub mod selection;
pub mod session;
pub mod settings;
pub mod sky_sun;
pub mod stage;
pub mod tempo;
pub mod timeline;
pub mod type_id_migration;
pub mod types;
pub mod units;
pub mod video;
pub mod warmup;
pub use audio_features::{AudioFeatureSnapshot, BandFeatures, SendFeatures};
pub use audio_mod::{
    AudioBand, AudioFeature, AudioFeatureKind, AudioModShape, AudioModSource, ParameterAudioMod,
};
pub use audio_setup::{
    AudioDeviceRef, AudioSend, AudioSendSource, AudioSetup, AudioSourceKind, SendAnalysisConfig,
};
pub use audio_trigger::{LayerClipTrigger, TransientEdge, TriggerFireMode, TriggerRoute};
pub use color::Color;
pub use control_history::{
    ControlContribution, ControlHistory, ControlHistoryError, ControlObservation,
    TriggerSourceStamp,
};
pub use effects::{EffectContainer, ParamSource};
pub use graph_target::GraphTarget;
pub use id::{
    AudioSendId, ClipId, EffectGroupId, EffectId, ItemId, LayerId, MarkerId, NodeId, SceneId,
};
pub use layer::OverlapAction;
pub use macro_bank::{
    MACRO_COUNT, MacroBank, MacroCurve, MacroMapping, MacroMappingTarget, MacroSlot,
};
pub use marker::TimelineMarker;
pub use math::{BeatQuantizer, MathUtils, short_id};
pub use preset_type_id::PresetTypeId;
pub use scene_modifier_preset::{
    EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS, SCENE_MODIFIER_RECIPE_VERSION, SceneAxis,
    SceneContextValue, SceneEndpoint, SceneMeshReferenceFrame, SceneModifierInstanceDef,
    SceneModifierRecipe, SceneModifierSchemaError, SceneModifierStageDef, SceneNodeInitializer,
    SceneParamCalibration, SceneScalarExpr, SceneStageInput, SceneStageOutput, SceneStageScope,
    SceneStageSource, SceneTargetSelection, validate_scene_modifier_schema,
};
pub use selection::SelectionRegion;
pub use session::{ClipSequence, Scene, SessionGrid, SessionSlot};
pub use stage::{
    DerivedStage, DisplayIdentity, DisplayPlacement, Island, OutputAdvanced, OutputId, Rotation,
    StageLayout, derive_stage,
};
pub use types::*;
pub use units::{Beats, Bpm, PlayedTime, Seconds};
pub use warmup::{WarmupBudget, WarmupCap, WarmupOutcome, WarmupPass, WarmupProgress, WarmupRun};

// Foundation modules manifold-app reaches through `manifold_core`; the
// layering test forbids a direct app -> foundation dependency.
pub use manifold_foundation::cold_touch;
pub use manifold_foundation::device_guard;
pub use manifold_foundation::param_range;
pub use manifold_foundation::scene_surface;
pub use manifold_foundation::scratch;
pub use manifold_foundation::work_count;
