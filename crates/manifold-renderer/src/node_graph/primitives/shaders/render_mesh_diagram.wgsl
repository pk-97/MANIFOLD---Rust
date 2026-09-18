struct U {
    view_proj: mat4x4<f32>, model: mat4x4<f32>, viewport: vec4<f32>,
    radius: f32, line_width: f32, geometry_hue: f32, path_hue: f32,
    grid: u32, fragments: u32, ghosts: u32, vectors: u32, trails: u32,
    tri_count: u32, vertex_count: u32, history_head: u32, history_len: u32,
    history_capacity: u32, history_stride: u32, axes: u32,
    depth_pass: u32, occlusion: u32, mode: u32, _depth_pad: u32,
    inv_view_proj: mat4x4<f32>, camera_pos_far: vec4<f32>,
    brightness: vec4<f32>, event_values: vec4<f32>, scan_values: vec4<f32>, event_targets: vec4<u32>,
    copy_count: u32, instances_wired: u32, _instances_pad: vec2<u32>,
};
struct V { position: vec3<f32>, _p: f32, normal: vec3<f32>, _n: f32, uv: vec2<f32>, _u: vec2<f32>, tangent: vec4<f32> };
struct I { pos_scale: vec4<f32>, rot_pad: vec4<f32> };
struct O { @builtin(position) p: vec4<f32>, @location(0) color: vec4<f32>, @location(1) @interpolate(flat) grid: u32 };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> current: array<V>;
@group(0) @binding(2) var<storage, read> reference: array<V>;
@group(0) @binding(3) var<storage, read> incoming: array<V>;
@group(0) @binding(4) var<storage, read> history: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read> mesh_weights: array<f32>;
@group(0) @binding(6) var<storage, read> scan_weights: array<f32>;
@group(0) @binding(7) var surface_depth: texture_2d<f32>;
@group(0) @binding(8) var scene_depth: texture_2d<f32>;
@group(0) @binding(9) var depth_sampler: sampler;
@group(0) @binding(10) var<storage, read> copy_instances: array<I>;

fn targeted(target_element: u32, element: u32) -> bool { return target_element == 0u || target_element == element; }
fn tone(color: vec4<f32>, gain: f32) -> vec4<f32> {
    return vec4<f32>(color.rgb * max(gain, 1.0), color.a * clamp(gain, 0.0, 1.0));
}
fn grid_gain(world: vec3<f32>) -> f32 {
    var gain = u.brightness.x;
    if targeted(u.event_targets.x, 1u) { gain *= u.event_values.y; }
    if targeted(u.event_targets.y, 1u) {
        let direction = u32(round(u.scan_values.y));
        let coordinate = select(select(world.z, world.y, direction / 2u == 1u), world.x, direction / 2u == 0u) / max(u.radius, 0.000001);
        let axis_position = coordinate * select(-1.0, 1.0, direction % 2u == 0u);
        let center = (2.0 * u.event_values.w - 1.0) * (1.0 + u.scan_values.x);
        var distance = abs(axis_position-center) - u.scan_values.x;
        if u.scan_values.z >= 0.5 { distance = axis_position-center; }
        var mask = 1.0 - smoothstep(0.0, 0.03, distance);
        if u.scan_values.z >= 0.5 {
            if u.event_values.w <= 0.0 { mask = 0.0; }
            if u.event_values.w >= 1.0 { mask = 1.0; }
            gain *= 1.0-u.event_values.z + u.event_values.z*mask;
        } else { gain *= 1.0+u.event_values.z*mask; }
    }
    return gain;
}
fn appearance(element: u32, triangle: u32) -> f32 {
    if element == 1u { return grid_gain(vec3<f32>(0.0)); }
    var gain = u.event_values.x;
    if element < 5u { gain = u.brightness[element-1u]; }
    if u.scan_values.w >= 0.5 && u.event_targets.z > 0u && u.tri_count > 0u {
        let face = source_face_index(triangle,u.event_targets.z,u.tri_count);
        // The same final composed buffer is bound to the scene object.
        return gain * mesh_weights[face*3u];
    }
    if targeted(u.event_targets.x,element) { gain *= u.event_values.y; }
    if targeted(u.event_targets.y,element) && triangle*3u < u.event_targets.w { gain *= scan_weights[triangle*3u]; }
    return gain;
}

fn hidden() -> O {
    var o: O; o.p = vec4<f32>(-2.0, -2.0, 0.0, 1.0); o.color = vec4<f32>(0.0); o.grid = 0u; return o;
}

fn hsv(h: f32) -> vec3<f32> {
    let k = vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0);
    return clamp(abs(fract(vec3<f32>(h) + k) * 6.0 - vec3<f32>(3.0)) - vec3<f32>(1.0), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn clip_point(p: vec3<f32>) -> vec4<f32> { return u.view_proj * u.model * vec4<f32>(p, 1.0); }
fn clip_line(ca_in: vec4<f32>, cb_in: vec4<f32>, color: vec4<f32>, vi: u32) -> O {
    var ca = ca_in;
    var cb = cb_in;
    // X-ray keeps the historic endpoint rejection and average depth exactly.
    // Depth-tested lines clip the segment against the camera near plane and
    // retain each endpoint's homogeneous depth for interpolation.
    if u.occlusion != 0u {
        // Reversed-Z's near plane is z=w for both camera modes. Clip the
        // centre line before expanding its stroke.
        let da = ca.w - ca.z;
        let db = cb.w - cb.z;
        if da < 0.0 && db < 0.0 { return hidden(); }
        if da < 0.0 { ca = mix(ca, cb, da / (da - db)); }
        if db < 0.0 { cb = mix(cb, ca, db / (db - da)); }
    }
    if (ca.w <= 0.001 || cb.w <= 0.001) { return hidden(); }
    let aa = ca.xy / ca.w; let bb = cb.xy / cb.w;
    let d = (bb - aa) * u.viewport.xy; let len = length(d);
    if (len <= 0.000001) { return hidden(); }
    let side = vec2<f32>(-d.y, d.x) / len * (2.0 * u.line_width / max(u.viewport.xy, vec2<f32>(1.0)));
    let c = vi % 6u;
    var p = aa;
    if (c == 0u || c == 3u) { p = aa + side; }
    if (c == 1u) { p = bb + side; }
    if (c == 2u || c == 4u) { p = bb - side; }
    if (c == 5u) { p = aa - side; }
    var o: O;
    o.p = vec4<f32>(p, (ca.z / ca.w + cb.z / cb.w) * 0.5, 1.0);
    if u.occlusion != 0u {
        let endpoint = select(ca, cb, c == 1u || c == 2u || c == 4u);
        o.p = vec4<f32>(p * endpoint.w, endpoint.z, endpoint.w);
    }
    o.color = color; o.grid = 0u; return o;
}

fn line(a: vec3<f32>, b: vec3<f32>, color: vec4<f32>, vi: u32) -> O {
    return clip_line(clip_point(a), clip_point(b), color, vi);
}

fn world_line(a: vec3<f32>, b: vec3<f32>, color: vec4<f32>, vi: u32) -> O {
    return clip_line(u.view_proj * vec4<f32>(a, 1.0), u.view_proj * vec4<f32>(b, 1.0), color, vi);
}

fn depth_surface_vertex(vi: u32, instance: u32) -> O {
    // Instance slots below tri_count * copy_count are current-surface
    // triangles, repeated per copy transform like the colour fragments.
    if u.fragments == 0u || vi >= 3u || instance >= u.tri_count * u.copy_count { return hidden(); }
    let c = instance / u.tri_count;
    let triangle = instance % u.tri_count;
    var position = current[triangle * 3u + vi].position;
    if u.instances_wired != 0u {
        let inst = copy_instances[c];
        if inactive_copy(inst) { return hidden(); }
        position = apply_copy(position, inst);
    }
    // Let the rasterizer clip whole triangles; moving one behind-camera
    // vertex to hidden() would invent a different surface at the near plane.
    let clip = clip_point(position);
    var o: O; o.p = clip; o.color = vec4<f32>(1.0, 1.0, 1.0, 1.0); o.grid = 0u; return o;
}

fn sample_position(which: u32, idx: u32) -> vec3<f32> {
    if (which == 0u) { return current[idx].position; }
    if (which == 1u) { return reference[idx].position; }
    return incoming[idx].position;
}

// Instance copy transform, object space: rotate (XYZ Euler, bit-for-bit the
// render_scene.wgsl / render_instanced_3d_mesh.wgsl convention), uniform
// scale, then translate. The diagram's clip_point applies the object model
// matrix afterwards, matching render_scene's model * T_instance order.
fn euler_xyz(angles: vec3<f32>) -> mat3x3<f32> {
    let cx = cos(angles.x);
    let sx = sin(angles.x);
    let cy = cos(angles.y);
    let sy = sin(angles.y);
    let cz = cos(angles.z);
    let sz = sin(angles.z);

    let rx = mat3x3<f32>(
        vec3<f32>(1.0, 0.0, 0.0),
        vec3<f32>(0.0, cx, sx),
        vec3<f32>(0.0, -sx, cx),
    );
    let ry = mat3x3<f32>(
        vec3<f32>(cy, 0.0, -sy),
        vec3<f32>(0.0, 1.0, 0.0),
        vec3<f32>(sy, 0.0, cy),
    );
    let rz = mat3x3<f32>(
        vec3<f32>(cz, sz, 0.0),
        vec3<f32>(-sz, cz, 0.0),
        vec3<f32>(0.0, 0.0, 1.0),
    );
    return rz * ry * rx;
}

fn apply_copy(p: vec3<f32>, inst: I) -> vec3<f32> {
    return euler_xyz(inst.rot_pad.xyz) * (p * inst.pos_scale.w) + inst.pos_scale.xyz;
}

// Producers collapse inactive slots to an all-zero transform (see
// generate_instance_transforms_body.wgsl and analytic_echo_instances'
// "zero scale is the inactive-hole marker"), so the diagram skips them just
// like the scene's zero-scale no-op draw.
fn inactive_copy(inst: I) -> bool {
    return inst.pos_scale.w == 0.0
        && all(inst.pos_scale.xyz == vec3<f32>(0.0))
        && all(inst.rot_pad.xyz == vec3<f32>(0.0));
}

// Copy transform for slot c. Unwired (vertices-only captures) uses an exact
// identity: every fragment/ghost/arrow instance keeps its historical
// positions and the output stays byte-identical.
fn copy_transform(c: u32) -> I {
    if u.instances_wired == 0u {
        return I(vec4<f32>(0.0, 0.0, 0.0, 1.0), vec4<f32>(0.0));
    }
    return copy_instances[c];
}

fn fragment_frame_count() -> u32 { return min(u.tri_count, 3u); }

fn fragment_frame_triangle(frame: u32) -> u32 {
    // Midpoints of up to three equal ranges of the existing face sample.
    // Avoid anchoring every object's frame to its arbitrary first face.
    return (2u * frame + 1u) * u.tri_count / (2u * max(fragment_frame_count(), 1u));
}

fn vertex_body(vi: u32, instance: u32) -> O {
    if u.depth_pass != 0u { return depth_surface_vertex(vi, instance); }
    // Draw the world grid first, behind the diagram marks. A fullscreen
    // triangle has no finite mesh boundary and never inherits object pose.
    if (instance == 0u) {
        if (u.grid == 0u || vi >= 3u) { return hidden(); }
        let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
        let y = f32(vi & 2u) * 2.0 - 1.0;
        var o: O; o.p = vec4<f32>(x, y, 0.0, 1.0); o.color = vec4<f32>(0.0); o.grid = 1u; return o;
    }
    let ii = instance - 1u;
    if (u.vertex_count == 0u) { return hidden(); }
    let tri = u.tri_count;
    // Fragment, reference, ghost and arrow blocks repeat per copy transform;
    // per == tri * copy_count, and copy_count is 1 for vertices-only captures.
    let per = tri * u.copy_count;
    let tri_end = per * 3u;
    let arrow_base = tri_end;
    let grid_base = arrow_base + per;
    let axes_base = grid_base;
    let trails_base = axes_base + 3u + fragment_frame_count() * 3u;

    if (ii < per) {
        if (u.fragments == 0u) { return hidden(); }
        let t = ii % tri;
        let inst = copy_transform(ii / tri);
        if (u.instances_wired != 0u && inactive_copy(inst)) { return hidden(); }
        let b = t * 3u; let e = vi / 6u;
        return line(apply_copy(sample_position(0u, b + e), inst), apply_copy(sample_position(0u, b + ((e + 1u) % 3u)), inst), vec4<f32>(hsv(u.geometry_hue), 0.95), vi);
    }
    if (ii < per * 2u) {
        if (u.fragments == 0u) { return hidden(); }
        let t = (ii - per) % tri;
        let inst = copy_transform((ii - per) / tri);
        if (u.instances_wired != 0u && inactive_copy(inst)) { return hidden(); }
        let b = t * 3u; let e = vi / 6u;
        return line(apply_copy(sample_position(1u, b + e), inst), apply_copy(sample_position(1u, b + ((e + 1u) % 3u)), inst), vec4<f32>(hsv(u.geometry_hue), 0.28), vi);
    }
    if (ii < per * 3u) {
        if (u.ghosts == 0u) { return hidden(); }
        let t = (ii - per * 2u) % tri;
        let inst = copy_transform((ii - per * 2u) / tri);
        if (u.instances_wired != 0u && inactive_copy(inst)) { return hidden(); }
        let b = t * 3u; let e = vi / 6u;
        return line(apply_copy(sample_position(2u, b + e), inst), apply_copy(sample_position(2u, b + ((e + 1u) % 3u)), inst), vec4<f32>(hsv(u.path_hue), 0.22), vi);
    }
    if (ii < grid_base) {
        if (u.vectors == 0u) { return hidden(); }
        let local = ii - arrow_base;
        let t = local % tri;
        let inst = copy_transform(local / tri);
        if (u.instances_wired != 0u && inactive_copy(inst)) { return hidden(); }
        let idx = t * 3u;
        // The tail stays on the undeformed reference; the head is the
        // copy-transformed current centroid, so the arrow reads as the total
        // reference -> copy displacement of the chain.
        let a = (incoming[idx].position + incoming[idx + 1u].position + incoming[idx + 2u].position) / 3.0;
        let b = apply_copy((current[idx].position + current[idx + 1u].position + current[idx + 2u].position) / 3.0, inst);
        let edge = vi / 6u;
        if (edge == 0u) { return line(a, b, vec4<f32>(hsv(u.path_hue), 0.85), vi); }
        if (length(b - a) < 0.000001) { return hidden(); }
        let d = normalize(b - a);
        let up = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(d.y) > 0.95);
        let side = normalize(cross(d, up));
        let head_size = min(length(b - a) * 0.2, u.radius * 0.035);
        let head = b - d * head_size;
        if (edge == 1u) { return line(b, head + side * head_size * 0.4, vec4<f32>(hsv(u.path_hue), 0.85), vi % 6u); }
        if (edge == 2u) { return line(b, head - side * head_size * 0.4, vec4<f32>(hsv(u.path_hue), 0.85), vi % 6u); }
        return hidden();
    }
    if (ii < trails_base) {
        if (vi >= 6u || u.axes == 0u) { return hidden(); }
        let a = ii - axes_base;
        if (a < 3u) {
            if (u.grid == 0u) { return hidden(); }
            if (a == 0u) { return world_line(vec3<f32>(0.0), vec3<f32>(1.0, 0.0, 0.0), vec4<f32>(1.0, 0.2, 0.2, 0.9), vi); }
            if (a == 1u) { return world_line(vec3<f32>(0.0), vec3<f32>(0.0, 1.0, 0.0), vec4<f32>(0.2, 1.0, 0.2, 0.9), vi); }
            return world_line(vec3<f32>(0.0), vec3<f32>(0.0, 0.0, 1.0), vec4<f32>(0.2, 0.4, 1.0, 0.9), vi);
        }
        if (u.fragments == 0u) { return hidden(); }
        let b = fragment_frame_triangle((a - 3u) / 3u) * 3u;
        let pivot = (current[b].position + current[b + 1u].position + current[b + 2u].position) / 3.0;
        let edge_x = current[b + 1u].position - current[b].position;
        let edge_y = current[b + 2u].position - current[b].position;
        let normal = cross(edge_x, edge_y);
        if (length(edge_x) < 0.000001 || length(normal) < 0.000001) { return hidden(); }
        let local_x = normalize(edge_x);
        let local_z = normalize(normal);
        let local_y = cross(local_z, local_x);
        let p = (a - 3u) % 3u;
        if (p == 0u) { return line(pivot, pivot + local_x * u.radius * 0.12, vec4<f32>(1.0, 0.2, 0.2, 0.7), vi); }
        if (p == 1u) { return line(pivot, pivot + local_y * u.radius * 0.12, vec4<f32>(0.2, 1.0, 0.2, 0.7), vi); }
        return line(pivot, pivot + local_z * u.radius * 0.12, vec4<f32>(0.2, 0.4, 1.0, 0.7), vi);
    }

    if (vi >= 6u || u.trails == 0u || u.history_len < 2u) { return hidden(); }
    let trail = ii - trails_base; let sample = trail / u.vertex_count; let vertex = trail % u.vertex_count;
    let count = min(u.history_len - 1u, 32u);
    if (sample >= count) { return hidden(); }
    let start = u.history_len - 1u - count;
    let slot_a = (u.history_head + u.history_capacity - u.history_len + start + sample) % u.history_capacity;
    let slot_b = (slot_a + 1u) % u.history_capacity;
    let a = history[slot_a * u.history_stride + vertex].xyz;
    let b = history[slot_b * u.history_stride + vertex].xyz;
    let alpha = 0.12 + 0.5 * f32(sample + 1u) / f32(count);
    return line(a, b, vec4<f32>(hsv(u.path_hue), alpha), vi);
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, @builtin(instance_index) instance: u32) -> O {
    var o=vertex_body(vi,instance);
    if u.depth_pass != 0u {
        // Instance repeats the same triangle per copy; appearance indexing
        // stays on the base triangle so connected weights cannot read past
        // the sampled face range.
        o.color = tone(o.color, appearance(2u, instance % max(u.tri_count, 1u)));
        return o;
    }
    if instance==0u || u.tri_count==0u { return o; }
    let ii=instance-1u;
    let per = u.tri_count * u.copy_count;
    var element=2u;
    var triangle=0u;
    if ii < 2u*per { triangle=ii%u.tri_count; }
    else if ii < 3u*per { element=3u; triangle=(ii-2u*per)%u.tri_count; }
    else if ii < 4u*per { element=4u; triangle=(ii-3u*per)%u.tri_count; }
    else if ii < 4u*per+3u { element=1u; }
    else if ii < 4u*per+3u+fragment_frame_count()*3u { triangle=fragment_frame_triangle((ii-4u*per-3u)/3u); }
    else { element=5u; triangle=((ii-4u*per-3u-fragment_frame_count()*3u)%u.vertex_count)/3u; }
    o.color=tone(o.color,appearance(element,triangle));
    return o;
}

fn grid_lines(p: vec2<f32>, footprint: vec2<f32>, spacing: f32) -> f32 {
    let cell = p / spacing;
    let distance = abs(fract(cell + vec2<f32>(0.5)) - vec2<f32>(0.5)) * spacing;
    let pixels = distance / footprint;
    let coverage = vec2<f32>(1.0) - smoothstep(vec2<f32>(u.line_width * 0.35), vec2<f32>(u.line_width * 0.35 + 1.0), pixels);
    // Retire the whole graduation together. Independent axis fades leave
    // a dense fan of longitudinal lines after the transverse cells vanish
    // at a grazing camera angle, especially around the vanishing point.
    let resolved = 1.0 - smoothstep(0.05, 0.2, max(footprint.x, footprint.y) / spacing);
    return max(coverage.x, coverage.y) * resolved;
}

fn world_grid(pixel: vec2<f32>) -> vec4<f32> {
    // Native Metal framebuffer Y runs down; Camera clip-space Y runs up.
    let ndc = pixel / u.viewport.xy * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    // Camera depth is reversed-Z: clip z=1 is near and z=0 is far.
    let near_h = u.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let far_h = u.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let near = near_h.xyz / near_h.w;
    let far = far_h.xyz / far_h.w;
    let ray = far - near;
    let denominator = select(-max(abs(ray.y), 0.000001), max(abs(ray.y), 0.000001), ray.y >= 0.0);
    let t = -near.y / denominator;
    let world = near + t * ray;
    let footprint = max(fwidth(world.xz), vec2<f32>(0.000001));
    // Fixed world-unit graduations at 1, 10 and 100 units remain anchored
    // during camera/object motion; finer levels fade out with perspective.
    let fine = grid_lines(world.xz, footprint, 1.0);
    let major = grid_lines(world.xz, footprint, 10.0);
    let coarse = grid_lines(world.xz, footprint, 100.0);
    let alpha = max(fine * 0.28, max(major * 0.40, coarse * 0.45));
    let distance = length(world - u.camera_pos_far.xyz);
    let fade_end = u.camera_pos_far.w * 0.9;
    let fade = 1.0 - smoothstep(fade_end * 0.35, fade_end, distance);
    let horizon = smoothstep(0.02, 0.08, abs(ray.y) / max(length(ray), 0.000001));
    let visible = select(0.0, 1.0, t > 0.0 && t < 1.0 && u.grid != 0u);
    return tone(vec4<f32>(0.18, 0.35, 0.40, alpha * fade * horizon * visible),grid_gain(world));
}

fn world_grid_position(pixel: vec2<f32>) -> vec3<f32> {
    let ndc = pixel / u.viewport.xy * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    // Camera depth is reversed-Z: clip z=1 is near and z=0 is far.
    let near_h = u.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let far_h = u.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let near = near_h.xyz / near_h.w;
    let far = far_h.xyz / far_h.w;
    let ray = far - near;
    let denominator = select(-max(abs(ray.y), 0.000001), max(abs(ray.y), 0.000001), ray.y >= 0.0);
    let t = -near.y / denominator;
    return near + t * ray;
}

fn depth_occluded(depth: f32, pixel: vec2<f32>, bias: f32) -> bool {
    if u.occlusion == 0u { return false; }
    let uv = pixel / u.viewport.xy;
    var cutoff = textureSampleLevel(surface_depth, depth_sampler, uv, 0.0).r;
    if u.mode == 2u { cutoff = max(cutoff, textureSampleLevel(scene_depth, depth_sampler, uv, 0.0).r); }
    return depth < cutoff - bias;
}

@fragment
fn fs_main(in: O) -> @location(0) vec4<f32> {
    // Keep the original entry point free of depth resources. Existing X-ray
    // callers bind only the authored array buffers, and this preserves their
    // pixel path and pipeline layout.
    let grid = world_grid(in.p.xy);
    return select(in.color, grid, in.grid != 0u);
}

@fragment
fn fs_depth_color(in: O) -> @location(0) vec4<f32> {
    // Evaluate derivatives outside divergent control flow, including quads
    // touched by line primitives, so the grid stays valid at the horizon.
    let grid = world_grid(in.p.xy);
    let world = world_grid_position(in.p.xy);
    let clip = u.view_proj * vec4<f32>(world, 1.0);
    let depth = select(in.p.z, clip.z / max(clip.w, 0.000001), in.grid != 0u);
    // Widen the tolerance with projected slope, but cap it so distant
    // surfaces do not acquire a large see-through band. Derivatives stay
    // outside divergent flow, just like the grid above.
    let bias = clamp(fwidth(depth) * (u.line_width + 1.0), 0.000002, 0.0002);
    if depth_occluded(depth, in.p.xy, bias) { return vec4<f32>(0.0); }
    return select(in.color, grid, in.grid != 0u);
}

@fragment
fn fs_depth(in: O) -> @location(0) f32 {
    if in.color.a <= 0.0 { return 0.0; }
    return clamp(in.p.z, 0.0, 1.0);
}
