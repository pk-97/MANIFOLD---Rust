// Where a cover rule holds (`CoverRule` in material.rs, packed by
// `CoverRule::packed`): on ground between two slopes and two heights, each
// edge softened by its Softness and pushed about by Breakup noise up to
// three Softnesses. A Material's Covers read it per pixel
// (`render_scene.wgsl`); Copies on a surface read it per copy
// (`scatter_on_mesh.wgsl`).

struct CoverRule {
    // x steeper than, y flatter than, z their softness, in radians.
    slope: vec4<f32>,
    // x above, y below, z their softness, in metres.
    height: vec4<f32>,
    // x Breakup, y 1 / Breakup Size.
    breakup: vec4<f32>,
};

fn cover_rule_hash(c: vec2<i32>) -> f32 {
    var h = bitcast<u32>(c.x) * 1664525u + bitcast<u32>(c.y) * 1013904223u + 374761393u;
    h = (h ^ (h >> 15u)) * 2246822519u;
    h = (h ^ (h >> 13u)) * 3266489917u;
    h = h ^ (h >> 16u);
    return f32(h >> 8u) / 16777216.0;
}

// Smooth value noise in [-1, 1], two octaves.
fn cover_rule_noise(p: vec2<f32>) -> f32 {
    var sum = 0.0;
    var q = p;
    var amplitude = 0.65;
    for (var octave = 0; octave < 2; octave++) {
        let c = vec2<i32>(floor(q));
        let f = fract(q);
        let w = f * f * (3.0 - 2.0 * f);
        let a = cover_rule_hash(c);
        let b = cover_rule_hash(c + vec2<i32>(1, 0));
        let d = cover_rule_hash(c + vec2<i32>(0, 1));
        let e = cover_rule_hash(c + vec2<i32>(1, 1));
        sum += amplitude * (mix(mix(a, b, w.x), mix(d, e, w.x), w.y) * 2.0 - 1.0);
        amplitude = 0.35;
        q = q * 2.7 + vec2<f32>(17.3, -5.1);
    }
    return sum;
}

// How much `rule` holds at point `p` on ground whose normal points `up` of
// the way to straight up: 0 to 1.
fn cover_rule_holds(rule: CoverRule, p: vec3<f32>, up: f32) -> f32 {
    let push = 3.0 * rule.breakup.x * cover_rule_noise(p.xz * rule.breakup.y);
    let angle = acos(clamp(up, -1.0, 1.0)) + push * rule.slope.z;
    let steeper = select(smoothstep(rule.slope.x - rule.slope.z, rule.slope.x + rule.slope.z, angle), 1.0, rule.slope.x <= 0.0);
    let flatter = select(1.0 - smoothstep(rule.slope.y - rule.slope.z, rule.slope.y + rule.slope.z, angle), 1.0, rule.slope.y >= 1.5707);
    let y = p.y + push * rule.height.z;
    let above = smoothstep(rule.height.x - rule.height.z, rule.height.x + rule.height.z, y);
    let below = 1.0 - smoothstep(rule.height.y - rule.height.z, rule.height.y + rule.height.z, y);
    return steeper * flatter * above * below;
}
