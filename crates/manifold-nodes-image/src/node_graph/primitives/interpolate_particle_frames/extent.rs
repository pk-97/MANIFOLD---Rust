//! Buffer extent rule owned by this node.
use manifold_node_engine::water::liquid::EXACT_F32_COUNT;
use manifold_node_engine::water::liquid::extent::{AtomExtent, ExtentRule, PARTICLE, Verdict};

fn interpolate_particle_frames(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // Output follows B's capacity, never A+B. Count tails are explicitly zeroed.
    let bytes = x.bytes("particles_b").ok_or_else(|| x.uncovered("particles_b is unbound".into()))?;
    x.covers("out", bytes)?;
    for (port, count, default) in [("particles_a", "count_a", 0.0), ("particles_b", "count_b", -1.0)] {
        if port == "particles_a" && !x.wired(port) { continue; }
        let value = x.scalar(count, default);
        if port == "particles_b" && value == -1.0 { continue; }
        if !value.is_finite() || !(0.0..=EXACT_F32_COUNT as f32).contains(&value) {
            return Err(Verdict::Refused(format!("{count} must be a finite count (only count_b accepts -1)")));
        }
        // The shader truncates positive counts, it does not round them.
        x.covers(port, value as u64 * PARTICLE)?;
    }
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.interpolate_particle_frames", check: interpolate_particle_frames }
}
