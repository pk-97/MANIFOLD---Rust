// node.mesh_texture_weights — fusable BUFFER body, COINCIDENT `in`, one f32
// weight per vertex. Mapping 0 (Box): triplanar projection of `field`'s red
// channel at the vertex's frame position q = (position + source_offset) /
// frame_radius * scale, three samples on the yz, xz and xy planes blended by
// the normal's fourth power. A function of position and normal only: split
// copies of one corner that share a welded normal (node.smooth_normals) read
// the same weight. Mapping 1 (UV): one sample at the vertex's own UV * scale,
// for an image painted for the mesh's UVs. Coordinates repeat mirrored, so
// any image tiles with no seam.
fn mirrored(t: vec2<f32>) -> vec2<f32> {
    return 1.0 - abs(fract(t * 0.5) * 2.0 - 1.0);
}

fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    tex_field: texture_2d<f32>,
    samp: sampler,
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    bias: f32,
    frame_radius: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    mapping: u32,
) -> f32 {
    let shift = vec2<f32>(offset_x, offset_y);
    if mapping == 1u {
        return textureSampleLevel(tex_field, samp, mirrored(e_in.uv * scale + shift), 0.0).r - bias;
    }
    let offset = vec3<f32>(source_offset_x, source_offset_y, source_offset_z);
    let q = (e_in.position + offset) / max(abs(frame_radius), 1e-6) * scale;
    let n = abs(e_in.normal);
    var w = n * n;
    w = w * w;
    let total = w.x + w.y + w.z;
    w = select(vec3<f32>(1.0 / 3.0), w / total, total > 1e-6);
    let sx = textureSampleLevel(tex_field, samp, mirrored(q.yz + shift), 0.0).r;
    let sy = textureSampleLevel(tex_field, samp, mirrored(q.xz + shift), 0.0).r;
    let sz = textureSampleLevel(tex_field, samp, mirrored(q.xy + shift), 0.0).r;
    return dot(w, vec3<f32>(sx, sy, sz)) - bias;
}
