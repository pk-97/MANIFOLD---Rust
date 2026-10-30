//! Buffer extent rule owned by this node.
use super::super::gas_volume::{LIGHT_BYTES, LightReadout, RING, force_lattice, gas_geometry};
use super::GasDomain;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::primitive::PrimitiveSpec;
use manifold_water_liquid::bodies::{LiquidBody, LiquidShape};

fn gas_domain(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let geometry = gas_geometry(
        |name, default| x.scalar(name, default),
        x.transform("domain"),
    )
    .map_err(Verdict::Refused)?;
    // The light reads 0 until a flame burns.
    for (name, value) in geometry
        .outputs()
        .into_iter()
        .chain(LightReadout::default().outputs())
    {
        x.publish(name, value);
    }
    let lattice = force_lattice(&geometry);
    for (port, bytes) in [
        ("forces", lattice.force_bytes()),
        ("impulses", lattice.impulse_bytes()),
        ("bodies", size_of::<LiquidBody>() as u64),
        ("shapes", size_of::<LiquidShape>() as u64),
        ("atlas", 4),
        ("collider_nodes", 16),
        ("collider_triangles", 16),
        ("moving", 32),
        ("light", LIGHT_BYTES),
    ] {
        x.provide(port, bytes);
        x.hold(bytes);
    }
    // The rest of the light ring.
    x.hold((RING as u64 - 1) * LIGHT_BYTES);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: <GasDomain as PrimitiveSpec>::TYPE_ID, check: gas_domain }
}
