//! Effect & generator graph system.
//!
//! See `docs/NODE_GRAPH_SYSTEM.md` for the full architecture overview.
//!
//! This module currently defines only the core abstractions — the [`EffectNode`]
//! trait, port and parameter types, and graph-level identifiers. The graph
//! runtime (topological sort, execution plan, lifetime planner, resource
//! bindings) lands in subsequent steps.

pub mod material_inspector;
pub mod scene_exposure;
pub mod viewport_gizmo;
pub mod viewport_overlay;
pub mod viewport_render;
pub mod viewport_session;
pub(crate) mod bundled_presets;
pub mod catalog_gen;
pub mod composites;
pub(crate) mod decode_cache;
mod gltf_anim_cache;
mod gltf_anim_identity;
pub mod gltf_import;
mod gltf_load;
pub mod primitives;
pub mod relight;
pub mod scene_modifier_authoring;
pub mod scene_modifier_legacy_migration;
pub mod scene_vm;


pub use backend::{Backend, MockBackend};
pub use bindings::{NodeInputs, NodeOutputs, Slot};
pub use content_revision::{ContentVersion, StorageRevision};
pub use camera::{Camera, CameraMode};
pub use light::{Light, LightMode, ShadowSoftness};
pub use material::{Material, MaterialKind};
pub use scene_object::SceneObject;
pub use transform::Transform;
pub use fluid_role::{FluidRole, FluidRoleKind, PreparedFluidGeometry, MAX_FLUID_ROLES};
pub use mesh_source::MeshSource;
pub use viewport_camera::ViewportCamera;
pub use viewport_overlay::{
    ScreenLine, ViewportOverlayConfig, WorldLine, build_overlay_lines, camera_frustum_lines,
    composite_overlay_lines_rgba8, grid_lines, light_billboard_lines, project_lines,
};
pub use viewport_gizmo::{
    GizmoAxis, GizmoMode, GizmoTarget, GizmoTargetKind, drag_write, gizmo_lines, gizmo_target_for, move_drag_delta,
    pick_axis, pick_object, rotate_drag_delta, scale_drag_delta,
};
pub use viewport_render::{ViewportRenderError, override_camera_def, render_viewport_frame};
pub use viewport_session::ViewportSession;
pub use boundary_nodes::{
    FINAL_OUTPUT_TYPE_ID, FinalOutput, GENERATOR_INPUT_TYPE_ID, GeneratorInput, SOURCE_TYPE_ID,
    Source,
};
pub use binding_migration::migrate_user_param_bindings_to_node_id;
pub use bound_graph::{
    BoundGraph, FusedRetarget, ShadowedDefParam, apply_inner_param_overrides,
    audible_shadow_findings, find_shadowed_def_params, is_baseline_shadow,
    shadow_baseline_entries, unretarget_shadow,
};
pub use bundled_presets::{
    bundled_preset_def, bundled_preset_json, bundled_preset_type_ids, loaded_presets_from_bundled,
    loaded_scene_modifier_presets_from_bundled,
};
pub use effect_node::{
    intern_name, EffectNode, EffectNodeContext, EffectNodeType, FrameTime, NodeInstanceId,
    NodeRequires, NodeWire, ParamValues, RtQuality,
};
#[cfg(feature = "gpu-proofs")]
pub use effect_node::NodeErrorTap;
pub use execution::{Executor, StepProfile};
pub use execution_plan::{ExecutionPlan, ExecutionStep, ResourceId, compile};
pub use chain_spec::{SpliceResult, splice_def_into_chain};
pub use graph::{Graph, NodeInstance, WireWalkMode};
pub use graph_loader::{
    BoundaryHandling, GraphBuildError, HandleScope, NodeInstantiation, PreAllocationError,
    WireSide as BuildWireSide, instantiate_def, log_build_error, pre_allocate_resources, allocate_resources,
};
pub(crate) use graph_loader::{has_retired_params, retire_params};
pub use loaded_preset_view::{
    LoadedPresetView, collect_node_handles, loaded_preset_view_by_id, outer_routings_from_view,
    snapshot_for_view,
};
pub use metal_backend::MetalBackend;
pub(crate) use metal_backend::PreparedMetalBackendResize;
pub use mesh_change::{
    MeshAspect, MeshDependency, MeshOutputRule, MeshRevision, MeshRevisionRule,
    PreparedMeshOutputRule, PreparedMeshRevisionRule, PreparedMeshRules,
};
pub use palette::{catalog_graph_def_for, palette_atoms, PaletteAtom};
pub(crate) use param_binding::Reshape;
pub use param_binding::{
    BindingCacheEntry, BindingSource, LastAppliedCache, ParamBinding, ParamConvert, ParamId,
    ParamTarget, ResolvedBinding, ResolvedTarget, apply_binding_defaults, apply_bindings,
    binding_value, convert_param_value, outer_routings_from_bindings,
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
pub use physics_events::{ImpulseTarget, ResolvedNodeImpulse};
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




#[cfg(test)]
mod catalog_tests;
#[cfg(test)]
mod scene_tests;
#[cfg(test)]
mod image_tests;

#[cfg(any(test, feature = "gpu-proofs"))]
pub mod liquid_conformance_fixtures;
