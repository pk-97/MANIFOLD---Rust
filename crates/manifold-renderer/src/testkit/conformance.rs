//! Shared conformance arithmetic and scene setup.
use super::*;
pub(crate) const G: f32 = super::G;
pub(crate) fn liquid_totals(words: &[u32]) -> LiquidTotals { super::liquid_totals(words) }
pub(crate) fn matter_totals(words: &[u32]) -> LiquidTotals { super::matter_totals(words) }
pub(crate) fn matter_faces(bytes: &[u8], cells: [u32; 3]) -> [Vec<f32>; 3] { super::matter_faces(bytes, cells) }
pub(crate) fn set_source_param(def: &mut EffectGraphDef, type_id: &str, port: &str, param: &str, value: f32) { super::set_source_param(def, type_id, port, param, value); }
impl BoxScene {
    #[doc(hidden)]
    pub(crate) fn test_stacked(&self, fixture: Fixture, def: EffectGraphDef) -> EffectGraphDef { self.stacked(fixture, def) }
    #[doc(hidden)]
    pub(crate) fn test_set(self, def: EffectGraphDef, type_id: &str) -> EffectGraphDef { self.set(def, type_id) }
}
