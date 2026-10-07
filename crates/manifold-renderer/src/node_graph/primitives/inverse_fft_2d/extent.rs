//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn inverse_fft_2d(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let size = x.param("size", 256.0).round();
    let batch = x.param("batch", 6.0).round();
    let n = size as u32;
    if !(16.0..=1024.0).contains(&size) || !n.is_power_of_two() || !(1.0..=16.0).contains(&batch) {
        return Err(Verdict::Refused(format!("Inverse FFT 2D: Size must be a power of two in 16..1024 and Batch 1..16 (got {size}, {batch})")));
    }
    let (n, batch) = (u64::from(n), batch as u64);
    // The MPSGraph transform reads B half spectra and writes B real fields.
    x.covers("spectrum", batch * n * (n / 2 + 1) * 8)?;
    x.covers("field", batch * n * n * 4)?;
    // metal/fft.rs keeps four BoundPairs, retaining whole buffers. On a
    // cache miss the new pair is retained before the old cache is truncated:
    // four old pairs can coexist with the current arrays already counted.
    x.hold(4 * (x.bytes("spectrum").expect("covered") + x.bytes("field").expect("covered")));
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.inverse_fft_2d", check: inverse_fft_2d }
}
