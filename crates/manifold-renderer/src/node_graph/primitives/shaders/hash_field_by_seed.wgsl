// node.hash_field_by_seed — hash an input value-field (RG) with an added
// scalar seed, so the same field re-randomizes as the seed changes. The
// "re-hash a cell-id per beat" atom: feed node.voronoi_cell_id's RG and a
// beat_floor seed to get per-cell randoms that jump each beat.
//   seeded = field.rg + seed * (seed_x, seed_y)
//   Hash2 (mode 0): out.rg = hash2(seeded)   in [0,1]^2
//   Hash1 (mode 1): out.r = hash1(seeded)    in [0,1]
// hash2 / hash1 are verbatim from fx_voronoi_prism. textureLoad (no
// interpolation) keeps the per-cell field exact across cell boundaries.

struct Uniforms {
    seed: f32,
    seed_x: f32,
    seed_y: f32,
    mode: u32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var field_tex: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

fn hash2(p: vec2<f32>) -> vec2<f32> {
    let q = vec2<f32>(
        dot(p, vec2<f32>(127.1, 311.7)),
        dot(p, vec2<f32>(269.5, 183.3)),
    );
    return fract(sin(q) * 43758.5453);
}

fn hash1(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(41.7, 289.3))) * 18743.291);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }
    let field = textureLoad(field_tex, vec2<i32>(id.xy), 0).rg;
    let seeded = field + u.seed * vec2<f32>(u.seed_x, u.seed_y);

    if u.mode == 1u {
        let h = hash1(seeded);
        textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(h, h, h, 1.0));
    } else {
        let h2 = hash2(seeded);
        textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(h2.x, h2.y, 0.0, 1.0));
    }
}
