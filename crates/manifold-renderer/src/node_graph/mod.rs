//! Effect & generator graph system.
//!
//! See `docs/NODE_GRAPH_SYSTEM.md` for the full architecture overview.
//!
//! This module currently defines only the core abstractions — the [`EffectNode`]
//! trait, port and parameter types, and graph-level identifiers. The graph
//! runtime (topological sort, execution plan, lifetime planner, resource
//! bindings) lands in subsequent steps.

pub mod atomic;
mod backend;
mod bindings;
pub mod atmosphere;
pub mod camera;
pub mod light;
pub mod material;
pub mod scene_exposure;
pub mod scene_object;
pub mod transform;
pub mod viewport_camera;
pub mod viewport_gizmo;
pub mod viewport_overlay;
pub mod viewport_render;
pub mod viewport_session;
mod binding_migration;
mod boundary_nodes;
mod bound_graph;
mod bundled_presets;
pub mod catalog_gen;
mod chain_spec;
pub mod composites;
pub mod depth_rule;
pub mod descriptor;
pub mod preview_encoding;
mod effect_node;
pub(crate) mod execution;
mod execution_plan;
pub mod freeze;
mod graph;
mod graph_loader;
mod gltf_anim_cache;
pub mod gltf_import;
mod gltf_load;
mod loaded_preset_view;
mod metal_backend;
mod palette;
mod param_binding;
pub mod param_doc;
mod param_tooltips_bulk;
mod param_tooltips_table;
mod parameters;
mod persistence;
pub mod ports;
pub mod primitive;
pub mod primitives;
pub mod relight;
pub mod scene_vm;
mod snapshot;
mod state_store;
pub mod temporal_reset;
pub mod trigger_shadow_lint;
pub mod validate;
mod validation;

/// Canonical channel-name registry for the Channel type system. The
/// `well_known_channels!` macro generates the constants and the
/// collision-check test from a single source list; see the module
/// docs and `docs/CHANNEL_TYPE_SYSTEM.md` §7.
pub mod channel_names;

pub use backend::{Backend, MockBackend};
pub use bindings::{NodeInputs, NodeOutputs, Slot};
pub use camera::{Camera, CameraMode};
pub use light::{Light, LightMode, ShadowSoftness};
pub use material::{Material, MaterialKind};
pub use scene_object::SceneObject;
pub use transform::Transform;
pub use viewport_camera::ViewportCamera;
pub use viewport_overlay::{
    ScreenLine, ViewportOverlayConfig, WorldLine, build_overlay_lines, camera_frustum_lines,
    composite_overlay_lines_rgba8, grid_lines, light_billboard_lines, project_lines,
};
pub use viewport_gizmo::{
    GizmoAxis, GizmoMode, GizmoTarget, drag_write, gizmo_lines, gizmo_target_for, move_drag_delta,
    pick_axis, pick_object, rotate_drag_delta, scale_drag_delta,
};
pub use viewport_render::{ViewportRenderError, override_camera_def, render_viewport_frame};
pub use viewport_session::ViewportSession;
pub use boundary_nodes::{
    FINAL_OUTPUT_TYPE_ID, FinalOutput, GENERATOR_INPUT_TYPE_ID, GeneratorInput, SOURCE_TYPE_ID,
    Source,
};
pub use binding_migration::migrate_user_param_bindings_to_node_id;
pub use bound_graph::{BoundGraph, apply_inner_param_overrides};
pub use bundled_presets::{
    bundled_preset_def, bundled_preset_json, bundled_preset_type_ids, loaded_presets_from_bundled,
};
pub use effect_node::{
    intern_name, EffectNode, EffectNodeContext, EffectNodeType, FrameTime, NodeInstanceId,
    NodeRequires, NodeWire, ParamValues,
};
pub use execution::{Executor, StepProfile};
pub use execution_plan::{ExecutionPlan, ExecutionStep, ResourceId, compile};
pub use chain_spec::{SkipMode, SpliceResult, is_skipped_for, splice_def_into_chain};
pub use graph::{Graph, NodeInstance, WireWalkMode};
pub use graph_loader::{
    BoundaryHandling, GraphBuildError, HandleScope, NodeInstantiation, PreAllocationError,
    WireSide as BuildWireSide, instantiate_def, log_build_error, pre_allocate_resources,
};
pub use loaded_preset_view::{
    LoadedPresetView, collect_node_handles, loaded_preset_view_by_id, outer_routings_from_view,
    snapshot_for_view,
};
pub use metal_backend::MetalBackend;
pub use palette::{catalog_graph_def_for, palette_atoms, PaletteAtom};
pub(crate) use param_binding::Reshape;
pub use param_binding::{
    BindingCacheEntry, BindingSource, LastAppliedCache, ParamBinding, ParamConvert, ParamId,
    ParamTarget, ResolvedBinding, ResolvedTarget, apply_binding_defaults, apply_bindings,
    binding_value, outer_routings_from_bindings,
};
pub use parameters::{ParamDef, ParamType, ParamValue};
pub use persistence::{
    EffectGraphDefExt, GRAPH_DOCUMENT_VERSION, GraphDocument, LoadError, NodeConstructor,
    NodeDocument, PrimitiveRegistry, SerializedParamValue, WireDocument, WireSide,
};
pub use ports::{
    ArrayType, ChannelElementType, ChannelName, ChannelSpec, KnownItem, MatchMode, NodeInput,
    NodeOutput, NodePort, PortKind, PortType, ScalarType, TextureChannels, std430_layout,
    std430_stride, std430_stride_and_align,
};
pub use descriptor::{Category, NodeDescriptor, Role, descriptor_for};
pub use preview_encoding::{LiveNodeParams, PreviewEncoding, PreviewScalarIo};
pub use param_doc::{ParamDoc, tooltip_for};
pub use primitive::{Primitive, PrimitiveDescription, PrimitiveSpec};
pub use snapshot::{
    ArrayMatchMode, ChannelSnapshot, GraphSnapshot, GroupSnapshot, NodeSnapshot, OuterParamRouting,
    OuterParamSource, ParamSnapshot, ParamSnapshotKind, PortKindSnapshot, PortSnapshot,
    WireSnapshot,
};
/// Crate-internal: the `ParamValue → f32` flattening the live-value tap shares
/// with the structural snapshot, so frozen and live values format identically.
pub(crate) use snapshot::param_default_to_f32;
pub use freeze::{FusionReport, NodeFusionInfo, RegionSummary, fusion_report};
pub use state_store::{NodeState, OwnerKey, StateStore};
pub use validate::{ValidateKind, ValidationIssue, ValidationReport, validate_def};
pub use validation::{
    ChannelMismatchInfo, ChannelMismatchReason, GraphError, TextureChannelMismatchInfo,
    TextureChannelMismatchReason, channels_compatible, texture_channels_compatible,
    topological_sort, validate,
};
