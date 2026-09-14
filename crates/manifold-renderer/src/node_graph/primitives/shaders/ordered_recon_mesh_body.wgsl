// node.ordered_recon_mesh — fusable BUFFER pose-blend body (reference is BufferGather).
// A reference centroid assigns one directional band to each triangle. The
// same band center is used as the shared pivot for all three corners.
const EPS: f32 = 1e-8;

fn safe_unit(v: vec3<f32>, fallback: vec3<f32>) -> vec3<f32> {
    let l = length(v);
    if l > EPS { return v / l; }
    return fallback;
}

fn rotate_about(v: vec3<f32>, axis: vec3<f32>, angle: f32) -> vec3<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return v * c + cross(axis, v) * s + axis * dot(axis, v) * (1.0 - c);
}

fn fallback_axis(direction: vec3<f32>) -> vec3<f32> {
    let basis = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), abs(direction.y) < 0.9);
    return safe_unit(cross(direction, basis), vec3<f32>(1.0, 0.0, 0.0));
}

fn blend_frame(original: vec3<f32>, rotated: vec3<f32>, weight: f32, fallback: vec3<f32>) -> vec3<f32> {
    return safe_unit(mix(original, rotated, clamp(weight, 0.0, 1.0)), safe_unit(original, fallback));
}

fn blend_tangent(original: vec3<f32>, rotated: vec3<f32>, normal: vec3<f32>, weight: f32) -> vec3<f32> {
    let mixed = safe_unit(mix(original, rotated, clamp(weight, 0.0, 1.0)), vec3<f32>(0.0, 0.0, 0.0));
    let orthogonal = mixed - normal * dot(mixed, normal);
    return safe_unit(orthogonal, vec3<f32>(0.0, 0.0, 0.0));
}

fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    progress: f32,
    bands: i32,
    separation: f32,
    rotation: f32,
    spread: f32,
    direction_x: f32,
    direction_y: f32,
    direction_z: f32,
    scale: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    enabled: f32,
) -> Element {
    if enabled <= 0.0 { return e_in; }
    if progress >= 1.0 { return e_in; }
    let base = (idx / 3u) * 3u;
    if base + 2u >= count { return e_in; }

    let safe_scale = max(abs(scale), 1e-6);
    let source_offset = vec3<f32>(source_offset_x, source_offset_y, source_offset_z);
    let centroid = (buf_reference[base].position + buf_reference[base + 1u].position + buf_reference[base + 2u].position) / 3.0 + source_offset;
    let direction = safe_unit(vec3<f32>(direction_x, direction_y, direction_z), vec3<f32>(0.0, 1.0, 0.0));
    let projected = clamp(0.5 + 0.5 * dot(centroid / safe_scale, direction), 0.0, 1.0);
    let band_count = max(bands, 1);
    let band = min(band_count - 1, i32(floor(projected * f32(band_count))));
    let band_center = (f32(band) + 0.5) / f32(band_count);
    // Fixed stagger keeps the response ordered: early bands lock while later
    // bands are still moving. Progress 1 is handled above as an exact
    // incoming/current bypass, so no reference attributes are substituted.
    let band_order = f32(band) / f32(max(band_count - 1, 1));
    let stagger = 0.72;
    let start = band_order * stagger;
    let local_raw = clamp((progress - start) / (1.0 - stagger), 0.0, 1.0);
    let local = smoothstep(0.0, 1.0, local_raw);
    if local >= 1.0 { return e_in; }

    let pivot_world = direction * ((band_center * 2.0 - 1.0) * safe_scale);
    let rotation_axis = fallback_axis(direction);
    let away = 1.0 - local;
    let lateral = safe_unit(cross(direction, rotation_axis), vec3<f32>(1.0, 0.0, 0.0));
    let local_position = e_in.position + source_offset - pivot_world;
    let rotated = rotate_about(local_position, rotation_axis, rotation) + pivot_world;
    let blended = mix(e_in.position + source_offset, rotated, away);
    let translated = blended + safe_scale * away * (direction * separation + lateral * (spread * (band_center * 2.0 - 1.0)));
    let position = translated - source_offset;
    let rotated_normal = rotate_about(e_in.normal, rotation_axis, rotation);
    let normal = blend_frame(e_in.normal, rotated_normal, away, vec3<f32>(0.0, 1.0, 0.0));
    let rotated_tangent = rotate_about(e_in.tangent.xyz, rotation_axis, rotation);
    let tangent = blend_tangent(e_in.tangent.xyz, rotated_tangent, normal, away);
    return Element(position, normal, e_in.uv, vec4<f32>(tangent, e_in.tangent.w));
}
