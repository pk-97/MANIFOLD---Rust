// `node.analytic_echo_instances` buffer body.
//
// The source array is a BufferGather input: output idx maps to source idx / 8,
// with a stable eight-slot stride per source.  Echo 0 returns the complete
// source record unchanged.  Later echoes translate in scene space and apply a
// coherent positive taper factor to position and scale; rotation, reflection
// marker, and inactive state remain stable.
fn body(
    idx: u32,
    count: u32,
    count_param: i32,
    radius: f32,
    rise: f32,
    phase: f32,
    arc: f32,
    taper: f32,
    enabled: f32,
    scene_radius: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
) -> Element {
    let source_count = arrayLength(&buf_instances);
    if (source_count == 0u) {
        return Element(vec4<f32>(0.0), vec4<f32>(0.0));
    }
    let source_idx = idx / 8u;
    if (source_idx >= source_count) {
        return Element(vec4<f32>(0.0), vec4<f32>(0.0));
    }
    let echo_idx = idx % 8u;
    let src = buf_instances[source_idx];

    // Echo 0 is the exact source, independent of every live control.  This
    // includes an inactive source record: the complete source slot survives so
    // downstream identity and marker data remain stable.
    if (echo_idx == 0u) {
        return Element(src.pos_scale, src.rot);
    }

    // A zero scale is the established inactive-hole marker.  Negative scale
    // remains active and is copied exactly, so reflection and handedness data
    // are never inferred from the sign.
    if (src.pos_scale.w == 0.0) {
        return Element(vec4<f32>(0.0), vec4<f32>(0.0));
    }
    let active_echoes = min(8u, u32(max(count_param, 1)));
    if (echo_idx >= active_echoes || enabled <= 0.0) {
        return Element(vec4<f32>(0.0), vec4<f32>(0.0));
    }

    // With no spatial extent there is no visible echo: duplicate meshes would
    // overlap exactly.  This neutral case is an inactive tail even if taper is
    // nonzero.
    if (radius == 0.0 && rise == 0.0) {
        return Element(vec4<f32>(0.0), vec4<f32>(0.0));
    }

    let u = f32(echo_idx) / 7.0;
    let angle = (phase + arc * u) * 6.283185307179586;
    let taper_factor = max(0.001, 1.0 - clamp(taper, 0.0, 1.0) * u);
    let source_offset = vec3<f32>(source_offset_x, source_offset_y, source_offset_z);
    // The renderer composes sourceOffset + instance position + mesh*scale.
    // Scale the complete object transform about scene origin so multi-part
    // objects retain their relative placement under taper.
    let pos = src.pos_scale.xyz * taper_factor + source_offset * (taper_factor - 1.0);
    let arc_offset = vec3<f32>(
        cos(angle) * radius * scene_radius * u,
        rise * scene_radius * u,
        sin(angle) * radius * scene_radius * u,
    );
    return Element(vec4<f32>(pos + arc_offset, src.pos_scale.w * taper_factor), src.rot);
}
