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

use manifold_ui::view::{
    SelectionRegion as UiSelectionRegion, UiAutomationLane, UiAutomationPoint, UiLayer, UiMarker,
    UiParamSlot, UiSegmentShape,
};
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
use manifold_core::effects::{ParamConvert, ParamSlot, PresetInstance, SegmentShape, resolve_param_in};
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
        automation_lane_count: 0,
    }
}

pub fn layers_to_ui(layers: &[Layer]) -> Vec<UiLayer> {
    layers.iter().map(layer_to_ui).collect()
}

/// Same as [`layers_to_ui`], but also resolves `automation_lane_count` — the
/// one field that feeds `CoordinateMapper::layer_height`'s Y-layout, so the
/// header column and viewport tracks grow together when automation mode is on
/// (`docs/AUTOMATION_LANES_DESIGN.md` §7). Callers that only need the
/// selection-shape fields (hit-testing, region math) should keep using the
/// plain `layers_to_ui` — computing lane counts there would be wasted work
/// and (more importantly) would NOT be the value `rebuild_mapper_layout` used,
/// so a second call site recomputing it independently is exactly the
/// "single-source-of-truth" trap this field exists to avoid.
pub fn layers_to_ui_for_layout(layers: &[Layer], automation_visible: bool) -> Vec<UiLayer> {
    layers
        .iter()
        .map(|l| {
            let mut ui = layer_to_ui(l);
            if automation_visible && !l.is_collapsed && !l.is_group() {
                ui.automation_lane_count = layer_automation_lanes_to_ui(l).len();
            }
            ui
        })
        .collect()
}

/// Collect UI-facing automation lane view-models for one layer's instances —
/// its effects chain plus its generator params, if any (mirrors the walk
/// shape of `manifold_playback::automation::evaluate_all_automation`). Only
/// `enabled` lanes produce a strip (Ableton: a deactivated lane draws
/// nothing). See `docs/AUTOMATION_LANES_DESIGN.md` §7.
pub fn layer_automation_lanes_to_ui(layer: &Layer) -> Vec<UiAutomationLane> {
    let mut out = Vec::new();
    if let Some(effects) = &layer.effects {
        for fx in effects {
            push_instance_automation_lanes(fx, &mut out);
        }
    }
    if let Some(gp) = layer.gen_params() {
        push_instance_automation_lanes(gp, &mut out);
    }
    out
}

fn push_instance_automation_lanes(instance: &PresetInstance, out: &mut Vec<UiAutomationLane>) {
    let Some(lanes) = instance.automation_lanes.as_ref() else {
        return;
    };
    if lanes.is_empty() {
        return;
    }
    let Some(def) = manifold_core::preset_definition_registry::try_get(instance.effect_type())
    else {
        return;
    };
    let effect_label = manifold_core::preset_type_registry::display_name(instance.effect_type());
    for lane in lanes {
        if !lane.enabled {
            continue;
        }
        let Some(resolved) = resolve_param_in(&def, instance, lane.param_id.as_ref()) else {
            continue;
        };
        let range = (resolved.max - resolved.min).abs().max(f32::EPSILON);
        let points = lane
            .points
            .iter()
            .map(|p| UiAutomationPoint {
                beat: p.beat,
                value_norm: ((p.value - resolved.min) / range).clamp(0.0, 1.0),
                shape: segment_shape_to_ui(p.shape),
            })
            .collect();
        out.push(UiAutomationLane {
            effect_id: instance.id.clone(),
            param_id: lane.param_id.clone(),
            label: format!("{effect_label}: {}", lane.param_id),
            points,
        });
    }
}

fn segment_shape_to_ui(s: SegmentShape) -> UiSegmentShape {
    match s {
        SegmentShape::Linear => UiSegmentShape::Linear,
        SegmentShape::Hold => UiSegmentShape::Hold,
        SegmentShape::Curved(bend) => UiSegmentShape::Curved(bend),
    }
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

// ── Editor graph snapshot (renderer → UI view-model) ─────────────────────────
//
// Phase 8 of `docs/UI_ARCHITECTURE_OVERHAUL.md`: the graph canvas lives in
// `manifold-ui` and reads `manifold_ui::graph_view`, so the app translates the
// renderer's `GraphSnapshot` at the boundary — the same pattern as the layer /
// marker view-models above. The node catalog stays renderer-side: the node
// `category` + `tooltip` and per-param `tooltip` are *resolved here* (via the
// renderer's `descriptor_for` / `tooltip_for`) and baked into the UI snapshot, so
// the canvas reads them straight off the data.

use manifold_renderer::node_graph as rg;
use manifold_ui::graph_view as gv;

/// Translate the renderer's editor-graph snapshot into the UI-local view-model
/// the canvas consumes. The whole nested structure (group bodies, ports, params,
/// wires, outer routings) is converted; `descriptor_for`/`tooltip_for` are
/// applied per node so the catalog never leaves the renderer crate.
pub fn graph_snapshot_to_ui(s: &rg::GraphSnapshot) -> gv::GraphSnapshot {
    gv::GraphSnapshot {
        nodes: s.nodes.iter().map(graph_node_to_ui).collect(),
        wires: s.wires.iter().map(graph_wire_to_ui).collect(),
        outer_routings: s.outer_routings.iter().map(outer_routing_to_ui).collect(),
    }
}

fn graph_node_to_ui(n: &rg::NodeSnapshot) -> gv::NodeSnapshot {
    let descriptor = rg::descriptor_for(&n.type_id);
    let category = descriptor
        .map(|d| category_to_ui(d.category))
        .unwrap_or(gv::Category::Uncategorized);
    let tooltip = descriptor
        .map(|d| d.summary)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    gv::NodeSnapshot {
        id: n.id,
        node_id: n.node_id.clone(),
        node_handle: n.node_handle.clone(),
        type_id: n.type_id.clone(),
        title: n.title.clone(),
        inputs: n.inputs.iter().map(graph_port_to_ui).collect(),
        outputs: n.outputs.iter().map(graph_port_to_ui).collect(),
        parameters: n
            .parameters
            .iter()
            .map(|p| graph_param_to_ui(&n.type_id, p))
            .collect(),
        editor_pos: n.editor_pos,
        breaks_dependency_cycle: n.breaks_dependency_cycle,
        group: n
            .group
            .as_deref()
            .map(|g| Box::new(graph_group_to_ui(g))),
        wgsl_source: n.wgsl_source.clone(),
        category,
        tooltip,
    }
}

fn graph_group_to_ui(g: &rg::GroupSnapshot) -> gv::GroupSnapshot {
    gv::GroupSnapshot {
        nodes: g.nodes.iter().map(graph_node_to_ui).collect(),
        wires: g.wires.iter().map(graph_wire_to_ui).collect(),
        tint: g.tint,
    }
}

fn graph_param_to_ui(type_id: &str, p: &rg::ParamSnapshot) -> gv::ParamSnapshot {
    gv::ParamSnapshot {
        name: p.name.clone(),
        label: p.label.clone(),
        kind: param_kind_to_ui(p.kind),
        default_value: p.default_value,
        current_value: p.current_value,
        range: p.range,
        enum_labels: p.enum_labels.clone(),
        exposed: p.exposed,
        summary: p.summary.clone(),
        vec_value: p.vec_value,
        string_value: p.string_value.clone(),
        table_value: p.table_value.clone(),
        tooltip: rg::tooltip_for(type_id, &p.name).map(str::to_owned),
    }
}

fn graph_port_to_ui(p: &rg::PortSnapshot) -> gv::PortSnapshot {
    gv::PortSnapshot {
        name: p.name.clone(),
        kind: port_kind_to_ui(&p.kind),
    }
}

fn graph_wire_to_ui(w: &rg::WireSnapshot) -> gv::WireSnapshot {
    gv::WireSnapshot {
        from_node: w.from_node,
        from_port: w.from_port.clone(),
        to_node: w.to_node,
        to_port: w.to_port.clone(),
    }
}

fn outer_routing_to_ui(r: &rg::OuterParamRouting) -> gv::OuterParamRouting {
    gv::OuterParamRouting {
        outer_label: r.outer_label.clone(),
        outer_param_id: r.outer_param_id.clone(),
        node_handle: r.node_handle.clone(),
        inner_param: r.inner_param.clone(),
        source: match r.source {
            rg::OuterParamSource::Static => gv::OuterParamSource::Static,
            rg::OuterParamSource::User => gv::OuterParamSource::User,
        },
    }
}

fn port_kind_to_ui(k: &rg::PortKindSnapshot) -> gv::PortKindSnapshot {
    match k {
        rg::PortKindSnapshot::Texture2D => gv::PortKindSnapshot::Texture2D,
        rg::PortKindSnapshot::Texture2DTyped { slots } => gv::PortKindSnapshot::Texture2DTyped {
            slots: slots.clone(),
        },
        rg::PortKindSnapshot::Texture3D => gv::PortKindSnapshot::Texture3D,
        rg::PortKindSnapshot::Scalar => gv::PortKindSnapshot::Scalar,
        rg::PortKindSnapshot::Array {
            channels,
            match_mode,
            item_size,
            item_align,
        } => gv::PortKindSnapshot::Array {
            channels: channels
                .iter()
                .map(|c| gv::ChannelSnapshot {
                    name: c.name.clone(),
                    ty: c.ty.clone(),
                })
                .collect(),
            match_mode: match match_mode {
                rg::ArrayMatchMode::Exact => gv::ArrayMatchMode::Exact,
                rg::ArrayMatchMode::Permissive => gv::ArrayMatchMode::Permissive,
            },
            item_size: *item_size,
            item_align: *item_align,
        },
        rg::PortKindSnapshot::Camera => gv::PortKindSnapshot::Camera,
        rg::PortKindSnapshot::Light => gv::PortKindSnapshot::Light,
        rg::PortKindSnapshot::Material => gv::PortKindSnapshot::Material,
    }
}

fn param_kind_to_ui(k: rg::ParamSnapshotKind) -> gv::ParamSnapshotKind {
    match k {
        rg::ParamSnapshotKind::Float => gv::ParamSnapshotKind::Float,
        rg::ParamSnapshotKind::Angle => gv::ParamSnapshotKind::Angle,
        rg::ParamSnapshotKind::Frequency => gv::ParamSnapshotKind::Frequency,
        rg::ParamSnapshotKind::Int => gv::ParamSnapshotKind::Int,
        rg::ParamSnapshotKind::Bool => gv::ParamSnapshotKind::Bool,
        rg::ParamSnapshotKind::Enum => gv::ParamSnapshotKind::Enum,
        rg::ParamSnapshotKind::Trigger => gv::ParamSnapshotKind::Trigger,
        rg::ParamSnapshotKind::Color => gv::ParamSnapshotKind::Color,
        rg::ParamSnapshotKind::Vec2 => gv::ParamSnapshotKind::Vec2,
        rg::ParamSnapshotKind::Vec3 => gv::ParamSnapshotKind::Vec3,
        rg::ParamSnapshotKind::Vec4 => gv::ParamSnapshotKind::Vec4,
        rg::ParamSnapshotKind::String => gv::ParamSnapshotKind::String,
        rg::ParamSnapshotKind::Other => gv::ParamSnapshotKind::Other,
    }
}

fn category_to_ui(c: rg::Category) -> gv::Category {
    match c {
        rg::Category::Uncategorized => gv::Category::Uncategorized,
        rg::Category::ColorAndTone => gv::Category::ColorAndTone,
        rg::Category::BlurAndSharpen => gv::Category::BlurAndSharpen,
        rg::Category::DistortAndWarp => gv::Category::DistortAndWarp,
        rg::Category::Stylize => gv::Category::Stylize,
        rg::Category::Generate => gv::Category::Generate,
        rg::Category::Noise => gv::Category::Noise,
        rg::Category::Mask => gv::Category::Mask,
        rg::Category::Composite => gv::Category::Composite,
        rg::Category::Geometry3D => gv::Category::Geometry3D,
        rg::Category::MaterialsAndLighting => gv::Category::MaterialsAndLighting,
        rg::Category::Particles2D => gv::Category::Particles2D,
        rg::Category::Particles3D => gv::Category::Particles3D,
        rg::Category::Control => gv::Category::Control,
        rg::Category::DetectionAndSampling => gv::Category::DetectionAndSampling,
        rg::Category::MathAndConvert => gv::Category::MathAndConvert,
        rg::Category::Routing => gv::Category::Routing,
        rg::Category::FieldsAndCoordinates => gv::Category::FieldsAndCoordinates,
    }
}
