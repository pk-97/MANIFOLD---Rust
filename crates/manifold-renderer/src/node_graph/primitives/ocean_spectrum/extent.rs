//! Buffer extent rule owned by this node.
use manifold_node_engine::water::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn ocean_spectrum(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let size = x.param("size", 256.0);
    let n = size.round() as u32;
    if !(16.0..=1024.0).contains(&size) || !n.is_power_of_two() {
        return Err(Verdict::Refused(format!("Ocean Spectrum: Size must be a power of two in 16..1024 (got {size})")));
    }
    // Six N × (N/2+1) half spectra, with one complex f32 pair per entry.
    let count = crate::node_graph::primitives::ocean_spectrum::spectrum_len(n);
    x.covers("spectrum", u64::from(count) * 8)
}
inventory::submit! {
    ExtentRule { type_id: "node.ocean_spectrum", check: ocean_spectrum }
}
