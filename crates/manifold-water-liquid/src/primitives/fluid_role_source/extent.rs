//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

use super::{bool_from, has_mesh, role_kind};
use crate::fluid_role::{FluidRoleKind, posed_column};

/// Publishes the role's kind and, while it is on, its placement: a liquid
/// domain's pool depends on both. A shaped Initial Fill places the box round
/// its unit box turned (a sphere's own box; the walk has no mesh to bound).
/// The node itself owns no device storage.
fn fluid_role_source(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let wired = |name: &str| x.wired(name).then(|| x.scalar(name, 0.0));
    let kind = role_kind(wired("role"), x.params().get("role")).map_err(Verdict::Refused)?;
    let enabled = bool_from(wired("enabled"), x.params().get("enabled"), "enabled", true)
        .map_err(Verdict::Refused)?;
    let shaped = kind == FluidRoleKind::InitialFill && has_mesh(x.params(), |port| x.wired(port));
    let placed = x
        .transform("transform")
        .filter(|_| enabled)
        .map(|t| match shaped {
            true => posed_column([[-0.5; 3], [0.5; 3]], t).unwrap_or(t),
            false => t,
        });
    x.publish_object("role", kind.index(), placed);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.fluid_role_source", check: fluid_role_source }
}
