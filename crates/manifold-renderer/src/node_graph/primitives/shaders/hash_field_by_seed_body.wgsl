// node.hash_field_by_seed — fusable body (freeze §12), CoincidentTexel. Hash the
// input value-field's RG with an added scalar seed: seeded = field.rg + seed *
// (seed_x, seed_y). Hash2 (mode 0) → out.rg; Hash1 (mode 1) → out.rgb. `field` is
// read at the OWN texel via integer textureLoad (no interpolation — keeps a per-
// cell-constant field exact across boundaries). hash2/hash1 use GPU sin, matching
// the hand bit-exact. Matches hash_field_by_seed.wgsl. PARAMS: [seed, seed_x,
// seed_y, mode (Enum->u32)].
fn hfs_hash2(p: vec2<f32>) -> vec2<f32> {
    let q = vec2<f32>(
        dot(p, vec2<f32>(127.1, 311.7)),
        dot(p, vec2<f32>(269.5, 183.3)),
    );
    return fract(sin(q) * 43758.5453);
}

fn hfs_hash1(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(41.7, 289.3))) * 18743.291);
}

fn body(c_field: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, seed: f32, seed_x: f32, seed_y: f32, mode: u32) -> vec4<f32> {
    let field = c_field.rg;
    let seeded = field + seed * vec2<f32>(seed_x, seed_y);

    if mode == 1u {
        let h = hfs_hash1(seeded);
        return vec4<f32>(h, h, h, 1.0);
    }
    let h2 = hfs_hash2(seeded);
    return vec4<f32>(h2.x, h2.y, 0.0, 1.0);
}
