// Full-screen cover draws for water fields: each pixel runs the same march as
// Pass B, so the pick pass and the selection mask see the water that is drawn.
// Starts after `world_tiles_read.wgsl`, `field_tiles.wgsl` and
// `water_march_core.wgsl`, which hold all the bindings.

// The cover draws bind no tile flags, so they search every held tile.
fn field_tile_dry(slot: u32) -> bool {
    return false;
}

@vertex
fn vs_cover(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    let x = select(-1.0, 3.0, vid == 1u);
    let y = select(-1.0, 3.0, vid == 2u);
    return vec4<f32>(x, y, 0.0, 1.0);
}

// The depth of the field's surface along the pixel's ray, or -1.0 for a miss.
// No opaque stop: the pick's depth test does the occluding and the mask wants
// the whole silhouette.
fn cover_depth(frag: vec4<f32>) -> f32 {
    field_pool = u.tiles.xy;
    let uv = frag.xy / u.target_size.xy;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let origin_w = unproject(ndc, 1.0);
    let dir_w = normalize(unproject(ndc, 0.5) - origin_w);

    let ray = local_ray(origin_w, dir_w);
    let span = lattice_span(ray.o, ray.d);
    if span.x >= span.y { return -1.0; }

    let t_hit = march_surface(ray.o, ray.d, span.x, span.y);
    if t_hit < 0.0 { return -1.0; }

    let p = ray.o + ray.d * t_hit;
    let world = u.local_to_world * vec4<f32>(p, 1.0);
    let clip = u.view_proj * world;
    return clip.z / clip.w;
}

struct CoverId {
    @location(0) id: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

// The pick pass's id: the field's render slot plus one, at the hit's depth.
@fragment
fn fs_cover_id(@builtin(position) frag: vec4<f32>) -> CoverId {
    let depth = cover_depth(frag);
    if depth < 0.0 { discard; }
    return CoverId(vec4<f32>(f32(u.tiles.z), 0.0, 0.0, 1.0), depth);
}

@fragment
fn fs_cover_mask(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    if cover_depth(frag) < 0.0 { discard; }
    return vec4<f32>(1.0);
}
