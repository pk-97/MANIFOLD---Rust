//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::nodes_total;

fn carry_liquid_into_solids(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    let solid_nodes = x.nodes(["solid_nodes_x", "solid_nodes_y", "solid_nodes_z"]);
    if nodes.iter().chain(&solid_nodes).any(|&n| n < 2.0) {
        return Err(x.uncovered(format!(
            "no lattice: nodes {nodes:?}, solid nodes {solid_nodes:?}"
        )));
    }
    let field = nodes_total(nodes) * 4;
    let solid = nodes_total(solid_nodes) * 4;
    x.covers("levelset", field)?;
    x.covers("out", field)?;
    x.covers("solid", solid)?;
    x.covers_if_bound("drawn_solid", solid)
}
inventory::submit! {
    ExtentRule { type_id: "node.carry_liquid_into_solids", check: carry_liquid_into_solids }
}
