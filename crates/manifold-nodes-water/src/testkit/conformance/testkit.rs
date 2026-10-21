//! Shared conformance arithmetic and scene setup.
use super::*;
pub const G: f32 = super::G;
pub fn liquid_totals(words: &[u32]) -> LiquidTotals { super::liquid_totals(words) }
pub fn matter_totals(words: &[u32]) -> LiquidTotals { super::matter_totals(words) }
pub fn matter_faces(bytes: &[u8], cells: [u32; 3]) -> [Vec<f32>; 3] { super::matter_faces(bytes, cells) }
pub fn set_fill_param(def: &mut EffectGraphDef, type_id: &str, param: &str, value: f32) { super::set_fill_param(def, type_id, param, value); }
pub fn drop_fill(def: &mut EffectGraphDef, type_id: &str) { super::drop_fill(def, type_id); }
impl BoxScene {
    #[doc(hidden)]
    pub fn test_stacked(&self, fixture: Fixture, def: EffectGraphDef) -> EffectGraphDef { self.stacked(fixture, def) }
    #[doc(hidden)]
    pub fn test_set(self, def: EffectGraphDef, type_id: &str) -> EffectGraphDef { self.set(def, type_id) }
}
