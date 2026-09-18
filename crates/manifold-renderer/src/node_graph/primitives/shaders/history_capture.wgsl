// GPU-only temporal capture for Math View motion trails. The source arrays are
// read by Metal after the authored graph has written them; the CPU never reads
// or mirrors the sampled positions. One dispatch records both the vertex ring
// and, when instances are wired, the per-copy InstanceTransform ring so copy
// trails can compose historical vertices with the transforms that were live
// in the same frame.
struct Capture {
    vertex_count: u32,
    history_slot: u32,
    history_stride: u32,
    copy_count: u32,
    instance_stride: u32,
    instance_capture: u32,
    _pad: vec2<u32>,
};
struct Vertex { position: vec3<f32>, _p: f32, normal: vec3<f32>, _n: f32, uv: vec2<f32>, _u: vec2<f32>, tangent: vec4<f32> };
struct I { pos_scale: vec4<f32>, rot_pad: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Capture;
@group(0) @binding(1) var<storage, read> source: array<Vertex>;
@group(0) @binding(2) var<storage, read_write> history: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> instances: array<I>;
@group(0) @binding(4) var<storage, read_write> instance_history: array<I>;
@group(0) @binding(5) var<storage, read_write> instance_counts: array<u32>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x < u.vertex_count) {
        history[u.history_slot * u.history_stride + id.x] = vec4<f32>(source[id.x].position, 1.0);
    }
    // Record the compiler's own per-frame instance output next to the vertex
    // sample of the same slot; copy c of slot s composes with vertex s. The
    // per-slot count lets the render pass drop marks for copies a slot never
    // held, so a copy-count change cannot leave phantom trail segments.
    if (u.instance_capture != 0u && id.x < u.copy_count) {
        instance_history[u.history_slot * u.instance_stride + id.x] = instances[id.x];
        if (id.x == 0u) { instance_counts[u.history_slot] = u.copy_count; }
    }
}
