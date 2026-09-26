struct UploadParams {
    start: u32,
    count: u32,
    source_count: u32,
    _pad0: u32,
    values: array<vec4<f32>, 250>,
};

@group(0) @binding(0) var<uniform> params: UploadParams;
@group(0) @binding(1) var<storage, read_write> mesh: array<vec4<f32>>;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.count {
        return;
    }

    let destination = (params.start + id.x) * 5u;
    if id.x >= params.source_count {
        mesh[destination] = vec4<f32>(0.0);
        mesh[destination + 1u] = vec4<f32>(0.0);
        mesh[destination + 2u] = vec4<f32>(0.0);
        mesh[destination + 3u] = vec4<f32>(0.0);
        mesh[destination + 4u] = vec4<f32>(0.0);
        return;
    }

    let source = id.x * 5u;
    mesh[destination] = params.values[source];
    mesh[destination + 1u] = params.values[source + 1u];
    mesh[destination + 2u] = params.values[source + 2u];
    mesh[destination + 3u] = params.values[source + 3u];
    mesh[destination + 4u] = params.values[source + 4u];
}
