//! Water recipe structure and as-rendered budget fixtures.
use super::*;
#[cfg(feature = "gpu-proofs")]
pub fn surface_group() -> Value { super::surface_group() }
pub fn family_outputs() -> [&'static str; 4] { super::FAMILY_OUTPUTS }
pub fn surface_detail_offset() -> usize { super::SURFACE_DETAIL_OFFSET }
pub fn assert_preset_root(nodes: &[EffectGraphNode]) { super::assert_preset_root(nodes); }
pub fn rendered_scene_bytes(scene: WaterScene) -> u64 { super::rendered_scene_bytes(scene) }
pub fn fused_as_rendered(def: &EffectGraphDef, registry: &crate::persistence::PrimitiveRegistry) -> Option<crate::freeze::install::FusedGeneratorView> { super::fused_as_rendered(def, registry) }
