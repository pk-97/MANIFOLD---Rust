//! Buffer extent rule owned by this node.
use std::mem::size_of;
use crate::water::fluid_particles::bin_counts;
use crate::water::fluid_particles::searched_bins;
use crate::water::liquid::bodies::LiquidBody;
use crate::water::liquid::bodies::LiquidShape;
use crate::water::whitewater::cell_total;
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict, searched, whitewater_lattice, whole};

fn keep_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let regions = u64::from(whole(x, "region_count", 0.0));
    if regions > 0 {
        x.covers("regions", (regions + u64::from(whole(x, "region_offset", 0.0))) * size_of::<LiquidBody>() as u64)?;
        x.covers("shapes", size_of::<LiquidShape>() as u64)?;
        x.covers("atlas", 4)?;
    }
    let (nodes, cells) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    x.covers("solid", cell_total(nodes) * 4)?;
    let ports = ["bins_x", "bins_y", "bins_z"];
    if ports.map(|port| x.scalar(port, 0.0)) == [0.0; 3] && ports.iter().all(|port| !x.wired(port)) {
        let size = ["size_x", "size_y", "size_z"].map(|name| x.scalar(name, 4.375));
        let ranges = x.bytes("cell_ranges").ok_or_else(|| x.uncovered("cell_ranges is unbound".into()))?;
        let bins = bin_counts(size, size[0] / cells[0] as f32).map(|n| n as f32);
        searched_bins(bins, ranges, "search").map_err(|error| x.uncovered(error))?;
    } else {
        searched(x)?;
    }
    x.covers("out", x.items("pool").unwrap_or(0) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.keep_whitewater", check: keep_whitewater }
}
