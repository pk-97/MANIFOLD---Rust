#![forbid(unsafe_code)]

pub mod ableton_mapping;
pub mod audio_clip_detection;
pub mod audio_features;
pub mod audio_mod;
pub mod audio_setup;
pub mod audio_trigger;
pub mod clip;
pub mod color;
pub mod effect_graph_def;
pub mod effect_registration;
pub mod effects;
pub mod flatten;
pub mod generator;
pub mod generator_metadata_submissions;
pub mod generator_registration;
pub mod graph_target;
pub mod group_edit;
pub mod id;
pub mod layer;
pub mod macro_bank;
pub mod marker;
pub mod math;
pub mod midi;
pub mod params;
pub mod percussion_analysis;
pub mod percussion_binding;
pub mod percussion_settings;
pub mod preset_def;
pub mod preset_definition_registry;
pub mod preset_type_id;
pub mod preset_type_registry;
pub mod project;
pub mod recording;
pub mod scene_object_migration;
pub mod selection;
pub mod session;
pub mod settings;
pub mod stage;
pub mod tempo;
pub mod timeline;
pub mod type_id_migration;
pub mod types;
pub mod units;
pub mod video;
pub use color::Color;
pub use effects::{EffectContainer, ParamSource};
pub use graph_target::GraphTarget;
pub use preset_type_id::PresetTypeId;
pub use audio_features::{AudioFeatureSnapshot, BandFeatures, SendFeatures};
pub use audio_mod::{
    AudioBand, AudioFeature, AudioFeatureKind, AudioModShape, AudioModSource, ParameterAudioMod,
};
pub use audio_setup::{
    AudioDeviceRef, AudioSend, AudioSendSource, AudioSetup, AudioSourceKind, SendAnalysisConfig,
};
pub use audio_trigger::{LayerClipTrigger, TransientEdge, TriggerFireMode, TriggerRoute};
pub use id::{AudioSendId, ClipId, EffectGroupId, EffectId, LayerId, MarkerId, NodeId, SceneId};
pub use layer::OverlapAction;
pub use macro_bank::{
    MACRO_COUNT, MacroBank, MacroCurve, MacroMapping, MacroMappingTarget, MacroSlot,
};
pub use marker::TimelineMarker;
pub use math::{BeatQuantizer, MathUtils, short_id};
pub use selection::{SelectionRegion, SelectionRegionTarget};
pub use session::{ClipSequence, Scene, SessionGrid, SessionSlot};
pub use stage::{
    DerivedStage, DisplayIdentity, DisplayPlacement, Island, OutputAdvanced, OutputId, Rotation,
    StageLayout, derive_stage,
};
pub use types::*;
pub use units::{Beats, Bpm, Seconds, beats_to_seconds, seconds_to_beats};
