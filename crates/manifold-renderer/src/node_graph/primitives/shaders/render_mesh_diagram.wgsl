struct U {
    view_proj: mat4x4<f32>, model: mat4x4<f32>, viewport: vec4<f32>,
    radius: f32, line_width: f32, geometry_hue: f32, path_hue: f32,
    grid: u32, fragments: u32, ghosts: u32, vectors: u32, trails: u32,
    tri_count: u32, vertex_count: u32, history_head: u32, history_len: u32,
    history_capacity: u32, history_stride: u32, _pad: u32,
    inv_view_proj: mat4x4<f32>, camera_pos_far: vec4<f32>,
};
struct V { position: vec3<f32>, _p: f32, normal: vec3<f32>, _n: f32, uv: vec2<f32>, _u: vec2<f32>, tangent: vec4<f32> };
struct O { @builtin(position) p: vec4<f32>, @location(0) color: vec4<f32>, @location(1) @interpolate(flat) grid: u32 };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> current: array<V>;
@group(0) @binding(2) var<storage, read> reference: array<V>;
@group(0) @binding(3) var<storage, read> incoming: array<V>;
@group(0) @binding(4) var<storage, read> history: array<vec4<f32>>;

fn hidden() -> O {
    var o: O; o.p = vec4<f32>(-2.0, -2.0, 0.0, 1.0); o.color = vec4<f32>(0.0); o.grid = 0u; return o;
}

fn hsv(h: f32) -> vec3<f32> {
    let k = vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0);
    return clamp(abs(fract(vec3<f32>(h) + k) * 6.0 - vec3<f32>(3.0)) - vec3<f32>(1.0), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn clip_point(p: vec3<f32>) -> vec4<f32> { return u.view_proj * u.model * vec4<f32>(p, 1.0); }
fn clip_line(ca: vec4<f32>, cb: vec4<f32>, color: vec4<f32>, vi: u32) -> O {
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
    var o: O; o.p = vec4<f32>(p, (ca.z / ca.w + cb.z / cb.w) * 0.5, 1.0); o.color = color; o.grid = 0u; return o;
}

fn line(a: vec3<f32>, b: vec3<f32>, color: vec4<f32>, vi: u32) -> O {
    return clip_line(clip_point(a), clip_point(b), color, vi);
}

fn world_line(a: vec3<f32>, b: vec3<f32>, color: vec4<f32>, vi: u32) -> O {
    return clip_line(u.view_proj * vec4<f32>(a, 1.0), u.view_proj * vec4<f32>(b, 1.0), color, vi);
}

fn sample_position(which: u32, idx: u32) -> vec3<f32> {
    if (which == 0u) { return current[idx].position; }
    if (which == 1u) { return reference[idx].position; }
    return incoming[idx].position;
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, @builtin(instance_index) instance: u32) -> O {
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
    let tri_end = tri * 3u;
    let arrow_base = tri_end;
    let grid_base = arrow_base + tri;
    let axes_base = grid_base;
    let trails_base = axes_base + 6u;

    if (ii < tri) {
        if (u.fragments == 0u) { return hidden(); }
        let b = ii * 3u; let e = vi / 6u;
        return line(sample_position(0u, b + e), sample_position(0u, b + ((e + 1u) % 3u)), vec4<f32>(hsv(u.geometry_hue), 0.95), vi);
    }
    if (ii < tri * 2u) {
        if (u.fragments == 0u) { return hidden(); }
        let t = ii - tri; let b = t * 3u; let e = vi / 6u;
        return line(sample_position(1u, b + e), sample_position(1u, b + ((e + 1u) % 3u)), vec4<f32>(hsv(u.geometry_hue), 0.28), vi);
    }
    if (ii < tri_end) {
        if (u.ghosts == 0u) { return hidden(); }
        let t = ii - tri * 2u; let b = t * 3u; let e = vi / 6u;
        return line(sample_position(2u, b + e), sample_position(2u, b + ((e + 1u) % 3u)), vec4<f32>(hsv(u.path_hue), 0.22), vi);
    }
    if (ii < grid_base) {
        if (u.vectors == 0u) { return hidden(); }
        let idx = (ii - arrow_base) * 3u;
        let a = (incoming[idx].position + incoming[idx + 1u].position + incoming[idx + 2u].position) / 3.0;
        let b = (current[idx].position + current[idx + 1u].position + current[idx + 2u].position) / 3.0;
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
        if (vi >= 6u) { return hidden(); }
        let a = ii - axes_base;
        let pivot = (current[0].position + current[1].position + current[2].position) / 3.0;
        if (a < 3u) {
            if (u.grid == 0u) { return hidden(); }
            if (a == 0u) { return world_line(vec3<f32>(0.0), vec3<f32>(1.0, 0.0, 0.0), vec4<f32>(1.0, 0.2, 0.2, 0.9), vi); }
            if (a == 1u) { return world_line(vec3<f32>(0.0), vec3<f32>(0.0, 1.0, 0.0), vec4<f32>(0.2, 1.0, 0.2, 0.9), vi); }
            return world_line(vec3<f32>(0.0), vec3<f32>(0.0, 0.0, 1.0), vec4<f32>(0.2, 0.4, 1.0, 0.9), vi);
        }
        if (u.fragments == 0u) { return hidden(); }
        let edge_x = current[1].position - current[0].position;
        let edge_y = current[2].position - current[0].position;
        let normal = cross(edge_x, edge_y);
        if (length(edge_x) < 0.000001 || length(normal) < 0.000001) { return hidden(); }
        let local_x = normalize(edge_x);
        let local_z = normalize(normal);
        let local_y = cross(local_z, local_x);
        let p = a - 3u;
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

fn grid_lines(p: vec2<f32>, footprint: vec2<f32>, spacing: f32) -> f32 {
    let cell = p / spacing;
    let distance = abs(fract(cell + vec2<f32>(0.5)) - vec2<f32>(0.5)) * spacing;
    let pixels = distance / footprint;
    let coverage = vec2<f32>(1.0) - smoothstep(vec2<f32>(u.line_width * 0.35), vec2<f32>(u.line_width * 0.35 + 1.0), pixels);
    // Fade each line family independently before subpixel cells alias.
    let resolved = vec2<f32>(1.0) - smoothstep(vec2<f32>(0.1), vec2<f32>(0.5), footprint / spacing);
    return max(coverage.x * resolved.x, coverage.y * resolved.y);
}

fn world_grid(pixel: vec2<f32>) -> vec4<f32> {
    // Native Metal framebuffer Y runs down; Camera clip-space Y runs up.
    let ndc = pixel / u.viewport.xy * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    let near_h = u.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let far_h = u.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
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
    let horizon = smoothstep(0.0, 0.04, abs(ray.y) / max(length(ray), 0.000001));
    let visible = select(0.0, 1.0, t > 0.0 && t < 1.0 && u.grid != 0u);
    return vec4<f32>(0.18, 0.35, 0.40, alpha * fade * horizon * visible);
}

@fragment
fn fs_main(in: O) -> @location(0) vec4<f32> {
    // Evaluate derivatives outside divergent control flow, including quads
    // touched by line primitives, so the grid stays valid at the horizon.
    let grid = world_grid(in.p.xy);
    return select(in.color, grid, in.grid != 0u);
}
