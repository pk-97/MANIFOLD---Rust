// FLIP Fluids influencegrid.cpp:88-101, :170-190 (MIT).
fn body(idx: u32, count: u32, e_values: f32, e_solid: f32, e_source: Element, base_level: f32, decay_rate: f32,
    dt: f32, cell_size: f32, reset: f32, source_present: f32) -> f32 {
    var value = select(e_values, base_level, reset > 0.5);
    if value < base_level { value = min(value + decay_rate * dt, base_level); }
    else if value > base_level { value = max(value - decay_rate * dt, base_level); }
    if source_present > 0.5 && abs(e_solid) <= 3.0 * cell_size {
        let source = e_source;
        if source.kind == 1u { value = base_level; }
        else if source.kind == 2u { value = source.influence; }
    }
    return value;
}
