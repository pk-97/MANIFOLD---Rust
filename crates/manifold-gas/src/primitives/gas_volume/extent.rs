//! Buffer extent rule owned by this node.
use super::{GasBytes, GasGeometry, GasVolume};
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::primitive::PrimitiveSpec;

fn gas_volume(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let whole = |value: f32| value.round().max(0.0) as u32;
    let geometry = GasGeometry::from_fine(
        x.scalar("cell_size", 0.0),
        ["origin_x", "origin_y", "origin_z"].map(|name| x.scalar(name, 0.0)),
        ["dims_x", "dims_y", "dims_z"].map(|name| whole(x.scalar(name, 0.0))),
    );
    // `run` owns the fields, the tile table and the blackbody table; the published state and
    // velocity are the tick loop's, written in place.
    x.hold(GasBytes::of(&geometry).total());
    // Water or sand wired: the solid field, two words a record.
    if x.input("faces").is_some() || x.input("sand_grid").is_some() {
        x.hold(geometry.records() * 8);
    }
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: <GasVolume as PrimitiveSpec>::TYPE_ID, check: gas_volume }
}
