// node.liquid_state — removes the particles whose position, radius or
// velocity is not finite. One thread per slot in use, dispatched from the
// identity's [groups, 1, 1] triple (words 4-6); word 7 is the slots in use.
// A removed slot reads as an unused one (radius 0, as the step's own removal
// leaves it) with every value zeroed, so the stats stop counting it. Its id
// is kept and the identity's counters are untouched: survivors keep theirs.
// The exponent-bits test is used because fast math may fold isfinite().

struct Particle { position_radius: vec4<f32>, velocity: vec3<f32>, id: u32 }

@group(0) @binding(0) var<storage, read> identity: array<u32>;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;

fn finite(x: f32) -> bool {
    return (bitcast<u32>(x) & 0x7f800000u) != 0x7f800000u;
}

@compute @workgroup_size(256)
fn remove_nonfinite(@builtin(global_invocation_id) gid: vec3<u32>) {
    let slots = min(identity[7], arrayLength(&particles));
    if gid.x >= slots { return; }
    let x = particles[gid.x].position_radius;
    let v = particles[gid.x].velocity;
    if finite(x.x) && finite(x.y) && finite(x.z) && finite(x.w)
        && finite(v.x) && finite(v.y) && finite(v.z) {
        return;
    }
    particles[gid.x].position_radius = vec4<f32>(0.0);
    particles[gid.x].velocity = vec3<f32>(0.0);
}
