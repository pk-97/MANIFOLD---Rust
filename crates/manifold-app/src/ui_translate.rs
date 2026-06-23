//! The single reconciliation point between the UI vocabulary (`manifold-ui`)
//! and the engine vocabulary (`manifold-core`).
//!
//! Phase 5 layering inversion: `manifold-ui` owns UI-local mirrors of the
//! engine's domain enums/structs and view-models of its domain entities (see
//! `docs/UI_LAYERING_INVERSION.md`). The app is the *only* place the two meet —
//! every core↔UI conversion lives here, as plain functions (the orphan rule
//! forbids `From` impls between two foreign types).
//!
//! Shared primitives (ids, `Beats`, `ParamId`) need no conversion — they are the
//! identical `manifold-foundation` type on both sides.

use manifold_ui::view::{SelectionRegion as UiSelectionRegion, UiLayer, UiMarker, UiParamSlot};
use manifold_ui::{
    AbletonMacroAddress as UiAbletonMacroAddress, AbletonMappingStatus as UiAbletonMappingStatus,
    AudioBand as UiAudioBand, AudioDeviceRef as UiAudioDeviceRef, AudioFeature as UiAudioFeature,
    AudioFeatureKind as UiAudioFeatureKind, AudioSourceKind as UiAudioSourceKind,
    LayerType as UiLayerType, MacroCurve as UiMacroCurve, MarkerColor as UiMarkerColor,
    MidiTriggerMode as UiMidiTriggerMode, ParamConvert as UiParamConvert,
    PresetTypeId as UiPresetTypeId, SerializedParamValue as UiSerializedParamValue,
    TonemapCurve as UiTonemapCurve,
};

use manifold_core::ableton_mapping::{
    AbletonDeviceIdentity, AbletonMacroAddress, AbletonMappingStatus,
};
use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind};
use manifold_core::audio_setup::{AudioDeviceRef, AudioSourceKind};
use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::effects::{ParamConvert, ParamSlot};
use manifold_core::layer::Layer;
use manifold_core::macro_bank::MacroCurve;
use manifold_core::marker::TimelineMarker;
use manifold_core::selection::SelectionRegion;
use manifold_core::types::{LayerType, MarkerColor, MidiTriggerMode, TonemapCurve};
use manifold_core::PresetTypeId;

// ── Leaf enums (bidirectional) ──────────────────────────────────────────

pub fn layer_type_to_ui(v: LayerType) -> UiLayerType {
    match v {
        LayerType::Video => UiLayerType::Video,
        LayerType::Generator => UiLayerType::Generator,
        LayerType::Group => UiLayerType::Group,
        LayerType::Audio => UiLayerType::Audio,
    }
}

pub fn marker_color_to_ui(v: MarkerColor) -> UiMarkerColor {
    match v {
        MarkerColor::Red => UiMarkerColor::Red,
        MarkerColor::Orange => UiMarkerColor::Orange,
        MarkerColor::Yellow => UiMarkerColor::Yellow,
        MarkerColor::Green => UiMarkerColor::Green,
        MarkerColor::Cyan => UiMarkerColor::Cyan,
        MarkerColor::Blue => UiMarkerColor::Blue,
        MarkerColor::Purple => UiMarkerColor::Purple,
        MarkerColor::White => UiMarkerColor::White,
    }
}

pub fn midi_trigger_mode_to_core(v: UiMidiTriggerMode) -> MidiTriggerMode {
    match v {
        UiMidiTriggerMode::SingleNote => MidiTriggerMode::SingleNote,
        UiMidiTriggerMode::AllNotes => MidiTriggerMode::AllNotes,
    }
}

pub fn tonemap_curve_to_core(v: UiTonemapCurve) -> TonemapCurve {
    match v {
        UiTonemapCurve::AcesNarkowicz => TonemapCurve::AcesNarkowicz,
        UiTonemapCurve::AcesHill => TonemapCurve::AcesHill,
        UiTonemapCurve::Agx => TonemapCurve::Agx,
        UiTonemapCurve::KhronosPbrNeutral => TonemapCurve::KhronosPbrNeutral,
    }
}

pub fn tonemap_curve_to_ui(v: TonemapCurve) -> UiTonemapCurve {
    match v {
        TonemapCurve::AcesNarkowicz => UiTonemapCurve::AcesNarkowicz,
        TonemapCurve::AcesHill => UiTonemapCurve::AcesHill,
        TonemapCurve::Agx => UiTonemapCurve::Agx,
        TonemapCurve::KhronosPbrNeutral => UiTonemapCurve::KhronosPbrNeutral,
    }
}

pub fn macro_curve_to_core(v: UiMacroCurve) -> MacroCurve {
    match v {
        UiMacroCurve::Linear => MacroCurve::Linear,
        UiMacroCurve::Exponential => MacroCurve::Exponential,
        UiMacroCurve::Logarithmic => MacroCurve::Logarithmic,
        UiMacroCurve::SCurve => MacroCurve::SCurve,
    }
}

pub fn macro_curve_to_ui(v: MacroCurve) -> UiMacroCurve {
    match v {
        MacroCurve::Linear => UiMacroCurve::Linear,
        MacroCurve::Exponential => UiMacroCurve::Exponential,
        MacroCurve::Logarithmic => UiMacroCurve::Logarithmic,
        MacroCurve::SCurve => UiMacroCurve::SCurve,
    }
}

pub fn audio_band_to_core(v: UiAudioBand) -> AudioBand {
    match v {
        UiAudioBand::Full => AudioBand::Full,
        UiAudioBand::Low => AudioBand::Low,
        UiAudioBand::Mid => AudioBand::Mid,
        UiAudioBand::High => AudioBand::High,
    }
}

pub fn audio_feature_kind_to_core(v: UiAudioFeatureKind) -> AudioFeatureKind {
    match v {
        UiAudioFeatureKind::Amplitude => AudioFeatureKind::Amplitude,
        UiAudioFeatureKind::Centroid => AudioFeatureKind::Centroid,
        UiAudioFeatureKind::Noisiness => AudioFeatureKind::Noisiness,
        UiAudioFeatureKind::Flux => AudioFeatureKind::Flux,
        UiAudioFeatureKind::Transients => AudioFeatureKind::Transients,
    }
}

pub fn audio_feature_to_core(v: UiAudioFeature) -> AudioFeature {
    AudioFeature {
        kind: audio_feature_kind_to_core(v.kind),
        band: audio_band_to_core(v.band),
    }
}

pub fn audio_source_kind_to_core(v: UiAudioSourceKind) -> AudioSourceKind {
    match v {
        UiAudioSourceKind::InputDevice => AudioSourceKind::InputDevice,
        UiAudioSourceKind::SystemAudio => AudioSourceKind::SystemAudio,
        UiAudioSourceKind::App => AudioSourceKind::App,
    }
}

pub fn audio_source_kind_to_ui(v: AudioSourceKind) -> UiAudioSourceKind {
    match v {
        AudioSourceKind::InputDevice => UiAudioSourceKind::InputDevice,
        AudioSourceKind::SystemAudio => UiAudioSourceKind::SystemAudio,
        AudioSourceKind::App => UiAudioSourceKind::App,
    }
}

pub fn ableton_mapping_status_to_ui(v: AbletonMappingStatus) -> UiAbletonMappingStatus {
    match v {
        AbletonMappingStatus::Dormant => UiAbletonMappingStatus::Dormant,
        AbletonMappingStatus::Active => UiAbletonMappingStatus::Active,
        AbletonMappingStatus::Ambiguous => UiAbletonMappingStatus::Ambiguous,
    }
}

pub fn param_convert_to_core(v: UiParamConvert) -> ParamConvert {
    match v {
        UiParamConvert::Float => ParamConvert::Float,
        UiParamConvert::IntRound => ParamConvert::IntRound,
        UiParamConvert::BoolThreshold => ParamConvert::BoolThreshold,
        UiParamConvert::EnumRound => ParamConvert::EnumRound,
        UiParamConvert::Trigger => ParamConvert::Trigger,
    }
}

// ── Small structs (bidirectional) ───────────────────────────────────────

pub fn audio_device_ref_to_core(v: &UiAudioDeviceRef) -> AudioDeviceRef {
    AudioDeviceRef {
        uid: v.uid.clone(),
        name: v.name.clone(),
        kind: audio_source_kind_to_core(v.kind),
    }
}

pub fn audio_device_ref_to_ui(v: &AudioDeviceRef) -> UiAudioDeviceRef {
    UiAudioDeviceRef {
        uid: v.uid.clone(),
        name: v.name.clone(),
        kind: audio_source_kind_to_ui(v.kind),
    }
}

pub fn ableton_macro_address_to_core(v: &UiAbletonMacroAddress) -> AbletonMacroAddress {
    AbletonMacroAddress {
        track_id: v.track_id,
        device_id: v.device_id,
        param_id: v.param_id,
        device_identity: AbletonDeviceIdentity {
            device_class_name: v.device_identity.device_class_name.clone(),
        },
        track_name: v.track_name.clone(),
        device_name: v.device_name.clone(),
        macro_name: v.macro_name.clone(),
    }
}

pub fn serialized_param_value_to_core(v: &UiSerializedParamValue) -> SerializedParamValue {
    match v {
        UiSerializedParamValue::Float { value } => SerializedParamValue::Float { value: *value },
        UiSerializedParamValue::Int { value } => SerializedParamValue::Int { value: *value },
        UiSerializedParamValue::Bool { value } => SerializedParamValue::Bool { value: *value },
        UiSerializedParamValue::Vec2 { value } => SerializedParamValue::Vec2 { value: *value },
        UiSerializedParamValue::Vec3 { value } => SerializedParamValue::Vec3 { value: *value },
        UiSerializedParamValue::Vec4 { value } => SerializedParamValue::Vec4 { value: *value },
        UiSerializedParamValue::Color { value } => SerializedParamValue::Color { value: *value },
        UiSerializedParamValue::Enum { value } => SerializedParamValue::Enum { value: *value },
        UiSerializedParamValue::Table { rows } => SerializedParamValue::Table { rows: rows.clone() },
    }
}

pub fn preset_type_id_to_core(v: &UiPresetTypeId) -> PresetTypeId {
    PresetTypeId::from_string(v.as_str().to_string())
}

// ── View-models (core → UI only; the app pushes render data into panels) ──

pub fn layer_to_ui(l: &Layer) -> UiLayer {
    UiLayer {
        layer_id: l.layer_id.clone(),
        parent_layer_id: l.parent_layer_id.clone(),
        layer_type: layer_type_to_ui(l.layer_type),
        is_collapsed: l.is_collapsed,
    }
}

pub fn layers_to_ui(layers: &[Layer]) -> Vec<UiLayer> {
    layers.iter().map(layer_to_ui).collect()
}

pub fn marker_to_ui(m: &TimelineMarker) -> UiMarker {
    UiMarker {
        id: m.id.clone(),
        beat: m.beat,
        name: m.name.clone(),
        color: marker_color_to_ui(m.color),
    }
}

pub fn markers_to_ui(markers: &[TimelineMarker]) -> Vec<UiMarker> {
    markers.iter().map(marker_to_ui).collect()
}

pub fn param_slot_to_ui(s: &ParamSlot) -> UiParamSlot {
    UiParamSlot {
        value: s.value,
        base: s.base,
        exposed: s.exposed,
    }
}

pub fn param_slots_to_ui(slots: &[ParamSlot]) -> Vec<UiParamSlot> {
    slots.iter().map(param_slot_to_ui).collect()
}

// ── Selection region (UI → core; UIState owns the UI-side region) ─────────

pub fn selection_region_to_core(r: &UiSelectionRegion) -> SelectionRegion {
    SelectionRegion {
        start_beat: r.start_beat,
        end_beat: r.end_beat,
        is_active: r.is_active,
        start_layer_id: r.start_layer_id.clone(),
        end_layer_id: r.end_layer_id.clone(),
        selected_layer_ids: r.selected_layer_ids.clone(),
    }
}
