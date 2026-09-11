// Yu–Turk 2013 connected components, original positions, distance <= ra.
// Native atomic scatter exemption: each invocation processes its incident
// edges; shared parents are monotone and no invocation waits on another.
struct U { connection_radius: f32, count: u32, _pad0: u32, _pad1: u32 };
struct Particle {
    position_mass: vec4<f32>, velocity_density: vec4<f32>,
    affine_x: vec4<f32>, affine_y: vec4<f32>, affine_z: vec4<f32>, previous_position: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> particles: array<Particle>;
@group(0) @binding(2) var<storage, read> heads: array<u32>;
@group(0) @binding(3) var<storage, read> next: array<u32>;
@group(0) @binding(4) var<storage, read_write> parents: array<atomic<u32>>;

fn component_root(start: u32) -> u32 {
    var current = start;
    while (current < u.count) {
        let parent = atomicLoad(&parents[current]);
        if (parent == current) { return current; }
        if (parent >= current) { return 0xffffffffu; }
        current = parent;
    }
    return 0xffffffffu;
}

fn join_components(first: u32, second: u32) {
    var a = first;
    var b = second;
    loop {
        a = component_root(a);
        b = component_root(b);
        if (a == b || a == 0xffffffffu || b == 0xffffffffu) { return; }
        let high = max(a, b);
        let low = min(a, b);
        let previous = atomicMin(&parents[high], low);
        if (previous == high) { return; }
        // A concurrent union changed high. Preserve that connection by
        // joining its previous parent to low before completing this edge.
        // Both candidates are < high: each retry strictly descends.
        a = previous;
        b = low;
    }
}

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i >= u.count || atomicLoad(&parents[i]) == 0xffffffffu) { return; }
    let origin = vec3<f32>(-2.0, 0.0, -2.0);
    let center = particles[i].position_mass.xyz;
    let radius = u.connection_radius;
    let lo = max(vec3<i32>(floor((center-vec3<f32>(radius)-origin)/0.125)), vec3<i32>(0));
    let hi = min(vec3<i32>(floor((center+vec3<f32>(radius)-origin)/0.125)), vec3<i32>(31));
    for (var z=lo.z; z<=hi.z; z++) { for (var y=lo.y; y<=hi.y; y++) { for (var x=lo.x; x<=hi.x; x++) {
        var link = heads[u32(x+32*(y+32*z))];
        var visits = 0u;
        loop {
            if (link == 0u || visits >= u.count) { break; }
            let j = link-1u;
            if (j >= u.count) { break; }
            link = next[j];
            visits++;
            // Process each undirected edge once; inactive slots have no edges.
            if (j <= i || atomicLoad(&parents[j]) == 0xffffffffu) { continue; }
            let delta = particles[j].position_mass.xyz-center;
            if (dot(delta,delta) <= radius*radius) { join_components(i,j); }
        }
    }}}
}
