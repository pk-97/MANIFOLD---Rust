#![forbid(unsafe_code)]

pub mod anim;
pub mod automation;
pub mod automation_drag;
pub mod automation_hit_tester;
pub mod bitmap_painter;
pub mod chrome;
pub mod clip_hit_tester;
pub mod color;
pub mod coordinate_mapper;
pub mod cursor_nav;
pub mod cursors;
pub mod dock;
pub mod drag;
pub mod draw;
pub mod fmt;
pub mod graph_canvas;
pub mod graph_edit;
pub mod graph_view;
pub mod hit;
pub mod hit_targets;
pub mod icons;
pub mod input;
pub mod intent;
pub mod interaction_overlay;
pub mod layout;
pub mod mini_timeline;
pub mod node;
pub mod panels;
pub mod param_surface;
pub mod scroll_container;
pub mod slider;
pub mod snap;
pub mod stepper;
pub mod text;
pub mod text_edit;
pub mod timeline_editing_host;
pub mod timeline_input_host;
pub mod timing_grid;
pub mod transform;
pub mod transform2d;
pub mod tree;
pub mod types;
pub mod ui_state;
pub mod value_cell;
pub mod view;
pub mod waveform_painter;
pub mod waveform_renderer;

pub use automation::{
    AssertCheck, AutomationAction, AutomationTarget, Gesture, MatchInfo, ResolveError,
    ResolvedTarget, SelectorQuery, interpolate_drag, resolve, resolve_all,
};
pub use coordinate_mapper::CoordinateMapper;
pub use dock::{Dock, DockDrawKey, DockEdge};
pub use draw::{Depth, Painter};
pub use graph_canvas::{GraphCanvas, MappingPopover};
pub use graph_edit::GraphEditCommand;
pub use hit_targets::{HitTargetEntry, HitTargets};
pub use input::{Modifiers, PointerAction, UIEvent, UIInputSystem};
pub use layout::ScreenLayout;
pub use mini_timeline::{MiniClip, MiniLayerLabel, MiniTimeline};
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
    ParamCardKind, ParamCardPanel, ParamCardState, ParamCardStringChoice, ParamCardStringInfo,
    RowMod,
};
pub use panels::transport::TransportPanel;
pub use panels::viewport::{
    ClipHitResult, SelectionRegion, TimelineViewportPanel, TrackInfo, ViewportClip,
};
pub use panels::{
    AudioSetupAction, AudioShapeParam, BandDivider, BrowserAction, ClipAction, DriverConfigAction,
    EditingAction, GraphParamTarget, HitRegion, InspectorTab, LayerAction, MappingAction,
    MarkerAction, ModulationAction, Panel, PanelAction, ParamsAction, ProjectAction, RootAction,
    ScrubPhase, ScrubValue, SyncSource, TransportAction, TrimKind, ValueRef,
};
pub use param_surface::{
    ColourGroup, MaterialColour, MaterialFeature, MaterialGroup, MaterialLook, MaterialMapFamily,
    MaterialParamRole, ParamRow, ParamSurface, RgbChannel, RgbMembers, RowMapping, RowSpec,
    RowValue, SamplerComponent, UvComponent,
};
pub use slider::{BitmapSlider, SliderColors, SliderNodeIds};
pub use transform2d::Affine2;
pub use tree::{RegionToken, UITree, ZTier};
pub use types::{
    AbletonDeviceIdentity, AbletonMacroAddress, AbletonMappingStatus, AudioBand, AudioDeviceRef,
    AudioFeature, AudioFeatureKind, AudioSourceKind, DriverWaveform, FLOOR_DB_OFF, LayerType,
    MACRO_COUNT, MacroCurve, MarkerColor, MidiTriggerMode, ParamConvert, PresetTypeId,
    SerializedParamValue, TonemapCurve, apply_card_reshape, is_default_macro_name,
    note_number_to_name,
};
pub use ui_state::{TimelineSelection, UIState};
pub use view::{UiLayer, UiMarker, UiParamSlot};
pub use waveform_renderer::WaveformRenderer;
