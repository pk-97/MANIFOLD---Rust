// GPU-only temporal capture for Math View motion trails. The source array is
// read by Metal after the authored graph has written it; the CPU never reads
// or mirrors the sampled positions.
struct Capture { vertex_count: u32, history_slot: u32, history_stride: u32, _pad: u32 };
struct Vertex { position: vec3<f32>, _p: f32, normal: vec3<f32>, _n: f32, uv: vec2<f32>, _u: vec2<f32>, tangent: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Capture;
@group(0) @binding(1) var<storage, read> source: array<Vertex>;
@group(0) @binding(2) var<storage, read_write> history: array<vec4<f32>>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= u.vertex_count) { return; }
    history[u.history_slot * u.history_stride + id.x] = vec4<f32>(source[id.x].position, 1.0);
}
