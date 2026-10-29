//! Buffer extent rule owned by this node.
use super::{GasBytes, GasVolume, LightReadout, gas_geometry};
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::primitive::PrimitiveSpec;

fn gas_volume(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let geometry = gas_geometry(|name, default| x.scalar(name, default), x.transform("domain"))
        .map_err(Verdict::Refused)?;
    // The light reads 0 until a flame burns.
    for (name, value) in geometry
        .outputs()
        .into_iter()
        .chain(LightReadout::default().outputs())
    {
        x.publish(name, value);
    }
    // `run` owns every buffer: the fields, the published state and velocity
    // (provided arrays), the light ring and the blackbody table.
    let bytes = GasBytes::of(&geometry);
    x.provide("state", bytes.packed);
    x.provide("velocity", bytes.packed);
    x.hold(bytes.total());
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: <GasVolume as PrimitiveSpec>::TYPE_ID, check: gas_volume }
}
