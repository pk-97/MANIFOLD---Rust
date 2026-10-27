//! Buffer extent rule owned by this node.
use crate::matter::{grid_bytes, pool_extent};
use crate::primitives::matter_face_component::matter_cells;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::scene::world_tiles::table_words;
use manifold_water_liquid::extent::whole;
use manifold_water_liquid::grid::face_len;
use manifold_water_liquid::primitives::face_sample_component::axis_param;

fn matter_face_component(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| whole(x, name, 71.0));
    let (Some(cells), Some(axis)) = (matter_cells(nodes), axis_param(x.params())) else {
        return Err(Verdict::Refused(format!(
            "a {nodes:?} node lattice has no cells, or the axis is not X, Y or Z"
        )));
    };
    let pool = pool_extent(x)?;
    x.covers("grid", grid_bytes(pool))?;
    x.covers("tiles", table_words(pool) * 4)?;
    x.covers("out", face_len(cells, axis) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_face_component", check: matter_face_component }
}
