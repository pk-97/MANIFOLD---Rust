// One pixel of the water G-buffer, 12 bytes. depth: reverse-Z raw depth of the
// nearest field hit, 0 = none. normal_oct: world normal, octahedral, pack2x16snorm.
// thickness_slot: pack2x16float(vec2(thickness in metres along the view ray, slot + 1)).
struct FieldHit {
    depth: f32,
    normal_oct: u32,
    thickness_slot: u32,
};

fn oct_encode(n: vec3<f32>) -> vec2<f32> {
    let p = n.xy / (abs(n.x) + abs(n.y) + abs(n.z));
    if n.z >= 0.0 { return p; }
    return (1.0 - abs(p.yx)) * select(vec2<f32>(-1.0), vec2<f32>(1.0), p >= vec2<f32>(0.0));
}

fn oct_decode(e: vec2<f32>) -> vec3<f32> {
    var n = vec3<f32>(e, 1.0 - abs(e.x) - abs(e.y));
    let t = max(-n.z, 0.0);
    n.x += select(t, -t, n.x >= 0.0);
    n.y += select(t, -t, n.y >= 0.0);
    return normalize(n);
}
