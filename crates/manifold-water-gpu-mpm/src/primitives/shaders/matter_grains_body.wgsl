// node.matter_grains — BUFFER body. Copy idx is grain j = idx mod K of point
// i = idx / K (K = grains): a sand grain at a hashed offset in the point's
// cube (spread · radius each way), scaled to size · radius times a hashed 0.7
// to 1.3, each hash keyed on the point's id and j, so a grain keeps its place
// and size on the point it rides. Its turn is a hashed start plus the point's
// position over the grain's radius, as a grain rolls: moving x turns it about
// z, moving z about x, falling about both, so still sand stays still and
// moving sand tumbles at its rolling rate.
// A point past the live count, with radius 0 or with no id draws holes.
// `particles` (FluidParticle → Element) is gathered.

fn mg_hash(v: u32) -> u32 {
    var x = v;
    x = x ^ (x >> 16u);
    x = x * 0x7feb352du;
    x = x ^ (x >> 15u);
    x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return x;
}

// Four uniform values in [0, 1) from one key.
fn mg_unit4(key: u32) -> vec4<f32> {
    let a = mg_hash(key);
    let b = mg_hash(a ^ 0x9e3779b9u);
    return vec4<f32>(
        f32(a & 0xffffu),
        f32(a >> 16u),
        f32(b & 0xffffu),
        f32(b >> 16u),
    ) / 65536.0;
}

fn body(idx: u32, count: u32, live_count: f32, grains: i32, size: f32, spread: f32) -> Element2 {
    let k = u32(max(grains, 1));
    let i = idx / k;
    let j = idx - i * k;
    let hole = Element2(vec4<f32>(0.0), vec4<f32>(0.0));
    if live_count >= 0.0 && f32(i) >= live_count {
        return hole;
    }
    let p = buf_particles[i];
    let radius = p.position_radius.w;
    if !(radius > 0.0) || p.id == 0u {
        return hole;
    }
    let key = mg_hash(p.id * 32u + j);
    let place = mg_unit4(key);
    let turn = mg_unit4(key ^ 0x632be5abu);
    let offset = (place.xyz * 2.0 - vec3<f32>(1.0)) * spread * radius;
    let scale = size * radius * (0.7 + 0.6 * place.w);
    let x = p.position_radius.xyz;
    let rolled = vec3<f32>(x.z + 0.5 * x.y, 0.0, 0.5 * x.y - x.x) / scale;
    return Element2(
        vec4<f32>(x + offset, scale),
        vec4<f32>(turn.xyz * 6.2831853 + rolled, 0.0),
    );
}
