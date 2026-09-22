struct UploadParams {
    start: u32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
    values: array<vec4<f32>, 128>,
};

@group(0) @binding(0) var<uniform> params: UploadParams;
@group(0) @binding(1) var<storage, read_write> instances: array<vec4<f32>>;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.count {
        return;
    }
    let source = id.x * 2u;
    let destination = (params.start + id.x) * 2u;
    instances[destination] = params.values[source];
    instances[destination + 1u] = params.values[source + 1u];
}
