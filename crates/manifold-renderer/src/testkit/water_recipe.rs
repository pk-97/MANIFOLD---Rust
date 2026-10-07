//! Water recipe structure and as-rendered budget fixtures.
use super::*;
#[cfg(feature = "gpu-proofs")]
pub(crate) fn surface_group() -> Value { super::surface_group() }
pub(crate) fn family_outputs() -> [&'static str; 4] { super::FAMILY_OUTPUTS }
pub(crate) fn surface_detail_offset() -> usize { super::SURFACE_DETAIL_OFFSET }
pub(crate) fn assert_preset_root(nodes: &[EffectGraphNode]) { super::assert_preset_root(nodes); }
pub(crate) fn rendered_scene_bytes(scene: WaterScene) -> u64 { super::rendered_scene_bytes(scene) }
pub(crate) fn fused_as_rendered(def: &EffectGraphDef, registry: &crate::node_graph::PrimitiveRegistry) -> Option<crate::node_graph::freeze::install::FusedGeneratorView> { super::fused_as_rendered(def, registry) }
