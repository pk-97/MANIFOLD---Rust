//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

use super::role_kind;

/// Publishes the role kind, which a liquid domain's pool depends on. The
/// node itself owns no device storage.
fn fluid_role_source(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let wired = x.wired("role").then(|| x.scalar("role", 1.0));
    let kind = role_kind(wired, x.params().get("role")).map_err(Verdict::Refused)?;
    x.publish_kind("role", kind.index());
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.fluid_role_source", check: fluid_role_source }
}
