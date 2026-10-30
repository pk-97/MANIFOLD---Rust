pub(crate) mod decode_cache;
pub mod gizmo_pass;
mod gltf_anim_cache;
mod gltf_anim_identity;
pub mod gltf_import;
pub(crate) mod hdri_sun;
pub mod material_inspector;
pub mod node_param_source;
pub mod scene_surface;
pub mod view_copy;
pub mod viewport_gizmo;
pub mod viewport_overlay;
pub mod viewport_render;
pub mod viewport_session;
manifold_core::testkit_visible! { mod gltf_load; }
pub mod primitives;
pub mod scene_modifier_authoring;
pub mod scene_vm;

mod cinematic_tail;
pub mod scene_camera;
pub mod scene_compile;
mod scene_graph;
