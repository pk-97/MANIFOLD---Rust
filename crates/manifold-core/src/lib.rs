#![forbid(unsafe_code)]

pub mod ableton_mapping;
pub mod clip;
pub mod color;
pub mod effect_category_registry;
pub mod effect_definition_registry;
pub mod effect_registration;
pub mod effect_type_id;
pub mod effect_type_registry;
pub mod effects;
pub mod generator;
pub mod generator_definition_registry;
pub mod generator_registration;
pub mod generator_type_id;
pub mod generator_type_registry;
pub mod id;
pub mod layer;
pub mod macro_bank;
pub mod marker;
pub mod math;
pub mod midi;
pub mod percussion;
pub mod percussion_analysis;
pub mod percussion_binding;
pub mod percussion_settings;
pub mod project;
pub mod recording;
pub mod selection;
pub mod settings;
pub mod tempo;
pub mod timeline;
pub mod types;
pub mod units;
pub mod video;
pub use color::Color;
pub use effect_type_id::EffectTypeId;
pub use effects::{EffectContainer, ParamSource};
pub use generator_type_id::GeneratorTypeId;
pub use id::{ClipId, EffectGroupId, EffectId, LayerId, MarkerId};
pub use layer::OverlapAction;
pub use macro_bank::{
    MACRO_COUNT, MacroBank, MacroCurve, MacroMapping, MacroMappingTarget, MacroSlot,
};
pub use marker::TimelineMarker;
pub use math::{BeatQuantizer, MathUtils, short_id};
pub use selection::{SelectionRegion, SelectionRegionTarget};
pub use types::*;
pub use units::{Beats, Bpm, Seconds, beats_to_seconds, seconds_to_beats};
