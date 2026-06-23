#![forbid(unsafe_code)]

pub mod bitmap_painter;
pub mod bitmap_renderer;
pub mod chrome;
pub mod clip_hit_tester;
pub mod color;
pub mod coordinate_mapper;
pub mod cursor_nav;
pub mod cursors;
pub mod drag;
pub mod draw;
pub mod driver_waveform_icons;
pub mod graph_edit;
pub mod hit;
pub mod input;
pub mod inspector_layout;
pub mod intent;
pub mod interaction_overlay;
pub mod layout;
pub mod node;
pub mod panels;
pub mod scroll_container;
pub mod slider;
pub mod snap;
pub mod text;
pub mod timeline_editing_host;
pub mod timeline_input_host;
pub mod transform;
pub mod tree;
pub mod trim;
pub mod types;
pub mod ui_state;
pub mod view;
pub mod waveform_painter;
pub mod waveform_renderer;
pub mod widget_layout;

pub use bitmap_renderer::{BitmapRepaintState, LayerBitmapRenderer};
pub use coordinate_mapper::CoordinateMapper;
pub use draw::{Depth, Painter};
pub use graph_edit::GraphEditCommand;
pub use input::{Modifiers, PointerAction, UIEvent, UIInputSystem};
pub use layout::ScreenLayout;
pub use node::*;
pub use panels::clip_chrome::ClipChromePanel;
pub use panels::dropdown::{DropdownAction, DropdownItem, DropdownPanel};
pub use panels::footer::FooterPanel;
pub use panels::header::HeaderPanel;
pub use panels::inspector::InspectorCompositePanel;
pub use panels::layer_chrome::LayerChromePanel;
pub use panels::layer_header::{LayerHeaderPanel, LayerInfo};
pub use panels::master_chrome::MasterChromePanel;
pub use panels::param_card::{
    ParamCardConfig, ParamCardKind, ParamCardPanel, ParamCardState, ParamCardStringInfo, ParamInfo,
};
pub use panels::stem_lane::{STEM_COUNT, STEM_NAMES, StemLaneGroupPanel};
pub use panels::transport::TransportPanel;
pub use panels::viewport::{
    ClipHitResult, SelectionRegion, TimelineViewportPanel, TrackInfo, ViewportClip,
};
pub use panels::waveform_lane::WaveformLanePanel;
pub use panels::{
    AudioShapeParam, BandDivider, DriverConfigAction, GraphParamTarget, HitRegion, InspectorTab,
    Panel, PanelAction, SyncSource, TrimKind,
};
pub use slider::{BitmapSlider, SliderColors, SliderNodeIds};
pub use tree::UITree;
pub use types::{
    AbletonDeviceIdentity, AbletonMacroAddress, AbletonMappingStatus, AudioBand, AudioDeviceRef,
    AudioFeature, AudioFeatureKind, AudioSourceKind, DriverWaveform, FLOOR_DB_OFF, LayerType,
    MACRO_COUNT, MacroCurve, MarkerColor, MidiTriggerMode, ParamConvert, PresetTypeId,
    SerializedParamValue, TonemapCurve, is_default_macro_name, note_number_to_name,
};
pub use ui_state::UIState;
pub use view::{UiLayer, UiMarker, UiParamSlot};
pub use waveform_renderer::WaveformRenderer;
