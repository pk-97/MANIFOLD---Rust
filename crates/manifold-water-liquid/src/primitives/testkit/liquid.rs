//! Liquid-seam node constructors for family proofs.
use manifold_node_engine::primitive::Primitive;
pub fn running_total() -> impl Primitive {
    crate::primitives::running_total::RunningTotal::new()
}
